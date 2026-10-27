// SPDX-License-Identifier: MIT
//! `claude -p` jobs launched by the app (generate/refine the roadmap, map the codebase, a planning chat reply): they run in a thread, one per
//! project at a time, and are written to `<repo>/.specs/project/loop/internal/jobs/<id>.json`.
//!
//! The claude `session_id` stays in the file (so that `refine` continues the same conversation) and NEVER leaves through the API. A job that was
//! `running` when the app went down shows up as `error` (interrupted) when read.

use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::infrastructure::claude_client::call_kind;
use crate::infrastructure::ids::random_hex;
use crate::repositories::loop_command_file::now_iso;
use engine_state::{egress_proxy, loop_state, runtime_isolation};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

pub const JOB_TYPE_ROADMAP: &str = "roadmap";
pub const JOB_TYPE_ROADMAP_REFINE: &str = "roadmap_refine";
pub const JOB_TYPE_MAP: &str = "map";
pub const JOB_TYPE_PLANNING: &str = "planning";
/// Not a claude run: a task (see [`ClaudeJobService::start_task_job`]).
pub const JOB_TYPE_WORKSPACE_CREATE: &str = "workspace-create";
pub const ROADMAP_JOB_TYPES: [&str; 2] = [JOB_TYPE_ROADMAP, JOB_TYPE_ROADMAP_REFINE];
pub const JOB_STATE_RUNNING: &str = "running";
pub const JOB_STATE_OK: &str = "ok";
pub const JOB_STATE_ERROR: &str = "error";
/// Model every map/validation job pins (RULES.md: no subagent above Sonnet), instead of the account default.
pub const JOB_MODEL_CEILING: &str = "sonnet";
pub const PREPARING_SNAPSHOT_PROGRESS: &str = "preparing the safety snapshot";
const RECORD_READ_RETRIES: u32 = 5; // a job record that exists but cannot be read right now (being replaced) is read again
const RECORD_READ_RETRY_SECONDS: f64 = 0.02;
pub const DEFAULT_JOB_TIMEOUT_SECONDS: u64 = 30 * 60;
const JOB_REPLY_LIMIT: usize = 20000;
const JOB_ID_RANDOM_BYTES: usize = 6; // bytes -> 12 hex characters
static JOB_ID_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^j-[0-9a-f]{12}\z"));
const JOBS_DIR_NAME: &str = "jobs";
pub const INTERRUPTED_JOB_MESSAGE: &str = "interrupted: the app was shut down while the job was running";
const PUBLIC_JOB_FIELDS: [&str; 8] = ["id", "type", "state", "started_at", "finished_at", "cost_usd", "reply", "error"];

pub fn build_jobs_dir(repo_path: &str) -> PathBuf {
    loop_state::internal_dir(repo_path).join(JOBS_DIR_NAME)
}

/// Raised by `before_start` to end the job as `error` with a stable `code` next to the message (the panel maps the code like the 4xx the same
/// refusal gave when it was answered synchronously).
#[derive(Clone, Debug)]
pub struct ClaudeJobError {
    pub message: String,
    pub code: Option<String>,
}

impl ClaudeJobError {
    pub fn new(message: impl Into<String>) -> Self {
        ClaudeJobError { message: message.into(), code: None }
    }

    pub fn with_code(message: impl Into<String>, code: &str) -> Self {
        ClaudeJobError { message: message.into(), code: Some(code.to_string()) }
    }
}

impl From<AppError> for ClaudeJobError {
    fn from(error: AppError) -> Self {
        ClaudeJobError { message: error.message, code: error.code }
    }
}

/// What `before_start` may return to replace the command's argv / stdin (`None` keeps the command as is).
#[derive(Clone, Debug, Default)]
pub struct Prepared {
    pub argv: Option<Vec<String>>,
    pub stdin_text: Option<String>,
}

pub type UpdateProgress<'a> = &'a dyn Fn(&str);
pub type BeforeStart = Box<dyn FnOnce(UpdateProgress) -> Result<Option<Prepared>, ClaudeJobError> + Send>;
pub type Finalize = Box<dyn FnOnce(Value) -> Value + Send>;
pub type OnPrepareFailed = Box<dyn FnOnce(&ClaudeJobError) + Send>;

