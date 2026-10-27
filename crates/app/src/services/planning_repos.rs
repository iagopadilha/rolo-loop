// SPDX-License-Identifier: MIT
//! Repositories decided in the main planning chat: the AI writes `.specs/project/REPOS-SUGGESTED.json`
//! (`{"repos": [{"name"?, "path", "role": "edit"|"reference", "reason"}]}`) and the server applies it to REPOS.json after each reply.
//!
//! The AI never edits REPOS.json (its guard profile stays narrow): this service validates every entry (folder exists, is a git repo, primary stays
//! `edit`, duplicates collapse with `edit` winning), merges the valid ones, and keeps the refusals in
//! `.specs/project/loop/internal/planning/repos_rejected.json` so the next status block and the workspace bar can show them. Nothing here touches a
//! folder on disk besides the two JSON files.

use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use engine_state::project_repos::{self, RepoRef};
use pycompat::json::py_json_dumps_indent;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const SUGGESTED_FILE_PARTS: [&str; 3] = [".specs", "project", "REPOS-SUGGESTED.json"];
const PLANNING_FOLDER_PARTS: [&str; 5] = [".specs", "project", "loop", "internal", "planning"];
pub const REJECTED_FILE_NAME: &str = "repos_rejected.json";
/// sha1 of the suggestion file already applied (a stale file never undoes a panel change).
const APPLIED_MARK_FILE_NAME: &str = "repos_applied.json";
pub const CLOSE_LOOP_FIRST_CODE: &str = "close_loop_first";
const SUGGESTED_LIMIT: usize = 20;
const TEXT_LIMIT: usize = 400;

pub fn normalize_path(path: &str) -> String {
    pycompat::nt_normcase(&pycompat::nt_realpath(&pycompat::nt_abspath(path)))
}

pub fn planning_file_path(context: &ProjectContext, file_name: &str) -> PathBuf {
    PLANNING_FOLDER_PARTS.iter().fold(PathBuf::from(context.work_dir()), |a, p| a.join(p)).join(file_name)
}

/// Refuses `edit` -> `reference` while a loop worktree exists for the repo (the loop is still open).
pub fn ensure_can_switch_to_reference(repo: &RepoRef) -> Result<(), AppError> {
    if !repo.primary && Path::new(&repo.loop_worktree_path()).is_dir() {
        return Err(AppError::new(
            Kind::RepoConflict,
            format!("close the loop first: repository {} already has a loop worktree, so it cannot become reference", repo.name),
        )
        .with_code(CLOSE_LOOP_FIRST_CODE));
    }
    Ok(())
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

/// `[{"path", "reason"}]` refused by the last application of the chat's suggestion (empty list = none, or file absent/broken).
pub fn load_last_rejections(context: &ProjectContext) -> Vec<Value> {
    let Some(data) = std::fs::read(planning_file_path(context, REJECTED_FILE_NAME)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return vec![] };
    data.get("rejected")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter(|r| r.is_object()).map(|r| json!({"path": text_of(r.get("path")), "reason": text_of(r.get("reason"))})).collect())
        .unwrap_or_default()
}

struct Valid {
    path: String,
    key: String,
    name: String,
    role: String,
    reason: String,
}

pub struct PlanningReposService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn planning_repos(&self) -> PlanningReposService<'_> {
        PlanningReposService { app: self }
    }
}

