// SPDX-License-Identifier: MIT
//! Reset specs: wipes the planning/loop state of a project so new features start from zero, WITHOUT deleting anything.
//!
//! Everything that is reset is MOVED to `<work>/.specs/.trash/<YYYYMMDD-HHMMSS>/` keeping the paths relative to `.specs/` (so the user can put it
//! back by hand). Then the files that have an app template (ROADMAP, FLOWS, STATE) are born again from `templates/specs` and `features/` is
//! recreated empty. Which paths move and which stay is declared in the two constants below (`RESET_MOVED_PATHS`, `RESET_KEPT_PATHS`) so the policy
//! is easy to read and change.
//!
//! The specs that are reset are ALWAYS the primary checkout's (`context.repo_path()`), even when a loop already ran and the work dir is the loop
//! worktree: right after the reset, the loop worktree(s) of the project are DELETED too (folder + local `loop/*` branch, see
//! `loop_worktree_removal`; no merge, no commit, no push, no backup), so the project goes back to planning on the primary's `.specs/`. A worktree
//! that cannot be removed is reported in `worktree_failures` and never aborts the reset.
//!
//! Safety: refused while a loop or a claude job runs in the project; only paths under `<primary>/.specs/` are touched; a symbolic link (or
//! junction) is never followed and never moved (it stays where it is); `.specs/.trash/` is git-ignored and the app never reads it as part of the
//! plan or the loop. Layout: the current one (`project/loop/{json,internal}`); old flat layouts are migrated by the engine before the loop runs.

use super::App;
use crate::domain::errors::{AppError, Kind, LOOP_RUNNING_CODE, PLANNING_BUSY_CODE};
use crate::domain::project::ProjectContext;
use crate::repositories::specs_file::{is_inside_folder, is_link_or_junction};
use engine_state::loop_state;
use pycompat::{nt_abspath, nt_normcase};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// MOVED to the trash (relative to the work dir). `.specs/project/loop` moves whole, except what is in the kept list below.
const RESET_MOVED_PATHS: [&str; 7] = [
    ".specs/features",           // recreated empty
    ".specs/project/ROADMAP.md", // recreated from the template
    ".specs/project/FLOWS.md",   // recreated from the template
    ".specs/project/STATE.md",   // recreated from the template
    ".specs/project/phases",
    ".specs/project/archive",
    ".specs/project/loop", // approval, answers, graph, turns, snapshots, chat, jobs, planning thread, logs, LOOP.json...
];
/// KEPT, never touched (relative to the work dir). Paths that do not exist are simply left out of the response.
const RESET_KEPT_PATHS: [&str; 9] = [
    ".specs/project/RULES.md",
    ".specs/project/PROJECT.md",
    ".specs/project/TEST-ENVIRONMENT.md",
    ".specs/project/REPOS.json", // the repo list of a multi-repo project: a reset never forgets it
    ".specs/project/.env.test",
    ".specs/project/ACCESS.json", // the user's Test environment choices (Grafana read, shared fallback): a reset never forgets them
    "docs/codebase",
    ".specs/codebase", // legacy location of the codebase map
    "CLAUDE.md",
];
/// Files of the loop folder that stay (resolved with `loop_state::loop_path`, so the folder layout lives in one place). `last-run.json`: how to
/// re-attach after Close loop.
pub const KEPT_LOOP_FILE_NAMES: [&str; 4] = ["rules.tests.json", "PERMISSIONS.json", "SCOPE.json", "last-run.json"];
/// Written by the app itself and deleted by it on shutdown: neither moved nor listed.
const LEFT_ALONE_LOOP_FILE_NAMES: [&str; 1] = ["MONITOR.json"];
const RECREATED_TEMPLATE_FILES: [&str; 3] = ["ROADMAP.md", "FLOWS.md", "STATE.md"];
const FEATURES_RELATIVE_DIR: &str = ".specs/features";

fn to_posix(path: &str) -> String {
    path.replace('\\', "/")
}

fn normalize(path: &Path) -> String {
    nt_normcase(&nt_abspath(&path.display().to_string()))
}

/// True when the folder has at least one file (or link) somewhere inside; folders of empty folders are not content.
fn contains_files(folder: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(folder) else { return false };
    for entry in entries.flatten() {
        let is_real_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_real_dir {
            if contains_files(&entry.path()) {
                return true;
            }
        } else if !(is_link_or_junction(&entry.path()) && entry.path().is_dir()) {
            return true;
        }
    }
    false
}