/// What to run: ready argv (built by the `ClaudeClient`) and the stdin text.
///
/// `timeout_seconds` (optional) replaces the service timeout for this job. `finalize(result) -> result` (optional) runs in the job thread after
/// claude ends and before the job is recorded as finished (e.g. the planning file guard and thread). `work_dir` (optional) is the folder claude
/// runs in (the loop worktree); default = the repo path the job is registered under. `before_start(update_progress)` (optional) runs in the job
/// thread BEFORE claude (e.g. the safety snapshot of a map job): the record says `progress: "preparing the safety snapshot"` meanwhile,
/// `update_progress(text)` replaces that text, and an error ends the job as `error` with its message (claude never starts and `finalize` does not
/// run); a coded error also puts its `code` in the record (and in the public job). `parallel_key` (optional, a normalized folder): the job may run
/// next to other jobs of the SAME repository that also carry a `parallel_key`, as long as the keys differ (the subproject maps of a monorepo).
/// Without it the job is exclusive both ways.
#[derive(Default)]
pub struct ClaudeJobCommand {
    pub argv: Vec<String>,
    pub stdin_text: String,
    pub resumed_session: String,
    pub timeout_seconds: Option<u64>,
    pub finalize: Option<Finalize>,
    pub work_dir: Option<String>,
    pub before_start: Option<BeforeStart>,
    pub parallel_key: Option<String>,
    /// Runs in the job thread when `before_start` failed (undo what the request persisted).
    pub on_prepare_failed: Option<OnPrepareFailed>,
}

impl ClaudeJobCommand {
    pub fn new(argv: Vec<String>, stdin_text: &str) -> Self {
        ClaudeJobCommand { argv, stdin_text: stdin_text.to_string(), ..Default::default() }
    }
}

pub type AddStep<'a> = &'a dyn Fn(Value);
pub type Task = Box<dyn FnOnce(AddStep) -> Value + Send>;

fn same_folder(a: &str, b: &str) -> bool {
    pycompat::nt_normcase(&pycompat::nt_abspath(a)) == pycompat::nt_normcase(&pycompat::nt_abspath(b))
}

fn error_result(message: &str) -> Value {
    json!({"ok": false, "reply": "", "cost_usd": null, "session_id": "", "error": message})
}

pub struct ClaudeJobService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn jobs(&self) -> ClaudeJobService<'_> {
        ClaudeJobService { app: self }
    }
}