impl PlanningReposService<'_> {
    /// Applies REPOS-SUGGESTED.json to REPOS.json. Returns `{"applied": [{name, path, role, reason, primary}], "rejected": [{path, reason}]}`. No
    /// file, a broken file or a file already applied = nothing applied and the previous rejections stay.
    pub fn apply_suggested_repos(&self, context: &ProjectContext) -> Value {
        let Some(raw) = self.read_suggestion_text(context) else { return json!({"applied": [], "rejected": []}) };
        let digest: String = Sha1::digest(raw.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        if self.read_applied_digest(context).as_deref() == Some(digest.as_str()) {
            return json!({"applied": [], "rejected": []});
        }
        let (entries, mut rejected) = Self::parse_entries(&raw);
        let valid = self.validate_entries(&entries, &mut rejected);
        let mut stored = project_repos::load_repos(context.repo_path());
        let applied = Self::merge_into(&mut stored, &valid, &mut rejected);
        if !applied.is_empty() || rejected.is_empty() {
            let _ = project_repos::save_repos(context.repo_path(), &stored, Some(true), Some(project_repos::SCOPE_SOURCE_PLANNING), &[], &self.app.config.user_data_dir);
        }
        self.write_json(&planning_file_path(context, REJECTED_FILE_NAME), &json!({"at": rfc3339_now(), "rejected": rejected}));
        self.write_json(&planning_file_path(context, APPLIED_MARK_FILE_NAME), &json!({"sha1": digest}));
        self.app.log.info("planning_repos_applied", &[("project", json!(context.project.id)), ("applied", json!(applied.len())), ("rejected", json!(rejected.len()))]);
        json!({"applied": applied, "rejected": rejected})
    }

    // ------------------------------------------------ parse and validate
    fn read_suggestion_text(&self, context: &ProjectContext) -> Option<String> {
        let path = SUGGESTED_FILE_PARTS.iter().fold(PathBuf::from(context.repo_path()), |a, p| a.join(p));
        std::fs::read(path).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// `(entries, rejected)`. Accepts `{"repos": [...]}` and the legacy bare list; anything else is one rejection.
    fn parse_entries(raw: &str) -> (Vec<Value>, Vec<Value>) {
        let Ok(data) = serde_json::from_str::<Value>(raw) else {
            return (vec![], vec![json!({"path": "REPOS-SUGGESTED.json", "reason": "the file is not valid JSON"})]);
        };
        let rows = if data.is_object() { data.get("repos").cloned().unwrap_or(Value::Null) } else { data };
        match rows {
            Value::Array(list) => (list.into_iter().take(SUGGESTED_LIMIT).collect(), vec![]),
            _ => (vec![], vec![json!({"path": "REPOS-SUGGESTED.json", "reason": "expected {\"repos\": [{\"path\", \"role\", \"reason\"}]}"})]),
        }
    }

    /// Valid entries collapsed by folder (edit wins). Refusals are appended to `rejected`.
    fn validate_entries(&self, entries: &[Value], rejected: &mut Vec<Value>) -> Vec<Valid> {
        let mut by_key: Vec<Valid> = Vec::new();
        for item in entries {
            let path = if item.is_object() { text_of(item.get("path")).trim().to_string() } else { String::new() };
            let label = if !path.is_empty() {
                path.clone()
            } else if item.is_object() {
                text_of(item.get("name"))
            } else {
                text_of(Some(item))
            };
            let problem = self.find_problem(item, &path);
            if !problem.is_empty() {
                rejected.push(json!({"path": label, "reason": problem}));
                continue;
            }
            let folder = pycompat::nt_abspath(&expand_user(&path, &self.app.env.home_dir()));
            let key = normalize_path(&folder);
            let role = item.get("role").and_then(Value::as_str).filter(|r| !r.is_empty()).unwrap_or(project_repos::ROLE_EDIT).to_string();
            let reason: String = text_of(item.get("reason")).trim().chars().take(TEXT_LIMIT).collect();
            let name = text_of(item.get("name")).trim().to_string();
            match by_key.iter_mut().find(|v| v.key == key) {
                None => by_key.push(Valid { path: folder, key, name, role, reason }),
                Some(current) => {
                    // two suggestions for the same folder: one entry, edit wins
                    if role == project_repos::ROLE_EDIT {
                        current.role = role;
                    }
                    if current.reason.is_empty() {
                        current.reason = reason;
                    }
                    if current.name.is_empty() {
                        current.name = name;
                    }
                }
            }
        }
        by_key
    }

    fn find_problem(&self, item: &Value, path: &str) -> String {
        if !item.is_object() {
            return "each repository must be an object with path, role and reason".into();
        }
        if path.is_empty() {
            return "give the repository folder in `path`".into();
        }
        match item.get("role") {
            None | Some(Value::Null) => {}
            Some(Value::String(role)) if role == project_repos::ROLE_EDIT || role == project_repos::ROLE_REFERENCE => {}
            _ => return "role must be edit or reference".into(),
        }
        let folder = pycompat::nt_abspath(&expand_user(path, &self.app.env.home_dir()));
        if !Path::new(&folder).is_dir() {
            return "folder does not exist".into();
        }
        if !self.app.git.is_git_repo(&folder) {
            return "not a git repository".into();
        }
        if self.app.git.get_current_branch(&folder).is_empty() {
            return "the repository has no current branch (detached HEAD)".into();
        }
        String::new()
    }

    // ------------------------------------------------ merge
    /// Updates `stored` (primary first) in place; returns the applied rows. A refused role switch goes to `rejected`.
    fn merge_into(stored: &mut Vec<RepoRef>, valid: &[Valid], rejected: &mut Vec<Value>) -> Vec<Value> {
        let mut applied = Vec::new();
        let mut taken: BTreeSet<String> = stored.iter().map(|r| r.name.clone()).collect();
        for entry in valid {
            let existing = stored.iter().position(|r| normalize_path(&r.path) == entry.key);
            let target_index = match existing {
                Some(index) if stored[index].primary => {
                    // the primary stays edit; only its reason may be filled
                    if stored[index].reason.is_empty() {
                        stored[index].reason = entry.reason.clone();
                    }
                    continue;
                }
                Some(index) => {
                    if stored[index].role == project_repos::ROLE_EDIT && entry.role == project_repos::ROLE_REFERENCE {
                        if let Err(error) = ensure_can_switch_to_reference(&stored[index]) {
                            rejected.push(json!({"path": entry.path, "reason": error.message}));
                            continue;
                        }
                    }
                    stored[index].role = entry.role.clone();
                    if !entry.reason.is_empty() {
                        stored[index].reason = entry.reason.clone();
                    }
                    index
                }
                None => {
                    let base = project_repos::short_name_of(if entry.name.is_empty() { &entry.path } else { &entry.name });
                    let name = project_repos::unique_name(&base, &taken);
                    taken.insert(name.clone());
                    let mut repo = RepoRef::new(&name, &entry.path, false);
                    repo.role = entry.role.clone();
                    repo.reason = entry.reason.clone();
                    stored.push(repo);
                    stored.len() - 1
                }
            };
            let target = &stored[target_index];
            applied.push(json!({"name": target.name, "path": target.path, "role": target.role, "reason": target.reason, "primary": false}));
        }
        applied
    }

    // ------------------------------------------------ files
    fn read_applied_digest(&self, context: &ProjectContext) -> Option<String> {
        let data = std::fs::read(planning_file_path(context, APPLIED_MARK_FILE_NAME)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok())?;
        data.get("sha1").and_then(Value::as_str).map(String::from)
    }

    fn write_json(&self, path: &Path, data: &Value) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, format!("{}\n", py_json_dumps_indent(data, false, 2)));
    }
}

