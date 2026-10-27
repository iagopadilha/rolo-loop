// SPDX-License-Identifier: MIT
//! Repo catalog job and reads (`/api/app/catalog`).
//!
//! The job summarizes each requested repo with a headless `claude -p` and stores the validated result in `~/.loop-harness/repo-catalog.json` (the
//! engine's `repo_catalog`). Confinement, in three layers:
//!   1. the AI has NO write tool: `--allowedTools Read Grep Glob Skill`, `--disallowedTools Bash Edit Write NotebookEdit Agent Web...`,
//!      `--permission-mode dontAsk`, no MCP; it only answers with a JSON sheet;
//!   2. the file guard snapshots the repo before and undoes ANY change after (nothing is allowed to change in a repo);
//!   3. only this service writes, and only the catalog file, through `repo_catalog::store_entry`, after validation.
//!
//! A repo without `docs/codebase` is not sent to the AI: it is stored as `weak` and the job asks to map it first. A repo whose HEAD did not change
//! keeps its sheet (`unchanged`). Jobs are kept in memory (one scan at a time, app-wide).

use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::infrastructure::claude_client::{call_kind, redact_secrets, CommandOptions};
use crate::infrastructure::workspace_file_guard::WorkspaceFileGuard;
use engine_state::loop_io::now_iso;
use engine_state::repo_catalog as catalog;
use pycompat::Re;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::LazyLock;

pub const CATALOG_JOB_TYPE: &str = "repo-catalog";
pub const CATALOG_BUSY_CODE: &str = "busy";
const JOB_STATE_RUNNING: &str = "running";
const JOB_STATE_OK: &str = "ok";
const JOB_STATE_ERROR: &str = "error";
const STATUS_SCANNED: &str = "scanned";
const STATUS_UNCHANGED: &str = "unchanged";
const STATUS_WEAK: &str = "weak";
const STATUS_ERROR: &str = "error";
/// Empty = the account's default model.
const CATALOG_MODEL: &str = "";
const CATALOG_BUDGET_USD: f64 = 1.0;
const CATALOG_TIMEOUT_SECONDS: u64 = 600;
const CATALOG_PERMISSION_MODE: &str = "dontAsk";
const CATALOG_ALLOWED_TOOLS: [&str; 4] = ["Read", "Grep", "Glob", "Skill"];
const CATALOG_DISALLOWED_TOOLS: [&str; 7] = ["Bash", "Edit", "Write", "NotebookEdit", "Agent", "WebFetch", "WebSearch"];
const GUARD_IGNORED_PATHS: [&str; 9] = [".git", "node_modules", "bin", "obj", "dist", "build", ".venv", "venv", "__pycache__"];
const PATHS_LIMIT: usize = 50;
const WEAK_MESSAGE: &str = "No docs/codebase; map it first.";
const PROMPT: &str = "Use the repo-catalog skill format. Summarize THIS repository (your working directory) as ONE JSON object and answer with only that object (no prose, no code fence):\n{\"purpose\": \"<one sentence, at most 200 characters>\", \"capabilities\": [{\"id\": \"<kebab-case>\", \"description\": \"<one sentence>\", \"evidence\": \"<path of an existing file, relative to the repo root, forward slashes>\"}]}\nStart from docs/codebase/*, then confirm each capability in code. Rules: never invent a capability; every evidence file must exist (open or search it before citing); never include secrets, hosts or customer names; 3 to 12 capabilities; you cannot and must not change any file.";

static FENCE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^```[a-zA-Z]*\s*|\s*```\z"));

/// First JSON object found in the reply (tolerates a code fence or text around it); `None` when there is none.
pub fn extract_json_object(reply: &str) -> Option<Value> {
    let text = FENCE.replace_lit(reply.trim(), "");
    for (index, _) in text.match_indices('{') {
        if let Some(Ok(value)) = serde_json::Deserializer::from_str(&text[index..]).into_iter::<Value>().next() {
            if value.is_object() {
                return Some(value);
            }
        }
    }
    None
}

/// Jobs of the catalog scan, in memory.
#[derive(Default)]
pub struct CatalogState {
    jobs: Vec<(String, Value)>,
    running_job_id: String,
    counter: u64,
}

pub struct RepoCatalogService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn repo_catalog(&self) -> RepoCatalogService<'_> {
        RepoCatalogService { app: self }
    }
}