impl ClaudeJobService<'_> {
    // ------------------------------------------------ launch
    /// Writes the job as `running` and starts the thread. Errors with `JobInProgress` when a job is already running in this repo.
    pub fn start_job(&self, repo_path: &str, job_type: &str, command: ClaudeJobCommand) -> Result<Value, AppError> {
        let mut record = Map::new();
        for (key, value) in [
            ("id", json!(format!("j-{}", random_hex(JOB_ID_RANDOM_BYTES)))),
            ("type", json!(job_type)),
            ("state", json!(JOB_STATE_RUNNING)),
            ("started_at", json!(now_iso())),
            ("finished_at", Value::Null),
            ("cost_usd", Value::Null),
            ("reply", json!("")),
            ("error", json!("")),
            ("dir", json!(repo_path)),
            ("session_id", json!(command.resumed_session)),
            ("order", json!(time_ns())), // internal ordering tiebreak (several jobs in the same second); never leaves through the API
        ] {
            record.insert(key.to_string(), value);
        }
        if command.before_start.is_some() {
            record.insert("progress".into(), json!(PREPARING_SNAPSHOT_PROGRESS));
        }
        let id = record["id"].as_str().unwrap_or("").to_string();
        {
            let mut tables = self.app.job_tables.lock().unwrap_or_else(|e| e.into_inner());
            if !self.find_blocking_id_in(&tables.running, &tables.parallel_keys, repo_path, command.parallel_key.as_deref()).is_empty() {
                return Err(AppError::new(Kind::JobInProgress, "a claude job is already running in this project: wait for it to finish"));
            }
            tables.running.push((id.clone(), repo_path.to_string()));
            if let Some(key) = command.parallel_key.as_ref().filter(|k| !k.is_empty()) {
                tables.parallel_keys.push((id.clone(), key.clone()));
            }
        }
        let record = Value::Object(record);
        if let Err(failure) = self.write_record(repo_path, &record) {
            self.forget_job(&id);
            return Err(AppError::new(Kind::JobInProgress, format!("the job record cannot be written: {failure}")));
        }
        let (app, thread_record) = (self.app.shared(), record.clone());
        if let Err(failure) = std::thread::Builder::new().name(format!("claude-job-{id}")).spawn(move || app.jobs().run_job(thread_record, command)) {
            self.forget_job(&id);
            return Err(AppError::new(Kind::JobInProgress, format!("the job thread cannot start: {failure}")));
        }
        self.app.log.info("claude_job_started", &[("job", json!(id)), ("type", json!(job_type)), ("repo", json!(repo_path))]);
        Ok(Self::to_public(&record))
    }

    fn forget_job(&self, id: &str) {
        let mut tables = self.app.job_tables.lock().unwrap_or_else(|e| e.into_inner());
        tables.running.retain(|(i, _)| i != id);
        tables.parallel_keys.retain(|(i, _)| i != id);
    }

    /// Same registry and files as `start_job`, but the thread runs `task(add_step) -> {"ok", "result", "error"}` instead of `claude -p`.
    ///
    /// `add_step(step)` appends a step to the job record, written to disk so a poll sees it. A panic becomes an error job. `JobInProgress` while any
    /// job (claude or task) runs in this repo.
    pub fn start_task_job(&self, repo_path: &str, job_type: &str, task: Task) -> Result<Value, AppError> {
        let id = format!("j-{}", random_hex(JOB_ID_RANDOM_BYTES));
        let record = json!({"id": id, "type": job_type, "state": JOB_STATE_RUNNING, "started_at": now_iso(), "finished_at": null, "cost_usd": null,
                            "reply": "", "error": "", "dir": repo_path, "session_id": "", "steps": [], "result": null, "order": time_ns()});
        {
            let mut tables = self.app.job_tables.lock().unwrap_or_else(|e| e.into_inner());
            if !self.find_running_id_in(&tables.running, repo_path).is_empty() {
                return Err(AppError::new(Kind::JobInProgress, "a job is already running in this project: wait for it to finish"));
            }
            tables.running.push((id.clone(), repo_path.to_string()));
        }
        if let Err(failure) = self.write_record(repo_path, &record) {
            self.forget_job(&id);
            return Err(AppError::new(Kind::JobInProgress, format!("the job record cannot be written: {failure}")));
        }
        let (app, thread_record) = (self.app.shared(), record.clone());
        if let Err(failure) = std::thread::Builder::new().name(format!("task-job-{id}")).spawn(move || app.jobs().run_task_job(thread_record, task)) {
            self.forget_job(&id);
            return Err(AppError::new(Kind::JobInProgress, format!("the job thread cannot start: {failure}")));
        }
        self.app.log.info("task_job_started", &[("job", json!(id)), ("type", json!(job_type)), ("repo", json!(repo_path))]);
        Ok(Self::to_public(&record))
    }


    fn run_task_job(&self, record: Value, task: Task) {
        let record = std::sync::Mutex::new(record);
        let add_step = |step: Value| {
            let snapshot = {
                let mut guard = record.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(Value::Array(steps)) = guard.get_mut("steps") {
                    steps.push(step);
                }
                guard.clone()
            };
            self.try_write_record(&snapshot);
        };
        let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task(&add_step))) {
            Ok(outcome) => outcome,
            Err(_) => {
                let id = record.lock().unwrap_or_else(|e| e.into_inner())["id"].clone();
                self.app.log.error("task_job_failed", &[("job", id), ("type", json!("panic")), ("message", json!("the task panicked"))]);
                json!({"ok": false, "result": null, "error": "internal failure: panic"})
            }
        };
        let finished = {
            let mut guard = record.lock().unwrap_or_else(|e| e.into_inner());
            let ok = engine_state::notify_truthy(outcome.get("ok"));
            guard["state"] = json!(if ok { JOB_STATE_OK } else { JOB_STATE_ERROR });
            guard["finished_at"] = json!(now_iso());
            guard["error"] = json!(text_of(outcome.get("error")));
            guard["result"] = outcome.get("result").cloned().unwrap_or(Value::Null);
            guard.clone()
        };
        self.try_write_record(&finished);
        self.forget_job(finished["id"].as_str().unwrap_or(""));
        self.app.log.info("task_job_finished", &[("job", finished["id"].clone()), ("type", finished["type"].clone()), ("state", finished["state"].clone())]);
    }

    fn try_write_record(&self, record: &Value) {
        if let Err(failure) = self.write_record(record["dir"].as_str().unwrap_or(""), record) {
            self.app.log.error("claude_job_write_failed", &[("job", record["id"].clone()), ("message", json!(failure.to_string().chars().take(200).collect::<String>()))]);
        }
    }

    fn run_job(&self, record: Value, mut command: ClaudeJobCommand) {
        let timeout = match command.timeout_seconds {
            Some(t) if t > 0 => t.min(self.app.job_timeout_seconds),
            _ => self.app.job_timeout_seconds,
        };
        let record = std::cell::RefCell::new(record);
        let mut result = self.run_before_start(&record, &mut command);
        let job_type = record.borrow()["type"].as_str().unwrap_or("").to_string();
        let job_dir = record.borrow()["dir"].as_str().unwrap_or("").to_string();
        if result.is_none() {
            let folder = command.work_dir.clone().unwrap_or_else(|| job_dir.clone());
            let layer = self.start_egress_layer(&folder);
            let _kind = call_kind(&job_type);
            let argv = command.argv.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.app.claude.run_prompt(&argv, &folder, &command.stdin_text, timeout, &[])));
            let mut value = outcome.unwrap_or_else(|_| error_result("internal failure: panic"));
            egress_proxy::release(layer.as_ref());
            if let Some(finalize) = command.finalize.take() {
                value = self.finalize_result(&record.borrow(), finalize, value);
            }
            result = Some(value);
        }
        self.record_finished(record.into_inner(), &result.unwrap_or_else(|| error_result("")));
    }

    /// RUN-01: the egress proxy of the project folder for the time `claude -p` runs (shared with a loop or another job of the same folder). Only jobs
    /// that run with the loop's isolation (`loop_isolation` on: they carry LOOP_ISOLATED, so the guard hands their Bash/PowerShell commands the
    /// variables) and only in a folder that already has `.specs`. Never fails: without the proxy the guard points the variables at a closed port.
    fn start_egress_layer(&self, folder: &str) -> Option<egress_proxy::Layer> {
        if !runtime_isolation::isolation_enabled(&runtime_isolation::read_config(&self.app.env)) {
            return None;
        }
        let root = runtime_isolation::project_root_of(folder);
        if !Path::new(&root).join(".specs").is_dir() {
            return None;
        }
        match egress_proxy::acquire(&self.app.env, &root, "") {
            Ok(layer) => Some(layer),
            Err(failure) => {
                self.app.log.error("egress_proxy_not_started", &[("type", json!("io")), ("message", json!(failure.to_string().chars().take(200).collect::<String>()))]);
                None
            }
        }
    }

    /// `None` when claude may start; otherwise the error result of the failed preparation.
    fn run_before_start(&self, record: &std::cell::RefCell<Value>, command: &mut ClaudeJobCommand) -> Option<Value> {
        let before_start = command.before_start.take()?;
        let update = |text: &str| self.update_progress(record, text);
        match before_start(&update) {
            Err(failure) => {
                let message: String = if failure.message.is_empty() { "ClaudeJobError".to_string() } else { failure.message.chars().take(500).collect() };
                self.app.log.error("claude_job_before_start_failed", &[("job", record.borrow()["id"].clone()), ("type", json!("ClaudeJobError")), ("message", json!(message.chars().take(200).collect::<String>()))]);
                let mut result = error_result(&message);
                if let Some(hook) = command.on_prepare_failed.take() {
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook(&failure))).is_err() {
                        self.app.log.error("claude_job_prepare_failed_hook_failed", &[("job", record.borrow()["id"].clone()), ("type", json!("panic"))]);
                    }
                }
                if let Some(code) = failure.code.filter(|c| !c.is_empty()) {
                    result["code"] = json!(code);
                }
                Some(result)
            }
            Ok(prepared) => {
                if let Some(prepared) = prepared {
                    if let Some(argv) = prepared.argv {
                        command.argv = argv;
                    }
                    if let Some(stdin_text) = prepared.stdin_text {
                        command.stdin_text = stdin_text;
                    }
                }
                self.update_progress(record, "");
                None
            }
        }
    }

    fn update_progress(&self, record: &std::cell::RefCell<Value>, text: &str) {
        record.borrow_mut()["progress"] = json!(text);
        let snapshot = record.borrow().clone();
        self.try_write_record(&snapshot);
    }

    fn finalize_result(&self, record: &Value, finalize: Finalize, result: Value) -> Value {
        let kept = result.clone();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| finalize(result))) {
            Ok(value) => value,
            Err(_) => {
                self.app.log.error("claude_job_finalize_failed", &[("job", record["id"].clone()), ("type", json!("panic")), ("message", json!("finalize panicked"))]);
                let mut failed = kept;
                failed["ok"] = json!(false);
                failed["error"] = json!("internal failure while finishing the job: panic");
                failed
            }
        }
    }

    fn record_finished(&self, mut record: Value, result: &Value) {
        let ok = engine_state::notify_truthy(result.get("ok"));
        let session = match result.get("session_id") {
            Some(v) if engine_state::notify_truthy(Some(v)) => text_of(Some(v)),
            _ => match record.get("session_id") {
                Some(v) if engine_state::notify_truthy(Some(v)) => text_of(Some(v)),
                _ => String::new(),
            },
        };
        record["state"] = json!(if ok { JOB_STATE_OK } else { JOB_STATE_ERROR });
        record["finished_at"] = json!(now_iso());
        record["cost_usd"] = result.get("cost_usd").cloned().unwrap_or(Value::Null);
        record["reply"] = json!(text_of(result.get("reply")).chars().take(JOB_REPLY_LIMIT).collect::<String>());
        record["error"] = json!(text_of(result.get("error")));
        record["session_id"] = json!(session);
        if engine_state::notify_truthy(result.get("code")) {
            record["code"] = json!(text_of(result.get("code")));
        }
        self.try_write_record(&record);
        self.forget_job(record["id"].as_str().unwrap_or(""));
        self.app.log.info("claude_job_finished", &[("job", record["id"].clone()), ("type", record["type"].clone()), ("state", record["state"].clone()), ("cost_usd", record["cost_usd"].clone())]);
    }

    // ------------------------------------------------ read
    pub fn list_jobs(&self, repo_path: &str) -> Vec<Value> {
        let mut records: Vec<Value> = self.app.specs.list_json_files(&build_jobs_dir(repo_path)).iter().filter_map(|p| self.read_corrected_record(p)).collect();
        records.sort_by(|a, b| Self::get_order_key(b).cmp(&Self::get_order_key(a)));
        records.iter().map(Self::to_public).collect()
    }

    pub fn get_job(&self, repo_path: &str, job_id: &str) -> Result<Value, AppError> {
        self.read_record_by_id(repo_path, job_id).map(|r| Self::to_public(&r)).ok_or_else(|| AppError::new(Kind::JobNotFound, "job not found"))
    }

    pub fn get_running_job(&self, repo_path: &str) -> Option<Value> {
        let job_id = self.find_running_id(repo_path);
        if job_id.is_empty() {
            return None;
        }
        self.read_record_by_id(repo_path, &job_id).map(|r| Self::to_public(&r))
    }

    /// The running job of this repository that would stop a job with `parallel_key` from starting (`None` = it may start): any job without a
    /// parallel key, and the one with the same key.
    pub fn get_blocking_job(&self, repo_path: &str, parallel_key: Option<&str>) -> Option<Value> {
        let job_id = {
            let tables = self.app.job_tables.lock().unwrap_or_else(|e| e.into_inner());
            self.find_blocking_id_in(&tables.running, &tables.parallel_keys, repo_path, parallel_key)
        };
        if job_id.is_empty() {
            return None;
        }
        self.read_record_by_id(repo_path, &job_id).map(|r| Self::to_public(&r))
    }

    /// `session_id` of the most recent job (finished or not) of one of `job_types` that has a session; '' when none.
    pub fn get_last_session(&self, repo_path: &str, job_types: &[&str]) -> String {
        let mut candidates: Vec<Value> = self
            .app
            .specs
            .list_json_files(&build_jobs_dir(repo_path))
            .iter()
            .filter_map(|p| self.read_corrected_record(p))
            .filter(|r| r.get("type").and_then(Value::as_str).is_some_and(|t| job_types.contains(&t)) && engine_state::notify_truthy(r.get("session_id")))
            .collect();
        candidates.sort_by(|a, b| Self::get_order_key(b).cmp(&Self::get_order_key(a)));
        candidates.first().map(|r| text_of(r.get("session_id"))).unwrap_or_default()
    }

    /// The app does not shut down for idleness while a claude job is running.
    pub fn is_busy(&self) -> bool {
        !self.app.job_tables.lock().unwrap_or_else(|e| e.into_inner()).running.is_empty()
    }

    // ------------------------------------------------ support
    fn find_running_id(&self, repo_path: &str) -> String {
        let tables = self.app.job_tables.lock().unwrap_or_else(|e| e.into_inner());
        self.find_running_id_in(&tables.running, repo_path)
    }

    fn find_running_id_in(&self, running: &[(String, String)], repo_path: &str) -> String {
        running.iter().find(|(_, repo)| same_folder(repo, repo_path)).map(|(id, _)| id.clone()).unwrap_or_default()
    }

    /// Id of a running job in `repo_path` that blocks a new job: without `parallel_key` any running job does; with one only the jobs that have no
    /// key or the same key.
    fn find_blocking_id_in(&self, running: &[(String, String)], keys: &[(String, String)], repo_path: &str, parallel_key: Option<&str>) -> String {
        for (job_id, repo) in running {
            if !same_folder(repo, repo_path) {
                continue;
            }
            let running_key = keys.iter().find(|(i, _)| i == job_id).map(|(_, k)| k.as_str());
            let wanted = parallel_key.filter(|k| !k.is_empty());
            if wanted.is_none() || running_key.is_none() || running_key == wanted {
                return job_id.clone();
            }
        }
        String::new()
    }

    fn read_record_by_id(&self, repo_path: &str, job_id: &str) -> Option<Value> {
        if !JOB_ID_PATTERN.is_match(job_id) {
            return None;
        }
        self.read_corrected_record(&build_jobs_dir(repo_path).join(format!("{job_id}.json")))
    }

    fn read_corrected_record(&self, path: &Path) -> Option<Value> {
        // The registry is asked BEFORE the file is read: a job writes its final record and only then leaves the registry, so a job that is already out
        // of the registry has its final record on disk. Asking after the read could pair a stale "running" record with a finished job.
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let alive_before = self.app.job_tables.lock().unwrap_or_else(|e| e.into_inner()).running.iter().any(|(i, _)| *i == stem);
        let mut record = self.app.specs.read_json(path);
        for _ in 0..RECORD_READ_RETRIES {
            // Windows: a read can fail while the job thread replaces the file (progress updates)
            if engine_state::notify_truthy(record.get("id")) || !path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_secs_f64(RECORD_READ_RETRY_SECONDS));
            record = self.app.specs.read_json(path);
        }
        if !engine_state::notify_truthy(record.get("id")) {
            return None;
        }
        let id = text_of(record.get("id"));
        let alive = alive_before || (id != stem && self.app.job_tables.lock().unwrap_or_else(|e| e.into_inner()).running.iter().any(|(i, _)| *i == id));
        if record.get("state").and_then(Value::as_str) == Some(JOB_STATE_RUNNING) && !alive {
            record.insert("state".into(), json!(JOB_STATE_ERROR));
            record.insert("error".into(), json!(INTERRUPTED_JOB_MESSAGE));
        }
        Some(Value::Object(record))
    }

    fn write_record(&self, repo_path: &str, record: &Value) -> std::io::Result<()> {
        self.app.specs.write_json_atomic(&build_jobs_dir(repo_path).join(format!("{}.json", record["id"].as_str().unwrap_or(""))), record)
    }

    fn get_order_key(record: &Value) -> (String, i64) {
        (text_of(record.get("started_at")), record.get("order").and_then(Value::as_i64).unwrap_or(0))
    }

    pub fn to_public(record: &Value) -> Value {
        let mut fields: Vec<&str> = PUBLIC_JOB_FIELDS.to_vec();
        if record.get("steps").is_some() {
            fields.extend(["steps", "result"]);
        }
        if record.get("progress").is_some() {
            fields.push("progress");
        }
        if engine_state::notify_truthy(record.get("code")) {
            fields.push("code");
        }
        Value::Object(fields.into_iter().map(|f| (f.to_string(), record.get(f).cloned().unwrap_or(Value::Null))).collect())
    }
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

