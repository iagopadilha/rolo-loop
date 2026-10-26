// SPDX-License-Identifier: MIT
//! One map job per subproject of a confirmed monorepo, ALL AT ONCE (installer, INS-04/05).
//!
//! Every selected subproject gets its own `claude` job (Sonnet, no spend cap), running in `<repo>/<path>` with the prompt
//! `/orchestrator map --subproject <path>`. The one-job-per-repository rule is relaxed only here: each job carries a `parallel_key` (its subproject
//! folder), so map jobs of different subproject folders run next to each other while any other kind of job still refuses (and is refused by) a
//! running map. At most `max_parallel_maps` run at the same time; the rest stay `queued` and start as the others end. The state is a file
//! (`.specs/project/loop/internal/subprojects/map-queue.json`):
//!
//! ```text
//! {"state": "idle"|"running"|"done"|"stopped",
//!  "items": {path: {"state": "queued"|"running"|"done"|"failed", "started_at", "finished_at", "error", "job_id", "documents"}},
//!  "system": {"state": "idle"|"waiting"|"running"|"done"|"failed", "error": "", "job_id"},
//!  "started_at": iso, "include_system": bool}
//! ```
//!
//! `include_system` is True for the "map all that are missing" call (no explicit paths) and for a retry of such a stopped run: when every item is
//! `done` the system map starts by itself (`SystemMapService::on_queue_finished` returns its job and the queue follows it to the end). A failed item
//! never stops the others; the run ends `stopped` and a new `start` retries the failed (and never started) items, keeping the finished ones. After an
//! app restart a `running` file with no job alive is read as `stopped`: the items that were running become `failed` ("interrupted") and show Retry.
//!
//! The public payload (`public_queue`) also keeps the keys of the old sequential queue (`queue`, `current`, `done`, `failed`, `budget_usd`).

use super::claude_jobs::{ClaudeJobError, JOB_MODEL_CEILING};
use super::code_inventory::inventory_name;
use super::map_job::MapStart;
use super::subprojects::{not_confirmed, MAP_RELATIVE_FOLDER};
use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use crate::repositories::loop_command_file::now_iso;
use crate::repositories::project_files::Read;
use engine_state::{run_hints, subprojects};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SUBPROJECT_MAP_PROMPT: &str = "/orchestrator map --subproject ";
pub const QUEUE_IDLE: &str = "idle";
pub const QUEUE_RUNNING: &str = "running";
pub const QUEUE_DONE: &str = "done";
pub const QUEUE_STOPPED: &str = "stopped";
pub const ITEM_QUEUED: &str = "queued";
pub const ITEM_RUNNING: &str = "running";
pub const ITEM_DONE: &str = "done";
pub const ITEM_FAILED: &str = "failed";
pub const SYSTEM_IDLE: &str = "idle";
pub const SYSTEM_WAITING: &str = "waiting";
pub const SYSTEM_RUNNING: &str = "running";
pub const SYSTEM_DONE: &str = "done";
pub const SYSTEM_FAILED: &str = "failed";
pub const INTERRUPTED_REASON: &str = "interrupted: the app was shut down while the job was running";
const WATCH_INTERVAL: Duration = Duration::from_millis(100);
/// No job has it: `get_blocking_job` then answers only for the jobs that cannot run in parallel.
const PROBE_PARALLEL_KEY: &str = "\0probe";
const ITEM_FIELDS: [&str; 6] = ["state", "started_at", "finished_at", "error", "job_id", "documents"];

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

