// SPDX-License-Identifier: MIT
//! Planning mode: the live chat that WRITES the planning documents (`/orchestrator plan`), the gate checklist and the document preview of a project
//! that never approved a roadmap.
//!
//! One conversation per project (`internal/planning/thread.json`), one `claude -p` session resumed on every message. Each reply is a claude job (one
//! job per project at a time, kept in `internal/jobs/`) with: the account's default model, US$ 2 per message, 600 s, MCP only the servers the user
//! allowed in Settings > MCP servers (`mcp_access`; config.json `mcp_servers`, none by default), the REPOS.json `reference` repos as `--add-dir` (read),
//! `--permission-mode dontAsk`, `--allowedTools Read Grep Glob Skill Edit(/.specs/project/PROJECT.md) Edit(/.specs/project/ROADMAP.md)
//! Edit(/.specs/project/FLOWS.md) Edit(/.specs/project/TEST-ENVIRONMENT.md) Edit(/.specs/project/RULES.md)` (an `Edit` rule also covers Write; `/path` in
//! a CLI rule = the session's working directory) and `--disallowedTools Bash NotebookEdit Agent`. The user's settings can widen those rules, so after
//! every reply the file guard undoes any change outside those documents (logged as `planning_guard_reverted`). The conversation never approves: only
//! the user's click does.

use super::claude_jobs::{BeforeStart, ClaudeJobCommand, ClaudeJobError, Prepared, JOB_STATE_ERROR, JOB_STATE_OK, JOB_TYPE_PLANNING};
use super::planning_gates::{are_gates_ok, are_prerequisites_ok, describe_gates_for_ai, read_roadmap_loop_settings, PREREQUISITE_GATES};
use super::project_capabilities::{replace_block, END_MARKER, START_MARKER};
use super::roadmap::extract_open_questions;
use super::App;
use crate::domain::errors::{AppError, Kind, LOOP_RUNNING_CODE, PLANNING_BUSY_CODE, PLANNING_PREREQUISITES_CODE};
use crate::domain::project::ProjectContext;
use crate::infrastructure::claude_client::{redact_secrets, CommandOptions};
use crate::infrastructure::workspace_file_guard::{WorkspaceFileGuard, WorkspaceSnapshot, DEFAULT_IGNORED_NAMES};
use crate::repositories::loop_command_file::now_iso;
use crate::repositories::small_stores::{read_planning_thread, write_planning_thread};
use engine_state::{loop_state, mcp_access, project_repos, run_hints};
use pycompat::Re;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

static RECOMMENDED_SEPARATOR: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\s*\|\s*recommended\s*:\s*"));
pub const PLANNING_DOCUMENTS: [(&str, &str); 4] = [("project", ".specs/project/PROJECT.md"), ("roadmap", ".specs/project/ROADMAP.md"), ("flows", ".specs/project/FLOWS.md"), ("environment", ".specs/project/TEST-ENVIRONMENT.md")];
pub const PLANNING_FIRST_PROMPT: &str = "/orchestrator plan --headless";
pub const PLANNING_MODEL: &str = ""; // empty = the account's default model, as in the loop
pub const PLANNING_BUDGET_USD: f64 = 2.0;
pub const PLANNING_TIMEOUT_SECONDS: u64 = 600;
pub const PLANNING_TEXT_LIMIT: usize = 8000;
pub const PLANNING_REPLY_LIMIT: usize = 20000;
/// What the rules do not pre-approve is denied (nobody answers a prompt in -p).
pub const PLANNING_PERMISSION_MODE: &str = "dontAsk";
const PLANNING_RULES_PATH: &str = ".specs/project/RULES.md"; // not previewed, but the chat fills it from what the user says cannot be done
const PLANNING_SUGGESTED_REPOS_PATH: &str = ".specs/project/REPOS-SUGGESTED.json"; // repos the chat proposes from the catalog; the user confirms in the panel
const PLANNING_CREDENTIALS_NOTE: &str = "[Test credentials: the file .specs/project/loop/internal/credential-names.json lists every $NAME of TEST-ENVIRONMENT.md with its source and whether it is set, NEVER the values. Read it to know what exists. Never ask the user for a password, token or any value and never write one in a document or in chat: write only the $NAME in TEST-ENVIRONMENT.md and tell the user to fill the value in the panel's Test environment tab.]";
const PLANNING_MCP_NOTE: &str = "\n[MCP read access: besides the files you may use ONLY these read tools: {}. Use them to find sample data (real ids, routes, payload shapes) for TEST-ENVIRONMENT.md and FLOWS.md and to check how the data is built. Never write through them. Never copy a secret, token or personal data from a result into a document.]";
/// guard profile of the PreToolUse hook: Write/Edit only inside .specs/ (extra layer, deterministic)
pub const PLANNING_HOOK_PROFILE: &str = "planning";
pub const GUARD_IGNORED_PATHS: [&str; 5] = [".git", "node_modules", ".specs/project/loop/internal", ".specs/project/loop/json/MONITOR.json", ".specs/.trash"];
const GUARD_NOTE_FILES_SHOWN: usize = 8;
const ROLE_USER: &str = "user";
const ROLE_AI: &str = "ai";
const PUBLIC_MESSAGE_FIELDS: [&str; 4] = ["role", "text", "at", "cost_usd"];
const PROJECT_FILE_PARTS: [&str; 3] = [".specs", "project", "PROJECT.md"];

