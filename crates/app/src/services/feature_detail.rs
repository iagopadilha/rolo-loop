// SPDX-License-Identifier: MIT
//! Body of `GET /feature/{slug}`: one feature seen from the inside (header, step timeline, task graph with the agent spawned for each task, and
//! every agent call of the feature).
//!
//! Everything is derived from what the loop already wrote (ROADMAP, tasks.md, task status file, Claude Code transcripts); what cannot be derived
//! is `null` (never invented). The live match of the executors of the running turn is best effort and never raises.

use super::agent_history::{ORCHESTRATOR_ROLE, VERIFIER_NOTE_MARK};
use super::claude_accounts::{build_turn_auth, build_turn_claude};
use super::loop_panel::compute_current_turn;
use super::pipeline::{compute_waves, get_step_for_role, GATE_NOTE_PREFIX, STEP_COMMIT, STEP_EXEC, STEP_QUALITY, STEP_REVIEW, STEP_SPEC, STEP_VERIFY};
use super::App;
use crate::domain::project::ProjectContext;
use crate::repositories::loop_read_formatting::text_or_empty;
use engine_state::{loop_state, notify_truthy};
use serde_json::{json, Map, Value};

const STEP_SPEC_NAME: &str = "spec";
const STEP_REVIEW_NAME: &str = "review";
const STEP_BUILD_NAME: &str = "build";
const STEP_VERIFY_NAME: &str = "verify";
const STEP_QUALITY_NAME: &str = "quality";
const STEP_COMMIT_NAME: &str = "commit";
/// The same pipeline as the loop views (pipeline.rs STEPS).
const STEP_ORDER: [&str; 6] = [STEP_SPEC_NAME, STEP_REVIEW_NAME, STEP_BUILD_NAME, STEP_VERIFY_NAME, STEP_QUALITY_NAME, STEP_COMMIT_NAME];
const STEP_DONE: &str = "done";
const STEP_RUNNING: &str = "running";
const STEP_PENDING: &str = "pending";
const STEP_FAILED: &str = "failed";
const BLOCKED_STATUS: &str = "blocked";
const BUILDING_STATUS: &str = "building";
const AGENT_RUNNING_STATE: &str = "running";
const AGENT_FAILED_STATE: &str = "failed";
/// Cut off with its turn: no return while the task is still running.
const AGENT_INTERRUPTED_STATE: &str = "interrupted";
/// The last four: command and sign-in inherited from the turn.
const EXECUTOR_FIELDS: [&str; 12] =
    ["agent_id", "role", "turn", "state", "return_status", "duration_s", "tokens", "started_at", "finished_at", "claude", "auth_method", "auth_email"];

fn name_by_pipeline_step(step: usize) -> &'static str {
    match step {
        STEP_SPEC => STEP_SPEC_NAME,
        STEP_REVIEW => STEP_REVIEW_NAME,
        STEP_EXEC => STEP_BUILD_NAME,
        STEP_VERIFY => STEP_VERIFY_NAME,
        STEP_QUALITY => STEP_QUALITY_NAME,
        STEP_COMMIT => STEP_COMMIT_NAME,
        _ => STEP_BUILD_NAME,
    }
}

fn current_step_by_status(status: &str) -> Option<&'static str> {
    match status {
        "pending" | "speccing" => Some(STEP_SPEC_NAME),
        "specced" | "approved" => Some(STEP_REVIEW_NAME),
        "building" => Some(STEP_BUILD_NAME),
        "verified" => Some(STEP_COMMIT_NAME),
        _ => None,
    }
}

fn current_step_state_by_status(status: &str) -> &'static str {
    match status {
        "speccing" | "specced" | "building" => STEP_RUNNING,
        "approved" | "verified" => STEP_DONE,
        "blocked" => STEP_FAILED,
        _ => STEP_PENDING,
    }
}

/// A task still `running` whose newest executor is not running and returned nothing was cut off with its turn (loop stopped, timeout, limit): that
/// executor is `interrupted`, never `done`.
fn mark_interrupted_executor(task: &mut Value) {
    if task["status"] != "running" {
        return;
    }
    let Some(newest) = task["executors"].as_array_mut().and_then(|list| list.last_mut()) else { return };
    if newest["state"] != AGENT_RUNNING_STATE && !notify_truthy(newest.get("return_status")) {
        newest["state"] = json!(AGENT_INTERRUPTED_STATE);
    }
}

fn is_failed_return(call: &Value) -> bool {
    matches!(call["return_status"].as_str(), Some("FAIL") | Some("BLOCKED"))
}

/// Step the feature is in. A blocked feature is in the step of its last agent, else the one its note shows, else unknown (`None`).
fn find_current_step(status: &str, note: &str, last_call_step: Option<&'static str>) -> Option<&'static str> {
    if status == BUILDING_STATUS && note.to_lowercase().contains(VERIFIER_NOTE_MARK) {
        return Some(STEP_VERIFY_NAME);
    }
    if let Some(step) = current_step_by_status(status) {
        return Some(step);
    }
    if status != BLOCKED_STATUS {
        return None;
    }
    if last_call_step.is_some() {
        return last_call_step;
    }
    if note.starts_with(GATE_NOTE_PREFIX) {
        return Some(STEP_REVIEW_NAME);
    }
    if note.to_lowercase().contains(VERIFIER_NOTE_MARK) {
        return Some(STEP_VERIFY_NAME);
    }
    None
}

fn find_last_call_step(calls: &[Value]) -> Option<&'static str> {
    calls.iter().rev().find_map(|call| get_step_for_role(call["role"].as_str().unwrap_or("")).map(name_by_pipeline_step))
}

