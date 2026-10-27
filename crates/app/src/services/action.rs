// SPDX-License-Identifier: MIT
//! Act mode: deterministic actions from a WHITELIST (code, not the AI), always with the user's `confirm:true`.
//!
//! `deliver` (the engine's delivery: push of the feature branch only + PR to the integration branch), `summary` (the engine's loop summary), `push`
//! (delivery with LOOP_PR_MODE=off per loop worktree) and `clean_worktrees` (with a verified backup in the output folder). The free-text router
//! (`claude -p` haiku, no tools and no MCP) only PROPOSES an action from the list; an invalid reply = `none`. Every run goes to
//! `internal/actions.log`. Actions apply only to the current version (the server ignores `turn`).

use super::chat::{busy_jobs_error, JOB_STATE_ERROR, JOB_STATE_OK, UNEXPECTED_FAILURE};
use super::loop_control::Reply;
use super::loop_worktree::{combine_output, extract_last_json, MAX_TARGETS};
use super::planning_gates::get_integration_branch;
use super::App;
use crate::domain::project::ProjectContext;
use crate::infrastructure::claude_client::{call_kind, redact_secrets, CommandOptions};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::sync::{Arc, LazyLock, Mutex};

pub const ACTION_DELIVER: &str = "deliver";
pub const ACTION_SUMMARY: &str = "summary";
pub const ACTION_PUSH: &str = "push";
pub const ACTION_CLEAN_WORKTREES: &str = "clean_worktrees";
pub const ACTION_NONE: &str = "none";
const ALLOWED_ACTIONS: [&str; 4] = [ACTION_DELIVER, ACTION_SUMMARY, ACTION_PUSH, ACTION_CLEAN_WORKTREES];
const ROUTER_ACTIONS: [&str; 5] = [ACTION_DELIVER, ACTION_SUMMARY, ACTION_PUSH, ACTION_CLEAN_WORKTREES, ACTION_NONE];
const WORKTREE_ACTIONS: [&str; 2] = [ACTION_PUSH, ACTION_CLEAN_WORKTREES];
const ACTION_TIMEOUT_SECONDS: u64 = 180;
const PREVIEW_TIMEOUT_SECONDS: u64 = 60;
const ROUTER_TIMEOUT_SECONDS: u64 = 60;
const OUTPUT_LIMIT: usize = 4000;
const PREVIEW_PUSH_LIMIT: usize = 500;
const REASON_LIMIT: usize = 300;
const LOG_SUMMARY_LIMIT: usize = 200;
const ROUTER_TEXT_LIMIT: usize = 2000;
const EXPLANATION_LIMIT: usize = 300;
const MAX_CONCURRENT_JOBS: usize = 3;
const ROUTER_MODEL: &str = "haiku";
const ROUTER_BUDGET_USD: f64 = 0.2;
const ROUTER_DISALLOWED_TOOLS: [&str; 7] = ["Read", "Grep", "Glob", "Bash", "Edit", "Write", "NotebookEdit"];
const LOOP_RUNNING_REASON: &str = "the loop is running";
const NO_LOOP_REASON: &str = "there is no loop in this worktree";
const NO_LOOP_WORKTREE_REASON: &str = "there is no loop worktree";
const NO_WORKTREE_REASON: &str = "no worktree";
const DEFAULT_EXPLANATION: &str = "I did not understand the request as one of the available actions.";
const ROUTER_PROMPT: &str = "You are a router. Pick ONE action for the user's request among: \"deliver\" (push the feature branch and open a PR to the integration branch), \"summary\" (generate the final loop summary), \"push\" (only push the feature branches, no PR), \"clean_worktrees\" (delete the loop worktrees) or \"none\" (anything else, a question or an instruction). Do not run anything and do not use tools. Reply ONLY with a JSON object: {\"action\":\"deliver|summary|push|clean_worktrees|none\",\"explanation\":\"1 to 2 sentences\",\"confidence\":\"high|low\",\"targets\":[\"branch or folder name cited by the user\"]} (targets only for push and clean_worktrees and only if the user cites them).\n\nUser request:\n";

