// SPDX-License-Identifier: MIT
//! User commands on the loop of ONE project: check the preflight, start, answer, resume, note, pause and instruct.
//!
//! Port of `loop_check`/`loop_start`/`answer`/`resume`/`note`/`pause`/`instruction`/`relaunch` of the legacy panel, same rules and responses. Each
//! public method returns `(http_status, body)`. Only the user's click reaches here (token on the server); the orchestrator/Claude never triggers
//! these commands.

use super::App;
use crate::domain::project::ProjectContext;
use crate::infrastructure::claude_client::redact_secrets;
use crate::infrastructure::process_runner::{START_FAILURE_CODE, TIMEOUT_CODE};
use crate::repositories::loop_command_file::{now_iso, read_json_object, write_json_atomic};
use crate::repositories::small_stores::{append_pending_answer, USER_INSTRUCTION_QUESTION, USER_NOTE_QUESTION};
use engine_state::roadmap_settings::{parse_loop_settings, read_roadmap_base_branch, ParsedSettings};
use engine_state::{context_files, loop_command, loop_io, loop_state, project_repos, subprojects};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

pub const LAUNCH_MARKER_PREFIX: &str = "[loop-launch]"; // read by the workspace service to find the current launch in monitor-relaunch.log
const STATUS_RUNNING: &str = "running";
const STATUS_AWAITING_USER: &str = "awaiting_user";
const STATUSES_RESUMABLE_BY_INSTRUCTION: [&str; 3] = ["error", "stopped", "done"];
const LEFTOVER_CHOICES: [&str; 2] = ["commit", "discard"];
/// The only keys of the start/check body; everything else is decided in the ROADMAP.
const RUN_CHOICE_FIELDS: [&str; 2] = ["push", "remove_worktree_on_finish"];
const MIN_WAVE: i64 = 1;
/// `loop` defaults: not repeated in the argv.
const ENGINE_DEFAULT_BUDGET_TURN: f64 = 5.0;
const ENGINE_DEFAULT_MAX_COST: f64 = 50.0;
const ENGINE_DEFAULT_TURN_MINUTES: i64 = 60;
/// Subagents keep `model: sonnet` in their frontmatter (MODEL-03).
const AGENTS_MODEL: &str = "sonnet";
/// internal/ of the REPOSITORY: the choice made before the worktree exists.
const DELIVERY_PENDING_FILE_NAME: &str = "delivery-pending.json";
pub const REPOS_NOT_DEFINED_CODE: &str = "repos_not_defined";
pub const REPOS_NOT_DEFINED_MESSAGE: &str = "Define the repositories first: tell the planning chat which repositories to change and which only to read, or choose them by hand (Repositories) and confirm. The loop creates the worktrees by itself when it starts.";
const CHECK_TIMEOUT_SECONDS: u64 = 60;
const CHECK_MESSAGE_LIMIT: usize = 500;
const INSTRUCTION_ATTACHMENT_LIMIT: usize = 2000;
const MAX_INSTRUCTION_ATTACHMENTS: usize = 5;
static FEATURE_SLUG_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,80}\z"));
const MONITOR_URL_VARIABLE: &str = "LOOP_MONITOR_URL";
const CHECK_PROBLEM_MARK: &str = "[check] PROBLEM:";
const CHECK_WARNING_MARK: &str = "[check] WARNING:";
const CHECK_READY_MARK: &str = "[check] READY";
const CHECK_COMMAND_MARK: &str = "Command:";
const ATTACHED_CONTEXT_HEADER: &str = "Attached context:";
const LOOP_ALREADY_RUNNING_ERROR: &str = "a loop is already running (or being relaunched) in this worktree";
const MISSING_LOOP_COMMAND_ERROR: &str = "LOOP-CMD.json missing or without argv: run run_loop.py once to record the command";
const NO_LOOP_ERROR: &str = "there is no loop yet: start one from the New loop panel";

pub type Reply = (u16, Value);