pub fn planning_editable_paths() -> Vec<String> {
    let mut paths: Vec<String> = PLANNING_DOCUMENTS.iter().map(|(_, p)| p.to_string()).collect();
    paths.push(PLANNING_RULES_PATH.into());
    paths.push(PLANNING_SUGGESTED_REPOS_PATH.into());
    paths
}

pub fn planning_allowed_tools() -> Vec<String> {
    let mut tools: Vec<String> = ["Read", "Grep", "Glob", "Skill"].iter().map(|s| s.to_string()).collect();
    tools.extend(planning_editable_paths().iter().map(|p| format!("Edit(/{p})")));
    tools
}

pub fn planning_disallowed_tools() -> Vec<String> {
    ["Bash", "NotebookEdit", "Agent", "WebSearch"].iter().map(|s| s.to_string()).collect()
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

/// `[{id, text, recommended}]` of the ROADMAP `## Open questions`, complete (the gate shows them cut); same parser as the gate. Item format:
/// `<question> | recommended: <answer>`; without the separator `recommended` is empty.
pub fn describe_open_questions(roadmap_text: &str) -> Vec<Value> {
    extract_open_questions(roadmap_text)
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let parts = RECOMMENDED_SEPARATOR.split(item);
            let question = parts[0].clone();
            let recommended = if parts.len() > 1 {
                // `split(maxsplit=1)`: everything after the first separator
                match RECOMMENDED_SEPARATOR.search(item) {
                    Some(found) => item[found.end..].to_string(),
                    None => String::new(),
                }
            } else {
                String::new()
            };
            json!({"id": format!("q{}", index + 1), "text": question.trim(), "recommended": recommended.trim()})
        })
        .collect()
}

type SnapshotSlot = Arc<Mutex<Option<WorkspaceSnapshot>>>;

pub struct PlanningService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn planning(&self) -> PlanningService<'_> {
        PlanningService { app: self }
    }
}

