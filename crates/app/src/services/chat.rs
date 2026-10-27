// SPDX-License-Identifier: MIT
//! Chat with the AI, ALWAYS read-only and independent of the loop (never touches LOCK/LOOP.json/ANSWERS.md).
//!
//! `claude -p` (sonnet, US$ 1, only Read/Grep/Glob, no MCP, text through stdin, environment without secrets), cwd = the project's work. Context:
//! `vNNN:orch` with a known session = `--resume <session> --fork-session`; a subagent or no session = a prompt with the role, initial prompt, last
//! actions, final return and the feature folder (by path); `general` = `.specs` files by path. An existing thread continues its own session
//! (`--resume`, no fork). Limits: 1 job per thread, 3 in the app.

use super::agent_history::HISTORY_ID;
use super::loop_control::Reply;
use super::App;
use crate::domain::project::ProjectContext;
use crate::infrastructure::claude_client::{call_kind, redact_secrets, CommandOptions};
use crate::repositories::claude_transcripts::{read_transcript_summary, read_turn_session_id};
use crate::repositories::loop_read_formatting::now_iso;
use crate::repositories::project_files::{generate_thread_id, THREAD_BUSY_REASON};
use engine_state::loop_state;
use pycompat::{nt_join, Re};
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

pub const CHAT_TIMEOUT_SECONDS: u64 = 180;
pub const MAX_CONCURRENT_JOBS: usize = 3;
const QUESTION_TEXT_LIMIT: usize = 8000;
pub const REPLY_LIMIT: usize = 20000;
const TITLE_LIMIT: usize = 60;
const CONTEXT_ACTION_TEXT_LIMIT: usize = 200;
const LAST_ACTIONS_COUNT: usize = 8;
const MAX_TURN_NUMBER: i64 = 999_999;
pub const READ_ONLY_PREFIX: &str = "You are in READ-ONLY mode. Answer questions; do not claim to have changed anything.";
const USER_QUESTION_HEADER: &str = "User question:\n";
pub const CHAT_MODEL: &str = "sonnet";
pub const CHAT_BUDGET_USD: f64 = 1.0;
pub const CHAT_ALLOWED_TOOLS: [&str; 3] = ["Read", "Grep", "Glob"];
pub const CHAT_DISALLOWED_TOOLS: [&str; 4] = ["Bash", "Edit", "Write", "NotebookEdit"];
const QUESTION_MODE: &str = "question";
pub const SCOPE_TYPE_AGENT: &str = "agent";
pub const SCOPE_TYPE_GENERAL: &str = "general";
pub const SCOPE_TYPE_ERROR: &str = "error";
pub const SCOPE_TYPE_QUARANTINE: &str = "quarantine";
pub const SCOPE_TYPE_PLAN: &str = "plan";
const SCOPE_TYPES: [&str; 5] = [SCOPE_TYPE_AGENT, SCOPE_TYPE_GENERAL, SCOPE_TYPE_ERROR, SCOPE_TYPE_QUARANTINE, SCOPE_TYPE_PLAN];
const PLAN_SCOPE_PREFIX: &str = "This chat only answers questions about the project plan: the roadmap, the specs, designs and tasks, the codebase maps (docs/codebase), the AI decisions and the loop progress. If the question is about something else, say you can only answer about those and stop. Read the files with Read, Grep and Glob; never claim to have changed anything.";
const ERROR_ID_LIMIT: usize = 200;
const ORCHESTRATOR_AGENT_ID: &str = "orch";
const ORCHESTRATOR_ROLE: &str = "orchestrator";
pub const JOB_STATE_OK: &str = "ok";
pub const JOB_STATE_ERROR: &str = "error";
pub const UNEXPECTED_FAILURE: &str = "unexpected failure";

static AGENT_ID: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z0-9:_.-]{1,120}\z"));
static FEATURE_SLUG: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,80}\z"));

pub fn is_number(value: Option<&Value>) -> bool {
    value.is_some_and(Value::is_number)
}

pub fn busy_jobs_error() -> String {
    format!("there are {MAX_CONCURRENT_JOBS} replies in progress; wait for one to finish")
}