static FENCES: LazyLock<Re> = LazyLock::new(|| Re::new(r"^```[a-zA-Z]*\s*|\s*```\z"));

fn failure(status: u16, message: impl Into<String>) -> Reply {
    (status, json!({"ok": false, "error": message.into()}))
}

fn default_proposal() -> Map<String, Value> {
    json!({"action": ACTION_NONE, "explanation": DEFAULT_EXPLANATION, "confidence": "low"}).as_object().cloned().unwrap_or_default()
}

fn action_title(action: &str) -> &'static str {
    match action {
        ACTION_SUMMARY => "Generate the loop summary",
        ACTION_PUSH => "Push the feature branches (no PR)",
        _ => "Clean loop worktrees (backup first)",
    }
}

/// Valid proposal `{action, explanation, confidence[, cited_targets]}`; an invalid reply or one outside the whitelist = `none`.
pub fn parse_router_proposal(text: &str) -> Map<String, Value> {
    let raw = FENCES.replace_lit(text.trim(), "");
    let mut data: Option<Value> = None;
    let mut start = raw.find('{');
    while data.is_none() {
        let Some(at) = start else { break };
        let parsed = raw.rfind('}').and_then(|end| raw.get(at..=end)).and_then(|slice| serde_json::from_str::<Value>(slice).ok());
        match parsed {
            Some(value) => data = Some(value),
            None => start = raw[at + 1..].find('{').map(|i| at + 1 + i),
        }
    }
    let Some(Value::Object(data)) = data else { return default_proposal() };
    let Some(action) = data.get("action").and_then(Value::as_str).filter(|a| ROUTER_ACTIONS.contains(a)) else { return default_proposal() };
    let explanation = redact_secrets(data.get("explanation").and_then(Value::as_str).unwrap_or(""), Some(EXPLANATION_LIMIT));
    let mut proposal = Map::new();
    proposal.insert("action".into(), json!(action));
    proposal.insert("explanation".into(), json!(if explanation.is_empty() { DEFAULT_EXPLANATION.to_string() } else { explanation }));
    proposal.insert("confidence".into(), json!(data.get("confidence").and_then(Value::as_str).filter(|c| *c == "high" || *c == "low").unwrap_or("low")));
    if WORKTREE_ACTIONS.contains(&action) {
        if let Some(Value::Array(targets)) = data.get("targets") {
            let cited: Vec<&Value> = targets.iter().filter(|t| t.is_string()).take(MAX_TARGETS).collect();
            proposal.insert("cited_targets".into(), json!(cited));
        }
    }
    proposal
}

pub struct ActionService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn actions(&self) -> ActionService<'_> {
        ActionService { app: self }
    }
}