impl PlanningService<'_> {
    // ------------------------------------------------ read
    pub fn get_planning(&self, context: &ProjectContext) -> Value {
        let gates = self.app.progress().build_planning_gates(context);
        let thread = read_planning_thread(&context.work_dir());
        let job = self.get_running_planning_job(context);
        let approval = loop_state::roadmap_approval(&context.work_dir());
        let mut docs = serde_json::Map::new();
        for (key, relative) in PLANNING_DOCUMENTS {
            let path = relative.split('/').fold(PathBuf::from(context.work_dir()), |a, p| a.join(p));
            docs.insert(key.to_string(), self.app.specs.read_text(&path).map(Value::from).unwrap_or(Value::Null));
        }
        let messages: Vec<Value> = thread["messages"].as_array().into_iter().flatten().map(|m| Value::Object(PUBLIC_MESSAGE_FIELDS.iter().map(|f| (f.to_string(), m.get(*f).cloned().unwrap_or(Value::Null))).collect())).collect();
        let roadmap = docs.get("roadmap").and_then(Value::as_str).unwrap_or("").to_string();
        json!({
            "required": self.app.planning_requirement().is_planning_required(context),
            "thread": {"messages": messages, "busy": job.is_some(), "job_id": job.as_ref().map(|j| j["id"].clone()).unwrap_or(Value::Null), "session": t(thread.get("session_id"))},
            "docs": docs,
            "open_questions": describe_open_questions(&roadmap),
            "codebase": self.read_codebase_map(context),
            "gates": gates,
            "can_approve": are_gates_ok(&gates) && !self.app.progress().is_loop_running(context),
            "approved": approval["approved"],
        })
    }

    /// The brownfield map for the side panel: `{folder, commit, docs: {name: text}}` (read-only; empty docs = not mapped yet).
    pub fn read_codebase_map(&self, context: &ProjectContext) -> Value {
        let facts = super::project_progress::ProjectProgressService::read_map_facts(&context.work_dir(), context.repo_path());
        let (relative, root) = run_hints::find_map_folder(&[&context.work_dir(), context.repo_path()]);
        let folder = relative.split('/').fold(PathBuf::from(root), |a, p| a.join(p));
        let docs: serde_json::Map<String, Value> = facts["map_files"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|name| (name.to_string(), self.app.specs.read_text(&folder.join(name)).map(Value::from).unwrap_or(Value::Null))).collect();
        json!({"folder": facts["map_folder"], "commit": facts["map_commit"], "docs": docs})
    }

    /// `{state, reply?, error?, cost_usd}` of a planning reply job; another job type = 404.
    pub fn get_planning_job(&self, context: &ProjectContext, job_id: &str) -> Result<Value, AppError> {
        let job = self.app.jobs().get_job(context.repo_path(), job_id)?;
        if job["type"] != JOB_TYPE_PLANNING {
            return Err(AppError::new(Kind::JobNotFound, "job not found"));
        }
        let mut result = json!({"ok": true, "state": job["state"], "cost_usd": job.get("cost_usd").cloned().unwrap_or(Value::Null)});
        if job["state"] == JOB_STATE_OK {
            result["reply"] = json!(text_of(job.get("reply")));
        } else if job["state"] == JOB_STATE_ERROR {
            result["error"] = json!(Some(text_of(job.get("error"))).filter(|e| !e.is_empty()).unwrap_or_else(|| "claude finished with an error".into()));
            if t(job.get("code")) {
                result["code"] = job["code"].clone();
            }
        }
        Ok(result)
    }

    pub fn get_running_planning_job(&self, context: &ProjectContext) -> Option<Value> {
        self.app.jobs().get_running_job(context.repo_path()).filter(|j| j["type"] == JOB_TYPE_PLANNING)
    }

    // ------------------------------------------------ send
    /// Answers at once (CHT-01): only the cheap refusals and the persisting of the user message happen in the request; the gates, test-environment
    /// template, credential names file, safety snapshot, MCP access and the argv/prompt are built in the job thread (`prepare_reply`, the job's
    /// `before_start`).
    pub fn send_message(&self, context: &ProjectContext, text: &Value, on_prepare_failed: Option<Box<dyn FnOnce(&ClaudeJobError) + Send>>) -> Result<Value, AppError> {
        let message = Self::validate_text(text)?;
        let guard = self.app.planning_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.ensure_can_send(context)?;
        let work_dir = context.work_dir();
        let mut thread = read_planning_thread(&work_dir);
        let session = text_of(thread.get("session_id"));
        if let Some(Value::Array(messages)) = thread.get_mut("messages") {
            messages.push(json!({"role": ROLE_USER, "text": message, "at": now_iso(), "cost_usd": null}));
        }
        write_planning_thread(&work_dir, &mut thread);
        let snapshot: SnapshotSlot = Arc::new(Mutex::new(None));
        let (prepare_app, prepare_context, prepare_session, prepare_message, prepare_slot) = (self.app.shared(), context.clone(), session.clone(), message.clone(), snapshot.clone());
        let before_start: BeforeStart = Box::new(move |_update| prepare_app.planning().prepare_reply(&prepare_context, &prepare_session, &prepare_message, &prepare_slot).map(Some));
        let (undo_app, undo_context, undo_message) = (self.app.shared(), context.clone(), message.clone());
        let on_failed: Box<dyn FnOnce(&ClaudeJobError) + Send> = Box::new(move |failure| undo_app.planning().undo_unsent_message(&undo_context, &undo_message, on_prepare_failed, failure));
        let (finish_app, finish_context) = (self.app.shared(), context.clone());
        let mut command = ClaudeJobCommand::new(vec![], "");
        command.resumed_session = session.clone();
        command.timeout_seconds = Some(PLANNING_TIMEOUT_SECONDS);
        command.before_start = Some(before_start);
        command.on_prepare_failed = Some(on_failed);
        command.finalize = Some(Box::new(move |result| finish_app.planning().finish_reply(&finish_context, &snapshot, result)));
        let job = match self.app.jobs().start_job(context.repo_path(), JOB_TYPE_PLANNING, command) {
            Ok(job) => job,
            Err(failure) => {
                if let Some(Value::Array(messages)) = thread.get_mut("messages") {
                    messages.pop();
                }
                write_planning_thread(&work_dir, &mut thread);
                return Err(failure);
            }
        };
        drop(guard);
        self.app.log.info("planning_message_sent", &[("project", json!(context.project.id)), ("job", job["id"].clone()), ("resuming_session", json!(!session.is_empty()))]);
        Ok(json!({"ok": true, "job_id": job["id"]}))
    }

    /// The job ended in `before_start` (claude never started): the user message goes out of the thread, as when the old request refused.
    fn undo_unsent_message(&self, context: &ProjectContext, message: &str, on_prepare_failed: Option<Box<dyn FnOnce(&ClaudeJobError) + Send>>, failure: &ClaudeJobError) {
        {
            let _guard = self.app.planning_lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut thread = read_planning_thread(&context.work_dir());
            let is_ours = thread["messages"].as_array().and_then(|m| m.last()).is_some_and(|last| last["role"] == ROLE_USER && last["text"] == message);
            if is_ours {
                if let Some(Value::Array(messages)) = thread.get_mut("messages") {
                    messages.pop();
                }
                write_planning_thread(&context.work_dir(), &mut thread);
            }
        }
        if let Some(callback) = on_prepare_failed {
            callback(failure);
        }
    }

    /// Job thread, before claude: the heavy part of a send. Returns the final argv and stdin; a red prerequisite gate ends the job as an error with the
    /// code `planning_prerequisites` (what the synchronous 409 used to say).
    fn prepare_reply(&self, context: &ProjectContext, session: &str, message: &str, slot: &SnapshotSlot) -> Result<Prepared, ClaudeJobError> {
        let work_dir = context.work_dir();
        let gates = self.app.progress().build_planning_gates(context);
        let missing = Self::describe_missing_prerequisites(&gates);
        if !missing.is_empty() {
            return Err(ClaudeJobError::with_code(missing, PLANNING_PREREQUISITES_CODE));
        }
        self.app.progress().ensure_test_environment_template(context); // before the snapshot: it is the app's file, not the AI's
        let names_file = self.app.test_environment().write_credential_names_file(context);
        let ignored: Vec<&str> = GUARD_IGNORED_PATHS.to_vec();
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(WorkspaceFileGuard::default().take_snapshot(&work_dir, &ignored, &DEFAULT_IGNORED_NAMES));
        let (allowed, disallowed, mcp_config, mcp_tools) = self.build_mcp_read_access();
        let options = CommandOptions {
            model: PLANNING_MODEL.into(),
            budget_usd: Some(PLANNING_BUDGET_USD),
            allowed_tools: allowed,
            disallowed_tools: disallowed,
            no_mcp: true,
            resume_session: session.into(),
            permission_mode: PLANNING_PERMISSION_MODE.into(),
            hook_profile: PLANNING_HOOK_PROFILE.into(),
            mcp_config_path: mcp_config,
            ..Default::default()
        };
        let mut argv = self.app.claude.build_command(&options);
        argv.extend(self.build_reference_repo_arguments(context));
        // every turn: the AI sees which documents and gates are still red, the repositories the app refused and the readiness
        let facts = self.app.progress().read_project_facts(context);
        let subprojects = facts.get("subprojects").cloned();
        let loop_settings = read_roadmap_loop_settings(&work_dir);
        let mut status = describe_gates_for_ai(self.app, &gates, &work_dir, None, subprojects.as_ref(), Some(&loop_settings));
        if !names_file.is_empty() {
            status.push_str("\n\n");
            status.push_str(PLANNING_CREDENTIALS_NOTE);
        }
        if !mcp_tools.is_empty() {
            status.push_str(&PLANNING_MCP_NOTE.replace("{}", &mcp_tools.join(", ")));
        }
        let stdin_text = if session.is_empty() { format!("{PLANNING_FIRST_PROMPT}\n{status}\n\n{message}") } else { format!("{status}\n\n{message}") };
        Ok(Prepared { argv: Some(argv), stdin_text: Some(stdin_text) })
    }

    /// `(allowed, disallowed, strict mcp config path, read tools)`: the planning rules plus, for every server the user allowed (and that this machine
    /// defines), its tools, the writer tools denied by name and the one strict config file (no config = no MCP flags).
    fn build_mcp_read_access(&self) -> (Vec<String>, Vec<String>, String, Vec<String>) {
        let (mut allowed, mut disallowed) = (planning_allowed_tools(), planning_disallowed_tools());
        let access = mcp_access::build_access(&self.app.env, Some(&self.app.config.user_data_dir), None, "");
        let config_path = text_of(access.get("config_path"));
        if config_path.is_empty() {
            return (allowed, disallowed, String::new(), vec![]);
        }
        let list = |key: &str| -> Vec<String> { access[key].as_array().map(|a| a.iter().map(|i| text_of(Some(i))).collect()).unwrap_or_default() };
        allowed.extend(list("allowed_tools"));
        disallowed.extend(list("denied_tools"));
        (allowed, disallowed, config_path, list("allowed_tools"))
    }

    /// `--add-dir <path>` for every `reference` repo of REPOS.json that exists, so the chat can read them. The other catalog repos are read by absolute
    /// path (read only, outside the planning rules' write scope).
    fn build_reference_repo_arguments(&self, context: &ProjectContext) -> Vec<String> {
        let repos = project_repos::load_repos(context.repo_path());
        let mut arguments = Vec::new();
        for repo in project_repos::reference_repos(&repos) {
            if Path::new(&repo.path).is_dir() {
                arguments.extend(["--add-dir".to_string(), repo.path.clone()]);
            }
        }
        arguments
    }

    /// The cheap refusals of the request, in the order of the old ones: loop running, loop worktree, prerequisites (only from the gates computed within
    /// the last 10 s: a red one refuses here; with none, the job thread checks them), a job running.
    fn ensure_can_send(&self, context: &ProjectContext) -> Result<(), AppError> {
        if self.app.progress().is_loop_running(context) {
            return Err(AppError::loop_running("loop running: planning happens with the loop stopped").with_code(LOOP_RUNNING_CODE));
        }
        if pycompat::nt_normcase(&pycompat::nt_abspath(&context.work_dir())) != pycompat::nt_normcase(&pycompat::nt_abspath(context.repo_path())) {
            return Err(AppError::new(Kind::LoopWorktreeExists, format!("the loop worktree already exists ({}): edit the roadmap in Files", context.work_dir())));
        }
        if let Some(cached) = self.app.progress().read_cached_planning_gates(context, super::project_progress::PLANNING_GATES_CACHE_SECONDS) {
            let missing = Self::describe_missing_prerequisites(&cached);
            if !missing.is_empty() {
                return Err(AppError::planning_gates(missing, json!(cached), PLANNING_PREREQUISITES_CODE));
            }
        }
        if self.app.jobs().get_running_job(context.repo_path()).is_some() {
            return Err(AppError::new(Kind::JobInProgress, "a reply (or another claude job) is already in progress in this project: wait for it to finish").with_code(PLANNING_BUSY_CODE));
        }
        Ok(())
    }

    /// The refusal text of the red prerequisite gates ('' when every one is green).
    fn describe_missing_prerequisites(gates: &[Value]) -> String {
        if are_prerequisites_ok(gates) {
            return String::new();
        }
        let failing: Vec<String> = gates.iter().filter(|g| PREREQUISITE_GATES.contains(&g["id"].as_str().unwrap_or("")) && g["ok"] != true && g["optional"] != true).map(|g| text_of(g.get("label"))).collect();
        format!("prerequisites missing before planning: {}", failing.join(", "))
    }

    /// Runs in the job thread: undoes changes outside the documents, then records the reply and the session.
    fn finish_reply(&self, context: &ProjectContext, slot: &SnapshotSlot, result: Value) -> Value {
        let snapshot = slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(snapshot) = snapshot else { return result };
        let editable = planning_editable_paths();
        let allowed: Vec<&str> = editable.iter().map(String::as_str).collect();
        let report = WorkspaceFileGuard::default().revert_unexpected_changes(&snapshot, &allowed);
        let list = |key: &str| -> Vec<String> { report[key].as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default() };
        let (reverted, removed, unrestorable) = (list("reverted"), list("removed"), list("unrestorable"));
        let touched: Vec<String> = reverted.iter().chain(&removed).chain(&unrestorable).cloned().collect();
        if !touched.is_empty() {
            let head = |items: &[String]| json!(items.iter().take(50).collect::<Vec<_>>());
            self.app.log.warn("planning_guard_reverted", &[("project", json!(context.project.id)), ("reverted", head(&reverted)), ("removed", head(&removed)), ("unrestorable", head(&unrestorable))]);
        }
        self.restore_capabilities_region(context, &snapshot); // logged, no note: the rest of the document is the AI's and stays
        self.apply_reply_effects(context);
        let mut result = result;
        if engine_state::notify_truthy(result.get("ok")) {
            let mut reply = redact_secrets(&text_of(result.get("reply")), Some(PLANNING_REPLY_LIMIT));
            if !touched.is_empty() {
                reply.push_str(&format!("\n\n(The app undid changes outside the planning documents: {})", touched.iter().take(GUARD_NOTE_FILES_SHOWN).cloned().collect::<Vec<_>>().join(", ")));
            }
            result["reply"] = json!(reply);
        }
        let _guard = self.app.planning_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut thread = read_planning_thread(&context.work_dir());
        if engine_state::notify_truthy(result.get("ok")) {
            let cost = result.get("cost_usd").filter(|c| c.is_number()).cloned().unwrap_or(Value::Null);
            if let Some(Value::Array(messages)) = thread.get_mut("messages") {
                messages.push(json!({"role": ROLE_AI, "text": result["reply"], "at": now_iso(), "cost_usd": cost}));
            }
        }
        let session = if t(result.get("session_id")) { result["session_id"].clone() } else { thread.get("session_id").cloned().unwrap_or(Value::Null) };
        thread["session_id"] = session;
        write_planning_thread(&context.work_dir(), &mut thread);
        result
    }

    /// The block between the capabilities markers of PROJECT.md is the app's: when the reply changed it (or deleted a marker), the block of the
    /// snapshot is put back and the rest of the file (the AI's edits) stays. Returns true when it restored something.
    fn restore_capabilities_region(&self, context: &ProjectContext, snapshot: &WorkspaceSnapshot) -> bool {
        let Some(before_bytes) = snapshot.copy_of(&PROJECT_FILE_PARTS.join("/")) else { return false };
        let before = String::from_utf8_lossy(before_bytes).replace("\r\n", "\n");
        let Some(start) = before.find(START_MARKER) else { return false };
        let Some(relative_end) = before[start + START_MARKER.len()..].find(END_MARKER) else { return false };
        let end = start + START_MARKER.len() + relative_end;
        let before_block = before[start..end + END_MARKER.len()].to_string();
        let path = PROJECT_FILE_PARTS.iter().fold(PathBuf::from(context.work_dir()), |a, p| a.join(p));
        let Ok(bytes) = std::fs::read(&path) else { return false };
        let current = String::from_utf8_lossy(&bytes).into_owned();
        if let Some(current_start) = current.find(START_MARKER) {
            if let Some(current_relative_end) = current[current_start + START_MARKER.len()..].find(END_MARKER) {
                let current_end = current_start + START_MARKER.len() + current_relative_end;
                if current[current_start..current_end + END_MARKER.len()].replace("\r\n", "\n") == before_block {
                    return false;
                }
            }
        }
        let restored = replace_block(&current, &before_block);
        if let Err(failure) = std::fs::write(&path, restored.as_bytes()) {
            self.app.log.warn("planning_capabilities_region_not_restored", &[("project", json!(context.project.id)), ("message", json!(failure.to_string().chars().take(200).collect::<String>()))]);
            return false;
        }
        self.app.log.warn("planning_capabilities_region_restored", &[("project", json!(context.project.id))]);
        true
    }

    /// After a reply (even a failed one: the documents it wrote stay): apply REPOS-SUGGESTED.json, rewrite the capabilities block, schedule the
    /// readiness check. Each step is isolated: one failing never loses the reply.
    fn apply_reply_effects(&self, context: &ProjectContext) {
        let steps: [(&str, Box<dyn Fn() + '_>); 3] = [
            ("repos", Box::new(|| drop(self.app.planning_repos().apply_suggested_repos(context)))),
            ("capabilities", Box::new(|| {
                let _ = self.app.capabilities().write(context);
            })),
            ("readiness", Box::new(|| {
                let _ = self.app.readiness_schedule(context);
            })),
        ];
        for (name, step) in steps {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(step)).is_err() {
                self.app.log.warn("planning_reply_step_failed", &[("project", json!(context.project.id)), ("step", json!(name)), ("type", json!("panic"))]);
            }
        }
    }

    // ------------------------------------------------ reset
    /// New conversation (new session on the next message). The documents stay.
    pub fn reset_conversation(&self, context: &ProjectContext, confirm: &Value) -> Result<Value, AppError> {
        if confirm != &Value::Bool(true) {
            return Err(AppError::invalid_request("confirm must be true"));
        }
        {
            let _guard = self.app.planning_lock.lock().unwrap_or_else(|e| e.into_inner());
            if self.get_running_planning_job(context).is_some() {
                return Err(AppError::new(Kind::JobInProgress, "a reply is in progress: wait for it to finish").with_code(PLANNING_BUSY_CODE));
            }
            let mut thread = read_planning_thread(&context.work_dir());
            thread["messages"] = json!([]);
            thread["session_id"] = Value::Null;
            write_planning_thread(&context.work_dir(), &mut thread);
        }
        self.app.log.info("planning_conversation_reset", &[("project", json!(context.project.id))]);
        Ok(json!({"ok": true}))
    }

    // ------------------------------------------------ support
    fn validate_text(text: &Value) -> Result<String, AppError> {
        let Some(text) = text.as_str().filter(|t| !t.trim().is_empty()) else { return Err(AppError::invalid_request("empty text")) };
        if text.chars().count() > PLANNING_TEXT_LIMIT {
            return Err(AppError::invalid_request(format!("text is longer than {PLANNING_TEXT_LIMIT} characters")));
        }
        Ok(text.trim().to_string())
    }
}

fn t(value: Option<&Value>) -> bool {
    engine_state::notify_truthy(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_questions_split_on_the_recommended_separator() {
        let described = describe_open_questions("## Open questions\n- Which db? | recommended: sqlite | maybe\n- Auth?\n");
        assert_eq!(described, vec![json!({"id": "q1", "text": "Which db?", "recommended": "sqlite | maybe"}), json!({"id": "q2", "text": "Auth?", "recommended": ""})]);
        assert!(planning_allowed_tools().contains(&"Edit(/.specs/project/REPOS-SUGGESTED.json)".to_string()));
        assert_eq!(planning_disallowed_tools().len(), 4);
        assert_eq!(PlanningService::validate_text(&json!("  hi  ")).unwrap(), "hi");
        assert_eq!(PlanningService::validate_text(&json!("")).unwrap_err().message, "empty text");
        assert_eq!(PlanningService::describe_missing_prerequisites(&[json!({"id": "git", "label": "Git repository", "ok": false, "optional": false})]), "prerequisites missing before planning: Git repository");
        assert_eq!(PlanningService::describe_missing_prerequisites(&[json!({"id": "goal", "label": "x", "ok": false, "optional": false})]), "");
    }
}