fn failure(status: u16, message: impl Into<String>) -> Reply {
    (status, json!({"ok": false, "error": message.into()}))
}

/// Python `str.isprintable` (no control, format or separator characters other than the plain space).
fn is_printable(text: &str) -> bool {
    text.chars().all(|c| {
        c == ' '
            || !(c.is_control()
                || c.is_whitespace()
                || matches!(c, '\u{00ad}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{feff}'))
    })
}

/// Normalized scope `{"type","agent_id","turn"}` or `None` when invalid. `error` adds `error_id` and `quarantine` adds `feature`.
pub fn validate_chat_scope(scope: &Value) -> Option<Value> {
    let kind = scope.get("type")?.as_str().filter(|t| SCOPE_TYPES.contains(t))?;
    match kind {
        SCOPE_TYPE_ERROR => {
            let error_id = scope.get("error_id").and_then(Value::as_str).filter(|e| !e.is_empty() && e.chars().count() <= ERROR_ID_LIMIT && is_printable(e))?;
            Some(json!({"type": kind, "agent_id": null, "turn": null, "error_id": error_id}))
        }
        SCOPE_TYPE_QUARANTINE => {
            let feature = scope.get("feature").and_then(Value::as_str).filter(|f| FEATURE_SLUG.full_match(f))?;
            Some(json!({"type": kind, "agent_id": null, "turn": null, "feature": feature}))
        }
        SCOPE_TYPE_PLAN => Some(json!({"type": kind, "agent_id": null, "turn": null})),
        _ => {
            let (agent_id, turn) = (scope.get("agent_id").cloned().unwrap_or(Value::Null), scope.get("turn").cloned().unwrap_or(Value::Null));
            if !agent_id.is_null() && !agent_id.as_str().is_some_and(|a| AGENT_ID.full_match(a)) {
                return None;
            }
            if !turn.is_null() && !turn.as_i64().is_some_and(|t| (0..=MAX_TURN_NUMBER).contains(&t)) {
                return None;
            }
            Some(json!({"type": kind, "agent_id": agent_id, "turn": turn}))
        }
    }
}

pub struct ChatService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn chat(&self) -> ChatService<'_> {
        ChatService { app: self }
    }
}