fn time_ns() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::process_runner::ProcessRunner;
    use std::sync::Mutex;

    /// Process runner that answers every claude call with a stream-json result.
    struct FakeClaude {
        calls: Mutex<Vec<Vec<String>>>,
        reply: String,
        delay_ms: u64,
    }

    impl ProcessRunner for FakeClaude {
        fn run_and_wait(&self, argv: &[String], _: &str, _: u64, _: Option<&str>, _: &[(String, String)], _: Option<&[(String, String)]>) -> (i32, String, String) {
            self.calls.lock().unwrap().push(argv.to_vec());
            std::thread::sleep(Duration::from_millis(self.delay_ms));
            (0, format!(r#"{{"type":"result","is_error":false,"result":"{}","session_id":"abc","total_cost_usd":0.25}}"#, self.reply), String::new())
        }
        fn spawn_detached(&self, _: &[String], _: &str, _: &str, _: &[(String, String)]) -> std::io::Result<i64> {
            Ok(1)
        }
        fn is_alive(&self, _: i64) -> bool {
            false
        }
        fn kill_tree(&self, _: i64) {}
    }

    fn wait_until(mut done: impl FnMut() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting");
    }

    fn app_with(dir: &Path, delay_ms: u64) -> (Arc<App>, String) {
        let runner = Arc::new(FakeClaude { calls: Mutex::new(vec![]), reply: "done".into(), delay_ms });
        let app = App::for_tests_with(dir, runner);
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        (app, repo.display().to_string())
    }

    #[test]
    fn a_job_runs_records_and_blocks_a_second_one() {
        let dir = tempfile::tempdir().unwrap();
        let (app, repo) = app_with(dir.path(), 300);
        let jobs = app.jobs();
        let first = jobs.start_job(&repo, JOB_TYPE_ROADMAP, ClaudeJobCommand::new(vec!["claude".into(), "-p".into()], "go")).unwrap();
        assert_eq!((first["state"].clone(), first["type"].clone()), (json!("running"), json!("roadmap")));
        assert!(first.get("session_id").is_none() && first.get("dir").is_none());
        assert!(jobs.is_busy());
        let blocked = jobs.start_job(&repo, JOB_TYPE_MAP, ClaudeJobCommand::new(vec!["claude".into()], "")).unwrap_err();
        assert_eq!(blocked.kind, Kind::JobInProgress);
        let id = first["id"].as_str().unwrap().to_string();
        wait_until(|| jobs.get_job(&repo, &id).map(|j| j["state"] == "ok").unwrap_or(false));
        let done = jobs.get_job(&repo, &id).unwrap();
        assert_eq!((done["reply"].clone(), done["cost_usd"].clone(), done["error"].clone()), (json!("done"), json!(0.25), json!("")));
        assert_eq!(jobs.get_last_session(&repo, &["roadmap"]), "abc");
        assert_eq!(jobs.get_last_session(&repo, &["map"]), "");
        wait_until(|| !jobs.is_busy());
        assert_eq!(jobs.list_jobs(&repo).len(), 1);
        assert!(jobs.get_running_job(&repo).is_none());
        assert_eq!(jobs.get_job(&repo, "j-nope").unwrap_err().kind, Kind::JobNotFound);
    }

    #[test]
    fn parallel_keys_allow_different_keys_only() {
        let dir = tempfile::tempdir().unwrap();
        let (app, repo) = app_with(dir.path(), 400);
        let jobs = app.jobs();
        let mut a = ClaudeJobCommand::new(vec!["claude".into()], "");
        a.parallel_key = Some("api".into());
        let mut b = ClaudeJobCommand::new(vec!["claude".into()], "");
        b.parallel_key = Some("web".into());
        let mut same = ClaudeJobCommand::new(vec!["claude".into()], "");
        same.parallel_key = Some("api".into());
        jobs.start_job(&repo, JOB_TYPE_MAP, a).unwrap();
        jobs.start_job(&repo, JOB_TYPE_MAP, b).unwrap();
        assert_eq!(jobs.start_job(&repo, JOB_TYPE_MAP, same).unwrap_err().kind, Kind::JobInProgress);
        assert!(jobs.get_blocking_job(&repo, Some("api")).is_some() && jobs.get_blocking_job(&repo, Some("db")).is_none());
        assert!(jobs.get_blocking_job(&repo, None).is_some());
        wait_until(|| !jobs.is_busy());
    }

    #[test]
    fn before_start_failures_finalize_and_progress() {
        let dir = tempfile::tempdir().unwrap();
        let (app, repo) = app_with(dir.path(), 0);
        let jobs = app.jobs();
        let undone = Arc::new(Mutex::new(false));
        let flag = undone.clone();
        let mut failing = ClaudeJobCommand::new(vec!["claude".into()], "");
        failing.before_start = Some(Box::new(|update| {
            update("copying");
            Err(ClaudeJobError::with_code("snapshot failed", "no_snapshot"))
        }));
        failing.on_prepare_failed = Some(Box::new(move |_| *flag.lock().unwrap() = true));
        let started = jobs.start_job(&repo, JOB_TYPE_MAP, failing).unwrap();
        assert_eq!(started["progress"], PREPARING_SNAPSHOT_PROGRESS);
        let id = started["id"].as_str().unwrap().to_string();
        wait_until(|| jobs.get_job(&repo, &id).map(|j| j["state"] == "error").unwrap_or(false));
        let failed = jobs.get_job(&repo, &id).unwrap();
        assert_eq!((failed["error"].clone(), failed["code"].clone()), (json!("snapshot failed"), json!("no_snapshot")));
        assert!(*undone.lock().unwrap());
        wait_until(|| !jobs.is_busy());
        let mut finalizing = ClaudeJobCommand::new(vec!["claude".into()], "");
        finalizing.before_start = Some(Box::new(|_| Ok(Some(Prepared { argv: Some(vec!["claude".into(), "--changed".into()]), stdin_text: Some("new".into()) }))));
        finalizing.finalize = Some(Box::new(|mut result| {
            result["reply"] = json!("finalized");
            result
        }));
        let started = jobs.start_job(&repo, JOB_TYPE_PLANNING, finalizing).unwrap();
        let id = started["id"].as_str().unwrap().to_string();
        wait_until(|| jobs.get_job(&repo, &id).map(|j| j["state"] == "ok").unwrap_or(false));
        assert_eq!(jobs.get_job(&repo, &id).unwrap()["reply"], "finalized");
        wait_until(|| !jobs.is_busy());
    }

    #[test]
    fn task_jobs_record_steps_and_a_dead_job_reads_as_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let (app, repo) = app_with(dir.path(), 0);
        let jobs = app.jobs();
        let started = jobs
            .start_task_job(&repo, JOB_TYPE_WORKSPACE_CREATE, Box::new(|add_step| {
                add_step(json!({"name": "one"}));
                add_step(json!({"name": "two"}));
                json!({"ok": true, "result": {"made": 2}, "error": ""})
            }))
            .unwrap();
        let id = started["id"].as_str().unwrap().to_string();
        wait_until(|| jobs.get_job(&repo, &id).map(|j| j["state"] == "ok").unwrap_or(false));
        let done = jobs.get_job(&repo, &id).unwrap();
        assert_eq!((done["steps"].as_array().unwrap().len(), done["result"]["made"].clone()), (2, json!(2)));
        wait_until(|| !jobs.is_busy());
        let ghost = json!({"id": "j-aaaaaaaaaaaa", "type": "map", "state": "running", "started_at": "2026-01-01T00:00:00", "dir": repo, "order": 1});
        app.specs.write_json_atomic(&build_jobs_dir(&repo).join("j-aaaaaaaaaaaa.json"), &ghost).unwrap();
        let read = jobs.get_job(&repo, "j-aaaaaaaaaaaa").unwrap();
        assert_eq!((read["state"].clone(), read["error"].clone()), (json!("error"), json!(INTERRUPTED_JOB_MESSAGE)));
        let panicked = jobs.start_task_job(&repo, JOB_TYPE_WORKSPACE_CREATE, Box::new(|_| panic!("boom"))).unwrap();
        let id = panicked["id"].as_str().unwrap().to_string();
        wait_until(|| jobs.get_job(&repo, &id).map(|j| j["state"] == "error").unwrap_or(false));
        assert_eq!(jobs.get_job(&repo, &id).unwrap()["error"], "internal failure: panic");
    }
}