impl RepoCatalogService<'_> {
    // ------------------------------------------------ reads
    /// `{"ok", "repos": [{name, path, weak, stale, purpose, capability_ids}]}`: what the planning chat reads.
    pub fn get_index(&self) -> Value {
        let catalog_file = catalog::load_catalog(&self.app.env);
        let mut repos: Vec<Value> = Vec::new();
        for (key, entry) in catalog_file["repos"].as_object().into_iter().flatten() {
            if !entry.is_object() {
                continue;
            }
            let path = entry.get("path").filter(|p| engine_state::notify_truthy(Some(p))).and_then(Value::as_str).unwrap_or(key).to_string();
            let ids: Vec<Value> = entry.get("capabilities").and_then(Value::as_array).into_iter().flatten().filter(|c| c.is_object()).map(|c| c.get("id").cloned().unwrap_or(Value::Null)).collect();
            repos.push(json!({
                "name": entry.get("name").cloned().unwrap_or(json!("")), "path": path, "weak": engine_state::notify_truthy(entry.get("weak")), "stale": self.is_stale(&path, entry),
                "purpose": entry.get("purpose").cloned().unwrap_or(json!("")), "capability_ids": ids,
            }));
        }
        let known: Vec<String> = repos.iter().map(|r| catalog::normalize_path(r["path"].as_str().unwrap_or(""))).collect();
        for path in self.list_known_paths() {
            let normalized = catalog::normalize_path(&path);
            if !known.contains(&normalized) {
                repos.push(json!({"name": pycompat::last_segment(&normalized), "path": normalized, "weak": true, "stale": true, "purpose": "", "capability_ids": []}));
            }
        }
        json!({"ok": true, "repos": repos})
    }

    /// The stored entry of the repo or `None` (the controller answers 404).
    pub fn get_sheet(&self, path: &Value) -> Result<Option<Value>, AppError> {
        let Some(path) = path.as_str().filter(|p| !p.trim().is_empty()) else { return Err(AppError::invalid_request("path is required")) };
        Ok(catalog::get_entry(&self.app.env, path).map(|entry| json!({"ok": true, "entry": entry})))
    }

    pub fn get_job(&self, job_id: &str) -> Result<Value, AppError> {
        let state = self.app.catalog_state.lock().unwrap_or_else(|e| e.into_inner());
        match state.jobs.iter().find(|(id, _)| id == job_id) {
            Some((_, job)) => Ok(json!({"ok": true, "job": job})),
            None => Err(AppError::new(Kind::JobNotFound, "job not found")),
        }
    }

    // ------------------------------------------------ scan job
    /// Starts the background job over `paths`; a conflict (409 `busy`) while another scan runs.
    pub fn start_scan(&self, paths: &Value) -> Result<Value, AppError> {
        let normalized = Self::validate_paths(paths)?;
        let job_id;
        {
            let mut state = self.app.catalog_state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.running_job_id.is_empty() {
                return Err(AppError::new(Kind::JobInProgress, "a catalog scan is already running: wait for it to finish").with_code(CATALOG_BUSY_CODE));
            }
            state.counter += 1;
            job_id = format!("cat-{}-{}", state.counter, crate::infrastructure::ids::random_hex(3));
            let job = json!({"id": job_id, "type": CATALOG_JOB_TYPE, "state": JOB_STATE_RUNNING, "started_at": now_iso(), "finished_at": null, "cost_usd": 0, "error": "", "results": []});
            state.jobs.push((job_id.clone(), job));
            state.running_job_id = job_id.clone();
        }
        let (app, id, thread_paths) = (self.app.shared(), job_id.clone(), normalized.clone());
        if std::thread::Builder::new().name(format!("repo-catalog-{job_id}")).spawn(move || app.repo_catalog().run_job(&id, &thread_paths)).is_err() {
            self.finish_job(&job_id, JOB_STATE_ERROR, "internal failure: thread");
            return Err(AppError::new(Kind::Internal, "the scan thread cannot start"));
        }
        self.app.log.info("repo_catalog_started", &[("job", json!(job_id)), ("repos", json!(normalized.len()))]);
        Ok(json!({"ok": true, "job_id": job_id}))
    }

    fn validate_paths(paths: &Value) -> Result<Vec<String>, AppError> {
        let Some(list) = paths.as_array().filter(|p| !p.is_empty() && p.len() <= PATHS_LIMIT) else {
            return Err(AppError::invalid_request(format!("paths must be a list of 1 to {PATHS_LIMIT} folders")));
        };
        let mut normalized: Vec<String> = Vec::new();
        for path in list {
            match path.as_str() {
                Some(text) if Path::new(text).is_dir() => {
                    if !normalized.iter().any(|n| n == text) {
                        normalized.push(text.to_string());
                    }
                }
                Some(text) => return Err(AppError::invalid_request(format!("not a folder: {text}"))),
                None => return Err(AppError::invalid_request("not a folder: (invalid)")),
            }
        }
        Ok(normalized)
    }

    pub fn run_job(&self, job_id: &str, paths: &[String]) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for path in paths {
                let (result, cost) = self.scan_one(path);
                let mut state = self.app.catalog_state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some((_, job)) = state.jobs.iter_mut().find(|(id, _)| id == job_id) {
                    job["results"].as_array_mut().expect("results").push(result);
                    let total = job["cost_usd"].as_f64().unwrap_or(0.0) + cost;
                    job["cost_usd"] = json!((total * 1e4).round_ties_even() / 1e4);
                }
            }
        }));
        let (state, error) = match outcome {
            Ok(()) => (JOB_STATE_OK, String::new()),
            Err(_) => {
                self.app.log.error("repo_catalog_failed", &[("job", json!(job_id)), ("type", json!("panic")), ("message", json!("the scan panicked"))]);
                (JOB_STATE_ERROR, "internal failure: panic".to_string())
            }
        };
        self.finish_job(job_id, state, &error);
        let cost = self.app.catalog_state.lock().unwrap_or_else(|e| e.into_inner()).jobs.iter().find(|(id, _)| id == job_id).map(|(_, j)| j["cost_usd"].clone()).unwrap_or(json!(0));
        self.app.log.info("repo_catalog_finished", &[("job", json!(job_id)), ("state", json!(state)), ("cost_usd", cost)]);
    }

    fn finish_job(&self, job_id: &str, state_text: &str, error: &str) {
        let mut state = self.app.catalog_state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, job)) = state.jobs.iter_mut().find(|(id, _)| id == job_id) {
            job["state"] = json!(state_text);
            job["error"] = json!(error);
            job["finished_at"] = json!(now_iso());
        }
        state.running_job_id.clear();
    }

    /// `(result, cost_usd)` of one repo. Never fails for a repo-level problem: it becomes status `error`.
    fn scan_one(&self, path: &str) -> (Value, f64) {
        let facts = catalog::scan(path);
        let mut result = json!({"path": facts["path"], "status": STATUS_ERROR, "weak": facts["has_docs_codebase"] != true, "rejected": [], "message": ""});
        let head = facts["head"].as_str().filter(|h| !h.is_empty());
        let Some(head) = head else {
            result["message"] = json!("Not a git repository, or it has no commit yet.");
            return (result, 0.0);
        };
        if catalog::is_cached(&self.app.env, path, Some(head)) {
            let entry = catalog::get_entry(&self.app.env, path).unwrap_or(json!({}));
            result["status"] = json!(STATUS_UNCHANGED);
            result["weak"] = json!(engine_state::notify_truthy(entry.get("weak")));
            return (result, 0.0);
        }
        if facts["has_docs_codebase"] != true {
            let _ = catalog::store_entry(&self.app.env, &catalog::build_weak_entry(&facts));
            result["status"] = json!(STATUS_WEAK);
            result["weak"] = json!(true);
            result["message"] = json!(WEAK_MESSAGE);
            return (result, 0.0);
        }
        let (sheet, cost, error) = self.ask_for_sheet(path);
        if !error.is_empty() {
            result["message"] = json!(error);
            return (result, cost);
        }
        let (entry, rejected) = catalog::build_entry(&facts, &sheet.unwrap_or(Value::Null), path);
        let Some(entry) = entry else {
            let reasons: Vec<&str> = rejected.iter().filter_map(|r| r["reason"].as_str()).collect();
            result["message"] = json!(format!("The AI sheet was refused: {}", redact_secrets(&reasons.join("; "), Some(300))));
            return (result, cost);
        };
        let _ = catalog::store_entry(&self.app.env, &entry);
        result["status"] = json!(STATUS_SCANNED);
        result["weak"] = entry["weak"].clone();
        result["rejected"] = json!(rejected);
        (result, cost)
    }

    /// `(sheet, cost_usd, error)`: runs the confined `claude -p` in the repo and undoes any change it made there.
    fn ask_for_sheet(&self, path: &str) -> (Option<Value>, f64, String) {
        let guard = WorkspaceFileGuard::default();
        let snapshot = guard.take_snapshot(path, &GUARD_IGNORED_PATHS, &crate::infrastructure::workspace_file_guard::DEFAULT_IGNORED_NAMES);
        let options = CommandOptions {
            model: CATALOG_MODEL.into(),
            budget_usd: Some(CATALOG_BUDGET_USD),
            allowed_tools: CATALOG_ALLOWED_TOOLS.iter().map(|t| t.to_string()).collect(),
            disallowed_tools: CATALOG_DISALLOWED_TOOLS.iter().map(|t| t.to_string()).collect(),
            no_mcp: true,
            permission_mode: CATALOG_PERMISSION_MODE.into(),
            ..CommandOptions::default()
        };
        let argv = self.app.claude.build_command(&options);
        let run = {
            let _kind = call_kind("catalog");
            self.app.claude.run_prompt(&argv, path, PROMPT, CATALOG_TIMEOUT_SECONDS, &[])
        };
        let report = guard.revert_unexpected_changes(&snapshot, &[]); // nothing may change in the repo
        let touched: Vec<Value> = ["reverted", "removed", "unrestorable"].iter().flat_map(|k| report[k].as_array().cloned().unwrap_or_default()).collect();
        if !touched.is_empty() {
            self.app.log.warn("repo_catalog_guard_reverted", &[("repo", json!(path)), ("files", json!(touched.into_iter().take(50).collect::<Vec<_>>()))]);
        }
        let cost = run.get("cost_usd").filter(|c| c.is_number()).and_then(Value::as_f64).unwrap_or(0.0);
        if run["ok"] != true {
            let error = run.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()).unwrap_or("claude finished with an error");
            return (None, cost, error.to_string());
        }
        match extract_json_object(run.get("reply").and_then(Value::as_str).unwrap_or("")) {
            Some(sheet) => (Some(sheet), cost, String::new()),
            None => (None, cost, "The AI did not return a JSON sheet.".into()),
        }
    }

    // ------------------------------------------------ support
    fn is_stale(&self, path: &str, entry: &Value) -> bool {
        let head = if Path::new(path).is_dir() { catalog::read_head(path) } else { String::new() };
        !head.is_empty() && entry.get("head").and_then(Value::as_str) != Some(head.as_str())
    }

    /// Every repo path listed in the REPOS.json of every known project (so unscanned repos show in the catalog as never scanned).
    fn list_known_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = Vec::new();
        for project in self.app.project_service().list_projects() {
            let context = crate::domain::project::ProjectContext::new(project);
            let (_, stored) = self.app.repo_registry().read_stored(&context);
            for repo in stored {
                if !paths.contains(&repo.path) {
                    paths.push(repo.path);
                }
            }
        }
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_found_in_fenced_or_chatty_replies() {
        assert_eq!(extract_json_object("```json\n{\"purpose\": \"x\"}\n```"), Some(json!({"purpose": "x"})));
        assert_eq!(extract_json_object("Sure! {\"a\": {\"b\": 1}} done"), Some(json!({"a": {"b": 1}})));
        assert_eq!(extract_json_object("{broken {\"ok\": true}"), Some(json!({"ok": true})));
        assert_eq!(extract_json_object("no object"), None);
        assert_eq!(extract_json_object("[1, 2]"), None);
    }

    #[test]
    fn paths_are_checked_and_unknown_jobs_are_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let service = app.repo_catalog();
        assert_eq!(service.start_scan(&json!([])).unwrap_err().message, "paths must be a list of 1 to 50 folders");
        assert_eq!(service.start_scan(&json!([7])).unwrap_err().message, "not a folder: (invalid)");
        assert!(service.start_scan(&json!(["C:\\definitely\\missing"])).unwrap_err().message.starts_with("not a folder: "));
        assert_eq!(service.get_job("cat-0-x").unwrap_err().kind, Kind::JobNotFound);
        assert_eq!(service.get_sheet(&json!(" ")).unwrap_err().message, "path is required");
        assert_eq!(service.get_sheet(&json!("C:\\nope")).unwrap(), None);
        assert_eq!(service.get_index(), json!({"ok": true, "repos": []}));
    }

    #[test]
    fn a_repo_without_a_commit_or_docs_is_reported_not_sent_to_the_ai() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let folder = dir.path().join("plain");
        std::fs::create_dir_all(&folder).unwrap();
        let (result, cost) = app.repo_catalog().scan_one(&folder.display().to_string());
        assert_eq!((result["status"].clone(), result["message"].clone(), cost), (json!("error"), json!("Not a git repository, or it has no commit yet."), 0.0));
    }
}