fn relative_to(base: &Path, path: &Path) -> String {
    to_posix(&path.strip_prefix(base).map(|p| p.display().to_string()).unwrap_or_else(|_| path.display().to_string()))
}

pub struct SpecsResetService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn specs_reset(&self) -> SpecsResetService<'_> {
        SpecsResetService { app: self }
    }
}

impl SpecsResetService<'_> {
    /// Moves the planning/loop state of the primary checkout to the trash, recreates the templates, then deletes the loop worktrees. Returns
    /// `{"ok", "moved", "trash", "kept", "worktrees_removed", "worktree_failures"}`. `trash` is `null` when there was nothing to move (and nothing
    /// was created).
    pub fn reset_specs(&self, context: &ProjectContext, confirm: &Value) -> Result<Value, AppError> {
        self.ensure_confirmed(context, confirm)?;
        let (moved, trash_relative, worktrees, work);
        {
            let _guard = self.app.specs_reset_lock.lock().unwrap_or_else(|e| e.into_inner());
            self.ensure_can_reset(context)?;
            work = context.repo_path().to_string(); // the primary's plan, never the loop worktree's (that one is deleted below)
            (moved, trash_relative) = self.reset_specs_folder(context, &work)?;
            worktrees = self.app.loop_worktree_removal().remove_loop_worktrees(context);
        }
        self.app.log.info(
            "specs_reset",
            &[
                ("project", json!(context.project.id)),
                ("moved", json!(moved.len())),
                ("trash", json!(trash_relative)),
                ("worktrees_removed", json!(worktrees["worktrees_removed"].as_array().map_or(0, Vec::len))),
                ("worktree_failures", json!(worktrees["worktree_failures"].as_array().map_or(0, Vec::len))),
            ],
        );
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("moved".into(), json!(moved));
        out.insert("trash".into(), json!(trash_relative));
        out.insert("kept".into(), json!(Self::list_kept(&work)));
        out.extend(worktrees.as_object().cloned().unwrap_or_default());
        Ok(Value::Object(out))
    }

    /// Read only, for the confirmation: the loop worktrees the reset would delete (path, branch, unpushed commits or `null`) and the ones it would
    /// leave alone. `{"ok", "worktrees": [...], "skipped": [...]}`.
    pub fn preview_reset(&self, context: &ProjectContext) -> Value {
        let mut preview = self.app.loop_worktree_removal().preview_removable_worktrees(context);
        preview["ok"] = json!(true);
        preview
    }

    /// `(moved, trash relative path)` of the `.specs/` reset of `work`; `([], None)` when there is no `.specs/` or nothing to move.
    fn reset_specs_folder(&self, context: &ProjectContext, work: &str) -> Result<(Vec<String>, Option<String>), AppError> {
        let specs_dir = Path::new(work).join(".specs");
        if !specs_dir.is_dir() {
            return Ok((vec![], None));
        }
        if is_link_or_junction(&specs_dir) {
            return Err(AppError::new(Kind::PathOutsideSpecs, ".specs/ is a symbolic link (or junction): refusing to reset outside the project"));
        }
        let templates = self.build_templates(work, &context.project.name);
        let sources = self.collect_sources(work, &specs_dir, &templates);
        let (mut moved, mut trash_relative) = (vec![], None);
        if !sources.is_empty() {
            let trash_dir = self.create_trash_dir(&specs_dir)?;
            trash_relative = Some(relative_to(Path::new(work), &trash_dir));
            moved = self.move_to_trash(work, &specs_dir, &trash_dir, &sources)?;
        }
        self.recreate_state(work, &templates);
        Ok((moved, trash_relative))
    }

    // ------------------------------------------------ guards
    fn ensure_confirmed(&self, context: &ProjectContext, confirm: &Value) -> Result<(), AppError> {
        if confirm.as_str().map(str::trim) != Some(context.project.name.as_str()) {
            return Err(AppError::invalid_request(format!("confirm must be the project name ({})", context.project.name)));
        }
        Ok(())
    }

    fn ensure_can_reset(&self, context: &ProjectContext) -> Result<(), AppError> {
        if self.app.progress().is_loop_running(context) {
            return Err(AppError::loop_running("loop running: stop the loop before resetting the specs").with_code(LOOP_RUNNING_CODE));
        }
        if self.app.jobs().get_running_job(context.repo_path()).is_some() {
            return Err(
                AppError::new(Kind::JobInProgress, "a claude job (planning reply, roadmap or map) is running: wait for it to finish").with_code(PLANNING_BUSY_CODE)
            );
        }
        Ok(())
    }

    // ------------------------------------------------ plan
    /// `[(absolute path, text)]` of the files the reset recreates, from the same templates as the structure initialization.
    fn build_templates(&self, work: &str, project_name: &str) -> Vec<(PathBuf, String)> {
        let project_dir = Path::new(work).join(".specs").join("project");
        let built = self.app.specs_structure().build_template_files(work, project_name);
        RECREATED_TEMPLATE_FILES.iter().filter_map(|name| built.iter().find(|(path, _)| *path == project_dir.join(name)).cloned()).collect()
    }

    /// Absolute paths the reset must never move: the kept list plus the loop files that stay or belong to the app.
    fn list_protected_paths(work: &str) -> BTreeSet<String> {
        let mut protected: Vec<PathBuf> = RESET_KEPT_PATHS.iter().map(|relative| relative.split('/').fold(PathBuf::from(work), |p, part| p.join(part))).collect();
        protected.extend(KEPT_LOOP_FILE_NAMES.iter().chain(LEFT_ALONE_LOOP_FILE_NAMES.iter()).map(|name| loop_state::loop_path(work, name)));
        protected.iter().map(|p| normalize(p)).collect()
    }

    fn collect_sources(&self, work: &str, specs_dir: &Path, templates: &[(PathBuf, String)]) -> Vec<PathBuf> {
        let protected = Self::list_protected_paths(work);
        let normalized: Vec<(String, &String)> = templates.iter().map(|(path, text)| (normalize(path), text)).collect();
        let mut sources = Vec::new();
        for relative in RESET_MOVED_PATHS {
            self.collect(&relative.split('/').fold(PathBuf::from(work), |p, part| p.join(part)), specs_dir, &protected, &normalized, &mut sources);
        }
        sources
    }

    /// Adds to `sources` what must move: the whole entry, or (when something protected lives inside it) its children.
    fn collect(&self, path: &Path, specs_dir: &Path, protected: &BTreeSet<String>, templates: &[(String, &String)], sources: &mut Vec<PathBuf>) {
        if std::fs::symlink_metadata(path).is_err() || protected.contains(&normalize(path)) {
            return;
        }
        if is_link_or_junction(path) || !is_inside_folder(&specs_dir.display().to_string(), &path.display().to_string()) {
            self.app.log.warn("specs_reset_skipped_link", &[("path", json!(path.display().to_string()))]);
            return;
        }
        if path.is_dir() {
            let prefix = format!("{}\\", normalize(path));
            if protected.iter().any(|item| item.starts_with(&prefix)) {
                let mut names: Vec<String> =
                    std::fs::read_dir(path).map(|entries| entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
                names.sort();
                for name in names {
                    self.collect(&path.join(name), specs_dir, protected, templates, sources);
                }
            } else if contains_files(path) {
                sources.push(path.to_path_buf());
            }
            return;
        }
        if let Some((_, template)) = templates.iter().find(|(key, _)| *key == normalize(path)) {
            if self.app.specs.read_text(path).as_deref() == Some(template.as_str()) {
                return; // already the pristine template: nothing to clean (keeps the reset idempotent)
            }
        }
        sources.push(path.to_path_buf());
    }

    // ------------------------------------------------ act
    fn create_trash_dir(&self, specs_dir: &Path) -> Result<PathBuf, AppError> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let stamp = engine_state::loop_io::local_from_epoch(now)
            .map(engine_state::loop_io::iso_seconds)
            .map(|iso| format!("{}{}{}-{}{}{}", &iso[..4], &iso[5..7], &iso[8..10], &iso[11..13], &iso[14..16], &iso[17..19]))
            .unwrap_or_default();
        let trash_root = specs_dir.join(".trash");
        if is_link_or_junction(&trash_root) {
            return Err(AppError::new(Kind::PathOutsideSpecs, ".specs/.trash is a symbolic link (or junction): refusing to move files into it"));
        }
        let (mut candidate, mut counter) = (trash_root.join(&stamp), 1);
        while candidate.exists() {
            counter += 1; // two resets in the same second
            candidate = trash_root.join(format!("{stamp}-{counter}"));
        }
        std::fs::create_dir_all(&candidate).map_err(|failure| AppError::new(Kind::Internal, format!("cannot create the trash folder: {failure}")))?;
        Ok(candidate)
    }

    fn move_to_trash(&self, work: &str, specs_dir: &Path, trash_dir: &Path, sources: &[PathBuf]) -> Result<Vec<String>, AppError> {
        let mut moved = Vec::new();
        for source in sources {
            let relative = source.strip_prefix(specs_dir).unwrap_or(source);
            let destination = trash_dir.join(relative);
            let outcome = destination.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::rename(source, &destination));
            if let Err(failure) = outcome {
                // a locked file on Windows: what moved so far is in the trash and logged
                self.app.log.error(
                    "specs_reset_move_failed",
                    &[
                        ("path", json!(source.display().to_string())),
                        ("message", json!(failure.to_string().chars().take(200).collect::<String>())),
                        ("moved", json!(moved.len())),
                    ],
                );
                return Err(AppError::new(Kind::Internal, format!("cannot move {} to the trash: {failure}", source.display())));
            }
            moved.push(relative_to(Path::new(work), source));
        }
        Ok(moved)
    }

    /// Born clean again: templates where they exist, `features/` empty, the loop folders the engine expects.
    fn recreate_state(&self, work: &str, templates: &[(PathBuf, String)]) {
        let _ = std::fs::create_dir_all(FEATURES_RELATIVE_DIR.split('/').fold(PathBuf::from(work), |p, part| p.join(part)));
        for folder in super::specs_structure::SpecsStructureService::list_structure_folders(work) {
            let _ = std::fs::create_dir_all(folder);
        }
        for (path, text) in templates {
            let _ = self.app.specs.create_file_if_missing(path, text);
        }
        self.app.specs_structure().ensure_specs_trash_in_gitignore(work);
    }

    fn list_kept(work: &str) -> Vec<String> {
        let mut kept: Vec<PathBuf> = RESET_KEPT_PATHS.iter().map(|relative| relative.split('/').fold(PathBuf::from(work), |p, part| p.join(part))).collect();
        kept.extend(KEPT_LOOP_FILE_NAMES.iter().map(|name| loop_state::loop_path(work, name)));
        kept.iter().filter(|p| std::fs::symlink_metadata(p).is_ok()).map(|p| relative_to(Path::new(work), p)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_moves_to_the_trash_and_kept_files_stay() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("proj");
        let specs = repo.join(".specs");
        std::fs::create_dir_all(specs.join("features/a")).unwrap();
        std::fs::create_dir_all(specs.join("project/loop/json")).unwrap();
        std::fs::write(specs.join("features/a/spec.md"), "spec").unwrap();
        std::fs::write(specs.join("project/ROADMAP.md"), "- [a] status=verified\n").unwrap();
        std::fs::write(specs.join("project/PROJECT.md"), "keep me").unwrap();
        std::fs::write(specs.join("project/loop/json/LOOP.json"), "{}").unwrap();
        std::fs::write(specs.join("project/loop/json/PERMISSIONS.json"), "{}").unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let service = app.specs_reset();
        assert_eq!(service.reset_specs(&context, &json!("wrong")).unwrap_err().message, "confirm must be the project name (proj)");
        let out = service.reset_specs(&context, &json!(" proj ")).unwrap();
        assert_eq!(out["ok"], true);
        let moved: Vec<&str> = out["moved"].as_array().unwrap().iter().map(|m| m.as_str().unwrap()).collect();
        assert!(moved.contains(&".specs/features") && moved.contains(&".specs/project/ROADMAP.md") && moved.contains(&".specs/project/loop/json/LOOP.json"), "{moved:?}");
        assert!(!moved.iter().any(|m| m.contains("PERMISSIONS")));
        let trash = repo.join(out["trash"].as_str().unwrap());
        assert_eq!(std::fs::read_to_string(trash.join("features/a/spec.md")).unwrap(), "spec");
        assert_eq!(std::fs::read_to_string(specs.join("project/PROJECT.md")).unwrap(), "keep me");
        assert!(specs.join("features").is_dir() && specs.join("project/loop/json/PERMISSIONS.json").is_file());
        assert!(out["kept"].as_array().unwrap().iter().any(|k| k == ".specs/project/PROJECT.md"));
        assert!(!std::fs::read_to_string(specs.join("project/ROADMAP.md")).unwrap().contains("[a]"));
        // a second reset finds the pristine templates and moves nothing
        let again = service.reset_specs(&context, &json!("proj")).unwrap();
        assert_eq!((again["moved"].clone(), again["trash"].clone()), (json!([]), Value::Null));
        assert_eq!(service.preview_reset(&context)["ok"], true);
    }
}
