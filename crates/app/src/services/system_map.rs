// SPDX-License-Identifier: MIT
//! The system map of a confirmed monorepo: ONE job that writes the root `docs/codebase/SYSTEM.md` (design D21).
//!
//! It runs as the last step of "Map all that are missing" (the subproject queue calls [`SystemMapService::on_queue_finished`]) and from its own
//! action. Job type `map`, prompt `/orchestrator map --system`, `claude` runs in the repository root with the hook profile `systemmap` (only
//! `docs/codebase/SYSTEM.md` may be written). After the job the app undoes every change outside `.specs` and that one file, fails the job when
//! `SYSTEM.md` was not written (or not touched), and writes the root `docs/codebase/README.md` index. It never writes the root `CLAUDE.md`: a
//! monorepo root has no `CODEBASE` block (the subprojects' context blocks point to `SYSTEM.md`).
//!
//! Only a confirmed monorepo with at least two mapped, non-skipped subprojects gets it; a single project never does (SYS-07). A failure of this job
//! never undoes the subproject maps.

use super::claude_jobs::{ClaudeJobCommand, JOB_MODEL_CEILING, JOB_TYPE_MAP};
use super::map_job::{guard_note, map_guard_ignored_paths, JOB_PERMISSION_MODE};
use super::subprojects::{not_confirmed, MAP_RELATIVE_FOLDER};
use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use crate::infrastructure::claude_client::CommandOptions;
use crate::infrastructure::workspace_file_guard::{WorkspaceFileGuard, WorkspaceSnapshot, DEFAULT_IGNORED_NAMES};
use crate::repositories::loop_command_file::now_iso as _now_iso;
use engine_state::{run_hints, subprojects};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

pub const SYSTEM_MAP_PROMPT: &str = "/orchestrator map --system";
pub const SYSTEM_HOOK_PROFILE: &str = "systemmap";
pub const SYSTEM_RELATIVE_PATH: &str = "docs/codebase/SYSTEM.md";
pub const MINIMUM_MAPPED_SUBPROJECTS: usize = 2;
pub const SYSTEM_NOT_WRITTEN: &str = "The AI finished but did not write docs/codebase/SYSTEM.md, so the system map was not made. The subproject maps are untouched.";
/// The previous job record says "finished" a moment before the job service frees the repository.
const START_RETRIES: u32 = 100;
const START_RETRY: Duration = Duration::from_millis(30);

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

fn system_path(folder: &str) -> PathBuf {
    SYSTEM_RELATIVE_PATH.split('/').fold(PathBuf::from(folder), |a, p| a.join(p))
}