fn state_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn clip(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn project_folder(folder: &str, path: &str) -> PathBuf {
    path.split('/').filter(|p| !p.is_empty()).fold(PathBuf::from(folder), |a, p| a.join(p))
}

fn map_folder(folder: &str, path: &str) -> PathBuf {
    MAP_RELATIVE_FOLDER.split('/').fold(project_folder(folder, path), |a, p| a.join(p))
}

pub fn folder_key(folder: &str) -> String {
    pycompat::nt_normcase(&pycompat::nt_abspath(folder))
}

fn parallel_key(folder: &str, path: &str) -> String {
    folder_key(&project_folder(folder, path).display().to_string())
}

pub fn new_item() -> Value {
    json!({"state": ITEM_QUEUED, "started_at": null, "finished_at": null, "error": "", "job_id": "", "documents": 0})
}

/// `running` while any item is queued or running or the system map is waiting/running; else `stopped` when something failed (an item or the system
/// map), else `done`.
pub fn compute_state(state: &Value) -> &'static str {
    let items: Vec<&Value> = state["items"].as_object().map(|m| m.values().collect()).unwrap_or_default();
    let system = state_of(&state["system"], "state");
    if items.iter().any(|i| matches!(state_of(i, "state"), ITEM_QUEUED | ITEM_RUNNING)) || matches!(system, SYSTEM_WAITING | SYSTEM_RUNNING) {
        return QUEUE_RUNNING;
    }
    if items.iter().any(|i| state_of(i, "state") == ITEM_FAILED) || system == SYSTEM_FAILED {
        return QUEUE_STOPPED;
    }
    QUEUE_DONE
}

/// Under the queue lock, once no item is queued or running: true (exactly once) when the system map must start now; a run with a failure never
/// starts it (the system map goes back to idle).
pub fn claim_system_step(state: &mut Value) -> bool {
    if state_of(&state["system"], "state") != SYSTEM_WAITING {
        return false;
    }
    let states: Vec<String> = state["items"].as_object().map(|m| m.values().map(|i| state_of(i, "state").to_string()).collect()).unwrap_or_default();
    if states.iter().any(|s| s == ITEM_QUEUED || s == ITEM_RUNNING) {
        return false;
    }
    if states.iter().any(|s| s == ITEM_FAILED) {
        state["system"]["state"] = json!(SYSTEM_IDLE);
        return false;
    }
    state["system"]["state"] = json!(SYSTEM_RUNNING);
    true
}

/// What the table and the installer read.
pub fn public_queue(state: &Value) -> Value {
    if !state.is_object() {
        return Value::Null;
    }
    let items: Map<String, Value> = state.get("items").and_then(Value::as_object).map(|items| {
        items.iter().map(|(path, item)| (path.clone(), Value::Object(ITEM_FIELDS.iter().map(|f| (f.to_string(), item.get(*f).cloned().unwrap_or(Value::Null))).collect()))).collect()
    }).unwrap_or_default();
    let by_state = |wanted: &str| -> Vec<String> { items.iter().filter(|(_, i)| state_of(i, "state") == wanted).map(|(p, _)| p.clone()).collect() };
    let running = by_state(ITEM_RUNNING);
    let system = state.get("system").cloned().unwrap_or(json!({}));
    let failed: Map<String, Value> = items.iter().filter(|(_, i)| state_of(i, "state") == ITEM_FAILED).map(|(p, i)| (p.clone(), i["error"].clone())).collect();
    json!({
        "state": state.get("state").cloned().unwrap_or(json!(QUEUE_IDLE)),
        "items": items,
        "system": {"state": system.get("state").cloned().unwrap_or(json!(SYSTEM_IDLE)), "error": system.get("error").cloned().unwrap_or(json!(""))},
        "started_at": state.get("started_at").cloned().unwrap_or(json!("")),
        "include_system": state.get("include_system") == Some(&Value::Bool(true)),
        // keys of the old sequential queue, kept for compatibility
        "queue": by_state(ITEM_QUEUED), "current": running.first(), "done": by_state(ITEM_DONE), "failed": failed, "budget_usd": null,
    })
}