fn failure(status: u16, message: impl Into<String>) -> Reply {
    (status, json!({"ok": false, "error": message.into()}))
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

fn is_finite_non_negative_number(value: Option<&Value>) -> bool {
    value.filter(|v| v.is_number()).and_then(Value::as_f64).is_some_and(|f| f.is_finite() && f >= 0.0)
}

/// `5.0` -> "5", `7.5` -> "7.5" (the text the engine option receives).
pub fn format_amount(value: f64) -> String {
    if value.fract() == 0.0 { format!("{}", value as i64) } else { format!("{value}") }
}

/// `Ok((push, remove_worktree_on_finish))` (each `None` when not given), or `Err(error)`. Other body keys are ignored.
pub fn read_run_choices(body: &Value) -> Result<(Option<bool>, Option<bool>), String> {
    let mut choices = [None, None];
    for (index, name) in RUN_CHOICE_FIELDS.iter().enumerate() {
        match body.get(*name) {
            None | Some(Value::Null) => {}
            Some(Value::Bool(b)) => choices[index] = Some(*b),
            Some(_) => return Err(format!("{name} must be a boolean")),
        }
    }
    Ok((choices[0], choices[1]))
}

/// `loop` options for the ROADMAP loop settings (`parse_loop_settings`). A value equal to the engine default is not repeated; `base_branch` = the
/// ROADMAP `Branch base` when the line is there and valid (else the engine falls back to the environment, then developer); `orchestrator_model`
/// `None` = no option (the check), `Some("")` = `--orchestrator-model ""` (no `--model` flag in the turns). Never a shell.
pub fn convert_settings_to_loop_flags(settings: &Map<String, Value>, wave: i64, push: bool, orchestrator_model: Option<&str>, base_branch: &str) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    let num = |key: &str| settings.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    if wave > MIN_WAVE || settings.get("dispatch") != Some(&json!("auto")) {
        // an explicit `Dispatch: 1` must still be sent (the engine default is 3)
        flags.extend(["--wave".to_string(), wave.to_string()]);
    }
    if settings.get("e2e") == Some(&Value::Bool(true)) {
        flags.push("--e2e".into());
    }
    if !push {
        flags.push("--no-push".into());
    }
    if settings.get("when_stuck") == Some(&json!("continue")) {
        flags.push("--continue-on-blocked".into());
    }
    if num("budget_turn") != ENGINE_DEFAULT_BUDGET_TURN {
        flags.extend(["--budget".to_string(), format_amount(num("budget_turn"))]);
    }
    if num("budget_loop") != ENGINE_DEFAULT_MAX_COST {
        flags.extend(["--max-cost".to_string(), format_amount(num("budget_loop"))]);
    }
    if settings.get("minutes_turn").and_then(Value::as_i64) != Some(ENGINE_DEFAULT_TURN_MINUTES) {
        flags.extend(["--timeout".to_string(), text_of(settings.get("minutes_turn"))]);
    }
    let branch = text_of(settings.get("branch"));
    if !branch.is_empty() {
        flags.extend(["--branch".to_string(), branch]);
    }
    if !base_branch.is_empty() {
        flags.extend(["--base-branch".to_string(), base_branch.to_string()]);
    }
    let setup = text_of(settings.get("setup"));
    if !setup.is_empty() {
        if setup.starts_with('-') {
            flags.push(format!("--setup={setup}")); // a single argument
        } else {
            flags.extend(["--setup".to_string(), setup]);
        }
    }
    if let Some(model) = orchestrator_model {
        flags.extend(["--orchestrator-model".to_string(), model.to_string()]);
    }
    flags
}

/// `{"ready", "problems", "warnings", "command"}` from the lines `[check] PROBLEM:`, `[check] WARNING:` and `[check] READY. Command:` (+ the
/// command on the same line or the next).
pub fn parse_check_output(text: &str) -> Value {
    let (mut problems, mut warnings, mut command, mut ready) = (Vec::<String>::new(), Vec::<String>::new(), String::new(), false);
    let lines = pycompat::splitlines(text);
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix(CHECK_PROBLEM_MARK) {
            problems.push(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix(CHECK_WARNING_MARK) {
            warnings.push(rest.trim().to_string());
        } else if line.starts_with(CHECK_READY_MARK) {
            ready = true;
            let on_same_line = line.split_once(CHECK_COMMAND_MARK).map(|p| p.1.trim()).unwrap_or("");
            command = if !on_same_line.is_empty() { on_same_line.to_string() } else { lines.get(index + 1).map(|l| l.trim().to_string()).unwrap_or_default() };
        }
    }
    json!({"ready": ready && problems.is_empty(), "problems": problems, "warnings": warnings, "command": command})
}

pub fn compose_instruction_text(text: &str, feature: Option<&str>, attachments: &[String]) -> String {
    let mut composed = format!("{}{}", feature.filter(|f| !f.is_empty()).map(|f| format!("[{f}] ")).unwrap_or_default(), text.trim());
    if !attachments.is_empty() {
        let items: Vec<String> = attachments.iter().filter(|i| !i.trim().is_empty()).map(|i| format!("- {}", i.trim())).collect();
        composed.push_str(&format!("\n{ATTACHED_CONTEXT_HEADER}\n{}", items.join("\n")));
    }
    composed
}

/// Default dispatch (`wave`) of a new loop: ALWAYS 3 features per turn unless the user chose another number. The number is how many features (lanes)
/// move in parallel in a turn, not a promise: each lane is one feature with its OWN agent at every step and advances by itself. The scheduler still
/// fills execution only with features whose dependencies are verified and whose `paths=` do not overlap.
pub fn compute_default_dispatch() -> Value {
    const MIN_WAVE_LIMIT: i64 = 1;
    const MAX_WAVE_LIMIT: i64 = 8;
    const DEFAULT_WAVE: i64 = 3;
    let wave = DEFAULT_WAVE.clamp(MIN_WAVE_LIMIT, MAX_WAVE_LIMIT);
    json!({"wave": wave, "auto": true, "stage": "", "reason": format!("{wave} features per turn, always, each one with its own agents at every step; dependent features and features sharing files wait for the next turn")})
}

pub struct LoopControlService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn loop_control(&self) -> LoopControlService<'_> {
        LoopControlService { app: self }
    }
}

