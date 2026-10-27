// SPDX-License-Identifier: MIT
//! Standards slots of a repository or of one subproject: the `## Standards` section of a `CLAUDE.md` (STD-04).
//!
//! A generic dev skill only knows the slot name; the company skill that fills it is bound here, in the project. This service holds the logic of the
//! `standards` routes, mirroring the Conformance ones: the same target resolution (primary or secondary repo, loop worktree when there is one), the same
//! refusal while a loop runs (409 `loop_running`) and, by rule R-03, no section at the root of a monorepo (it lives in each subproject's `CLAUDE.md`).
//! The file format is owned by the engine's `standards`; nothing here parses or renders it.

use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use crate::repositories::project_files::Read;
use engine_state::{standards, subprojects};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub const INVALID_STANDARDS_CODE: &str = "invalid_standards";
pub const MONOREPO_ROOT_STANDARDS_CODE: &str = "monorepo_root_standards";
pub const MONOREPO_ROOT_STANDARDS_MESSAGE: &str = "this repository is a monorepo: bind the Standards slots per subproject";
pub const FOLDER_MISSING_CODE: &str = "folder_missing";

/// An unknown slot, a value that is not a skill name or a body that is not a mapping (400, `code` = invalid_standards).
fn invalid_standards(message: impl Into<String>) -> AppError {
    AppError::invalid_request(message).with_code(INVALID_STANDARDS_CODE)
}

pub struct StandardsService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn standards(&self) -> StandardsService<'_> {
        StandardsService { app: self }
    }
}

impl StandardsService<'_> {
    // ------------------------------------------------ folder level
    /// `{"bindings", "notes", "slots"}` of `<folder>/CLAUDE.md`.
    pub fn get(&self, folder: &str) -> Value {
        let found = standards::read(Path::new(folder));
        json!({
            "bindings": Value::Object(found.bindings.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<Map<String, Value>>()),
            "notes": found.notes,
            "slots": standards::SLOTS,
        })
    }

    /// Replaces the bindings of `<folder>/CLAUDE.md` (an empty value clears its slot; an empty mapping removes the section).
    pub fn put(&self, context: &ProjectContext, folder: &str, bindings: &Value) -> Result<Value, AppError> {
        let validated = Self::validate(bindings)?;
        self.app.subprojects().ensure_loop_stopped(context)?;
        if !Path::new(folder).is_dir() {
            return Err(AppError::new(Kind::InvalidRepo, format!("the folder does not exist: {folder}")).with_code(FOLDER_MISSING_CODE));
        }
        let _ = standards::write(Path::new(folder), &validated);
        let mut slots: Vec<String> = validated.iter().map(|(k, _)| k.clone()).collect();
        slots.sort();
        self.app.log.info("standards_set", &[("folder", json!(folder)), ("slots", json!(slots))]);
        Ok(self.get(folder))
    }

    /// The mapping without its empty values; a 400 `invalid_standards` for anything else the engine refuses.
    fn validate(bindings: &Value) -> Result<Vec<(String, String)>, AppError> {
        let Some(map) = bindings.as_object() else { return Err(invalid_standards("bindings must be an object {slot: skill}")) };
        let pairs: Vec<(String, String)> = map
            .iter()
            .map(|(slot, value)| {
                let text = match value {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => pycompat::json::py_json_dumps(other, false),
                };
                (slot.clone(), text)
            })
            .collect();
        standards::validate(&pairs).map_err(invalid_standards)?;
        Ok(pairs
            .into_iter()
            .filter_map(|(slot, value)| {
                let trimmed = value.trim().to_string();
                (!trimmed.is_empty()).then_some((slot, trimmed))
            })
            .collect())
    }

    // ------------------------------------------------ repository root
    fn resolve_repo(&self, context: &ProjectContext, repo_name: Option<&str>) -> Result<(String, String), AppError> {
        self.app.subprojects().resolve_target(context, repo_name)
    }

    fn is_confirmed_monorepo(&self, context: &ProjectContext, key: &str) -> bool {
        matches!(self.app.subprojects_files().read_repo_entry(&context.work_dir(), key), Read::Data(entry) if entry.get("monorepo") == Some(&Value::Bool(true)) && entry.get("confirmed") == Some(&Value::Bool(true)))
    }

    pub fn get_repo(&self, context: &ProjectContext, repo_name: Option<&str>) -> Result<Value, AppError> {
        let (_, folder) = self.resolve_repo(context, repo_name)?;
        Ok(self.get(&folder))
    }

    pub fn put_repo(&self, context: &ProjectContext, repo_name: Option<&str>, bindings: &Value) -> Result<Value, AppError> {
        let (key, folder) = self.resolve_repo(context, repo_name)?;
        if self.is_confirmed_monorepo(context, &key) {
            // rule R-03: the section lives in each subproject, never in the root CLAUDE.md
            return Err(AppError::new(Kind::RepoConflict, MONOREPO_ROOT_STANDARDS_MESSAGE).with_code(MONOREPO_ROOT_STANDARDS_CODE));
        }
        self.put(context, &folder, bindings)
    }

    // ------------------------------------------------ one subproject
    fn resolve_subproject(&self, context: &ProjectContext, path: &str, repo_name: Option<&str>) -> Result<PathBuf, AppError> {
        let (_, folder, entry) = self.app.subprojects().require_confirmed(context, repo_name)?;
        let project = super::subprojects::SubprojectsService::find_active_project(&entry, path)?;
        let relative = subprojects::normalize(project.get("path").and_then(Value::as_str).unwrap_or(""));
        let dir = relative.split('/').fold(PathBuf::from(&folder), |a, p| a.join(p));
        if !dir.is_dir() {
            return Err(AppError::invalid_path(format!("the subproject folder does not exist: {relative}")));
        }
        Ok(dir)
    }

    pub fn get_subproject(&self, context: &ProjectContext, path: &str, repo_name: Option<&str>) -> Result<Value, AppError> {
        Ok(self.get(&self.resolve_subproject(context, path, repo_name)?.display().to_string()))
    }

    pub fn put_subproject(&self, context: &ProjectContext, path: &str, bindings: &Value, repo_name: Option<&str>) -> Result<Value, AppError> {
        let dir = self.resolve_subproject(context, path, repo_name)?;
        self.put(context, &dir.display().to_string(), bindings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_are_validated_written_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(&repo).unwrap();
        let project = app.project_service().open_existing_project(&repo.display().to_string()).unwrap();
        let context = ProjectContext::new(project);
        let service = app.standards();
        assert_eq!(service.get_repo(&context, None).unwrap()["bindings"], json!({}));
        assert_eq!(service.put_repo(&context, None, &json!([1])).unwrap_err().code.as_deref(), Some("invalid_standards"));
        assert_eq!(service.put_repo(&context, None, &json!({"nope": "x"})).unwrap_err().code.as_deref(), Some("invalid_standards"));
        let written = service.put_repo(&context, None, &json!({"git": "git-flow", "backend": "  ", "frontend": "react-kit"})).unwrap();
        assert_eq!(written["bindings"], json!({"git": "git-flow", "frontend": "react-kit"}));
        assert_eq!(written["slots"], json!(["git", "backend", "frontend", "app-map", "observability"]));
        assert!(std::fs::read_to_string(repo.join("CLAUDE.md")).unwrap().contains("## Standards"));
        let cleared = service.put_repo(&context, None, &json!({})).unwrap();
        assert_eq!(cleared["bindings"], json!({}));
        assert_eq!(service.get_subproject(&context, "api", None).unwrap_err().kind, Kind::SubprojectsNotConfirmed);
    }
}
