// SPDX-License-Identifier: MIT
//! RepoRegistry: the repos of ONE project (`<primary repo>/.specs/project/REPOS.json`), each with a role (edit | reference).
//!
//! The file is shared with the engine (`engine_state::project_repos` reads it and owns the writer). Nothing here changes a folder on disk: adding or
//! removing a repo only edits the list. No file = one primary repo (single-repo project, no migration). `multi` is derived (more than one repo); the
//! engine sees every listed repo. The planning chat fills the list (`planning_repos`).

use super::planning_repos::{ensure_can_switch_to_reference, load_last_rejections};
use super::App;
use crate::domain::errors::{AppError, Kind, LOOP_RUNNING_CODE};
use crate::domain::project::ProjectContext;
use engine_state::detect_setup::{self, HEAVY_FOLDER_NAMES, IGNORED_FILES_ARGUMENTS};
use engine_state::project_repos::{self, RepoRef, ROLE_EDIT, ROLE_REFERENCE};
use engine_state::{loop_state, repo_catalog};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const DETACHED_HEAD_CODE: &str = "detached_head";
pub const ALREADY_LISTED_CODE: &str = "already_listed";
pub const PRIMARY_CODE: &str = "primary";
const SUGGESTED_FILE_PARTS: [&str; 3] = [".specs", "project", "REPOS-SUGGESTED.json"];
const SUGGESTED_LIMIT: usize = 10;
const SUGGESTED_TEXT_LIMIT: usize = 400;
const SETUP_LIMIT: usize = 200;
const REASON_LIMIT: usize = 400;

fn normalize_path(path: &str) -> String {
    pycompat::nt_normcase(&pycompat::nt_realpath(&pycompat::nt_abspath(path)))
}

fn to_relative_posix(path: &str) -> String {
    path.replace('\\', "/").trim().to_string()
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

fn not_found(name: &str) -> AppError {
    AppError::new(Kind::RepoNotFound, format!("repo not found: {name}"))
}

pub struct RepoRegistryService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn repo_registry(&self) -> RepoRegistryService<'_> {
        RepoRegistryService { app: self }
    }
}