fn convert_legacy_queue(old: &Value) -> Value {
    let mut items = Map::new();
    let with_state = |state: &str| {
        let mut item = new_item();
        item["state"] = json!(state);
        item
    };
    for path in old.get("done").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        items.insert(path.to_string(), with_state(ITEM_DONE));
    }
    if let Some(failed) = old.get("failed").and_then(Value::as_object) {
        for (path, reason) in failed {
            let mut item = with_state(ITEM_FAILED);
            item["error"] = json!(text_of(Some(reason)));
            items.insert(path.clone(), item);
        }
    }
    if let Some(current) = old.get("current").and_then(Value::as_str).filter(|c| !c.is_empty()) {
        items.insert(current.to_string(), with_state(ITEM_RUNNING));
    }
    for path in old.get("queue").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        items.insert(path.to_string(), new_item());
    }
    let legacy_state = old.get("state").and_then(Value::as_str).unwrap_or("");
    json!({"state": if legacy_state == "finished" { QUEUE_DONE } else if legacy_state.is_empty() { QUEUE_IDLE } else { legacy_state },
           "items": items, "system": {"state": SYSTEM_IDLE, "error": ""}, "started_at": old.get("started_at").cloned().unwrap_or(json!("")),
           "include_system": old.get("include_system") == Some(&Value::Bool(true))})
}

pub struct SubprojectMapService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn subproject_maps(&self) -> SubprojectMapService<'_> {
        SubprojectMapService { app: self }
    }
}

