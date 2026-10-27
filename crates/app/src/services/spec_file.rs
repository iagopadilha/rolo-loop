// SPDX-License-Identifier: MIT
//! View and edit of the `.md` files of `.specs/` (grouped tree, read with a version, write with a version conflict check).
//!
//! Edit rules: editable `features/**`, `codebase/**`, `project/PROJECT.md`, `FLOWS.md`, `RULES.md` and `TEST-ENVIRONMENT.md`; `ROADMAP.md` and
//! `STATE.md` only with the loop stopped; `loop/json` and `loop/internal` never (not even read). Every path goes through the repository's safe
//! resolver (no `..`, absolute path or symbolic link).

use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use crate::repositories::specs_file::{compute_bytes_version, split_relative_path};
use engine_state::loop_state;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const EDITABLE_EXTENSION: &str = ".md";
const LOOP_DIR_RELATIVE: &str = "project/loop";
/// `.specs/.trash/`: what "Reset specs" moved out; never listed or read.
const TRASH_DIR_RELATIVE: &str = ".trash";
pub const FORBIDDEN_DIRS: [&str; 3] = ["project/loop/json", "project/loop/internal", TRASH_DIR_RELATIVE];
const EDITABLE_PROJECT_FILES: [&str; 4] = ["project/PROJECT.md", "project/FLOWS.md", "project/RULES.md", "project/TEST-ENVIRONMENT.md"];
const EDITABLE_WITH_LOOP_STOPPED_FILES: [&str; 2] = ["project/ROADMAP.md", "project/STATE.md"];
/// features/<slug>/<file>.md
const FEATURE_FILE_MIN_DEPTH: usize = 3;
/// project/loop/<file>.md
const LOOP_ROOT_FILE_DEPTH: usize = 3;
pub const CONTENT_CHARACTER_LIMIT: usize = 1024 * 1024;
pub const READ_ONLY_REASON: &str = "read-only: written by the loop/orchestrator";
pub const LOOP_RUNNING_REASON: &str = "loop running: ROADMAP.md and STATE.md can only be edited with the loop stopped";

/// `(editable, reason)` for a normalized path (`a/b.md`) relative to `.specs/`.
pub fn evaluate_edit(relative: &str, loop_running: bool) -> (bool, &'static str) {
    let parts: Vec<&str> = relative.split('/').collect();
    if parts[0] == "features" && parts.len() >= FEATURE_FILE_MIN_DEPTH {
        return (true, "");
    }
    if parts[0] == "codebase" && parts.len() >= 2 {
        return (true, "");
    }
    if EDITABLE_PROJECT_FILES.contains(&relative) {
        return (true, "");
    }
    if EDITABLE_WITH_LOOP_STOPPED_FILES.contains(&relative) {
        return if loop_running { (false, LOOP_RUNNING_REASON) } else { (true, "") };
    }
    (false, READ_ONLY_REASON)
}

/// Case-insensitive: on Windows `Project/Loop/JSON` is the same folder.
pub fn is_in_forbidden_dir(relative: &str) -> bool {
    let text = relative.to_lowercase();
    FORBIDDEN_DIRS.iter().any(|folder| {
        let folder = folder.to_lowercase();
        text == folder || text.starts_with(&format!("{folder}/"))
    })
}

pub struct SpecFileService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn spec_files(&self) -> SpecFileService<'_> {
        SpecFileService { app: self }
    }
}

