// SPDX-License-Identifier: MIT
//! Readiness of the repos of a multi-repo project: the minimum a repo needs before the loop works in it.
//!
//! The primary repo has its own checklist (`ProjectProgressService`). Every OTHER repo listed in a multi-repo project gets the same two prerequisites
//! here: a `Conformance: STANDARD|LEGACY` line in its CLAUDE.md and, when it has code, a codebase map (`docs/codebase`). `list_unready` feeds the
//! planning gate "repos"; `set_conformance` and `map_repo` are the two actions the panel offers per repo. The folder read and written is the repo's loop
//! worktree when one exists (like the primary's work dir), else the repo folder itself.

use super::project_progress::ProjectProgressService;
use super::specs_structure::{MONOREPO_ROOT_CONFORMANCE_CODE, MONOREPO_ROOT_CONFORMANCE_MESSAGE};
use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::{Project, ProjectContext};
use crate::repositories::project_files::Read;
use engine_state::project_repos::{self, RepoRef};
use serde_json::{json, Value};
use std::path::Path;

const MISSING_CONFORMANCE: &str = "conformance";
const MISSING_MAP: &str = "map";
const MISSING_FOLDER: &str = "folder";
const MISSING_SUBPROJECTS: &str = "subprojects";
const MAP_JOB_TYPE: &str = "map";
pub const FOLDER_MISSING_CODE: &str = "folder_missing";

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

pub struct RepoReadinessService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn repo_readiness(&self) -> RepoReadinessService<'_> {
        RepoReadinessService { app: self }
    }
}