fn expand_user(path: &str, home: &Path) -> String {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => format!("{}{rest}", home.display()),
        _ => path.to_string(),
    }
}

/// ISO-8601 UTC with microseconds and `+00:00`, like `datetime.now(timezone.utc).isoformat()`.
fn rfc3339_now() -> String {
    let now = time::OffsetDateTime::now_utc();
    let text = now.format(time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]")).unwrap_or_default();
    format!("{text}+00:00")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::project::Project;

    fn git_repo(dir: &Path, name: &str, app: &App) -> String {
        let folder = dir.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("a.txt"), "x").unwrap();
        let folder = folder.display().to_string();
        assert!(app.git.init_repo_with_initial_commit(&folder, "init", "").0);
        folder
    }

    #[test]
    fn suggestions_are_validated_merged_and_applied_once() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let primary = git_repo(dir.path(), "main-app", &app);
        let other = git_repo(dir.path(), "shared-lib", &app);
        let plain = dir.path().join("not-git");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(Path::new(&primary).join(".specs/project")).unwrap();
        let context = ProjectContext::new(Project {
            id: "p".into(),
            name: "main-app".into(),
            repo_path: primary.clone(),
            opened_at: String::new(),
            last_opened_at: String::new(),
            favorite: false,
        });
        let suggestion = json!({"repos": [
            {"path": other, "role": "reference", "reason": "contracts"},
            {"path": other, "role": "edit"},
            {"path": plain.display().to_string(), "role": "edit"},
            {"path": "C:\\no\\such\\folder"},
            {"path": primary, "reason": "me"},
            {"path": other, "role": "weird"},
            "oops",
        ]});
        let file = Path::new(&primary).join(".specs/project/REPOS-SUGGESTED.json");
        std::fs::write(&file, pycompat::json::py_json_dumps(&suggestion, true)).unwrap();
        let service = app.planning_repos();
        let result = service.apply_suggested_repos(&context);
        let applied = result["applied"].as_array().unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!((applied[0]["name"].clone(), applied[0]["role"].clone(), applied[0]["reason"].clone()), (json!("shared-lib"), json!("edit"), json!("contracts")));
        let reasons: Vec<&str> = result["rejected"].as_array().unwrap().iter().map(|r| r["reason"].as_str().unwrap()).collect();
        assert_eq!(
            reasons,
            ["not a git repository", "folder does not exist", "role must be edit or reference", "each repository must be an object with path, role and reason"]
        );
        let stored = project_repos::load_repos(&primary);
        assert_eq!((stored.len(), stored[0].reason.clone(), stored[1].name.clone()), (2, "me".to_string(), "shared-lib".to_string()));
        assert_eq!(load_last_rejections(&context).len(), 4);
        assert_eq!(service.apply_suggested_repos(&context), json!({"applied": [], "rejected": []}));
        std::fs::write(&file, "not json").unwrap();
        let broken = service.apply_suggested_repos(&context);
        assert_eq!(broken["rejected"][0]["reason"], "the file is not valid JSON");
        std::fs::write(&file, "{}").unwrap();
        assert_eq!(service.apply_suggested_repos(&context)["rejected"][0]["reason"], "expected {\"repos\": [{\"path\", \"role\", \"reason\"}]}");
    }

    #[test]
    fn a_repo_with_a_loop_worktree_cannot_become_reference() {
        let dir = tempfile::tempdir().unwrap();
        let repo = RepoRef::new("lib", &dir.path().join("lib").display().to_string(), false);
        assert!(ensure_can_switch_to_reference(&repo).is_ok());
        std::fs::create_dir_all(repo.loop_worktree_path()).unwrap();
        let error = ensure_can_switch_to_reference(&repo).unwrap_err();
        assert_eq!((error.kind, error.code.as_deref()), (Kind::RepoConflict, Some("close_loop_first")));
        assert!(ensure_can_switch_to_reference(&RepoRef::new("main", &repo.path, true)).is_ok());
    }
}