/// True when the newest executor call failed and none is running (the feature is stalled in the build step).
fn is_last_build_call_failed(calls: &[Value]) -> bool {
    let executors: Vec<&Value> = calls.iter().filter(|c| get_step_for_role(c["role"].as_str().unwrap_or("")) == Some(STEP_EXEC)).collect();
    if executors.is_empty() || executors.iter().any(|c| c["state"] == AGENT_RUNNING_STATE) {
        return false;
    }
    let last = executors[executors.len() - 1];
    last["state"] == AGENT_FAILED_STATE || is_failed_return(last)
}

/// `(current step, [{step, state}])` from the ROADMAP status and the agents found.
fn build_steps(row: &Value, calls: &[Value]) -> (Option<&'static str>, Vec<Value>) {
    let status = row["status"].as_str().unwrap_or("");
    let current = find_current_step(status, &text_or_empty(row.get("note")), find_last_call_step(calls));
    let mut state = current_step_state_by_status(status);
    if status == BUILDING_STATUS && current == Some(STEP_BUILD_NAME) && is_last_build_call_failed(calls) {
        state = STEP_FAILED;
    }
    let Some(current) = current else { return (None, STEP_ORDER.iter().map(|s| json!({"step": s, "state": STEP_PENDING})).collect()) };
    let position = STEP_ORDER.iter().position(|s| *s == current).unwrap_or(0);
    let steps = STEP_ORDER
        .iter()
        .enumerate()
        .map(|(index, step)| json!({"step": step, "state": if index < position { STEP_DONE } else if index == position { state } else { STEP_PENDING }}))
        .collect();
    (Some(current), steps)
}

/// One call in the contract shape; a running agent has no finish time, duration or token total yet.
fn build_call(agent: &Value, turn: &Value) -> Map<String, Value> {
    let running = agent["state"] == AGENT_RUNNING_STATE;
    let get = |key: &str| agent.get(key).cloned().unwrap_or(Value::Null);
    let unless_running = |key: &str| if running { Value::Null } else { get(key) };
    let mut call = Map::new();
    for (key, value) in [
        ("agent_id", get("id")),
        ("role", get("role")),
        ("turn", turn.clone()),
        ("task", get("task")),
        ("state", get("state")),
        ("return_status", get("return_status")),
        ("duration_s", unless_running("duration_s")),
        ("tokens", unless_running("tokens")),
        ("started_at", get("started_at")),
        ("finished_at", unless_running("finished_at")),
        ("summary", get("summary")),
    ] {
        call.insert(key.into(), value);
    }
    call
}

fn with_claude(mut call: Map<String, Value>, claude: &Value) -> Value {
    call.extend(claude.as_object().cloned().unwrap_or_default());
    Value::Object(call)
}

pub struct FeatureDetailService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn feature_detail(&self) -> FeatureDetailService<'_> {
        FeatureDetailService { app: self }
    }
}