impl RepoRegistryService<'_> {
    fn notify_changed(&self, context: &ProjectContext) {
        // repo added/removed/role changed: capabilities block + readiness check
        self.app.on_plan_changed(context);
    }

    // ------------------------------------------------ read
    /// `multi` is derived (more than one repo). Each repo carries `role`, `reason` and `changed_by_feature` (a ROADMAP feature names it in
    /// `paths=`; the primary counts whenever any feature has a path). `rejected` = suggestions of the planning chat that were refused.
    pub fn list_repos(&self, context: &ProjectContext) -> Value {
        let (multi, stored) = self.read_stored(context);
        let changed = self.read_changed_repo_names(context, &stored);
        json!({
            "ok": true, "multi": multi, "repos": stored.iter().map(|r| self.describe_repo(r, Some(&changed))).collect::<Vec<_>>(),
            "suggested": self.read_suggested_repos(context), "defined": project_repos::is_scope_defined(context.repo_path()), "rejected": load_last_rejections(context),
        })
    }

    /// Names of the repos some ROADMAP feature changes (`paths=<repo>:...`); empty without a readable ROADMAP.
    pub fn read_changed_repo_names(&self, context: &ProjectContext, stored: &[RepoRef]) -> BTreeSet<String> {
        let mut folders: Vec<String> = Vec::new();
        for folder in [context.specs_dir(), Path::new(context.repo_path()).join(".specs").display().to_string()] {
            if !folders.contains(&folder) {
                folders.push(folder);
            }
        }
        for folder in folders {
            let Ok(bytes) = std::fs::read(Path::new(&folder).join("project").join("ROADMAP.md")) else { continue };
            let (features, _) = loop_state::parse_roadmap(&String::from_utf8_lossy(&bytes));
            return project_repos::repos_changed_by_roadmap(&project_repos::primary_of(stored).name, &features);
        }
        BTreeSet::new()
    }

    /// The listed repo called `name` (the stored list, so a repo of an unchecked project can still be reached by name).
    pub fn resolve(&self, context: &ProjectContext, name: &str) -> Result<RepoRef, AppError> {
        let (_, stored) = self.read_stored(context);
        project_repos::find_repo(&stored, name).cloned().ok_or_else(|| not_found(name))
    }

    /// What the engine sees: primary first, every listed repo with its role.
    pub fn list_effective_repos(&self, context: &ProjectContext) -> Vec<RepoRef> {
        project_repos::load_repos(context.repo_path())
    }

    fn save(&self, context: &ProjectContext, stored: &[RepoRef], scope_defined: Option<bool>, approve_setup_for: &[&str]) {
        let _ = project_repos::save_repos(context.repo_path(), stored, scope_defined, None, approve_setup_for, &self.app.config.user_data_dir);
    }

    // ------------------------------------------------ write
    /// Adds a repository (the chat or the Workspace dialog). `role` defaults to `edit`; `reason` = one sentence: why it is in the project.
    pub fn add_repo(&self, context: &ProjectContext, path: &Value, copy: Option<&Value>, setup: Option<&Value>, role: Option<&Value>, reason: Option<&Value>) -> Result<Value, AppError> {
        if let Some(role) = role.filter(|r| !r.is_null()) {
            if ![ROLE_EDIT, ROLE_REFERENCE].iter().any(|r| role.as_str() == Some(r)) {
                return Err(AppError::invalid_request("role must be edit or reference"));
            }
        }
        let (_, stored) = self.read_stored(context);
        let folder = self.validate_candidate_folder(path)?;
        if stored.iter().any(|repo| normalize_path(&repo.path) == normalize_path(&folder)) {
            return Err(AppError::new(Kind::InvalidRepo, "this folder is already listed").with_code(ALREADY_LISTED_CODE));
        }
        let copy_value = copy.filter(|c| !c.is_null());
        let copy_list = Self::validate_copy_list(copy_value.unwrap_or(&json!([])))?;
        let taken: BTreeSet<String> = stored.iter().map(|r| r.name.clone()).collect();
        let name = project_repos::unique_name(&project_repos::short_name_of(&folder), &taken);
        let mut added = RepoRef::new(&name, &folder, false);
        added.copy = copy_list;
        added.setup = Self::validate_setup(setup)?;
        added.copy_configured = copy_value.is_some();
        added.role = role.and_then(Value::as_str).unwrap_or(ROLE_EDIT).to_string();
        added.reason = Self::validate_reason(reason)?;
        let mut all = stored;
        all.push(added.clone());
        self.save(context, &all, Some(true), &[name.as_str()]); // typed by the user
        self.app.log.info("repo_added", &[("project", json!(context.project.id)), ("repo", json!(name))]);
        self.notify_changed(context);
        Ok(json!({"ok": true, "repo": self.describe_repo(&added, None), "repos": self.list_repos(context)["repos"], "multi": true}))
    }

    /// `PATCH repos/<name>` `{"role"}`: `edit` or `reference`. The primary is always edit; `edit` -> `reference` is refused while a loop worktree
    /// exists for the repo ("close the loop first"). Persists and returns the listing.
    pub fn set_role(&self, context: &ProjectContext, name: &str, role: &Value) -> Result<Value, AppError> {
        let Some(role) = role.as_str().filter(|r| *r == ROLE_EDIT || *r == ROLE_REFERENCE) else { return Err(AppError::invalid_request("role must be edit or reference")) };
        let (_, mut stored) = self.read_stored(context);
        let index = stored.iter().position(|r| r.name == name).ok_or_else(|| not_found(name))?;
        if stored[index].primary && role != ROLE_EDIT {
            return Err(AppError::new(Kind::RepoConflict, "the primary repo of the project is always edit").with_code(PRIMARY_CODE));
        }
        self.ensure_loop_not_running(context)?;
        if stored[index].role == ROLE_EDIT && role == ROLE_REFERENCE {
            ensure_can_switch_to_reference(&stored[index])?;
        }
        stored[index].role = role.to_string();
        self.save(context, &stored, Some(true), &[]);
        self.app.log.info("repo_role_changed", &[("project", json!(context.project.id)), ("repo", json!(name)), ("role", json!(role))]);
        self.notify_changed(context);
        Ok(self.list_repos(context))
    }

    /// `PATCH repos/<name>` `{role?, reason?}`: the role (same rules as `set_role`) and/or the reason (one sentence, empty = none).
    pub fn update_repo(&self, context: &ProjectContext, name: &str, role: Option<&Value>, reason: Option<&Value>) -> Result<Value, AppError> {
        let role = role.filter(|r| !r.is_null());
        let reason = reason.filter(|r| !r.is_null());
        if role.is_none() && reason.is_none() {
            return Err(AppError::invalid_request("give role and/or reason"));
        }
        if let Some(role) = role {
            self.set_role(context, name, role)?;
        }
        if let Some(reason) = reason {
            let (_, mut stored) = self.read_stored(context);
            let index = stored.iter().position(|r| r.name == name).ok_or_else(|| not_found(name))?;
            stored[index].reason = Self::validate_reason(Some(reason))?;
            self.save(context, &stored, Some(true), &[]);
            self.app.log.info("repo_reason_changed", &[("project", json!(context.project.id)), ("repo", json!(name))]);
            self.notify_changed(context);
        }
        Ok(self.list_repos(context))
    }

    pub fn remove_repo(&self, context: &ProjectContext, name: &str) -> Result<Value, AppError> {
        let (_, stored) = self.read_stored(context);
        let target = project_repos::find_repo(&stored, name).ok_or_else(|| not_found(name))?;
        if target.primary {
            return Err(AppError::new(Kind::RepoConflict, "the primary repo of the project cannot be removed").with_code(PRIMARY_CODE));
        }
        self.ensure_loop_not_running(context)?;
        let remaining: Vec<RepoRef> = stored.iter().filter(|r| r.name != name).cloned().collect();
        self.save(context, &remaining, Some(true), &[]);
        self.app.log.info("repo_removed", &[("project", json!(context.project.id)), ("repo", json!(name))]);
        self.notify_changed(context);
        Ok(json!({"ok": true, "removed": name, "note": "no folder or worktree was deleted"}))
    }

    /// The user confirms the repositories listed now (even only the primary) as the scope of the loop. Done BEFORE the loop exists.
    pub fn confirm_scope(&self, context: &ProjectContext) -> Result<Value, AppError> {
        self.ensure_loop_not_running(context)?;
        let (_, stored) = self.read_stored(context);
        self.save(context, &stored, Some(true), &[]);
        self.app.log.info("repo_scope_confirmed", &[("project", json!(context.project.id)), ("repos", json!(stored.len()))]);
        self.notify_changed(context);
        Ok(self.list_repos(context))
    }

    pub fn set_copy_list(&self, context: &ProjectContext, name: &str, copy: &Value) -> Result<Value, AppError> {
        let (_, mut stored) = self.read_stored(context);
        let index = stored.iter().position(|r| r.name == name).ok_or_else(|| not_found(name))?;
        if copy.is_null() {
            // back to automatic: the `copy` key disappears from REPOS.json
            stored[index].copy = vec![];
            stored[index].copy_configured = false;
        } else {
            stored[index].copy = Self::validate_copy_list(copy)?;
            stored[index].copy_configured = true;
        }
        self.save(context, &stored, None, &[]);
        Ok(json!({"ok": true, "name": name, "copy": stored[index].copy, "copy_configured": stored[index].copy_configured}))
    }

    /// Stores the one-time setup command of a repo (empty = none). It runs in that repo's worktree when the worktree is created.
    pub fn set_setup(&self, context: &ProjectContext, name: &str, setup: &Value) -> Result<Value, AppError> {
        let (_, mut stored) = self.read_stored(context);
        let index = stored.iter().position(|r| r.name == name).ok_or_else(|| not_found(name))?;
        stored[index].setup = Self::validate_setup(Some(setup))?;
        self.save(context, &stored, None, &[name]); // typed by the user: the engine runs only an approved text
        let setup_text = stored[index].setup.clone();
        Ok(json!({"ok": true, "name": name, "setup": if setup_text.is_empty() { Value::Null } else { json!(setup_text) }}))
    }

    /// `.specs/project/REPOS-SUGGESTED.json` written by the planning AI: `[{name, reason, evidence}]`. Keeps only entries whose name is in the repo
    /// catalog (and not already listed in the project), at most `SUGGESTED_LIMIT`. Absent or broken file = `[]`.
    pub fn read_suggested_repos(&self, context: &ProjectContext) -> Vec<Value> {
        let path = SUGGESTED_FILE_PARTS.iter().fold(PathBuf::from(context.repo_path()), |a, p| a.join(p));
        let Some(Value::Array(data)) = std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return vec![] };
        let catalog_paths = self.read_catalog_paths_by_name();
        let listed: BTreeSet<String> = self.read_stored(context).1.into_iter().map(|r| r.name).collect();
        let mut suggested = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for item in data {
            let name = if item.is_object() { text_of(item.get("name")).trim().to_string() } else { String::new() };
            let Some(path) = catalog_paths.iter().find(|(n, _)| *n == name).map(|(_, p)| p.clone()) else { continue };
            if name.is_empty() || seen.contains(&name) || listed.contains(&name) {
                continue;
            }
            seen.insert(name.clone());
            let clip = |v: Option<&Value>| -> String { text_of(v).trim().chars().take(SUGGESTED_TEXT_LIMIT).collect() };
            suggested.push(json!({"name": name, "path": path, "reason": clip(item.get("reason")), "evidence": clip(item.get("evidence"))}));
            if suggested.len() >= SUGGESTED_LIMIT {
                break;
            }
        }
        suggested
    }

    /// `[(catalog repo name, folder path)]` of the repo catalog (empty when it cannot be read).
    fn read_catalog_paths_by_name(&self) -> Vec<(String, String)> {
        let catalog = repo_catalog::load_catalog(&self.app.env);
        catalog["repos"]
            .as_object()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(_, e)| e.is_object() && engine_state::notify_truthy(e.get("name")))
                    .map(|(key, e)| (text_of(e.get("name")), if engine_state::notify_truthy(e.get("path")) { text_of(e.get("path")) } else { key.clone() }))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Git-ignored files that look like local configuration; heavy folders are never listed.
    pub fn list_copy_suggestions(&self, context: &ProjectContext, name: &str) -> Result<Value, AppError> {
        let target = self.resolve(context, name)?;
        let (code, output, _) = if Path::new(&target.path).is_dir() { self.app.git.run_git(&target.path, &IGNORED_FILES_ARGUMENTS) } else { (1, String::new(), String::new()) };
        let mut suggestions = detect_setup::suggest_copy_files(&target.path, if code == 0 { &output } else { "" });
        for item in suggestions.iter_mut() {
            // never configured: everything suggested is copied automatically, so it shows ticked
            let ticked = !target.copy_configured || target.copy.iter().any(|c| Some(c.as_str()) == item["path"].as_str());
            item["ticked"] = json!(ticked);
        }
        Ok(json!({"ok": true, "name": name, "copy_configured": target.copy_configured, "suggestions": suggestions}))
    }

    // ------------------------------------------------ support
    /// `(multi, every repo in the file, primary FIRST)`. No file = `(false, [the primary])`. `multi` = more than one repo.
    pub fn read_stored(&self, context: &ProjectContext) -> (bool, Vec<RepoRef>) {
        let repos = project_repos::load_repos(context.repo_path());
        (repos.len() > 1, repos)
    }

    pub fn describe_repo(&self, repo: &RepoRef, changed_names: Option<&BTreeSet<String>>) -> Value {
        let exists = Path::new(&repo.path).is_dir();
        let is_git = exists && self.app.git.is_git_repo(&repo.path);
        let branch = if is_git { Some(self.app.git.get_current_branch(&repo.path)).filter(|b| !b.is_empty()) } else { None };
        json!({
            "name": repo.name, "path": repo.path, "primary": repo.primary, "branch": if exists { json!(branch) } else { Value::Null },
            "worktree_path": repo.loop_worktree_path(), "copy": repo.copy, "setup": if repo.setup.is_empty() { Value::Null } else { json!(repo.setup) },
            "exists": exists, "is_git": is_git, "detected_setup": detect_setup::detect_setup(&repo.path), "setup_configured": !repo.setup.is_empty(),
            "copy_configured": repo.copy_configured, "role": repo.role, "reason": if repo.reason.is_empty() { Value::Null } else { json!(repo.reason) },
            "changed_by_feature": changed_names.is_some_and(|c| c.contains(&repo.name)),
        })
    }

    fn validate_candidate_folder(&self, path: &Value) -> Result<String, AppError> {
        let invalid = |message: String| AppError::new(Kind::InvalidRepo, message);
        let Some(text) = path.as_str().filter(|p| !p.trim().is_empty()) else { return Err(invalid("give the repo folder".into())) };
        let expanded = match text.trim().strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => format!("{}{rest}", self.app.env.home_dir().display()),
            _ => text.trim().to_string(),
        };
        let folder = pycompat::nt_abspath(&expanded);
        if !Path::new(&folder).is_dir() {
            return Err(invalid(format!("not a folder: {folder}")));
        }
        if !self.app.git.is_git_repo(&folder) {
            return Err(invalid(format!("not a git repository: {folder}")));
        }
        if self.app.git.get_current_branch(&folder).is_empty() {
            return Err(invalid("the repo has no current branch (detached HEAD): check out a branch first".into()).with_code(DETACHED_HEAD_CODE));
        }
        Ok(folder)
    }

    fn validate_reason(reason: Option<&Value>) -> Result<String, AppError> {
        let invalid = |message: String| AppError::new(Kind::InvalidRepo, message);
        match reason {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(text)) => {
                let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if cleaned.chars().count() > REASON_LIMIT {
                    return Err(invalid(format!("reason is limited to {REASON_LIMIT} characters")));
                }
                Ok(cleaned)
            }
            Some(_) => Err(invalid("reason must be text".into())),
        }
    }

    fn validate_setup(setup: Option<&Value>) -> Result<String, AppError> {
        let invalid = |message: String| AppError::new(Kind::InvalidRepo, message);
        match setup {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(text)) if !text.contains(['\n', '\r']) => {
                let cleaned = text.trim().to_string();
                if cleaned.chars().count() > SETUP_LIMIT {
                    return Err(invalid(format!("setup is limited to {SETUP_LIMIT} characters")));
                }
                Ok(cleaned)
            }
            Some(_) => Err(invalid("setup must be a single-line command".into())),
        }
    }

    fn validate_copy_list(copy: &Value) -> Result<Vec<String>, AppError> {
        let invalid = |message: String| AppError::new(Kind::InvalidRepo, message);
        let Some(list) = copy.as_array().filter(|l| l.iter().all(|e| e.as_str().is_some_and(|s| !s.trim().is_empty()))) else { return Err(invalid("copy must be a list of file paths".into())) };
        let mut cleaned: Vec<String> = Vec::new();
        for entry in list.iter().filter_map(Value::as_str) {
            let mut relative = to_relative_posix(entry);
            while let Some(rest) = relative.strip_prefix("./") {
                relative = rest.to_string();
            }
            let parts: Vec<&str> = relative.split('/').collect();
            if pycompat::nt_isabs(entry) || relative.starts_with('/') || parts.contains(&"..") || relative.ends_with('/') || parts[0].contains(':') {
                return Err(invalid(format!("copy entries are files relative to the repo: {entry}")));
            }
            if parts.iter().any(|p| HEAVY_FOLDER_NAMES.contains(p)) {
                return Err(invalid(format!("never copy files from {}: {entry}", HEAVY_FOLDER_NAMES.join("/"))));
            }
            if !cleaned.contains(&relative) {
                cleaned.push(relative);
            }
        }
        Ok(cleaned)
    }

    fn ensure_loop_not_running(&self, context: &ProjectContext) -> Result<(), AppError> {
        if self.app.loop_status().is_loop_running(context) {
            return Err(AppError::loop_running("a loop is running in this project: stop it first").with_code(LOOP_RUNNING_CODE));
        }
        Ok(())
    }
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

    fn context(path: &str) -> ProjectContext {
        ProjectContext::new(Project { id: "p".into(), name: "main".into(), repo_path: path.into(), opened_at: String::new(), last_opened_at: String::new(), favorite: false })
    }

    #[test]
    fn repos_are_added_changed_and_removed_with_the_documented_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let primary = git_repo(dir.path(), "main-app", &app);
        let lib = git_repo(dir.path(), "lib", &app);
        std::fs::create_dir_all(Path::new(&primary).join(".specs/project")).unwrap();
        let ctx = context(&primary);
        let service = app.repo_registry();
        let listing = service.list_repos(&ctx);
        assert_eq!((listing["multi"].clone(), listing["repos"].as_array().unwrap().len(), listing["defined"].clone()), (json!(false), 1, json!(true))); // a single-repo project counts as defined
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(service.add_repo(&ctx, &json!(plain.display().to_string()), None, None, None, None).unwrap_err().message.starts_with("not a git repository: "));
        assert_eq!(service.add_repo(&ctx, &json!(""), None, None, None, None).unwrap_err().message, "give the repo folder");
        assert_eq!(service.add_repo(&ctx, &json!(lib), None, None, Some(&json!("boss")), None).unwrap_err().kind, Kind::InvalidRequest);
        let added = service.add_repo(&ctx, &json!(lib), Some(&json!([".env", "./cfg/local.json"])), Some(&json!(" npm ci ")), None, Some(&json!("  shared   code "))).unwrap();
        assert_eq!((added["repo"]["name"].clone(), added["repo"]["copy"].clone(), added["repo"]["setup"].clone(), added["repo"]["reason"].clone()), (json!("lib"), json!([".env", "cfg/local.json"]), json!("npm ci"), json!("shared code")));
        assert_eq!(added["multi"], true);
        assert_eq!(service.add_repo(&ctx, &json!(lib), None, None, None, None).unwrap_err().code.as_deref(), Some("already_listed"));
        assert_eq!(service.set_role(&ctx, "main-app", &json!("reference")).unwrap_err().code.as_deref(), Some("primary"));
        assert_eq!(service.set_role(&ctx, "ghost", &json!("edit")).unwrap_err().kind, Kind::RepoNotFound);
        let switched = service.set_role(&ctx, "lib", &json!("reference")).unwrap();
        assert_eq!(switched["repos"][1]["role"], "reference");
        assert_eq!(service.update_repo(&ctx, "lib", None, None).unwrap_err().message, "give role and/or reason");
        let updated = service.update_repo(&ctx, "lib", Some(&json!("edit")), Some(&json!(""))).unwrap();
        assert_eq!((updated["repos"][1]["role"].clone(), updated["repos"][1]["reason"].clone()), (json!("edit"), Value::Null));
        assert_eq!(service.set_copy_list(&ctx, "lib", &json!(["../x"])).unwrap_err().kind, Kind::InvalidRepo);
        assert_eq!(service.set_copy_list(&ctx, "lib", &json!(["node_modules/a"])).unwrap_err().message, "never copy files from node_modules/bin/obj/dist: node_modules/a");
        assert_eq!(service.set_copy_list(&ctx, "lib", &Value::Null).unwrap()["copy_configured"], false);
        assert_eq!(service.set_setup(&ctx, "lib", &json!("a\nb")).unwrap_err().message, "setup must be a single-line command");
        assert_eq!(service.set_setup(&ctx, "lib", &json!("")).unwrap()["setup"], Value::Null);
        let suggestions = service.list_copy_suggestions(&ctx, "lib").unwrap();
        assert_eq!(suggestions["copy_configured"], false);
        assert_eq!(service.confirm_scope(&ctx).unwrap()["defined"], true);
        assert_eq!(service.remove_repo(&ctx, "main-app").unwrap_err().code.as_deref(), Some("primary"));
        assert_eq!(service.remove_repo(&ctx, "lib").unwrap()["removed"], "lib");
        assert_eq!(service.list_repos(&ctx)["repos"].as_array().unwrap().len(), 1);
    }
}