impl SpecFileService<'_> {
    // ------------------------------------------------ tree
    pub fn build_specs_tree(&self, context: &ProjectContext) -> Value {
        let specs_dir = context.specs_dir();
        let loop_running = self.app.progress().is_loop_running(context);
        let relatives = self.app.specs.list_markdown_files(Path::new(&specs_dir), &FORBIDDEN_DIRS);
        let roadmap_text = self.app.specs.read_text(&Path::new(&specs_dir).join("project").join("ROADMAP.md")).unwrap_or_default();
        let (roadmap_features, _) = loop_state::parse_roadmap(&roadmap_text);
        let (mut project, mut codebase, mut loop_files, mut other): (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) = Default::default();
        let mut files_by_feature: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for relative in &relatives {
            let item = self.build_file_item(&specs_dir, relative, loop_running);
            let parts: Vec<&str> = relative.split('/').collect();
            if parts[0] == "features" && parts.len() >= FEATURE_FILE_MIN_DEPTH {
                files_by_feature.entry(parts[1].to_string()).or_default().push(item);
            } else if relative.starts_with(&format!("{LOOP_DIR_RELATIVE}/")) {
                if parts.len() == LOOP_ROOT_FILE_DEPTH {
                    loop_files.push(item); // only the .md files at the loop root (ANSWERS, VALIDATE, DEPENDENCIES, GRAPH)
                }
            } else if parts[0] == "project" {
                project.push(item);
            } else if parts[0] == "codebase" {
                codebase.push(item);
            } else {
                other.push(item);
            }
        }
        json!({
            "exists": Path::new(&specs_dir).is_dir(), "specs_dir": specs_dir, "loop_running": loop_running,
            "project": project, "codebase": codebase, "features": Self::build_features(&roadmap_features, &files_by_feature),
            "loop": loop_files, "other": other,
        })
    }

    /// ROADMAP features (in its order) and then the folders without a ROADMAP line (material ready to adopt).
    fn build_features(roadmap_features: &loop_state::Features, files_by_feature: &BTreeMap<String, Vec<Value>>) -> Vec<Value> {
        let mut features = Vec::new();
        for (slug, fields) in roadmap_features.iter() {
            // the folder is the slug with ':' as '-'
            let files = files_by_feature.get(&loop_state::feature_folder_name(slug)).or_else(|| files_by_feature.get(slug.as_str())).cloned().unwrap_or_default();
            features.push(json!({"slug": slug, "status": fields.get("status"), "stage": fields.text("stage"), "in_roadmap": true, "files": files}));
        }
        let mut claimed: BTreeSet<String> = roadmap_features.iter().map(|(slug, _)| loop_state::feature_folder_name(slug)).collect();
        claimed.extend(roadmap_features.slugs());
        for (slug, files) in files_by_feature.iter().filter(|(slug, _)| !claimed.contains(*slug)) {
            features.push(json!({"slug": slug, "status": null, "stage": "", "in_roadmap": false, "files": files}));
        }
        features
    }

    fn build_file_item(&self, specs_dir: &str, relative: &str, loop_running: bool) -> Value {
        let (editable, reason) = evaluate_edit(relative, loop_running);
        let mut item = Map::new();
        item.insert("path".into(), json!(relative));
        item.insert("name".into(), json!(relative.rsplit('/').next().unwrap_or(relative)));
        item.insert("editable".into(), json!(editable));
        item.insert("not_editable_reason".into(), if reason.is_empty() { Value::Null } else { json!(reason) });
        let target = relative.split('/').fold(Path::new(specs_dir).to_path_buf(), |path, part| path.join(part));
        if let Value::Object(info) = self.app.specs.get_file_info(&target) {
            item.extend(info);
        }
        Value::Object(item)
    }

    // ------------------------------------------------ read and write
    pub fn read_spec_file(&self, context: &ProjectContext, path: &str) -> Result<Value, AppError> {
        let (relative, target) = self.resolve_markdown(context, path)?;
        let content = self.app.specs.read_bytes(&target);
        let Some(content) = content.filter(|_| target.is_file()) else {
            return Err(AppError::new(Kind::SpecFileNotFound, format!("file not found in .specs/: {relative}")));
        };
        let (editable, reason) = evaluate_edit(&relative, self.app.progress().is_loop_running(context));
        Ok(json!({
            "path": relative, "text": String::from_utf8_lossy(&content), "version": compute_bytes_version(&content),
            "editable": editable, "not_editable_reason": if reason.is_empty() { Value::Null } else { json!(reason) },
        }))
    }

    pub fn write_spec_file(&self, context: &ProjectContext, path: &str, content: &Value, version: &Value) -> Result<Value, AppError> {
        let Some(content) = content.as_str() else { return Err(AppError::invalid_request("content must be text")) };
        if content.chars().count() > CONTENT_CHARACTER_LIMIT {
            return Err(AppError::invalid_request(format!("content longer than {CONTENT_CHARACTER_LIMIT} characters")));
        }
        let (relative, target) = self.resolve_markdown(context, path)?;
        let (editable, reason) = evaluate_edit(&relative, self.app.progress().is_loop_running(context));
        if !editable {
            return Err(if reason == LOOP_RUNNING_REASON { AppError::loop_running(reason) } else { AppError::new(Kind::FileEditRefused, format!("{relative}: {reason}")) });
        }
        let current = self.app.specs.read_bytes(&target);
        Self::check_version(&relative, current.as_deref(), version)?;
        self.app.specs.write_text_atomic(&target, content).map_err(|e| AppError::invalid_path(format!("cannot write {relative}: {e}")))?;
        let written = self.app.specs.read_bytes(&target).unwrap_or_default();
        self.app.log.info("spec_file_written", &[("project", json!(context.project.id)), ("path", json!(relative)), ("created", json!(current.is_none()))]);
        Ok(json!({"path": relative, "version": compute_bytes_version(&written), "created": current.is_none()}))
    }

    /// An existing file requires the version that was read (sha1); a new file requires an empty version.
    fn check_version(relative: &str, current: Option<&[u8]>, version: &Value) -> Result<(), AppError> {
        let Some(current) = current else {
            if !(version.is_null() || version == "") {
                return Err(AppError::new(Kind::FileVersionConflict, format!("the file {relative} no longer exists (it was deleted after it was read)")));
            }
            return Ok(());
        };
        let current_version = compute_bytes_version(current);
        if version.as_str() != Some(current_version.as_str()) {
            return Err(AppError::version_conflict(format!("the file {relative} changed since it was read: reload before saving"), &current_version));
        }
        Ok(())
    }

    /// `(normalized relative path, absolute path)` of a `.md` outside the forbidden folders, or `PathOutsideSpecs`.
    pub fn resolve_markdown(&self, context: &ProjectContext, path: &str) -> Result<(String, std::path::PathBuf), AppError> {
        let relative = split_relative_path(path)?.join("/");
        if is_in_forbidden_dir(&relative) {
            return Err(AppError::new(Kind::PathOutsideSpecs, "loop/json and loop/internal are not accessible here"));
        }
        if !relative.to_lowercase().ends_with(EDITABLE_EXTENSION) {
            return Err(AppError::new(Kind::PathOutsideSpecs, "only .md files of .specs/"));
        }
        let target = self.app.specs.resolve_path_inside_specs(&context.specs_dir(), &relative)?;
        Ok((relative, target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_rules_and_forbidden_dirs() {
        assert_eq!(evaluate_edit("features/a/spec.md", false), (true, ""));
        assert_eq!(evaluate_edit("project/ROADMAP.md", true), (false, LOOP_RUNNING_REASON));
        assert_eq!(evaluate_edit("project/loop/VALIDATE.md", false), (false, READ_ONLY_REASON));
        assert!(is_in_forbidden_dir("Project/Loop/JSON/x.md") && is_in_forbidden_dir(".trash") && !is_in_forbidden_dir("project/loop/json2/x.md"));
    }

    #[test]
    fn tree_read_and_versioned_write() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        let specs = repo.join(".specs");
        std::fs::create_dir_all(specs.join("features/orphan")).unwrap();
        std::fs::create_dir_all(specs.join("project/loop/internal")).unwrap();
        std::fs::write(specs.join("features/orphan/spec.md"), "x").unwrap();
        std::fs::write(specs.join("project/PROJECT.md"), "p").unwrap();
        std::fs::write(specs.join("project/loop/internal/secret.md"), "s").unwrap();
        std::fs::write(specs.join("project/loop/VALIDATE.md"), "v").unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let service = app.spec_files();
        let tree = service.build_specs_tree(&context);
        assert_eq!((tree["project"].as_array().unwrap().len(), tree["loop"].as_array().unwrap().len()), (1, 1));
        assert_eq!(tree["features"][0], json!({"slug": "orphan", "status": null, "stage": "", "in_roadmap": false, "files": tree["features"][0]["files"]}));
        assert_eq!(service.read_spec_file(&context, "project/loop/internal/secret.md").unwrap_err().kind, Kind::PathOutsideSpecs);
        assert_eq!(service.read_spec_file(&context, "notes.txt").unwrap_err().kind, Kind::PathOutsideSpecs);
        assert_eq!(service.read_spec_file(&context, "project/NOPE.md").unwrap_err().kind, Kind::SpecFileNotFound);
        let read = service.read_spec_file(&context, "project/PROJECT.md").unwrap();
        assert_eq!(read["text"], "p");
        let version = read["version"].clone();
        let written = service.write_spec_file(&context, "project/PROJECT.md", &json!("new"), &version).unwrap();
        assert_eq!(written["created"], false);
        let stale = service.write_spec_file(&context, "project/PROJECT.md", &json!("again"), &version).unwrap_err();
        assert_eq!((stale.kind, stale.current_version.is_some()), (Kind::FileVersionConflict, true));
        assert_eq!(service.write_spec_file(&context, "project/loop/VALIDATE.md", &json!("z"), &json!("")).unwrap_err().kind, Kind::FileEditRefused);
        assert_eq!(service.write_spec_file(&context, "features/orphan/new.md", &json!("n"), &json!("abc")).unwrap_err().kind, Kind::FileVersionConflict);
        assert_eq!(service.write_spec_file(&context, "features/orphan/new.md", &json!("n"), &json!("")).unwrap()["created"], true);
        assert_eq!(service.write_spec_file(&context, "features/orphan/new.md", &json!(5), &json!("")).unwrap_err().message, "content must be text");
    }
}