impl FeatureDetailService<'_> {
    /// The contract of `GET /feature/{slug}`; `None` when the ROADMAP has no such feature.
    pub fn build_feature_detail(&self, context: &ProjectContext, slug: &str) -> Option<Value> {
        let work_dir = context.work_dir();
        let files = &self.app.loop_files;
        let roadmap_text = files.read_roadmap(&work_dir);
        let (_, feature_rows, _, _) = self.app.roadmap_view().build_roadmap_view(&work_dir, &roadmap_text, None);
        let row = feature_rows.iter().find(|candidate| candidate["id"] == slug)?;
        let summary = self.app.loop_status().get_loop_summary(context);
        let numbers = files.list_turn_numbers(&work_dir);
        let calls = self.list_feature_calls(&work_dir, slug, &numbers, &summary);
        let (current_step, steps) = build_steps(row, &calls);
        let deps: Vec<&Value> = row["deps"].as_array().into_iter().flatten().filter(|d| !d.as_str().unwrap_or("").starts_with(loop_state::DIR_PREFIX)).collect();
        Some(json!({
            "id": row["id"], "name": row["name"], "status": row["status"], "stage": row["stage"], "wave": compute_waves(&feature_rows).get(slug),
            "deps": deps, "paths": row["paths"], "scope": row["summary"].get("size").cloned().unwrap_or(Value::Null), "kind": row["kind"],
            "current_step": current_step, "steps": steps, "claude": self.read_last_turn_claude(&work_dir, &numbers),
            "tasks": self.build_tasks_block(&work_dir, slug, &calls), "agents": calls,
        }))
    }

    /// Every subagent call of the feature, oldest first: the finished turns plus, while the loop runs, the live session.
    fn list_feature_calls(&self, work_dir: &str, slug: &str, numbers: &[u64], summary: &Value) -> Vec<Value> {
        let mut calls = Vec::new();
        let mut session_ids: Vec<String> = Vec::new();
        for number in numbers {
            let record = self.app.loop_files.read_turn(work_dir, *number);
            let claude = build_turn_claude(&Value::Object(record.clone())); // every agent uses the command and sign-in of the turn that spawned it
            if let Some(session) = record.get("session_id").and_then(Value::as_str) {
                session_ids.push(session.to_string());
            }
            let (agents, _) = self.app.agent_history().list_turn_agents(work_dir, *number, &record);
            calls.extend(agents.iter().filter(|a| a["role"] != ORCHESTRATOR_ROLE && a["feature"] == slug).map(|a| with_claude(build_call(a, &json!(number)), &claude)));
        }
        if summary["running"] == true {
            let loop_json = summary["json"].as_object().cloned().unwrap_or_default();
            if let Some(live_number) = compute_current_turn(&loop_json, &summary["meta"], numbers, true) {
                let live_agents = self.app.agent_history().list_live_subagents(work_dir, live_number.as_u64().unwrap_or(0), &session_ids);
                let live_claude = build_turn_claude(&summary["meta"]); // the live turn has no file yet: the account of the last turn is the best known
                calls.extend(live_agents.iter().filter(|a| a["feature"] == slug).map(|a| with_claude(build_call(a, &live_number), &live_claude)));
            }
        }
        calls
    }

    /// The task block of `/state` plus, on every task, the executors spawned for it (all attempts, oldest first).
    fn build_tasks_block(&self, work_dir: &str, slug: &str, calls: &[Value]) -> Value {
        let mut block = crate::repositories::small_stores::read_feature_tasks(work_dir, slug);
        if let Some(tasks) = block["tasks"].as_array_mut() {
            for task in tasks.iter_mut() {
                let executors: Vec<Value> = calls
                    .iter()
                    .filter(|call| call["task"] == task["id"])
                    .map(|call| json!(EXECUTOR_FIELDS.iter().filter_map(|f| call.get(*f).map(|v| (f.to_string(), v.clone()))).collect::<Map<String, Value>>()))
                    .collect();
                task["executors"] = json!(executors);
                mark_interrupted_executor(task);
            }
        }
        block
    }

    /// `{command, account, auth_method, auth_email, auth_plan}` of the last turn; `null` without a turn.
    fn read_last_turn_claude(&self, work_dir: &str, numbers: &[u64]) -> Value {
        let Some(last) = numbers.last() else { return Value::Null };
        let record = Value::Object(self.app.loop_files.read_turn(work_dir, *last));
        let or_null = |key: &str| if notify_truthy(record.get(key)) { record[key].clone() } else { Value::Null };
        let mut out = Map::new();
        out.insert("command".into(), or_null("claude"));
        out.insert("account".into(), or_null("claude_account"));
        out.extend(build_turn_auth(&record).as_object().cloned().unwrap_or_default());
        Value::Object(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_follow_status_and_blocked_features_stay_where_they_stopped() {
        let row = |status: &str, note: &str| json!({"status": status, "note": note});
        let (current, steps) = build_steps(&row("building", ""), &[]);
        assert_eq!(current, Some("build"));
        assert_eq!((steps[0]["state"].clone(), steps[2]["state"].clone(), steps[3]["state"].clone()), (json!("done"), json!("running"), json!("pending")));
        assert_eq!(build_steps(&row("building", "verifying"), &[]).0, Some("verify"));
        assert_eq!(build_steps(&row("blocked", "gate refused"), &[]).0, Some("review"));
        assert_eq!(build_steps(&row("blocked", ""), &[]).0, None);
        let failed = vec![json!({"role": "executor-back", "state": "done", "return_status": "FAIL"})];
        assert_eq!(build_steps(&row("building", ""), &failed).1[2]["state"], "failed");
    }

    #[test]
    fn running_task_with_a_stopped_executor_is_interrupted() {
        let mut task = json!({"status": "running", "executors": [{"state": "done", "return_status": null}]});
        mark_interrupted_executor(&mut task);
        assert_eq!(task["executors"][0]["state"], "interrupted");
        let mut done = json!({"status": "done", "executors": [{"state": "done"}]});
        mark_interrupted_executor(&mut done);
        assert_eq!(done["executors"][0]["state"], "done");
    }

    #[test]
    fn detail_of_an_unknown_and_a_known_feature() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        std::fs::write(repo.join(".specs/project/ROADMAP.md"), "- [a] status=specced paths=src/a\n- [b] status=pending deps=a,dir:x\n").unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let service = app.feature_detail();
        assert!(service.build_feature_detail(&context, "zzz").is_none());
        let detail = service.build_feature_detail(&context, "b").unwrap();
        assert_eq!(
            (detail["wave"].clone(), detail["deps"].clone(), detail["current_step"].clone(), detail["claude"].clone()),
            (json!(2), json!(["a"]), json!("spec"), Value::Null)
        );
        assert_eq!(service.build_feature_detail(&context, "a").unwrap()["current_step"], "review");
    }
}