impl ActionService<'_> {
    fn action_lock(&self, project_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.app.action_locks.lock().unwrap_or_else(|e| e.into_inner());
        locks.entry(project_id.to_string()).or_default().clone()
    }

    /// Reason not to act now (loop running, relaunched loop alive, no loop) or `None`.
    fn decide_block_reason(&self, context: &ProjectContext, summary: &Value) -> Option<&'static str> {
        if summary["running"] == true || summary["status"] == "running" || self.app.loop_control().has_live_relaunch(&context.project.id) {
            return Some(LOOP_RUNNING_REASON);
        }
        if summary["status"] == "no_loop" {
            return Some(NO_LOOP_REASON);
        }
        None
    }

    fn get_output_dir(&self) -> String {
        pycompat::nt_abspath(&self.app.config.output_dir)
    }

    fn record_action(&self, context: &ProjectContext, action: &str, ok: bool, summary: &str) {
        self.app.loop_commands.record_executed_action(&context.work_dir(), action, ok, &redact_secrets(summary, Some(LOG_SUMMARY_LIMIT)));
        self.app.log.info("action_executed", &[("project", json!(context.project.id)), ("action", json!(action)), ("ok", json!(ok))]);
    }

    fn context_integration_branch(&self, context: &ProjectContext) -> String {
        let text = std::fs::read(std::path::Path::new(&context.specs_dir()).join("project").join("ROADMAP.md"))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        get_integration_branch(self.app, &text)
    }

    // ------------------------------------------------ GET actions
    fn build_deliver_preview(&self, context: &ProjectContext, meta: &Map<String, Value>, block_reason: Option<&str>) -> Value {
        let base = self.context_integration_branch(context);
        let existing_pr = if crate::repositories::loop_read_formatting::text_or_empty(meta.get("pr_status")).starts_with("PR") {
            meta.get("pr").cloned().unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let mut deliver = json!({
            "id": ACTION_DELIVER, "title": format!("Deliver: push the branch + PR to {base}"), "available": false, "unavailable_reason": block_reason,
            "preview": {"branch": meta.get("branch"), "base": base, "existing_pr": existing_pr, "open_features": self.app.loop_commands.list_open_features(&context.work_dir()), "push": null},
        });
        if block_reason.is_some() {
            return deliver;
        }
        let work_dir = context.work_dir();
        if !std::path::Path::new(&work_dir).is_dir() {
            deliver["unavailable_reason"] = json!(NO_WORKTREE_REASON);
            return deliver;
        }
        let result = self.app.engine_scripts().run_engine_script_and_wait("delivery", &work_dir, &["--dry-run".to_string()], PREVIEW_TIMEOUT_SECONDS, &[]);
        let push = extract_last_json(&result.output).get("push").and_then(Value::as_str).map(String::from);
        if result.timed_out {
            deliver["unavailable_reason"] = json!(format!("the --dry-run took longer than {PREVIEW_TIMEOUT_SECONDS} s"));
        } else if let Some(push) = push {
            deliver["preview"]["push"] = json!(redact_secrets(&push, Some(PREVIEW_PUSH_LIMIT)));
            let available = push.starts_with("dry-run");
            deliver["available"] = json!(available);
            deliver["unavailable_reason"] = if available { Value::Null } else { json!(redact_secrets(&push, Some(PREVIEW_PUSH_LIMIT))) };
        } else {
            let text = if result.error.is_empty() { result.output.trim() } else { result.error.trim() };
            let last = text.lines().last().filter(|_| !text.is_empty()).unwrap_or("the --dry-run did not answer");
            deliver["unavailable_reason"] = json!(redact_secrets(last, Some(REASON_LIMIT)));
        }
        deliver
    }

    /// Preview of each action; the `deliver` one comes from `delivery --dry-run` (never pushes).
    pub fn list_available_actions(&self, context: &ProjectContext) -> Reply {
        let summary = self.app.loop_status().get_loop_summary(context);
        let block_reason = self.decide_block_reason(context, &summary);
        let meta = match summary.get("meta") {
            Some(Value::Object(meta)) => meta.clone(),
            _ => self.app.loop_commands.read_meta(&context.work_dir()),
        };
        let deliver = self.build_deliver_preview(context, &meta, block_reason);
        let generate_summary = json!({"id": ACTION_SUMMARY, "title": action_title(ACTION_SUMMARY), "available": block_reason.is_none(), "unavailable_reason": block_reason, "preview": {"destination": self.get_output_dir()}});
        let loop_running = summary["running"] == true || block_reason == Some(LOOP_RUNNING_REASON);
        let worktrees = if loop_running { vec![] } else { self.app.loop_worktrees().list_loop_worktrees(context) };
        let worktree_reason = if loop_running {
            Some(LOOP_RUNNING_REASON)
        } else if worktrees.is_empty() {
            Some(NO_LOOP_WORKTREE_REASON)
        } else {
            None
        };
        let push = json!({"id": ACTION_PUSH, "title": action_title(ACTION_PUSH), "available": worktree_reason.is_none(), "unavailable_reason": worktree_reason, "preview": {"worktrees": worktrees}});
        let clean = json!({"id": ACTION_CLEAN_WORKTREES, "title": action_title(ACTION_CLEAN_WORKTREES), "available": worktree_reason.is_none(), "unavailable_reason": worktree_reason, "preview": {"worktrees": worktrees, "backup_dir": self.get_output_dir()}});
        (200, json!({"actions": [deliver, generate_summary, push, clean]}))
    }

    // ------------------------------------------------ POST action/run
    pub fn run_confirmed_action(&self, context: &ProjectContext, body: &Value) -> Reply {
        let Some(action) = body.get("action").and_then(Value::as_str).filter(|a| ALLOWED_ACTIONS.contains(a)) else {
            return failure(400, format!("action outside the allowed list: {}", ALLOWED_ACTIONS.join(", ")));
            // nothing outside the whitelist, by any path
        };
        if body.get("confirm") != Some(&Value::Bool(true)) {
            return failure(400, "confirm must be exactly true");
        }
        let lock = self.action_lock(&context.project.id);
        let Ok(_guard) = lock.try_lock() else { return failure(409, "an action is already running") };
        if WORKTREE_ACTIONS.contains(&action) {
            return self.run_worktree_action(context, action, body);
        }
        self.run_delivery_or_summary_script(context, action)
    }

    fn run_delivery_or_summary_script(&self, context: &ProjectContext, action: &str) -> Reply {
        if let Some(reason) = self.decide_block_reason(context, &self.app.loop_status().get_loop_summary(context)) {
            return failure(409, reason);
        }
        let script_name = if action == ACTION_DELIVER { "delivery" } else { "loop_summary" };
        let args: Vec<String> = if action == ACTION_SUMMARY { vec!["--out".into(), self.get_output_dir()] } else { vec![] }; // always: the summary alone writes to the cwd
        self.app.log.info("action_launched", &[("project", json!(context.project.id)), ("action", json!(action))]);
        let result = self.app.engine_scripts().run_engine_script_and_wait(script_name, &context.work_dir(), &args, ACTION_TIMEOUT_SECONDS, &[]);
        let mut output = redact_secrets(&combine_output(&result.output, &result.error), Some(OUTPUT_LIMIT));
        let returned = if action == ACTION_DELIVER { extract_last_json(&result.output) } else { Map::new() };
        let text_of = |key: &str, limit: usize| returned.get(key).and_then(Value::as_str).map(|v| redact_secrets(v, Some(limit)));
        let push = text_of("push", PREVIEW_PUSH_LIMIT);
        let pr = returned.get("pr").and_then(Value::as_str).map(String::from);
        let pr_status = text_of("pr_status", REASON_LIMIT);
        let ok = !result.timed_out && result.code == Some(0);
        if result.timed_out {
            output = format!("{}timed out ({ACTION_TIMEOUT_SECONDS} s): the process was killed", if output.is_empty() { String::new() } else { format!("{output}\n") });
        }
        if action == ACTION_DELIVER && ok && push.as_deref().is_some_and(|p| p.starts_with("done")) {
            let _ = self.app.loop_commands.write_delivery_to_meta(&context.work_dir(), &json!(push), &json!(pr), &json!(pr_status));
        }
        let first_line = match (&push, action == ACTION_DELIVER) {
            (Some(push), true) if !push.is_empty() => push.clone(),
            _ => output.lines().next().map(String::from).unwrap_or_else(|| (if ok { "ok" } else { "failed" }).to_string()),
        };
        self.record_action(context, action, ok, &first_line);
        (200, json!({"ok": ok, "output": output, "pr": pr, "pr_status": pr_status, "push": push}))
    }

    /// push (feature branches only, no PR) and clean_worktrees (verified backup). Call with the action lock held.
    fn run_worktree_action(&self, context: &ProjectContext, action: &str, body: &Value) -> Reply {
        let force = match body.get("force") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return failure(400, "force must be a boolean"),
        };
        if self.decide_block_reason(context, &self.app.loop_status().get_loop_summary(context)) == Some(LOOP_RUNNING_REASON) {
            return failure(409, LOOP_RUNNING_REASON);
        }
        let worktrees = self.app.loop_worktrees().list_loop_worktrees(context);
        let targets = if body.get("targets").is_none() && action == ACTION_PUSH {
            worktrees
        } else {
            match self.app.loop_worktrees().select_targets_from_preview(body.get("targets"), &worktrees) {
                Ok(targets) => targets,
                Err(error) => return failure(400, error),
            }
        };
        if targets.is_empty() {
            return failure(400, "no target: give targets from the /api/actions preview");
        }
        self.app
            .log
            .info("action_launched", &[("project", json!(context.project.id)), ("action", json!(action)), ("targets", json!(targets.len())), ("force", json!(force))]);
        let service = self.app.loop_worktrees();
        let results: Vec<Value> = targets
            .iter()
            .map(|target| if action == ACTION_PUSH { service.push_worktree_branch(target) } else { service.clean_worktree_with_backup(context, target, force) })
            .collect();
        let ok = results.iter().all(|item| item["ok"] == true);
        let py = |value: &Value| match value {
            Value::Null => "None".to_string(),
            Value::String(s) => s.clone(),
            other => pycompat::json::py_json_dumps(other, false),
        };
        let summary = results
            .iter()
            .map(|item| {
                let shown = if action == ACTION_PUSH {
                    py(&item["push"])
                } else if engine_state::notify_truthy(item.get("error")) {
                    py(&item["error"])
                } else {
                    "deleted".to_string()
                };
                format!("{}: {shown}", py(&item["branch"]))
            })
            .collect::<Vec<_>>()
            .join("; ");
        self.record_action(context, action, ok, &summary);
        (200, json!({"ok": ok, "results": results, "output": redact_secrets(&summary, Some(OUTPUT_LIMIT))}))
    }

    // ------------------------------------------------ POST action/propose (router)
    /// Asynchronous like the chat: 202 `{job}`; the result is in `GET chat/job/<job>` (or the alias `action/job/<job>`) with `proposal`.
    pub fn propose_action_from_text(&self, context: &ProjectContext, body: &Value) -> Reply {
        let Some(text) = body.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) else { return failure(400, "empty text") };
        if text.chars().count() > ROUTER_TEXT_LIMIT {
            return failure(413, format!("text is longer than {ROUTER_TEXT_LIMIT} characters"));
        }
        let initial = json!({"proposal": null}).as_object().cloned().unwrap_or_default();
        let Ok(job_id) = self.app.chat_jobs.reserve_job(&context.project.id, None, MAX_CONCURRENT_JOBS, Some(&initial)) else { return failure(409, busy_jobs_error()) };
        self.app.log.info("router_job_started", &[("project", json!(context.project.id)), ("job", json!(job_id))]);
        let (app, job_context, job, stdin_text) = (self.app.shared(), context.clone(), job_id.clone(), format!("{ROUTER_PROMPT}{}", text.trim()));
        if std::thread::Builder::new().name(format!("router-{job_id}")).spawn(move || app.actions().run_router_job(&job_context, &job, &stdin_text)).is_err() {
            let result = json!({"state": JOB_STATE_ERROR, "reply": null, "cost_usd": null, "error": UNEXPECTED_FAILURE, "thread": null, "proposal": null});
            self.app.chat_jobs.finish_job(&job_id, result.as_object().unwrap());
            return failure(500, "the router thread cannot start");
        }
        (202, json!({"ok": true, "job": job_id}))
    }

    pub fn run_router_job(&self, context: &ProjectContext, job_id: &str, stdin_text: &str) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let error = |message: &str| json!({"state": JOB_STATE_ERROR, "reply": null, "cost_usd": null, "error": message, "thread": null, "proposal": null});
            let options = CommandOptions {
                model: ROUTER_MODEL.into(),
                budget_usd: Some(ROUTER_BUDGET_USD),
                allowed_tools: vec![String::new()], // `--allowedTools ""`: no tool at all
                disallowed_tools: ROUTER_DISALLOWED_TOOLS.iter().map(|t| t.to_string()).collect(),
                no_mcp: true,
                ..CommandOptions::default()
            };
            let argv = self.app.claude.build_command(&options);
            let work_dir = context.work_dir();
            let folder = if std::path::Path::new(&work_dir).is_dir() { work_dir } else { context.repo_path().to_string() };
            let returned = {
                let _kind = call_kind("action");
                self.app.claude.run_prompt(&argv, &folder, stdin_text, ROUTER_TIMEOUT_SECONDS, &[])
            };
            if returned["ok"] != true {
                return error(returned.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()).unwrap_or(UNEXPECTED_FAILURE));
            }
            let mut proposal = parse_router_proposal(returned.get("reply").and_then(Value::as_str).unwrap_or(""));
            if let Some(Value::Array(cited)) = proposal.remove("cited_targets") {
                // only targets that exist in the preview; the rest is dropped
                let cited: Vec<String> = cited.iter().filter_map(Value::as_str).map(String::from).collect();
                let worktrees = self.app.loop_worktrees().list_loop_worktrees(context);
                proposal.insert("targets".into(), json!(self.app.loop_worktrees().match_cited_targets(&cited, &worktrees)));
            }
            json!({"state": JOB_STATE_OK, "reply": proposal["explanation"], "cost_usd": returned.get("cost_usd"), "error": null, "thread": null, "proposal": proposal})
        }));
        let result = outcome.unwrap_or_else(
            |_| json!({"state": JOB_STATE_ERROR, "reply": null, "cost_usd": null, "error": "failed to run claude: panic", "thread": null, "proposal": null}),
        );
        self.app.chat_jobs.finish_job(job_id, result.as_object().unwrap());
        self.app.log.info("router_job_finished", &[("project", json!(context.project.id)), ("job", json!(job_id)), ("state", result["state"].clone())]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_replies_are_parsed_strictly() {
        let proposal = parse_router_proposal("```json\n{\"action\":\"push\",\"explanation\":\"push it\",\"confidence\":\"high\",\"targets\":[\"loop/x\", 5]}\n```");
        assert_eq!(Value::Object(proposal), json!({"action": "push", "explanation": "push it", "confidence": "high", "cited_targets": ["loop/x"]}));
        assert_eq!(parse_router_proposal("Sure! {\"action\":\"summary\"} thanks")["action"], "summary");
        assert_eq!(parse_router_proposal("{\"action\":\"rm -rf\"}"), default_proposal());
        assert_eq!(parse_router_proposal("no json"), default_proposal());
        assert_eq!(parse_router_proposal("{\"action\":\"none\",\"confidence\":\"maybe\"}")["confidence"], "low");
        assert!(!parse_router_proposal("{\"action\":\"deliver\",\"targets\":[\"x\"]}").contains_key("cited_targets"));
    }

    #[test]
    fn running_needs_the_whitelist_and_the_exact_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let service = app.actions();
        assert_eq!(service.run_confirmed_action(&context, &json!({"action": "rm", "confirm": true})).0, 400);
        assert_eq!(service.run_confirmed_action(&context, &json!({"action": "push", "confirm": "true"})), failure(400, "confirm must be exactly true"));
        assert_eq!(service.run_confirmed_action(&context, &json!({"action": "summary", "confirm": true})), failure(409, NO_LOOP_REASON));
        assert_eq!(service.propose_action_from_text(&context, &json!({"text": " "})), failure(400, "empty text"));
        let (status, listing) = service.list_available_actions(&context);
        assert_eq!(status, 200);
        assert_eq!(
            (listing["actions"][1]["unavailable_reason"].clone(), listing["actions"][2]["unavailable_reason"].clone()),
            (json!(NO_LOOP_REASON), json!(NO_LOOP_WORKTREE_REASON))
        );
    }
}