impl RepoReadinessService<'_> {
    // ------------------------------------------------ read
    /// `{"multi", "repos": [readiness of every repo the engine sees, primary first]}` (a single-repo project: only the primary).
    pub fn describe_repos(&self, context: &ProjectContext) -> Value {
        let registry = self.app.repo_registry();
        let (multi, _) = registry.read_stored(context);
        let repos: Vec<Value> = registry.list_effective_repos(context).iter().map(|repo| self.describe_repo(repo, Some(context))).collect();
        let ready = repos.iter().all(|r| r["ready"] == true);
        json!({"ok": true, "multi": multi, "repos": repos, "ready": ready})
    }

    /// The NON-primary `edit` repos that miss something: `[{name, missing: [conformance|map], detail}]`. `[]` for a single-repo project. A `reference`
    /// repo is only read by the agents, so it needs no Conformance nor map. A secondary repo that is a monorepo (its entry in the primary's
    /// SUBPROJECTS.json) reports one item per subproject that misses something (`missing` is what that subproject lacks, `detail` reads
    /// "<repo>/<path>: no Conformance, no codebase map"), or one item while its list is not confirmed yet.
    pub fn list_unready(&self, context: &ProjectContext) -> Vec<Value> {
        let mut unready = Vec::new();
        for repo in self.describe_repos(context)["repos"].as_array().cloned().unwrap_or_default() {
            if repo["primary"] == true || repo["role"] == project_repos::ROLE_REFERENCE {
                continue;
            }
            if engine_state::notify_truthy(repo.get("monorepo")) {
                unready.extend(self.list_unready_subprojects(context, &repo));
            } else if repo["ready"] != true {
                unready.push(json!({"name": repo["name"], "missing": repo["missing"], "detail": repo["detail"]}));
            }
        }
        unready
    }

    /// One WARNING per `reference` repo with code and no codebase map. Never a problem: the agents can read the raw code.
    pub fn list_reference_warnings(&self, context: &ProjectContext) -> Vec<String> {
        self.describe_repos(context)["repos"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|repo| {
                repo["role"] == project_repos::ROLE_REFERENCE
                    && repo["primary"] != true
                    && repo["exists"] == true
                    && repo["has_code"] == true
                    && repo["map"]["files"].as_array().is_none_or(Vec::is_empty)
            })
            .map(|repo| format!("{} is a reference repository without a codebase map: optional, but a map helps the agents understand it", text_of(repo.get("name"))))
            .collect()
    }

    fn list_unready_subprojects(&self, context: &ProjectContext, repo: &Value) -> Vec<Value> {
        let name = text_of(repo.get("name"));
        if repo["monorepo"]["confirmed"] != true {
            return vec![json!({"name": name, "missing": [MISSING_SUBPROJECTS], "detail": format!("{name}: confirm the list of subprojects first")})];
        }
        let Ok(description) = self.app.subprojects().describe(context, Some(&name)) else { return vec![] };
        let mut lines = Vec::new();
        for row in description["projects"].as_array().into_iter().flatten() {
            if row["ready"] == true {
                continue;
            }
            let missing: Vec<String> = row["missing"].as_array().map(|m| m.iter().map(|i| text_of(Some(i))).collect()).unwrap_or_default();
            let mut parts: Vec<&str> = Vec::new();
            if missing.iter().any(|m| m == MISSING_FOLDER) {
                parts.push("the folder does not exist");
            }
            if missing.iter().any(|m| m == MISSING_CONFORMANCE) {
                parts.push("no Conformance");
            }
            if missing.iter().any(|m| m == MISSING_MAP) {
                parts.push("no codebase map");
            }
            lines.push(json!({"name": name, "missing": missing, "detail": format!("{name}/{}: {}", text_of(row.get("path")), parts.join(", "))}));
        }
        lines
    }

    /// `{"confirmed": bool}` for a secondary repo whose entry in the primary's SUBPROJECTS.json says monorepo, else `None`.
    fn read_monorepo_state(&self, context: Option<&ProjectContext>, repo: &RepoRef) -> Option<Value> {
        let context = context?;
        if repo.primary {
            return None;
        }
        match self.app.subprojects_files().read_repo_entry(&context.work_dir(), &repo.name) {
            Read::Data(entry) if entry.get("monorepo") == Some(&Value::Bool(true)) => Some(json!({"confirmed": entry.get("confirmed") == Some(&Value::Bool(true))})),
            _ => None,
        }
    }

    /// The primary is described too (same facts, for a complete table); its own checklist stays the project's `progress`.
    pub fn describe_repo(&self, repo: &RepoRef, context: Option<&ProjectContext>) -> Value {
        let folder = Self::work_folder(repo);
        let exists = Path::new(&folder).is_dir();
        let structure = self.app.specs_structure();
        let conformance = if exists { structure.get_conformance(&folder) } else { None };
        let has_code = exists && ProjectProgressService::repo_has_code(&folder);
        let map_facts = if exists { ProjectProgressService::read_map_facts(&folder, &folder) } else { json!({"map_files": [], "map_folder": "", "map_commit": null}) };
        let job = if exists { self.app.jobs().get_running_job(&repo.path) } else { None };
        let map_running = job.as_ref().is_some_and(|j| j["type"] == MAP_JOB_TYPE);
        let monorepo = self.read_monorepo_state(context, repo);
        let confirmed_monorepo = monorepo.as_ref().is_some_and(|m| m["confirmed"] == true);
        let mut missing: Vec<&str> = Vec::new();
        if !confirmed_monorepo && exists && conformance.is_none() {
            missing.push(MISSING_CONFORMANCE); // for a confirmed monorepo the Conformance and the map live per subproject (`list_unready` reports them)
        }
        if !confirmed_monorepo && exists && has_code && map_facts["map_files"].as_array().is_none_or(Vec::is_empty) {
            missing.push(MISSING_MAP);
        }
        let suggestion = if exists && conformance.is_none() && monorepo.is_none() { Some(structure.suggest_conformance(&folder)) } else { None };
        json!({
            "name": repo.name, "primary": repo.primary, "role": repo.role, "path": repo.path, "folder": folder, "exists": exists, "has_code": has_code,
            "conformance": conformance, "suggestion": suggestion,
            "map": {"files": map_facts["map_files"], "folder": map_facts["map_folder"], "commit": map_facts["map_commit"], "running": map_running, "job": if map_running { job.as_ref().map(|j| j["id"].clone()).unwrap_or(Value::Null) } else { Value::Null }},
            "monorepo": monorepo, "missing": missing, "ready": exists && missing.is_empty() && !map_running, "detail": Self::describe_missing(repo, exists, &missing, map_running),
        })
    }

    fn describe_missing(repo: &RepoRef, exists: bool, missing: &[&str], map_running: bool) -> String {
        if !exists {
            return format!("{}: the folder does not exist", repo.name);
        }
        let mut parts: Vec<&str> = Vec::new();
        if missing.contains(&MISSING_CONFORMANCE) {
            parts.push("no Conformance line (STANDARD or LEGACY)");
        }
        if missing.contains(&MISSING_MAP) {
            parts.push("no codebase map");
        }
        if map_running {
            parts.push("mapping now");
        }
        if parts.is_empty() {
            format!("{}: ready", repo.name)
        } else {
            format!("{}: {}", repo.name, parts.join(", "))
        }
    }

    fn work_folder(repo: &RepoRef) -> String {
        let worktree = repo.loop_worktree_path();
        if Path::new(&worktree).is_dir() {
            worktree
        } else {
            repo.path.clone()
        }
    }

    // ------------------------------------------------ actions
    /// Writes the Conformance block in the CLAUDE.md of one repo (the primary has its own route: `/p/<id>/api/conformance`).
    pub fn set_conformance(&self, context: &ProjectContext, name: &str, value: &str) -> Result<Value, AppError> {
        let repo = self.resolve_secondary(context, name)?;
        self.ensure_loop_stopped(context)?;
        let folder = Self::work_folder(&repo);
        if !Path::new(&folder).is_dir() {
            return Err(AppError::new(Kind::InvalidRepo, format!("the repo folder does not exist: {folder}")).with_code(FOLDER_MISSING_CODE));
        }
        let monorepo = self.read_monorepo_state(Some(context), &repo);
        if monorepo.as_ref().is_some_and(|m| m["confirmed"] == true) {
            // the folder's own .specs says nothing about it: the primary's SUBPROJECTS.json does
            return Err(AppError::new(Kind::RepoConflict, MONOREPO_ROOT_CONFORMANCE_MESSAGE).with_code(MONOREPO_ROOT_CONFORMANCE_CODE));
        }
        let structure = self.app.specs_structure();
        let written = structure.write_conformance(&folder, value)?;
        structure.ensure_specs_in_gitignore(&folder); // defining the Conformance always leaves `.specs/` out of git
        if repo.role != project_repos::ROLE_REFERENCE && monorepo.is_none() {
            self.refresh_context_block(context, &repo, &folder);
        }
        self.app.log.info("repo_conformance_set", &[("project", json!(context.project.id)), ("repo", json!(repo.name)), ("value", json!(written))]);
        Ok(json!({"ok": true, "name": repo.name, "conformance": written}))
    }

    /// Starts the codebase map job of one repo (claude runs in THAT repo; it writes only `docs/codebase`, like the primary's map).
    pub fn map_repo(&self, context: &ProjectContext, name: &str) -> Result<Value, AppError> {
        let repo = self.resolve_secondary(context, name)?;
        self.ensure_loop_stopped(context)?;
        if !Path::new(&repo.path).is_dir() {
            return Err(AppError::new(Kind::InvalidRepo, format!("the repo folder does not exist: {}", repo.path)).with_code(FOLDER_MISSING_CODE));
        }
        if Path::new(&repo.loop_worktree_path()).is_dir() {
            return Err(AppError::new(
                Kind::RepoConflict,
                format!("the worktree of {} already exists: map it from the loop worktree or remove the worktree first", repo.name),
            )
            .with_code("worktree_exists"));
        }
        self.app.specs_structure().ensure_specs_in_gitignore(&repo.path); // the job record lives in the repo's .specs: keep git clean of it
        let repo_context = ProjectContext::new(Project {
            id: format!("{}/{}", context.project.id, repo.name),
            name: repo.name.clone(),
            repo_path: repo.path.clone(),
            opened_at: String::new(),
            last_opened_at: String::new(),
            favorite: false,
        });
        let job = self.app.roadmap().map_codebase(&repo_context)?;
        self.app.log.info("repo_map_started", &[("project", json!(context.project.id)), ("repo", json!(repo.name)), ("job", job["id"].clone())]);
        Ok(json!({"ok": true, "name": repo.name, "job": job["id"], "detail": job}))
    }

    // ------------------------------------------------ support
    /// The `maestro:context` block at the root of a non-monorepo secondary edit repo (a failure is logged, never raised).
    fn refresh_context_block(&self, context: &ProjectContext, repo: &RepoRef, folder: &str) {
        self.app.context_blocks().refresh_folder(folder, Some(&repo.name), "", Some(&context.work_dir()));
    }

    fn resolve_secondary(&self, context: &ProjectContext, name: &str) -> Result<RepoRef, AppError> {
        let repo = self.app.repo_registry().resolve(context, name)?;
        if repo.primary {
            return Err(AppError::new(Kind::RepoConflict, "the primary repo uses the project checklist, not this route").with_code("primary"));
        }
        Ok(repo)
    }

    fn ensure_loop_stopped(&self, context: &ProjectContext) -> Result<(), AppError> {
        if self.app.loop_status().is_loop_running(context) {
            return Err(AppError::loop_running("a loop is running in this project: stop it first").with_code("loop_running"));
        }
        Ok(())
    }
}