impl ChatService<'_> {
    /// IBusyProvider: a chat (or router) reply in progress keeps the app on.
    pub fn is_busy(&self) -> bool {
        self.app.chat_jobs.count_running_jobs() > 0
    }

    // ------------------------------------------------ context
    pub fn describe_feature_dir(work_dir: &str, slug: &str) -> String {
        format!("{} (read with Read: spec.md and the other files)", loop_state::feature_dir(work_dir, slug))
    }

    pub fn build_general_context(work_dir: &str) -> String {
        let paths = [
            nt_join(&nt_join(&nt_join(work_dir, ".specs"), "project"), "ROADMAP.md"),
            loop_state::state_file(work_dir, "STATE.md").display().to_string(),
            loop_state::state_file(work_dir, "VALIDATE.md").display().to_string(),
            loop_state::state_file(work_dir, "LOOP.json").display().to_string(),
        ];
        format!("General loop context. Read what you need with Read:\n{}", paths.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n"))
    }

    /// Context of the `plan` scope: the limiting prefix plus where the plan files live (the work folder = the worktree when it exists).
    pub fn build_plan_context(work_dir: &str) -> String {
        let specs = nt_join(work_dir, ".specs");
        let folders = [nt_join(&specs, "project"), nt_join(&specs, "features"), nt_join(&nt_join(work_dir, "docs"), "codebase")];
        format!("{PLAN_SCOPE_PREFIX}\nPlan files, read what you need with Read/Glob:\n{}", folders.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n"))
    }

    /// `(session_id for resume+fork or None, context text)` of the `agent` scope.
    pub fn build_agent_context(&self, work_dir: &str, agent_id: &str) -> (Option<String>, String) {
        if let Some(found) = HISTORY_ID.match_start(agent_id) {
            return self.build_history_agent_context(work_dir, found[1].parse().unwrap_or(0), &found[2]);
        }
        let slug = agent_id.split(':').next().unwrap_or("");
        if !agent_id.is_empty() && agent_id != ORCHESTRATOR_AGENT_ID && FEATURE_SLUG.full_match(slug) {
            return (
                None,
                format!("Agent {agent_id} (feature {slug}). Feature files: {}\n\n{}", Self::describe_feature_dir(work_dir, slug), Self::build_general_context(work_dir)),
            );
        }
        (None, Self::build_general_context(work_dir))
    }

    fn build_history_agent_context(&self, work_dir: &str, turn_number: u64, agent_name: &str) -> (Option<String>, String) {
        let session_id = read_turn_session_id(work_dir, turn_number);
        let (main_path, subagents) = self.app.transcripts.locate_session_files(work_dir, session_id.as_deref().unwrap_or(""));
        let (summary, role): (Option<Value>, Option<String>) = if agent_name == ORCHESTRATOR_AGENT_ID {
            if let Some(session) = session_id {
                return (Some(session), String::new());
            }
            (main_path.and_then(|p| read_transcript_summary(&p)), Some(ORCHESTRATOR_ROLE.into()))
        } else {
            let files = subagents.iter().find(|item| item.0 == agent_name);
            (files.and_then(|f| read_transcript_summary(&f.1)), files.and_then(|f| self.app.transcripts.read_agent_type(&f.2)))
        };
        let Some(summary) = summary else { return (None, Self::build_general_context(work_dir)) };
        let actions: Vec<String> = summary["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| format!("- {} {}", a["type"].as_str().unwrap_or(""), a["text"].as_str().unwrap_or("").chars().take(CONTEXT_ACTION_TEXT_LIMIT).collect::<String>()))
            .collect();
        let actions = &actions[actions.len().saturating_sub(LAST_ACTIONS_COUNT)..];
        let text_or = |key: &str, fallback: &str| summary.get(key).and_then(Value::as_str).filter(|t| !t.is_empty()).unwrap_or(fallback).to_string();
        let feature = summary.get("feature").and_then(Value::as_str).filter(|f| !f.is_empty());
        let mut parts = vec![
            format!("Agent {} of turn {turn_number} (feature: {}).", role.as_deref().unwrap_or("?"), feature.unwrap_or("?")),
            format!("Initial agent prompt:\n{}", text_or("initial_prompt", "(unavailable)")),
            format!("Last actions:\n{}", if actions.is_empty() { "(none)".to_string() } else { actions.join("\n") }),
            format!("Final return:\n{}", text_or("final_return", "(no return yet)")),
        ];
        if let Some(feature) = feature {
            parts.push(format!("Feature files: {}", Self::describe_feature_dir(work_dir, feature)));
        }
        (None, redact_secrets(&parts.join("\n\n"), None))
    }

    /// Server-built context of an `error` or `quarantine` scope; `None` when the item does not exist.
    fn build_scope_context(&self, context: &ProjectContext, scope: &Value) -> Option<String> {
        if scope["type"] == SCOPE_TYPE_ERROR {
            return self.app.chat_scope().build_error_context(context, scope["error_id"].as_str().unwrap_or(""));
        }
        self.app.quarantine().build_chat_context(context, scope["feature"].as_str().unwrap_or(""))
    }

    // ------------------------------------------------ send
    pub fn send_question(&self, context: &ProjectContext, body: &Value) -> Reply {
        let Some(text) = body.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) else { return failure(400, "empty text") };
        if text.chars().count() > QUESTION_TEXT_LIMIT {
            return failure(413, format!("text is longer than {QUESTION_TEXT_LIMIT} characters"));
        }
        let work_dir = context.work_dir();
        let requested_thread = body.get("thread").filter(|t| !t.is_null());
        let (job_id, thread_id, session, context_text, fork);
        {
            let _guard = self.app.chat_send_lock.lock().unwrap_or_else(|e| e.into_inner());
            let thread_text = requested_thread.map(|t| t.as_str().unwrap_or("").to_string());
            if thread_text.as_deref().is_some_and(|t| self.app.chat_jobs.has_running_job_in_thread(t)) {
                return failure(409, "this thread already has a reply in progress");
            }
            if self.app.chat_jobs.count_running_jobs() >= MAX_CONCURRENT_JOBS {
                return failure(409, busy_jobs_error());
            }
            let mut thread: Map<String, Value>;
            let resumed;
            if let Some(id) = &thread_text {
                let Some(found) = self.app.chat_threads.read_thread(&work_dir, id) else { return failure(404, "thread not found") };
                resumed = found.get("session_id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
                context_text = if resumed.is_some() { String::new() } else { found.get("context").and_then(Value::as_str).unwrap_or("").to_string() };
                session = resumed.clone();
                fork = false;
                thread_id = id.clone();
                thread = found;
            } else {
                let default_scope = json!({"type": SCOPE_TYPE_GENERAL});
                let scope_input = body.get("scope").filter(|s| engine_state::notify_truthy(Some(s))).unwrap_or(&default_scope);
                let Some(scope) = validate_chat_scope(scope_input) else { return failure(400, "invalid scope") };
                let kind = scope["type"].as_str().unwrap_or("");
                let (found_session, found_context): (Option<String>, String) = if kind == SCOPE_TYPE_ERROR || kind == SCOPE_TYPE_QUARANTINE {
                    match self.build_scope_context(context, &scope) {
                        Some(text) => (None, text),
                        None => return failure(404, format!("{} not found", if kind == SCOPE_TYPE_ERROR { "error" } else { "quarantined feature" })),
                    }
                } else if kind == SCOPE_TYPE_PLAN {
                    (None, Self::build_plan_context(&work_dir))
                } else if kind == SCOPE_TYPE_AGENT && engine_state::notify_truthy(scope.get("agent_id")) {
                    self.build_agent_context(&work_dir, scope["agent_id"].as_str().unwrap_or(""))
                } else {
                    (None, Self::build_general_context(&work_dir))
                };
                fork = found_session.is_some();
                session = found_session;
                context_text = found_context;
                thread_id = generate_thread_id();
                let title: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(TITLE_LIMIT).collect();
                thread = json!({"thread": thread_id, "scope": scope, "title": title, "session_id": null, "context": context_text, "created_at": now_iso(), "messages": [], "cost_usd": 0})
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
            }
            job_id = match self.app.chat_jobs.reserve_job(&context.project.id, Some(&thread_id), MAX_CONCURRENT_JOBS, None) {
                Ok(id) => id,
                Err(reason) => {
                    return failure(409, if reason == THREAD_BUSY_REASON { "this thread already has a reply in progress".to_string() } else { busy_jobs_error() })
                }
            };
            if let Some(Value::Array(messages)) = thread.get_mut("messages") {
                messages.push(json!({"role": "user", "text": text.trim(), "t": now_iso(), "cost_usd": null, "mode": QUESTION_MODE}));
            }
            self.app.chat_threads.write_thread(&work_dir, &mut thread);
        }
        let options = CommandOptions {
            model: CHAT_MODEL.into(),
            budget_usd: Some(CHAT_BUDGET_USD),
            allowed_tools: CHAT_ALLOWED_TOOLS.iter().map(|t| t.to_string()).collect(),
            disallowed_tools: CHAT_DISALLOWED_TOOLS.iter().map(|t| t.to_string()).collect(),
            no_mcp: true,
            resume_session: session.clone().unwrap_or_default(),
            fork_session: fork,
            ..CommandOptions::default()
        };
        let argv = self.app.claude.build_command(&options);
        let stdin_text =
            format!("{READ_ONLY_PREFIX}\n\n{}{USER_QUESTION_HEADER}{}", if context_text.is_empty() { String::new() } else { format!("{context_text}\n\n") }, text.trim());
        let run_dir = if std::path::Path::new(&work_dir).is_dir() { work_dir.clone() } else { context.repo_path().to_string() };
        self.app.log.info(
            "chat_job_started",
            &[("project", json!(context.project.id)), ("job", json!(job_id)), ("thread", json!(thread_id)), ("resuming_session", json!(session.is_some()))],
        );
        let (app, job_context, job, thread_for_job) = (self.app.shared(), context.clone(), job_id.clone(), thread_id.clone());
        if std::thread::Builder::new()
            .name(format!("chat-{job_id}"))
            .spawn(move || app.chat().run_chat_job(&job_context, &job, &thread_for_job, &argv, &stdin_text, &run_dir))
            .is_err()
        {
            self.app.chat_jobs.finish_job(
                &job_id,
                json!({"state": JOB_STATE_ERROR, "reply": null, "cost_usd": null, "error": UNEXPECTED_FAILURE, "thread": thread_id}).as_object().unwrap(),
            );
            return failure(500, "the reply thread cannot start");
        }
        (202, json!({"ok": true, "job": job_id, "thread": thread_id}))
    }

    pub fn run_chat_job(&self, context: &ProjectContext, job_id: &str, thread_id: &str, argv: &[String], stdin_text: &str, run_dir: &str) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let returned = {
                let _kind = call_kind("chat");
                self.app.claude.run_prompt(argv, run_dir, stdin_text, CHAT_TIMEOUT_SECONDS, &[])
            };
            if returned["ok"] == true {
                let session_id = returned.get("session_id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
                let cost = if is_number(returned.get("cost_usd")) { returned["cost_usd"].clone() } else { Value::Null };
                let reply = redact_secrets(returned.get("reply").and_then(Value::as_str).unwrap_or(""), Some(REPLY_LIMIT));
                let result = json!({"state": JOB_STATE_OK, "reply": reply, "cost_usd": cost, "error": null, "thread": thread_id});
                self.write_reply_to_thread(&context.work_dir(), thread_id, &result, session_id.as_deref());
                result
            } else {
                let error = returned.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()).unwrap_or(UNEXPECTED_FAILURE);
                json!({"state": JOB_STATE_ERROR, "reply": null, "cost_usd": null, "error": error, "thread": thread_id})
            }
        }));
        let result =
            outcome.unwrap_or_else(|_| json!({"state": JOB_STATE_ERROR, "reply": null, "cost_usd": null, "error": "failed to run claude: panic", "thread": thread_id}));
        self.app.chat_jobs.finish_job(job_id, result.as_object().unwrap());
        self.app.log.info("chat_job_finished", &[("project", json!(context.project.id)), ("job", json!(job_id)), ("state", result["state"].clone())]);
    }

    fn write_reply_to_thread(&self, work_dir: &str, thread_id: &str, result: &Value, session_id: Option<&str>) {
        let _guard = self.app.chat_send_lock.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mut thread) = self.app.chat_threads.read_thread(work_dir, thread_id) else { return };
        if let Some(Value::Array(messages)) = thread.get_mut("messages") {
            messages.push(json!({"role": "ai", "text": result["reply"], "t": now_iso(), "cost_usd": result["cost_usd"], "mode": QUESTION_MODE}));
        }
        let previous = thread.get("cost_usd").filter(|c| c.is_number()).and_then(Value::as_f64).unwrap_or(0.0);
        let added = result["cost_usd"].as_f64().unwrap_or(0.0);
        thread.insert("cost_usd".into(), json!(((previous + added) * 1e6).round_ties_even() / 1e6));
        if let Some(session) = session_id {
            thread.insert("session_id".into(), json!(session));
        }
        self.app.chat_threads.write_thread(work_dir, &mut thread);
    }

    // ------------------------------------------------ read
    /// Chat or router job. A job of another project = 404 (each panel only sees its own).
    pub fn get_project_job(&self, context: &ProjectContext, job_id: &str) -> Reply {
        let Some(mut job) = self.app.chat_jobs.get_job(job_id) else { return failure(404, "job not found") };
        if job.remove("project_id").as_ref().and_then(Value::as_str) != Some(context.project.id.as_str()) {
            return failure(404, "job not found");
        }
        (200, Value::Object(job))
    }

    pub fn list_project_threads(&self, context: &ProjectContext, agent_id: Option<&str>, turn: Option<i64>) -> Reply {
        let mut rows: Vec<Value> = Vec::new();
        for thread in self.app.chat_threads.list_threads(&context.work_dir()) {
            let scope = thread.get("scope").filter(|s| s.is_object()).cloned().unwrap_or_else(|| json!({}));
            if agent_id.is_some() && scope.get("agent_id").and_then(Value::as_str) != agent_id {
                continue;
            }
            if turn.is_some() && scope.get("turn").and_then(Value::as_i64) != turn {
                continue;
            }
            rows.push(json!({
                "thread": thread["thread"], "title": thread.get("title").filter(|t| engine_state::notify_truthy(Some(t))).cloned().unwrap_or(json!("")), "scope": scope,
                "updated_at": thread.get("updated_at"), "message_count": thread["messages"].as_array().map_or(0, Vec::len),
                "cost_usd": thread.get("cost_usd").filter(|c| c.is_number()).cloned().unwrap_or(json!(0)),
            }));
        }
        let key = |row: &Value| crate::repositories::loop_read_formatting::text_or_empty(row.get("updated_at"));
        rows.sort_by(|a, b| key(b).cmp(&key(a)));
        (200, json!({"threads": rows}))
    }

    pub fn get_project_thread(&self, context: &ProjectContext, thread_id: &str) -> Reply {
        match self.app.chat_threads.read_thread(&context.work_dir(), thread_id) {
            Some(thread) => (200, json!({"thread": thread_id, "scope": thread.get("scope"), "messages": thread["messages"]})),
            None => failure(404, "thread not found"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_are_validated_and_normalized() {
        assert_eq!(validate_chat_scope(&json!({"type": "general"})), Some(json!({"type": "general", "agent_id": null, "turn": null})));
        assert_eq!(validate_chat_scope(&json!({"type": "agent", "agent_id": "v001:orch", "turn": 1})).unwrap()["agent_id"], "v001:orch");
        assert!(validate_chat_scope(&json!({"type": "agent", "agent_id": "bad id"})).is_none());
        assert!(validate_chat_scope(&json!({"type": "agent", "turn": -1})).is_none());
        assert!(validate_chat_scope(&json!({"type": "agent", "turn": true})).is_none());
        assert!(validate_chat_scope(&json!({"type": "nope"})).is_none());
        assert!(validate_chat_scope(&json!({"type": "error", "error_id": ""})).is_none());
        assert_eq!(validate_chat_scope(&json!({"type": "quarantine", "feature": "a-1"})).unwrap()["feature"], "a-1");
        assert!(validate_chat_scope(&json!({"type": "quarantine", "feature": "../x"})).is_none());
        assert_eq!(validate_chat_scope(&json!({"type": "plan", "agent_id": "x"})).unwrap()["agent_id"], Value::Null);
    }

    #[test]
    fn contexts_name_the_files_to_read() {
        let general = ChatService::build_general_context("C:\\w");
        assert!(general.starts_with("General loop context. Read what you need with Read:\n- C:\\w\\.specs\\project\\ROADMAP.md"));
        assert!(ChatService::build_plan_context("C:\\w").contains("- C:\\w\\docs\\codebase"));
    }

    #[test]
    fn question_checks_run_before_anything_is_started() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let chat = app.chat();
        assert_eq!(chat.send_question(&context, &json!({"text": "  "})), failure(400, "empty text"));
        assert_eq!(chat.send_question(&context, &json!({"text": "x".repeat(9000)})).0, 413);
        assert_eq!(chat.send_question(&context, &json!({"text": "hi", "scope": {"type": "bad"}})), failure(400, "invalid scope"));
        assert_eq!(chat.send_question(&context, &json!({"text": "hi", "thread": "t-000000000000"})), failure(404, "thread not found"));
        assert_eq!(chat.send_question(&context, &json!({"text": "hi", "scope": {"type": "error", "error_id": "nope"}})), failure(404, "error not found"));
        assert_eq!(chat.get_project_job(&context, "j-none"), failure(404, "job not found"));
        assert_eq!(chat.list_project_threads(&context, None, None), (200, json!({"threads": []})));
        assert_eq!(chat.get_project_thread(&context, "t-000000000000"), failure(404, "thread not found"));
    }
}