impl SubprojectMapService<'_> {
    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.app.map_queue_lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn max_parallel(&self) -> usize {
        self.app.config.max_parallel_maps.max(1) as usize
    }

    // ------------------------------------------------ start (202 path)
    /// Validates, writes the queue, starts every job (up to `max_parallel_maps`) and returns `{ok, job, queue}` at once. `budget_usd` is accepted and
    /// ignored: the setup jobs have no spend cap.
    pub fn start(&self, context: &ProjectContext, paths: Option<&Value>, repo_name: Option<&str>) -> Result<Value, AppError> {
        let project_id = context.project.id.clone();
        self.app.log.info("map_requested", &[("project", json!(project_id)), ("scope", json!("subprojects")), ("paths", paths.cloned().unwrap_or(Value::Null))]);
        let prepared = (|| -> Result<(String, String, Option<Value>, Vec<String>), AppError> {
            let (key, folder) = self.app.subprojects().resolve_target(context, repo_name)?;
            self.ensure_free_for_map_jobs(context)?;
            let projects = self.read_confirmed_projects(context, &key)?;
            let previous = self.read_queue_state(context, repo_name)?;
            if previous.as_ref().is_some_and(|p| state_of(p, "state") == QUEUE_RUNNING) {
                return Err(AppError::new(Kind::JobInProgress, "the subprojects are already being mapped: wait for them to finish"));
            }
            let selected = self.select_paths(&folder, &projects, paths, previous.as_ref())?;
            for path in &selected {
                if self.app.jobs().get_blocking_job(&folder, Some(&parallel_key(&folder, path))).is_some() {
                    return Err(AppError::new(Kind::JobInProgress, "a claude job is already running in this project: wait for it to finish"));
                }
            }
            Ok((key, folder, previous, selected))
        })();
        let (key, folder, previous, selected) = match prepared {
            Ok(parts) => parts,
            Err(refusal) => {
                self.app.log.info("map_refused", &[("project", json!(project_id)), ("scope", json!("subprojects")), ("reason", json!(format!("{:?}", refusal.kind))), ("message", json!(clip(&refusal.message, 200)))]);
                return Err(refusal);
            }
        };
        let stopped = previous.filter(|p| state_of(p, "state") == QUEUE_STOPPED);
        let include_system = paths.is_none() || stopped.as_ref().is_some_and(|s| s.get("include_system") == Some(&Value::Bool(true)));
        let _guard = self.lock();
        let mut items = Map::new();
        if let Some(stopped) = &stopped {
            for (path, item) in stopped["items"].as_object().into_iter().flatten() {
                if state_of(item, "state") == ITEM_DONE {
                    items.insert(path.clone(), item.clone());
                }
            }
        }
        for path in &selected {
            items.insert(path.clone(), new_item());
        }
        let mut state = json!({"state": QUEUE_RUNNING, "items": items, "started_at": now_iso(), "include_system": include_system,
                               "system": {"state": if include_system { SYSTEM_WAITING } else { SYSTEM_IDLE }, "error": ""}});
        self.app.map_active_folders.lock().unwrap_or_else(|e| e.into_inner()).insert(folder_key(&folder));
        let jobs = match self.start_ready_items(context, &key, &folder, &mut state, true) {
            Ok(jobs) => jobs,
            Err(failure) => {
                self.app.map_active_folders.lock().unwrap_or_else(|e| e.into_inner()).remove(&folder_key(&folder));
                return Err(failure);
            }
        };
        self.write_queue(&folder, &state);
        Ok(json!({"ok": true, "job": jobs.first().cloned().unwrap_or(Value::Null), "queue": public_queue(&state)}))
    }

    /// The root map's refusals (loop running, worktree exists), except "a job is running" when every running job is itself a parallel subproject map
    /// (those are checked per folder by `start`).
    fn ensure_free_for_map_jobs(&self, context: &ProjectContext) -> Result<(), AppError> {
        match self.app.roadmap().ensure_repo_free_for_job(context) {
            Err(error) if error.kind == Kind::JobInProgress => {
                if self.app.jobs().get_blocking_job(context.repo_path(), Some(PROBE_PARALLEL_KEY)).is_some() { Err(error) } else { Ok(()) }
            }
            other => other,
        }
    }

    /// `{normalized path: project}` of the confirmed list.
    fn read_confirmed_projects(&self, context: &ProjectContext, key: &str) -> Result<Vec<(String, Map<String, Value>)>, AppError> {
        match self.app.subprojects_files().read_repo_entry(&context.work_dir(), key) {
            Read::Data(entry) if entry.get("monorepo") == Some(&Value::Bool(true)) && entry.get("confirmed") == Some(&Value::Bool(true)) => {
                Ok(subprojects::entry_projects(Some(&entry)).into_iter().map(|p| (subprojects::normalize(&text_of(p.get("path"))), p)).collect())
            }
            _ => Err(not_confirmed()),
        }
    }

    /// Explicit paths must be listed, not skipped and exist on disk (else the request is refused). Without paths: the failed and not-started items of
    /// a stopped queue, else every non-skipped project with code and no map yet (missing folders are left out).
    fn select_paths(&self, folder: &str, projects: &[(String, Map<String, Value>)], requested: Option<&Value>, previous: Option<&Value>) -> Result<Vec<String>, AppError> {
        let find = |path: &str| projects.iter().find(|(p, _)| p == path).map(|(_, project)| project);
        if let Some(requested) = requested {
            let list = requested.as_array().filter(|l| !l.is_empty() && l.iter().all(Value::is_string));
            let Some(list) = list else { return Err(AppError::invalid_request("paths must be a non-empty list of subproject paths")) };
            let mut selected: Vec<String> = Vec::new();
            for raw in list.iter().filter_map(Value::as_str) {
                let path = subprojects::normalize(raw);
                let Some(project) = find(&path).filter(|p| p.get("skip") != Some(&Value::Bool(true))) else {
                    return Err(AppError::invalid_request(format!("{raw} is not a mappable subproject of the confirmed list")));
                };
                let _ = project;
                if !project_folder(folder, &path).is_dir() {
                    return Err(AppError::invalid_path(format!("the subproject folder does not exist: {path}")));
                }
                if !selected.contains(&path) {
                    selected.push(path);
                }
            }
            return Ok(selected);
        }
        let stopped = previous.filter(|p| state_of(p, "state") == QUEUE_STOPPED);
        let candidates: Vec<String> = match stopped {
            Some(stopped) => stopped["items"].as_object().into_iter().flatten().filter(|(_, i)| matches!(state_of(i, "state"), ITEM_FAILED | ITEM_QUEUED)).map(|(p, _)| p.clone()).collect(),
            None => projects.iter().filter(|(path, p)| p.get("has_code") == Some(&Value::Bool(true)) && run_hints::list_map_documents(&map_folder(folder, path)).is_empty()).map(|(p, _)| p.clone()).collect(),
        };
        let mut selected: Vec<String> = Vec::new();
        for path in candidates {
            let Some(project) = find(&path) else { continue };
            if project.get("skip") == Some(&Value::Bool(true)) || !project_folder(folder, &path).is_dir() {
                continue;
            }
            if !selected.contains(&path) {
                selected.push(path);
            }
        }
        if selected.is_empty() {
            return Err(AppError::invalid_request("no subproject to map: every non-skipped project with code already has its map"));
        }
        Ok(selected)
    }

    // ------------------------------------------------ queue execution
    /// Starts queued items while fewer than `max_parallel_maps` run (under the queue lock). Returns the started jobs. An item that cannot start becomes
    /// `failed` (the first one errors instead when `raise_on_first_failure` and nothing started yet).
    fn start_ready_items(&self, context: &ProjectContext, key: &str, folder: &str, state: &mut Value, raise_on_first_failure: bool) -> Result<Vec<Value>, AppError> {
        let mut started = Vec::new();
        let mut running = state["items"].as_object().map(|m| m.values().filter(|i| state_of(i, "state") == ITEM_RUNNING).count()).unwrap_or(0);
        let paths: Vec<String> = state["items"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        for path in paths {
            if running >= self.max_parallel() {
                break;
            }
            if state_of(&state["items"][&path], "state") != ITEM_QUEUED {
                continue;
            }
            match self.start_item(context, key, folder, state, &path) {
                Ok(job) => {
                    started.push(job);
                    running += 1;
                }
                Err(failure) => {
                    if raise_on_first_failure && started.is_empty() {
                        return Err(failure);
                    }
                    self.app.log.warn("subproject_map_next_failed", &[("path", json!(path)), ("type", json!(format!("{:?}", failure.kind))), ("message", json!(clip(&failure.message, 200)))]);
                    let item = &mut state["items"][&path];
                    item["state"] = json!(ITEM_FAILED);
                    item["finished_at"] = json!(now_iso());
                    item["error"] = json!(format!("could not start: {}", if failure.message.is_empty() { format!("{:?}", failure.kind) } else { clip(&failure.message, 300) }));
                }
            }
        }
        Ok(started)
    }

    /// Starts the job of one item and its watcher thread. Returns the public job record.
    fn start_item(&self, context: &ProjectContext, key: &str, folder: &str, state: &mut Value, path: &str) -> Result<Value, AppError> {
        let prompt = format!("{SUBPROJECT_MAP_PROMPT}{path}{}", self.app.inventory.prompt_argument(&inventory_name(path))); // the prompt extension
        let mut params = MapStart::new(folder, &context.project.id);
        params.prompt = prompt;
        params.work_dir = Some(project_folder(folder, path).display().to_string());
        params.allowed_folders = Self::allowed_folders(state, path);
        params.map_root = path.to_string();
        params.model = JOB_MODEL_CEILING.into();
        params.parallel_key = Some(parallel_key(folder, path));
        let inventory_app = self.app.shared();
        let (inventory_folder, inventory_path) = (folder.to_string(), path.to_string());
        params.extra_before_start = Some(Box::new(move |update_progress| {
            // the code inventory of the subproject, after the snapshot (runs in the job thread)
            inventory_app.inventory.run_for_job(&inventory_folder, &inventory_path, &inventory_name(&inventory_path), update_progress);
            Ok::<(), ClaudeJobError>(())
        }));
        let job = self.app.map_jobs().start(params)?;
        let item = &mut state["items"][path];
        item["state"] = json!(ITEM_RUNNING);
        item["started_at"] = json!(now_iso());
        item["finished_at"] = Value::Null;
        item["error"] = json!("");
        item["job_id"] = job["id"].clone();
        item["documents"] = json!(0);
        let (app, context, key, folder, path, job_id) = (self.app.shared(), context.clone(), key.to_string(), folder.to_string(), path.to_string(), text_of(job.get("id")));
        let thread_name = format!("subproject-map-{job_id}");
        let _ = std::thread::Builder::new().name(thread_name).spawn(move || app.subproject_maps().watch_job(&context, &key, &folder, &path, &job_id));
        Ok(job)
    }

    /// What the guard of one job leaves alone. Jobs of the same run share the repository, so each guard also leaves the map folder and the
    /// `CLAUDE.md` of the other subprojects of the run alone (their jobs and the app write there meanwhile).
    fn allowed_folders(state: &Value, path: &str) -> Vec<String> {
        let mut allowed = vec![".specs".to_string(), format!("{path}/{MAP_RELATIVE_FOLDER}")];
        for other in state["items"].as_object().into_iter().flatten().map(|(p, _)| p).filter(|p| *p != path) {
            allowed.push(format!("{other}/{MAP_RELATIVE_FOLDER}"));
            allowed.push(format!("{other}/CLAUDE.md"));
        }
        allowed
    }

    /// Job thread of one item: waits for its job to end, records the outcome, starts the next queued item and, when this was the last one and
    /// everything succeeded, runs the system map.
    fn watch_job(&self, context: &ProjectContext, key: &str, folder: &str, path: &str, job_id: &str) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.watch_job_inner(context, key, folder, path, job_id)));
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(failure)) => {
                self.app.log.error("subproject_map_watch_failed", &[("path", json!(path)), ("type", json!(format!("{:?}", failure.kind))), ("message", json!(clip(&failure.message, 200)))]);
                self.fail_item(folder, path, job_id, &format!("internal failure: {:?}", failure.kind));
            }
            Err(_) => {
                self.app.log.error("subproject_map_watch_failed", &[("path", json!(path)), ("type", json!("panic")), ("message", json!("the watcher panicked"))]);
                self.fail_item(folder, path, job_id, "internal failure: panic");
            }
        }
        let _guard = self.lock();
        let still_running = self.read_queue_file(folder).is_some_and(|s| state_of(&s, "state") == QUEUE_RUNNING);
        if !still_running {
            self.app.map_active_folders.lock().unwrap_or_else(|e| e.into_inner()).remove(&folder_key(folder));
        }
    }

    fn watch_job_inner(&self, context: &ProjectContext, key: &str, folder: &str, path: &str, job_id: &str) -> Result<(), AppError> {
        let job = self.wait_for_job(folder, job_id)?;
        if state_of(&job, "state") == "ok" {
            self.app.context_blocks().refresh_folder(folder, Some(key), path, Some(&context.work_dir()));
        }
        let documents = run_hints::list_map_documents(&map_folder(folder, path));
        let (run_system, snapshot) = {
            let _guard = self.lock();
            let Some(mut state) = self.read_queue_file(folder) else { return Ok(()) };
            let owns = state["items"].get(path).is_some_and(|i| state_of(i, "job_id") == job_id);
            if !owns {
                return Ok(());
            }
            {
                let item = &mut state["items"][path];
                if state_of(&job, "state") == "ok" {
                    item["state"] = json!(ITEM_DONE);
                    item["finished_at"] = json!(now_iso());
                    item["error"] = json!("");
                    item["documents"] = json!(documents.len());
                } else {
                    item["state"] = json!(ITEM_FAILED);
                    item["finished_at"] = json!(now_iso());
                    item["documents"] = json!(documents.len());
                    let error = Some(text_of(job.get("error"))).filter(|e| !e.is_empty()).unwrap_or_else(|| "the map job failed".into());
                    item["error"] = json!(clip(&error, 500));
                    self.app.log.warn("subproject_map_failed", &[("path", json!(path)), ("reason", json!(clip(&text_of(item.get("error")), 200)))]);
                }
            }
            let _ = self.start_ready_items(context, key, folder, &mut state, false);
            let run_system = claim_system_step(&mut state);
            state["state"] = json!(compute_state(&state));
            self.write_queue(folder, &state);
            (run_system, public_queue(&state))
        };
        if run_system {
            self.run_system_step(context, key, folder, &snapshot);
        }
        Ok(())
    }

    fn wait_for_job(&self, folder: &str, job_id: &str) -> Result<Value, AppError> {
        loop {
            let job = self.app.jobs().get_job(folder, job_id)?;
            if state_of(&job, "state") != "running" {
                return Ok(job);
            }
            std::thread::sleep(WATCH_INTERVAL);
        }
    }

    /// The system map after the last map: the callback starts it and returns its job, the queue then waits for that job to end.
    fn run_system_step(&self, context: &ProjectContext, key: &str, folder: &str, snapshot: &Value) {
        let (mut job, mut refusal, mut failure) = (None::<Value>, String::new(), String::new());
        match self.app.system_maps().on_queue_finished(context, key, folder, snapshot) {
            Ok(started) => job = started,
            Err(invalid) if matches!(invalid.kind, Kind::InvalidRequest | Kind::SubprojectsNotConfirmed) => {
                // e.g. fewer than two mapped subprojects: not a failure, the system map is not made
                refusal = clip(&invalid.message, 300);
            }
            Err(error) => {
                // the subproject maps stay: the reason is shown next to the system map
                failure = Some(clip(&error.message, 300)).filter(|m| !m.is_empty()).unwrap_or_else(|| format!("{:?}", error.kind));
                self.app.log.warn("subproject_map_finished_callback_failed", &[("type", json!(format!("{:?}", error.kind))), ("message", json!(clip(&error.message, 200)))]);
            }
        }
        let Some(job) = job else {
            self.finish_system_step(folder, if failure.is_empty() { SYSTEM_IDLE } else { SYSTEM_FAILED }, if failure.is_empty() { &refusal } else { &failure });
            return;
        };
        {
            let _guard = self.lock();
            if let Some(mut state) = self.read_queue_file(folder) {
                state["system"]["job_id"] = job["id"].clone();
                self.write_queue(folder, &state);
            }
        }
        match self.wait_for_job(folder, &text_of(job.get("id"))) {
            Ok(ended) if state_of(&ended, "state") == "ok" => self.finish_system_step(folder, SYSTEM_DONE, ""),
            Ok(ended) => {
                let error = Some(text_of(ended.get("error"))).filter(|e| !e.is_empty()).unwrap_or_else(|| "the system map failed".into());
                self.finish_system_step(folder, SYSTEM_FAILED, &clip(&error, 500));
            }
            Err(lost) => self.finish_system_step(folder, SYSTEM_FAILED, &clip(&lost.message, 500)),
        }
    }

    fn finish_system_step(&self, folder: &str, system_state: &str, error: &str) {
        let _guard = self.lock();
        let Some(mut state) = self.read_queue_file(folder) else { return };
        state["system"]["state"] = json!(system_state);
        state["system"]["error"] = json!(error);
        state["state"] = json!(compute_state(&state));
        self.write_queue(folder, &state);
    }

    fn fail_item(&self, folder: &str, path: &str, job_id: &str, reason: &str) {
        let _guard = self.lock();
        let Some(mut state) = self.read_queue_file(folder) else { return };
        if !state["items"].get(path).is_some_and(|i| state_of(i, "job_id") == job_id) {
            return;
        }
        let item = &mut state["items"][path];
        item["state"] = json!(ITEM_FAILED);
        item["finished_at"] = json!(now_iso());
        item["error"] = json!(reason);
        state["state"] = json!(compute_state(&state));
        self.write_queue(folder, &state);
    }

    // ------------------------------------------------ read (describe) and restart recovery
    /// The public queue for the table and the installer (the idle payload when there never was one). Marks the items an app restart left running as
    /// failed "interrupted"; the documents of a running item are read live from its map folder.
    pub fn read_queue(&self, context: &ProjectContext, repo_name: Option<&str>) -> Result<Value, AppError> {
        let Some(mut state) = self.read_queue_state(context, repo_name)? else {
            return Ok(public_queue(&json!({"state": QUEUE_IDLE, "items": {}, "system": {"state": SYSTEM_IDLE, "error": ""}})));
        };
        let (_, folder) = self.app.subprojects().resolve_target(context, repo_name)?;
        let paths: Vec<String> = state["items"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        for path in paths {
            if state_of(&state["items"][&path], "state") == ITEM_RUNNING {
                state["items"][&path]["documents"] = json!(run_hints::list_map_documents(&map_folder(&folder, &path)).len());
            }
        }
        Ok(public_queue(&state))
    }

    pub fn read_queue_state(&self, context: &ProjectContext, repo_name: Option<&str>) -> Result<Option<Value>, AppError> {
        let (_, folder) = self.app.subprojects().resolve_target(context, repo_name)?;
        let _guard = self.lock();
        let Some(mut state) = self.read_queue_file(&folder) else { return Ok(None) };
        if state_of(&state, "state") == QUEUE_RUNNING && !self.is_queue_alive(&folder) {
            self.mark_interrupted(&folder, &mut state);
        }
        Ok(Some(state))
    }

    fn is_queue_alive(&self, folder: &str) -> bool {
        self.app.map_active_folders.lock().unwrap_or_else(|e| e.into_inner()).contains(&folder_key(folder)) || self.app.jobs().get_running_job(folder).is_some()
    }

    /// Per item: the running ones become failed (Retry); the never started stay queued (a retry runs them); a waiting system map goes back to idle and a
    /// running one is failed.
    fn mark_interrupted(&self, folder: &str, state: &mut Value) {
        for item in state["items"].as_object_mut().into_iter().flat_map(|m| m.values_mut()) {
            if state_of(item, "state") == ITEM_RUNNING {
                item["state"] = json!(ITEM_FAILED);
                item["finished_at"] = json!(now_iso());
                item["error"] = json!(INTERRUPTED_REASON);
            }
        }
        match state_of(&state["system"], "state") {
            SYSTEM_RUNNING => {
                state["system"]["state"] = json!(SYSTEM_FAILED);
                state["system"]["error"] = json!(INTERRUPTED_REASON);
            }
            SYSTEM_WAITING => state["system"]["state"] = json!(SYSTEM_IDLE),
            _ => {}
        }
        let has_queued = state["items"].as_object().is_some_and(|m| m.values().any(|i| state_of(i, "state") == ITEM_QUEUED));
        state["state"] = json!(if has_queued { QUEUE_STOPPED } else { compute_state(state) });
        self.write_queue(folder, state);
    }

    // ------------------------------------------------ files
    fn read_queue_file(&self, folder: &str) -> Option<Value> {
        let Read::Data(map) = self.app.subprojects_files().read_map_queue(folder) else { return None };
        let mut state = Value::Object(map);
        if !state.get("items").is_some_and(Value::is_object) {
            // the file of the old sequential queue: converted to the item shape
            state = convert_legacy_queue(&state);
        }
        state.as_object_mut()?.entry("state").or_insert(json!(QUEUE_IDLE));
        if !state.get("system").is_some_and(Value::is_object) {
            state["system"] = json!({"state": SYSTEM_IDLE, "error": ""});
        }
        state["system"].as_object_mut()?.entry("state").or_insert(json!(SYSTEM_IDLE));
        state["system"].as_object_mut()?.entry("error").or_insert(json!(""));
        for item in state["items"].as_object_mut()?.values_mut() {
            if let Some(map) = item.as_object_mut() {
                for (field, default) in [("state", json!(ITEM_QUEUED)), ("started_at", Value::Null), ("finished_at", Value::Null), ("error", json!("")), ("job_id", json!("")), ("documents", json!(0))] {
                    map.entry(field).or_insert(default);
                }
            }
        }
        Some(state)
    }

    fn write_queue(&self, folder: &str, state: &Value) {
        if let Err(failure) = self.app.subprojects_files().write_map_queue(folder, state) {
            self.app.log.warn("subproject_map_queue_write_failed", &[("type", json!("OSError")), ("message", json!(clip(&failure.to_string(), 200)))]);
        }
    }
}

#[allow(dead_code)]
fn unused(_: &Path) {}