fn mtime_seconds(path: &Path) -> Option<f64> {
    Some(std::fs::metadata(path).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

/// `(mtime_ns, size)` of `SYSTEM.md`, `None` when missing: a job that left it as it was did not write it.
fn read_system_stamp(folder: &str) -> Option<(u128, u64)> {
    let info = std::fs::metadata(system_path(folder)).ok()?;
    Some((info.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos(), info.len()))
}

/// `%Y-%m-%dT%H:%M:%SZ` of an epoch timestamp.
fn format_time(timestamp: f64) -> String {
    time::OffsetDateTime::from_unix_timestamp(timestamp as i64)
        .ok()
        .and_then(|t| t.format(time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z")).ok())
        .unwrap_or_default()
}

/// Epoch seconds of an ISO text (a naive time counts as UTC); `None` when it does not parse.
fn parse_time(text: &str) -> Option<f64> {
    crate::repositories::loop_read_formatting::parse_iso_date(text).map(|m| m.seconds)
}

pub struct SystemMapService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn system_maps(&self) -> SystemMapService<'_> {
        SystemMapService { app: self }
    }
}

impl SystemMapService<'_> {
    // ------------------------------------------------ status
    /// `{exists, mapped_at, older_than_list, enabled, reason}`. `reason` says why the action is disabled ("" when enabled). `older_than_list`:
    /// `SYSTEM.md` was written before the subproject list was last confirmed (the list changed since).
    pub fn status(&self, context: &ProjectContext, repo_name: Option<&str>) -> Result<Value, AppError> {
        let (key, folder) = self.app.subprojects().resolve_target(context, repo_name)?;
        let (entry, _) = subprojects::load(&folder, Some(&key));
        let path = system_path(&folder);
        let exists = path.is_file();
        let mtime = mtime_seconds(&path);
        let mapped_at = if exists { mtime.map(format_time) } else { None };
        let older = exists && entry.as_ref().is_some_and(|e| mtime.is_some_and(|m| Self::is_older_than_list(m, e)));
        let reason = self.refusal_reason(&folder, entry.as_ref());
        Ok(json!({"exists": exists, "mapped_at": mapped_at, "older_than_list": older, "enabled": reason.is_empty(), "reason": reason}))
    }

    fn is_older_than_list(system_mtime: f64, entry: &Map<String, Value>) -> bool {
        parse_time(&text_of(entry.get("confirmed_at"))).is_some_and(|confirmed_at| system_mtime < confirmed_at)
    }

    /// "" when the system map may run; else the sentence shown next to the disabled action.
    fn refusal_reason(&self, folder: &str, entry: Option<&Map<String, Value>>) -> String {
        let Some(entry) = entry.filter(|e| !e.is_empty()) else { return "only a monorepo has a system map".into() };
        if entry.get("monorepo") != Some(&Value::Bool(true)) {
            return "only a monorepo has a system map".into();
        }
        if entry.get("confirmed") != Some(&Value::Bool(true)) {
            return "confirm the list of subprojects first".into();
        }
        let mapped = Self::mapped_subprojects(folder, entry);
        if mapped.len() < MINIMUM_MAPPED_SUBPROJECTS {
            return format!("map at least {MINIMUM_MAPPED_SUBPROJECTS} subprojects first (mapped: {})", mapped.len());
        }
        String::new()
    }

    fn mapped_subprojects(folder: &str, entry: &Map<String, Value>) -> Vec<String> {
        subprojects::entry_projects(Some(entry))
            .iter()
            .filter(|p| p.get("skip") != Some(&Value::Bool(true)))
            .map(|p| subprojects::normalize(&text_of(p.get("path"))))
            .filter(|path| {
                let map_folder = MAP_RELATIVE_FOLDER.split('/').fold(path.split('/').fold(PathBuf::from(folder), |a, p| a.join(p)), |a, p| a.join(p));
                !run_hints::list_map_documents(&map_folder).is_empty()
            })
            .collect()
    }

    // ------------------------------------------------ start (202 path)
    /// Validates and starts the job; returns `{ok, job}` at once. No spend cap.
    pub fn start(&self, context: &ProjectContext, repo_name: Option<&str>) -> Result<Value, AppError> {
        let project_id = context.project.id.clone();
        self.app.log.info("map_requested", &[("project", json!(project_id)), ("scope", json!("system"))]);
        let started = (|| -> Result<Value, AppError> {
            let (key, folder) = self.app.subprojects().resolve_target(context, repo_name)?;
            let (entry, _) = subprojects::load(&folder, Some(&key));
            self.check_allowed(&folder, entry.as_ref())?;
            self.app.roadmap().ensure_repo_free_for_job(context)?;
            if self.app.jobs().get_running_job(&folder).is_some() {
                return Err(AppError::new(Kind::JobInProgress, "a claude job is already running in this project: wait for it to finish"));
            }
            self.start_job(context, &folder, Some(&key))
        })();
        match started {
            Ok(job) => Ok(json!({"ok": true, "job": job})),
            Err(refusal) => {
                self.app.log.info(
                    "map_refused",
                    &[
                        ("project", json!(project_id)),
                        ("scope", json!("system")),
                        ("reason", json!(format!("{:?}", refusal.kind))),
                        ("message", json!(refusal.message.chars().take(200).collect::<String>())),
                    ],
                );
                Err(refusal)
            }
        }
    }

    fn check_allowed(&self, folder: &str, entry: Option<&Map<String, Value>>) -> Result<(), AppError> {
        let Some(entry) = entry.filter(|e| e.get("monorepo") == Some(&Value::Bool(true))) else {
            return Err(AppError::invalid_request("only a monorepo has a system map"));
        };
        if entry.get("confirmed") != Some(&Value::Bool(true)) {
            return Err(not_confirmed());
        }
        let reason = self.refusal_reason(folder, Some(entry));
        if !reason.is_empty() {
            return Err(AppError::invalid_request(reason));
        }
        Ok(())
    }

    fn start_job(&self, context: &ProjectContext, folder: &str, repo_key: Option<&str>) -> Result<Value, AppError> {
        let holder: Arc<Mutex<(Option<WorkspaceSnapshot>, Option<(u128, u64)>)>> = Arc::new(Mutex::new((None, None)));
        let allowed_folders = vec![".specs".to_string(), SYSTEM_RELATIVE_PATH.to_string()];
        let (slot, snapshot_folder) = (holder.clone(), folder.to_string());
        let before_start: super::claude_jobs::BeforeStart = Box::new(move |_update| {
            let ignored = map_guard_ignored_paths();
            let mut state = slot.lock().unwrap_or_else(|e| e.into_inner());
            state.0 = Some(WorkspaceFileGuard::default().take_snapshot(&snapshot_folder, &ignored, &DEFAULT_IGNORED_NAMES));
            state.1 = read_system_stamp(&snapshot_folder);
            Ok(None)
        });
        let options = CommandOptions {
            model: JOB_MODEL_CEILING.into(),
            budget_usd: None,
            no_mcp: true,
            prompt_argument: SYSTEM_MAP_PROMPT.into(),
            permission_mode: JOB_PERMISSION_MODE.into(),
            hook_profile: SYSTEM_HOOK_PROFILE.into(),
            ..Default::default()
        };
        let argv = self.app.claude.build_command(&options);
        let (app, project_id, finish_folder, finish_context, finish_key) =
            (self.app.shared(), context.project.id.clone(), folder.to_string(), context.clone(), repo_key.map(String::from));
        let mut command = ClaudeJobCommand::new(argv, "");
        command.work_dir = Some(folder.to_string());
        command.before_start = Some(before_start);
        command.finalize = Some(Box::new(move |result| {
            app.system_maps().finish(&project_id, &finish_folder, &holder, &allowed_folders, result, Some(&finish_context), finish_key.as_deref())
        }));
        self.app.jobs().start_job(folder, JOB_TYPE_MAP, command)
    }

    /// Runs in the job thread: undoes changes outside `.specs` and `SYSTEM.md`, fails when `SYSTEM.md` was not written and writes the root index. The
    /// root `CLAUDE.md` is never written.
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        project_id: &str,
        folder: &str,
        holder: &Arc<Mutex<(Option<WorkspaceSnapshot>, Option<(u128, u64)>)>>,
        allowed_folders: &[String],
        result: Value,
        context: Option<&ProjectContext>,
        repo_key: Option<&str>,
    ) -> Value {
        let (snapshot, written_before) = {
            let state = holder.lock().unwrap_or_else(|e| e.into_inner());
            (state.0.clone(), state.1)
        };
        let Some(snapshot) = snapshot else { return result };
        let allowed: Vec<&str> = allowed_folders.iter().map(String::as_str).collect();
        let report = WorkspaceFileGuard::default().revert_unexpected_changes(&snapshot, &allowed);
        let list = |key: &str| -> Vec<String> { report[key].as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default() };
        let (reverted, removed, unrestorable) = (list("reverted"), list("removed"), list("unrestorable"));
        let touched: Vec<String> = reverted.iter().chain(&removed).chain(&unrestorable).cloned().collect();
        if !touched.is_empty() {
            let head = |items: &[String]| json!(items.iter().take(50).collect::<Vec<_>>());
            self.app.log.warn(
                "map_guard_reverted",
                &[
                    ("project", json!(project_id)),
                    ("scope", json!("system")),
                    ("reverted", head(&reverted)),
                    ("removed", head(&removed)),
                    ("unrestorable", head(&unrestorable)),
                ],
            );
        }
        let mut result = result;
        if engine_state::notify_truthy(result.get("ok")) && !touched.is_empty() {
            let reply = text_of(result.get("reply"));
            result["reply"] = json!(format!("{reply}{}", guard_note(&touched)));
        }
        if engine_state::notify_truthy(result.get("ok")) {
            let stamp = read_system_stamp(folder);
            if stamp.is_none() || stamp == written_before {
                result["ok"] = json!(false);
                result["error"] = json!(SYSTEM_NOT_WRITTEN);
            }
        }
        if engine_state::notify_truthy(result.get("ok")) {
            // the app (not the AI) writes the index; never the root CLAUDE.md
            run_hints::write_map_index(folder, self.app.map_jobs().read_short_head(folder).as_deref(), None);
            self.app.context_blocks().refresh_repo(folder, repo_key, Some(&context.map(|c| c.work_dir()).unwrap_or_else(|| folder.to_string())));
        }
        result
    }

    // ------------------------------------------------ last step of the subproject queue
    /// Starts the system map after a queue whose maps all succeeded and that was the "map all" call, and returns the started job (the queue follows it
    /// to its end). Returns `None` when the queue did not ask for it. Runs in the queue watcher thread, so it waits briefly for the repository to be
    /// free. A refusal is an error (the queue records the reason); the subproject maps stay.
    pub fn on_queue_finished(&self, context: &ProjectContext, repo_name: &str, _folder: &str, queue_state: &Value) -> Result<Option<Value>, AppError> {
        if queue_state.get("include_system") != Some(&Value::Bool(true)) {
            return Ok(None);
        }
        for _ in 0..START_RETRIES {
            match self.start(context, Some(repo_name)) {
                Ok(started) => return Ok(started.get("job").cloned()),
                Err(error) if error.kind == Kind::JobInProgress => std::thread::sleep(START_RETRY),
                Err(error) => return Err(error),
            }
        }
        Err(AppError::new(Kind::JobInProgress, "the repository stayed busy"))
    }
}

#[allow(dead_code)]
fn unused() -> String {
    _now_iso()
}