impl LoopControlService<'_> {
    // ------------------------------------------------ busy state and locks
    fn relaunch_lock(&self, project_id: &str) -> Arc<Mutex<()>> {
        self.app.relaunch_locks.lock().unwrap_or_else(|e| e.into_inner()).entry(project_id.to_string()).or_default().clone()
    }

    pub fn has_live_relaunch(&self, project_id: &str) -> bool {
        let pid = self.app.relaunched_pids.lock().unwrap_or_else(|e| e.into_inner()).get(project_id).copied().unwrap_or(0);
        self.app.processes.is_alive(pid)
    }

    /// A loop launched by this app that is still alive keeps the app on.
    pub fn is_busy(&self) -> bool {
        let pids: Vec<i64> = self.app.relaunched_pids.lock().unwrap_or_else(|e| e.into_inner()).values().copied().collect();
        pids.into_iter().any(|pid| self.app.processes.is_alive(pid))
    }

    pub fn is_loop_alive(&self, context: &ProjectContext) -> bool {
        self.app.loop_status().get_loop_summary(context)["running"] == true || self.has_live_relaunch(&context.project.id)
    }

    pub fn get_project_panel_url(&self, context: &ProjectContext) -> String {
        format!("{}p/{}/", self.app.app_url(), context.project.id)
    }

    // ------------------------------------------------ process launch
    /// Call with the relaunch lock held. Output goes to `internal/monitor-relaunch.log`.
    fn spawn_detached_loop(&self, context: &ProjectContext, action: &str, argv: &[String], folder: &str, extra_env: &[(String, String)]) -> Reply {
        let log_path = self.app.loop_commands.get_relaunch_log_path(&context.work_dir());
        let spawned = self.write_launch_marker(&log_path, action).and_then(|_| self.app.processes.spawn_detached(argv, folder, &log_path.display().to_string(), extra_env));
        match spawned {
            Ok(pid) => {
                self.app.relaunched_pids.lock().unwrap_or_else(|e| e.into_inner()).insert(context.project.id.clone(), pid);
                self.app.log.info("loop_command_launched", &[("project", json!(context.project.id)), ("action", json!(action)), ("pid", json!(pid))]);
                (200, json!({"ok": true, "pid": pid}))
            }
            Err(error) => {
                self.app.log.error("loop_command_failed", &[("project", json!(context.project.id)), ("action", json!(action)), ("error", json!(error.to_string().chars().take(300).collect::<String>()))]);
                failure(500, format!("could not relaunch the loop: {error}"))
            }
        }
    }

    /// One `[loop-launch] <iso time> <action>` line right before the process starts: the log accumulates across launches and the workspace reader
    /// needs to know where the CURRENT launch begins (everything after the last marker).
    fn write_launch_marker(&self, log_path: &Path, action: &str) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::OpenOptions::new().create(true).append(true).open(log_path)?.write_all(format!("{LAUNCH_MARKER_PREFIX} {} {action}\n", now_iso()).as_bytes())
    }

    /// `Ok(command)` or `Err(reply)`. LOOP-CMD.json is never run as it is: `loop_command::rebuild_loop_command` keeps only the known options and
    /// builds the rest itself (the app's own executable, the project repo), and the claude binaries come from the app config, not from the file.
    /// Missing file = 409; a command that is not trusted = 409 `loop_command_invalid`.
    fn rebuild_saved_loop_command(&self, context: &ProjectContext) -> Result<Value, Reply> {
        let saved = Value::Object(self.app.loop_commands.read_loop_command(&context.work_dir()));
        let usable = saved.get("argv").and_then(Value::as_array).is_some_and(|a| !a.is_empty() && a.iter().all(Value::is_string));
        if !usable {
            return Err(failure(409, MISSING_LOOP_COMMAND_ERROR));
        }
        let allowed = vec![context.repo_path().to_string(), context.project.loop_worktree_path()];
        let mut command = match loop_command::rebuild_loop_command(&saved, context.repo_path(), &allowed, &self.app.engine_program()) {
            Ok(command) => command,
            Err(refusal) => {
                self.app.log.warn("loop_command_refused", &[("project", json!(context.project.id)), ("reason", json!(refusal.0.chars().take(300).collect::<String>()))]);
                return Err((409, json!({"ok": false, "code": loop_command::LOOP_COMMAND_INVALID_CODE, "error": format!("LOOP-CMD.json was not trusted, nothing was started ({}). Start the loop again from the New loop panel to record it again.", refusal.0)})));
            }
        };
        let dropped = command["dropped"].as_array().cloned().unwrap_or_default();
        if !dropped.is_empty() {
            let mut names: Vec<String> = dropped.iter().filter_map(Value::as_str).map(String::from).collect();
            names.sort();
            names.dedup();
            self.app.log.info("loop_command_options_dropped", &[("project", json!(context.project.id)), ("options", json!(names))]);
        }
        let mut argv: Vec<String> = command["argv"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
        if !self.app.config.claude_binary.is_empty() {
            argv.extend(["--claude".to_string(), self.app.config.claude_binary.clone()]);
        }
        for fallback in &self.app.config.claude_fallback_binaries {
            argv.extend(["--claude-fallback".to_string(), fallback.clone()]);
        }
        command["argv"] = json!(argv);
        Ok(command)
    }

    /// Detached `loop` rebuilt from LOOP-CMD.json (+ `--answer <answer>`, + extra flags such as `--leftovers commit`).
    fn relaunch_loop_from_saved_command(&self, context: &ProjectContext, action: &str, answer: Option<&str>, extra_flags: &[String]) -> Reply {
        let lock = self.relaunch_lock(&context.project.id);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_loop_alive(context) {
            return failure(409, LOOP_ALREADY_RUNNING_ERROR);
        }
        let command = match self.rebuild_saved_loop_command(context) {
            Ok(command) => command,
            Err(refusal) => return refusal,
        };
        let mut argv: Vec<String> = command["argv"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
        if let Some(answer) = answer.filter(|a| !a.is_empty()) {
            argv.extend(["--answer".to_string(), answer.to_string()]);
        }
        argv.extend(extra_flags.iter().cloned());
        self.spawn_detached_loop(context, action, &argv, command["cwd"].as_str().unwrap_or(""), &[])
    }

    /// Synchronous `loop <repo> --check <flags>` (reads the output, creates nothing, spends nothing).
    fn run_loop_check(&self, context: &ProjectContext, flags: &[String]) -> Reply {
        let mut argv = self.app.engine_argv("run_loop");
        argv.push(context.repo_path().to_string());
        argv.push("--check".into());
        argv.extend(flags.iter().cloned());
        let (code, output, error) = self.app.processes.run_and_wait(&argv, context.repo_path(), CHECK_TIMEOUT_SECONDS, None, &[], None);
        if code == TIMEOUT_CODE {
            return failure(504, format!("the --check took longer than {CHECK_TIMEOUT_SECONDS} s"));
        }
        if code == START_FAILURE_CODE && output.is_empty() {
            return failure(500, format!("could not run run_loop.py: {}", redact_secrets(&error, Some(CHECK_MESSAGE_LIMIT))));
        }
        let mut result = parse_check_output(&output);
        if result["ready"] != true && result["problems"].as_array().is_some_and(|p| p.is_empty()) {
            let source = if error.is_empty() { &output } else { &error };
            let last = pycompat::splitlines(source.trim()).last().map(|l| l.to_string());
            result["problems"] = json!([last.unwrap_or_else(|| format!("the --check did not confirm READY (code {code})"))]);
        }
        for key in ["problems", "warnings"] {
            let cleaned: Vec<String> = result[key].as_array().map(|a| a.iter().map(|i| redact_secrets(i.as_str().unwrap_or(""), Some(CHECK_MESSAGE_LIMIT))).collect()).unwrap_or_default();
            result[key] = json!(cleaned);
        }
        let mut body = Map::from_iter([("ok".to_string(), json!(true))]);
        body.extend(result.as_object().cloned().unwrap_or_default());
        (200, Value::Object(body))
    }

    // ------------------------------------------------ commands
    /// `parse_loop_settings` of the ROADMAP in the work folder (the worktree copy is the live one once a loop ran).
    pub fn read_loop_settings(&self, context: &ProjectContext) -> ParsedSettings {
        parse_loop_settings(&self.read_roadmap_text(context))
    }

    fn read_roadmap_text(&self, context: &ProjectContext) -> String {
        std::fs::read(Path::new(&context.specs_dir()).join("project").join("ROADMAP.md")).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
    }

    /// `(flags, dispatch)` or `Err(error)`. Everything but `push` comes from the ROADMAP header; `Dispatch: auto` is computed, a number is explicit.
    fn resolve_flags_and_dispatch(&self, context: &ProjectContext, body: &Value) -> Result<(Vec<String>, Value), String> {
        let (push_choice, _) = read_run_choices(body)?;
        let settings = self.read_loop_settings(context).settings;
        let dispatch = if settings.get("dispatch") == Some(&json!("auto")) {
            compute_default_dispatch()
        } else {
            json!({"wave": settings.get("dispatch").cloned().unwrap_or(Value::Null), "auto": false, "reason": "set in the ROADMAP"})
        };
        let push = push_choice.unwrap_or_else(|| settings.get("push_pr") == Some(&Value::Bool(true)));
        let base_branch = read_roadmap_base_branch(&self.read_roadmap_text(context));
        let mut flags = convert_settings_to_loop_flags(&settings, dispatch["wave"].as_i64().unwrap_or(1), push, None, &base_branch);
        if !self.app.config.claude_binary.is_empty() {
            // AUD-36: the engine runs the same `claude` the app runs (a demo/test fake), not always the one on PATH
            flags.extend(["--claude".to_string(), self.app.config.claude_binary.clone()]);
        }
        for fallback in &self.app.config.claude_fallback_binaries {
            // e.g. `cc`: another account when this one hits its limit
            flags.extend(["--claude-fallback".to_string(), fallback.clone()]);
        }
        Ok((flags, dispatch))
    }

    pub fn check_preflight(&self, context: &ProjectContext, body: &Value) -> Reply {
        let (flags, dispatch) = match self.resolve_flags_and_dispatch(context, body) {
            Ok(resolved) => resolved,
            Err(error) => return failure(400, error),
        };
        self.app.log.info("loop_command_launched", &[("project", json!(context.project.id)), ("action", json!("loop_check")), ("pid", Value::Null)]);
        let (status, mut result) = self.run_loop_check(context, &flags);
        if status != 200 {
            return (status, result);
        }
        let mut warnings: Vec<Value> = result["warnings"].as_array().cloned().unwrap_or_default();
        warnings.extend(Self::describe_settings_warnings(&self.read_loop_settings(context)).into_iter().map(Value::from));
        result["warnings"] = json!(warnings);
        result["dispatch"] = dispatch;
        result["repos"] = json!(self.describe_uncommitted_context_by_repo(context));
        (status, result)
    }

    /// One warning per invalid ROADMAP loop setting (the default is used); never a problem, the loop still starts.
    fn describe_settings_warnings(loop_settings: &ParsedSettings) -> Vec<String> {
        loop_settings.problems.iter().map(|item| format!("ROADMAP setting \"{}: {}\" is invalid ({})", text_of(item.get("name")), text_of(item.get("value")), text_of(item.get("message")))).collect()
    }

    /// `[{"name", "uncommitted_context"}]` per repo, the same list as `GET workspace` (empty once the repo's worktree exists).
    pub fn describe_uncommitted_context_by_repo(&self, context: &ProjectContext) -> Vec<Value> {
        project_repos::load_repos(context.repo_path())
            .iter()
            .map(|repo| {
                let pending = if Path::new(&repo.path).is_dir() && !Path::new(&repo.loop_worktree_path()).is_dir() {
                    let (entry, _) = subprojects::load(context.repo_path(), Some(&repo.name));
                    context_files::uncommitted_context(&repo.path, entry.as_ref())
                } else {
                    vec![]
                };
                json!({"name": repo.name, "uncommitted_context": pending})
            })
            .collect()
    }

    /// New loop (or a continuation, if the worktree exists): preflight first; `LOOP_MONITOR_URL` = this project's panel.
    pub fn start_new_loop(&self, context: &ProjectContext, body: &Value) -> Reply {
        let (flags, _dispatch) = match self.resolve_flags_and_dispatch(context, body) {
            Ok(resolved) => resolved,
            Err(error) => return failure(400, error),
        };
        let lock = self.relaunch_lock(&context.project.id);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_loop_alive(context) {
            return failure(409, "a loop is already running (or being started) in this worktree");
        }
        if !Path::new(&context.project.loop_worktree_path()).is_dir() && !project_repos::is_scope_defined(context.repo_path()) {
            // a loop that already exists keeps the scope it has
            return (409, json!({"ok": false, "code": REPOS_NOT_DEFINED_CODE, "error": REPOS_NOT_DEFINED_MESSAGE}));
        }
        let (status, checked) = self.run_loop_check(context, &flags);
        if status != 200 {
            return (status, checked);
        }
        if checked["ready"] != true {
            return (409, json!({"ok": false, "error": "preflight failed: the loop was not started", "problems": checked["problems"], "warnings": checked["warnings"]}));
        }
        self.record_delivery_choices(context, body);
        let mut argv = self.app.engine_argv("run_loop");
        argv.push(context.repo_path().to_string());
        argv.extend(flags);
        argv.extend(["--orchestrator-model".to_string(), self.app.config.orchestrator_model.clone()]); // "" = the engine sends no --model
        self.spawn_detached_loop(context, "loop_start", &argv, context.repo_path(), &[(MONITOR_URL_VARIABLE.to_string(), self.get_project_panel_url(context))])
    }

    // ------------------------------------------------ run choices (delivery), settings and spend cap
    /// internal/, where `merge_pending_delivery` reads it (`loop_path` would put a .json in json/).
    fn pending_delivery_path(&self, context: &ProjectContext) -> std::path::PathBuf {
        loop_state::internal_dir(context.repo_path()).join(DELIVERY_PENDING_FILE_NAME)
    }

    /// `{push_pr, remove_worktree_on_finish}`: the two checkboxes of the Start dialog (push defaults to the ROADMAP `Push and PR`).
    pub fn build_delivery_choices(&self, context: &ProjectContext, body: &Value) -> Map<String, Value> {
        let (push_choice, remove_choice) = read_run_choices(body).unwrap_or((None, None));
        let push = push_choice.unwrap_or_else(|| self.read_loop_settings(context).settings.get("push_pr") == Some(&Value::Bool(true)));
        Map::from_iter([("push_pr".to_string(), json!(push)), ("remove_worktree_on_finish".to_string(), json!(remove_choice.unwrap_or(true)))])
    }

    /// START-06. Worktree already there (continuation): merged into its meta.json BEFORE the engine starts (it loads meta once and saves its own copy
    /// afterwards, so a later write would be lost). No worktree yet: the engine creates meta.json itself, so the choice waits in
    /// `internal/delivery-pending.json` of the repository and `read_delivery` falls back to it until the engine merges it.
    fn record_delivery_choices(&self, context: &ProjectContext, body: &Value) {
        let delivery = Value::Object(self.build_delivery_choices(context, body));
        let meta_path = loop_state::loop_path(&context.project.loop_worktree_path(), "meta.json");
        if meta_path.is_file() {
            let mut meta = read_json_object(&meta_path);
            meta.insert("delivery".into(), delivery);
            let _ = write_json_atomic(&meta_path, &Value::Object(meta));
            return;
        }
        let _ = write_json_atomic(&self.pending_delivery_path(context), &delivery);
    }

    /// The recorded delivery choices: worktree meta, else the pending file, else the defaults (push per ROADMAP, remove on Finish).
    pub fn read_delivery(&self, context: &ProjectContext) -> Value {
        let meta = self.app.loop_commands.read_meta(&context.work_dir());
        let recorded = match meta.get("delivery") {
            Some(Value::Object(d)) => d.clone(),
            _ => read_json_object(&self.pending_delivery_path(context)),
        };
        let defaults = self.build_delivery_choices(context, &json!({}));
        Value::Object(defaults.iter().map(|(name, default)| (name.clone(), if recorded.get(name).is_some_and(Value::is_boolean) { recorded[name].clone() } else { default.clone() })).collect())
    }

    /// `GET loop/settings`: what the Start dialog shows, read-only (the values are decided in planning).
    pub fn get_loop_settings(&self, context: &ProjectContext) -> Reply {
        let loop_settings = self.read_loop_settings(context);
        let worktree = context.project.loop_worktree_path();
        let worktree_created = Path::new(&worktree).is_dir();
        let meta = if worktree_created { self.app.loop_commands.read_meta(&worktree) } else { Map::new() };
        let repos = self.describe_uncommitted_context_by_repo(context);
        let reattach_branch = if worktree_created { Value::Null } else { self.find_reattach_branch(context).map(Value::from).unwrap_or(Value::Null) };
        let uncommitted = repos.iter().any(|item| item["uncommitted_context"].as_array().is_some_and(|a| !a.is_empty()));
        (
            200,
            json!({
                "settings": loop_settings.settings, "problems": loop_settings.problems, "missing": loop_settings.missing, "defaults": loop_settings.defaults,
                "integration_branch": super::planning_gates::get_integration_branch(self.app, &self.read_roadmap_text(context)),
                "base_branch_resolved": super::planning_gates::resolve_base_branch(self.app, &loop_settings),
                "delivery": self.read_delivery(context),
                "workspace": {"repos": repos, "worktree_created": worktree_created, "branch": text_of(meta.get("branch")), "reattach_branch": reattach_branch, "uncommitted": uncommitted},
                "models": {"orchestrator": self.app.config.orchestrator_model, "agents": AGENTS_MODEL},
            }),
        )
    }

    /// The branch the next start would re-attach to (same rule as the engine's `reattach_branch_of`): the primary repo's branch recorded by the last
    /// Close loop, still existing, and that run NOT finished by a wrap-up Finish. `None` when the next start creates a new loop branch.
    fn find_reattach_branch(&self, context: &ProjectContext) -> Option<String> {
        let repos = project_repos::load_repos(context.repo_path());
        let primary = project_repos::primary_of(&repos);
        let last_run = read_json_object(&loop_state::loop_path(context.repo_path(), "last-run.json"));
        let recorded = last_run.get("repos").and_then(|r| r.get(&primary.name)).filter(|r| r.is_object())?;
        let branch = text_of(recorded.get("branch"));
        if engine_state::notify_truthy(recorded.get("finished")) || branch.is_empty() {
            return None;
        }
        let reference = format!("refs/heads/{branch}");
        let (code, _, _) = loop_io::run_timeout_raw("git", &["-C", &primary.path, "rev-parse", "--verify", "--quiet", &reference], None, 15);
        (code == 0).then_some(branch)
    }

    /// `POST loop/raise-limit {max_cost_usd}`: the spend cap of THIS run goes up (meta `max_cost_usd`, which wins over `--max-cost`) and the same loop
    /// resumes. Refused unless the new cap is above what was already spent (`cost_usd`, never reset).
    pub fn raise_loop_limit(&self, context: &ProjectContext, body: &Value) -> Reply {
        let value = body.get("max_cost_usd");
        let Some(limit) = value.filter(|_| is_finite_non_negative_number(value)).and_then(Value::as_f64).filter(|v| *v > 0.0) else { return failure(400, "max_cost_usd must be a number greater than 0") };
        let mut meta = self.app.loop_commands.read_meta(&context.work_dir());
        let spent = if is_finite_non_negative_number(meta.get("cost_usd")) { meta["cost_usd"].as_f64().unwrap_or(0.0) } else { 0.0 };
        if limit <= spent {
            let rounded = (spent * 10000.0).round_ties_even() / 10000.0;
            return (400, json!({"ok": false, "error": format!("the new limit must be above the US$ {} already spent", format_amount(rounded)), "cost_usd": spent}));
        }
        if self.is_loop_alive(context) {
            // no status gate: a cap pause may still read as `no_loop` until loop_state knows `paused`
            return failure(409, LOOP_ALREADY_RUNNING_ERROR);
        }
        meta.insert("max_cost_usd".into(), json!(limit));
        let _ = write_json_atomic(&loop_state::loop_path(&context.work_dir(), "meta.json"), &Value::Object(meta));
        let (code, data) = self.resume_loop(context, None);
        if code != 200 {
            return (code, data);
        }
        self.app.log.info("loop_command_launched", &[("project", json!(context.project.id)), ("action", json!("raise_limit")), ("pid", data.get("pid").cloned().unwrap_or(Value::Null))]);
        (202, json!({"ok": true, "cost_usd": spent, "max_cost_usd": limit}))
    }

    pub fn answer_loop_question(&self, context: &ProjectContext, body: &Value) -> Reply {
        let summary = self.app.loop_status().get_loop_summary(context);
        if summary["status"] != STATUS_AWAITING_USER {
            return failure(409, format!("the loop is not waiting for an answer (status: {})", text_of(summary.get("status"))));
        }
        let mut text = body.get("text").cloned().unwrap_or(Value::Null);
        let option = body.get("option").filter(|o| !o.is_null());
        if let Some(option) = option {
            let Some(number) = (option.is_i64() || option.is_u64()).then(|| option.as_i64()).flatten() else { return failure(400, "option must be an integer") };
            let options: Vec<Value> = summary["json"].get("options").and_then(Value::as_array).cloned().unwrap_or_default();
            if number < 1 || number as usize > options.len() {
                return failure(400, format!("invalid option {number}: the notice listed {} option(s)", options.len()));
            }
            text = json!(text_of(Some(&options[number as usize - 1])));
        }
        let Some(answer) = text.as_str().filter(|t| !t.trim().is_empty()) else { return failure(400, "give a text or an option") };
        self.relaunch_loop_from_saved_command(context, "answer", Some(answer.trim()), &[])
    }

    /// Relaunch; `{"leftovers": "commit"|"discard"}` also settles the files an earlier turn left uncommitted (`--leftovers`).
    pub fn resume_loop(&self, context: &ProjectContext, body: Option<&Value>) -> Reply {
        if self.app.loop_status().get_loop_summary(context)["running"] == true {
            return failure(409, "the loop is already running");
        }
        let leftovers = body.and_then(|b| b.get("leftovers")).cloned().unwrap_or(Value::Null);
        let chosen = match &leftovers {
            Value::Null => None,
            Value::String(s) if s.is_empty() => None,
            Value::String(s) if LEFTOVER_CHOICES.contains(&s.as_str()) => Some(s.clone()),
            _ => return failure(400, format!("leftovers must be one of: {}", LEFTOVER_CHOICES.join(", "))),
        };
        let extra: Vec<String> = chosen.map(|c| vec!["--leftovers".to_string(), c]).unwrap_or_default();
        self.relaunch_loop_from_saved_command(context, "resume", None, &extra)
    }

    pub fn write_user_note(&self, context: &ProjectContext, body: &Value) -> Reply {
        let Some(text) = body.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) else { return failure(400, "empty text") };
        if self.app.loop_status().get_loop_summary(context)["status"] != STATUS_RUNNING {
            return failure(409, "a note only works with the loop running (when stopped, use answer or resume)");
        }
        append_pending_answer(&context.work_dir(), USER_NOTE_QUESTION, text);
        self.app.log.info("loop_command_launched", &[("project", json!(context.project.id)), ("action", json!("note")), ("pid", Value::Null)]);
        (200, json!({"ok": true}))
    }

    pub fn request_loop_pause(&self, context: &ProjectContext) -> Reply {
        if self.app.loop_status().get_loop_summary(context)["status"] != STATUS_RUNNING {
            return failure(409, "the loop can only be paused while it is running");
        }
        self.app.loop_commands.create_pause_file(&context.work_dir());
        self.app.log.info("loop_command_launched", &[("project", json!(context.project.id)), ("action", json!("pause")), ("pid", Value::Null)]);
        (200, json!({"ok": true}))
    }

    /// `(text, feature, attachments)` or `Err(error)`.
    fn validate_instruction_body(body: &Value) -> Result<(String, Option<String>, Vec<String>), String> {
        let Some(text) = body.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) else { return Err("empty text".into()) };
        let feature = match body.get("feature") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if FEATURE_SLUG_PATTERN.is_match(s) => Some(s.clone()),
            Some(_) => return Err("invalid feature".into()),
        };
        let limits_error = format!("attachments: up to {MAX_INSTRUCTION_ATTACHMENTS} texts of up to {INSTRUCTION_ATTACHMENT_LIMIT} characters");
        let attachments = match body.get("attachments") {
            None | Some(Value::Null) => vec![],
            Some(Value::Array(list)) => {
                let texts: Option<Vec<String>> = list.iter().map(|i| i.as_str().filter(|s| s.chars().count() <= INSTRUCTION_ATTACHMENT_LIMIT).map(String::from)).collect();
                match texts {
                    Some(texts) if texts.len() <= MAX_INSTRUCTION_ATTACHMENTS => texts,
                    _ => return Err(limits_error),
                }
            }
            Some(_) => return Err(limits_error),
        };
        Ok((text.to_string(), feature, attachments))
    }

    /// Instruction to the CURRENT version: note (running), answer (awaiting), note+resume (error/stopped/done).
    pub fn send_instruction_to_loop(&self, context: &ProjectContext, body: &Value) -> Reply {
        let (text, feature, attachments) = match Self::validate_instruction_body(body) {
            Ok(parts) => parts,
            Err(error) => return failure(400, error),
        };
        let composed = compose_instruction_text(&text, feature.as_deref(), &attachments);
        let summary = self.app.loop_status().get_loop_summary(context);
        let status = text_of(summary.get("status"));
        if status == STATUS_RUNNING {
            append_pending_answer(&context.work_dir(), USER_INSTRUCTION_QUESTION, &composed);
            self.app.log.info("loop_command_launched", &[("project", json!(context.project.id)), ("action", json!("instruction_note")), ("pid", Value::Null)]);
            return (200, json!({"ok": true, "action": "note"}));
        }
        if status == STATUS_AWAITING_USER {
            let (code, mut data) = self.answer_loop_question(context, &json!({"text": composed}));
            if code == 200 {
                data["action"] = json!("answer");
            }
            return (code, data);
        }
        if STATUSES_RESUMABLE_BY_INSTRUCTION.contains(&status.as_str()) {
            return self.write_instruction_and_resume(context, &composed);
        }
        failure(409, NO_LOOP_ERROR)
    }

    fn write_instruction_and_resume(&self, context: &ProjectContext, composed: &str) -> Reply {
        if let Err(refusal) = self.rebuild_saved_loop_command(context) {
            return refusal; // before the note: a refused command leaves no block behind
        }
        if self.is_loop_alive(context) {
            return failure(409, LOOP_ALREADY_RUNNING_ERROR);
        }
        append_pending_answer(&context.work_dir(), USER_INSTRUCTION_QUESTION, composed);
        let (code, mut data) = self.relaunch_loop_from_saved_command(context, "instruction_resume", None, &[]);
        if code == 200 {
            data["action"] = json!("note+resume");
        }
        (code, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_output_is_parsed_and_flags_follow_the_settings() {
        let parsed = parse_check_output("noise\n[check] WARNING: slow disk\n[check] PROBLEM: no git\n[check] READY. Command:\nharness loop x\n");
        assert_eq!((parsed["ready"].clone(), parsed["problems"].clone(), parsed["warnings"].clone(), parsed["command"].clone()), (json!(false), json!(["no git"]), json!(["slow disk"]), json!("harness loop x")));
        assert_eq!(parse_check_output("[check] READY. Command: run it")["command"], "run it");
        assert_eq!(parse_check_output("[check] READY")["ready"], true);
        let settings = parse_loop_settings("**Dispatch:** 2\n**E2E:** yes\n**Max spend per turn:** 7.5\n**Max minutes per turn:** 30\n**Branch:** feature/x\n**Setup:** -weird\n**When a feature gets stuck:** continue\n").settings;
        let flags = convert_settings_to_loop_flags(&settings, 2, false, Some(""), "release/1");
        assert_eq!(flags, ["--wave", "2", "--e2e", "--no-push", "--continue-on-blocked", "--budget", "7.5", "--timeout", "30", "--branch", "feature/x", "--base-branch", "release/1", "--setup=-weird", "--orchestrator-model", ""]);
        let defaults = parse_loop_settings("").settings;
        assert!(convert_settings_to_loop_flags(&defaults, 3, true, None, "").is_empty() || convert_settings_to_loop_flags(&defaults, 3, true, None, "") == ["--wave", "3"]);
        assert_eq!((format_amount(5.0), format_amount(7.5)), ("5".to_string(), "7.5".to_string()));
        assert_eq!(compose_instruction_text(" fix ", Some("login"), &["  a ".into(), " ".into()]), "[login] fix\nAttached context:\n- a");
        assert_eq!(read_run_choices(&json!({"push": "yes"})).unwrap_err(), "push must be a boolean");
        assert_eq!(read_run_choices(&json!({"push": true, "x": 1})).unwrap(), (Some(true), None));
        assert_eq!(compute_default_dispatch()["wave"], 3);
    }

    #[test]
    fn instruction_bodies_are_validated() {
        let check = |body: Value| LoopControlService::validate_instruction_body(&body);
        assert_eq!(check(json!({"text": " "})).unwrap_err(), "empty text");
        assert_eq!(check(json!({"text": "x", "feature": "-bad"})).unwrap_err(), "invalid feature");
        assert!(check(json!({"text": "x", "attachments": ["a", "b", "c", "d", "e", "f"]})).unwrap_err().starts_with("attachments: up to 5 texts"));
        assert!(check(json!({"text": "x", "attachments": [1]})).is_err());
        assert_eq!(check(json!({"text": "x", "feature": "login", "attachments": ["a"]})).unwrap(), ("x".to_string(), Some("login".to_string()), vec!["a".to_string()]));
    }
}
