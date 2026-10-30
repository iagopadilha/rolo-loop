// SPDX-License-Identifier: MIT
//! Loop agents: history per turn (Claude Code transcripts), full log of one agent and the agents of `/state` (real ones from the last turn when
//! there is a transcript and the loop is stopped; otherwise inferred from the feature statuses).
//!
//! Never invents a number: tokens/cost per agent only with a transcript or `*.events.jsonl`; the rest is `null`.

use super::claude_accounts::build_turn_claude;
use super::turns::get_turn_status;
use super::App;
use crate::domain::project::ProjectContext;
use crate::repositories::loop_read_formatting::{format_minutes_clock, ms_to_seconds, number_value, redact_value, sum_usage_tokens, text_or_empty};
use engine_state::notify_truthy;
use pycompat::{splitlines, Re};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::LazyLock;

pub static HISTORY_ID: LazyLock<Re> = LazyLock::new(|| Re::new(r"^v(\d{1,6}):([A-Za-z0-9_-]{1,80})\z"));
pub const ORCHESTRATOR_ID: &str = "orch";
pub const ORCHESTRATOR_ROLE: &str = "orchestrator";
const CLOSED_STATUSES: [&str; 2] = ["verified", "blocked"];
pub const VERIFIER_ROLE: &str = "verifier";
pub const VERIFIER_NOTE_MARK: &str = "verif";
pub const WORKING_STATUSES: [&str; 2] = ["speccing", "building"];
const LOOP_DONE_STATUS: &str = "done";
const LOG_TYPES: [&str; 5] = ["sys", "tool", "ai", "user", "guard"];
const SUMMARY_LIMIT: usize = 200;
const ORCHESTRATOR_STATE_SUMMARY_LIMIT: usize = 140;
const ACTION_LIMIT: usize = 160;
const LOG_TEXT_LIMIT: usize = 2000;
const ORCHESTRATOR_RESULT_LIMIT: usize = 4000;
const LOG_ITEMS_IN_STATE: usize = 8;
const GUARD_RULE_LIMIT: usize = 60;

pub fn history_id(number: u64, name: &str) -> String {
    format!("v{number:03}:{name}")
}

pub fn build_event_log_item(event: &Map<String, Value>) -> Value {
    let kind = event.get("type").and_then(Value::as_str).filter(|t| LOG_TYPES.contains(t)).unwrap_or("sys");
    let mut item = Map::new();
    item.insert("type".into(), json!(kind));
    item.insert("text".into(), json!(redact_value(event.get("text"), Some(LOG_TEXT_LIMIT))));
    item.insert("result".into(), json!(redact_value(event.get("result"), Some(LOG_TEXT_LIMIT))));
    item.insert("t".into(), json!(text_or_empty(event.get("t"))));
    if kind == "guard" {
        // a live block by the PreToolUse guard keeps its rule and reason
        item.insert("rule".into(), json!(redact_value(event.get("rule"), Some(GUARD_RULE_LIMIT))));
        item.insert("reason".into(), json!(redact_value(event.get("reason"), Some(LOG_TEXT_LIMIT))));
    }
    Value::Object(item)
}

/// `event.get("agent") or event.get("id")` as the grouping key.
fn event_agent(event: &Map<String, Value>) -> String {
    let agent = event.get("agent").filter(|v| notify_truthy(Some(v)));
    text_or_empty(agent.or_else(|| event.get("id")))
}

fn is_turn_failed(record: &Map<String, Value>) -> bool {
    let exit_failed = record.get("exit_code").is_some_and(|c| !c.is_null() && c.as_f64() != Some(0.0));
    notify_truthy(record.get("is_error")) || notify_truthy(record.get("timeout")) || exit_failed
}

/// From the transcript; without it, the turn result as model speech; else `null`.
fn orchestrator_last_action(record: &Map<String, Value>, transcript: Option<&Value>) -> Value {
    if let Some(action) = transcript.and_then(|t| t.get("last_action")).filter(|a| notify_truthy(Some(a))) {
        return action.clone();
    }
    let text = redact_value(record.get("result"), None).split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        Value::Null
    } else {
        json!({"type": "ai", "text": text.chars().take(ACTION_LIMIT).collect::<String>(), "t": null})
    }
}

/// 1st line with text of the turn `result`; else the one derived from the transcript.
fn orchestrator_decision(record: &Map<String, Value>, transcript: Option<&Value>) -> Value {
    let text = text_or_empty(record.get("result"));
    match splitlines(&text).into_iter().map(str::trim).find(|l| !l.is_empty()) {
        Some(line) => json!(redact_value(Some(&json!(line.trim_start_matches(['-', '*', '#', ' ']).trim())), Some(SUMMARY_LIMIT))),
        None => transcript.and_then(|t| t.get("decision")).cloned().unwrap_or(Value::Null),
    }
}

pub fn get_inferred_role(feature: &Map<String, Value>) -> String {
    let status = feature.get("status").and_then(Value::as_str).unwrap_or("");
    if status == "building" && text_or_empty(feature.get("note")).to_lowercase().contains(VERIFIER_NOTE_MARK) {
        return VERIFIER_ROLE.into();
    }
    match status {
        "pending" | "speccing" => "spec-writer".into(),
        "specced" => "spec-reviewer".into(),
        _ => "executor".into(),
    }
}

fn sum_numbers(values: &[&Value]) -> Value {
    if values.iter().all(|v| v.is_i64() || v.is_u64()) {
        return json!(values.iter().filter_map(|v| v.as_i64()).sum::<i64>());
    }
    json!(values.iter().filter_map(|v| v.as_f64()).sum::<f64>())
}

pub struct AgentHistoryService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn agent_history(&self) -> AgentHistoryService<'_> {
        AgentHistoryService { app: self }
    }
}

type Subagent = (String, PathBuf, PathBuf);

impl AgentHistoryService<'_> {
    fn session_of(record: &Map<String, Value>) -> &str {
        record.get("session_id").and_then(Value::as_str).unwrap_or("")
    }

    // ------------------------------------------------ history (per turn)
    /// Cost/duration/result come from turns/NNN.json; tokens and times from the transcript when there is one.
    pub fn build_orchestrator_agent(&self, work_dir: &str, number: u64, record: &Map<String, Value>, transcript: Option<&Value>) -> Value {
        let files = &self.app.loop_files;
        let snapshot = files.load_snapshot(work_dir, number);
        let data = |key: &str| transcript.and_then(|t| t.get(key)).cloned().unwrap_or(Value::Null);
        let tokens = match transcript.and_then(|t| t.get("tokens")).filter(|t| !t.is_null()) {
            Some(tokens) => tokens.clone(),
            None => json!(sum_usage_tokens(record.get("usage").unwrap_or(&Value::Null))),
        };
        let duration = ms_to_seconds(record.get("duration_ms").unwrap_or(&Value::Null));
        let summary = redact_value(record.get("result"), Some(SUMMARY_LIMIT)).replace('\n', " ");
        let status = get_turn_status(record, &files.read_snapshot_loop_json(snapshot.as_ref()));
        json!({
            "id": history_id(number, ORCHESTRATOR_ID), "role": ORCHESTRATOR_ROLE, "feature": null,
            "state": if is_turn_failed(record) { "failed" } else { "done" },
            "summary": if summary.is_empty() { data("summary") } else { json!(summary) },
            "tokens": tokens, "duration_s": match duration { Some(d) => json!(d), None => data("duration_s") },
            "started_at": data("started_at"), "finished_at": data("finished_at"),
            "last_action": orchestrator_last_action(record, transcript),
            "return_status": match status { Some(s) => json!(s), None => data("return_status") },
            "decision": orchestrator_decision(record, transcript),
        })
    }

    /// `(agents, transcript_found)`: orchestrator + one agent per subagents/agent-*.jsonl of the session.
    pub fn list_turn_agents(&self, work_dir: &str, number: u64, record: &Map<String, Value>) -> (Vec<Value>, bool) {
        let (main_path, subagents) = self.app.transcripts.locate_session_files(work_dir, Self::session_of(record));
        let main = main_path.and_then(|path| self.app.transcripts.read_transcript(&path));
        let mut agents = vec![self.build_orchestrator_agent(work_dir, number, record, main.as_ref())];
        let rows = self.build_subagent_rows(number, &subagents, false);
        let found = main.is_some() || !rows.is_empty();
        agents.extend(rows);
        (agents, found)
    }

    /// One row per readable subagent transcript, oldest first. `live` = the session is still running: an unfinished, unfailed transcript is
    /// `running` (in a finished turn it stays `done`).
    pub fn build_subagent_rows(&self, number: u64, subagents: &[Subagent], live: bool) -> Vec<Value> {
        let mut rows: Vec<Value> = Vec::new();
        for (agent_id, path, meta_path) in subagents {
            let Some(transcript) = self.app.transcripts.read_transcript(path) else { continue };
            let get = |key: &str| transcript.get(key).cloned().unwrap_or(Value::Null);
            let state = if notify_truthy(transcript.get("failed")) {
                "failed"
            } else if live && !notify_truthy(transcript.get("final")) {
                "running"
            } else {
                "done"
            };
            rows.push(json!({
                "id": history_id(number, agent_id), "role": self.app.transcripts.read_agent_type(meta_path), "feature": get("feature"), "task": get("task"),
                "state": state, "summary": get("summary"), "tokens": get("tokens"), "duration_s": get("duration_s"), "started_at": get("started_at"),
                "finished_at": get("finished_at"), "last_action": get("last_action"), "return_status": get("return_status"), "decision": get("decision"),
            }));
        }
        let key = |row: &Value| (row["started_at"].is_null(), row["started_at"].as_str().unwrap_or("").to_string(), row["id"].as_str().unwrap_or("").to_string());
        rows.sort_by_key(key);
        rows
    }

    /// Session of the turn in progress (a running turn has no turns/NNN.json yet): the most recent session of the project modified after the turn
    /// started (lock file or last turn file, the later one). Best effort: `None` when it cannot be told.
    pub fn find_live_session_id(&self, work_dir: &str) -> Option<String> {
        let files = &self.app.loop_files;
        let mut paths = vec![files.get_lock_path(work_dir)];
        paths.extend(files.list_turn_numbers(work_dir).last().map(|n| files.get_turn_path(work_dir, *n)));
        let newest = paths.iter().filter_map(|p| files.get_mtime(p)).fold(None, |best: Option<f64>, m| Some(best.map_or(m, |b| b.max(m))))?;
        self.app.transcripts.find_live_session_id(work_dir, newest)
    }

    /// Subagent rows (id `vNNN:...` of the turn in progress, state `running` while unfinished) of the live session; empty when not found or when
    /// the session is one that a finished turn already owns (`excluded_session_ids`).
    pub fn list_live_subagents(&self, work_dir: &str, number: u64, excluded_session_ids: &[String]) -> Vec<Value> {
        let Some(session_id) = self.find_live_session_id(work_dir) else { return vec![] };
        if excluded_session_ids.contains(&session_id) {
            return vec![];
        }
        let (_, subagents) = self.app.transcripts.locate_session_files(work_dir, &session_id);
        self.build_subagent_rows(number, &subagents, true)
    }

    pub fn build_history(&self, context: &ProjectContext) -> Value {
        let work_dir = context.work_dir();
        let files = &self.app.loop_files;
        let mut turns: Vec<(u64, Value)> = Vec::new();
        for number in files.list_turn_numbers(&work_dir) {
            let record = files.read_turn(&work_dir, number);
            let snapshot = files.load_snapshot(&work_dir, number);
            let snapshot_loop = files.read_snapshot_loop_json(snapshot.as_ref());
            let (agents, found) = self.list_turn_agents(&work_dir, number, &record);
            let claude = build_turn_claude(&Value::Object(record.clone()));
            let merge = |base: Value| -> Value {
                let mut map = base.as_object().cloned().unwrap_or_default();
                map.extend(claude.as_object().cloned().unwrap_or_default());
                Value::Object(map)
            };
            let mut turn = Map::new();
            turn.insert("n".into(), json!(number));
            turn.insert("session_id".into(), record.get("session_id").filter(|s| s.is_string()).cloned().unwrap_or(Value::Null));
            turn.insert("cost_usd".into(), number_value(record.get("total_cost_usd")));
            turn.insert("duration_s".into(), json!(ms_to_seconds(record.get("duration_ms").unwrap_or(&Value::Null))));
            turn.insert("status".into(), json!(get_turn_status(&record, &snapshot_loop)));
            turn.insert("transcript".into(), json!(found));
            turn.extend(claude.as_object().cloned().unwrap_or_default());
            turn.insert("agents".into(), json!(agents.into_iter().map(merge).collect::<Vec<_>>()));
            turns.push((number, Value::Object(turn)));
        }
        turns.sort_by(|a, b| b.0.cmp(&a.0));
        json!({"turns": turns.into_iter().map(|(_, t)| t).collect::<Vec<_>>()})
    }

    /// `(list, truncated)` of a `vNNN:...` id; id outside the pattern = `None`; missing/corrupted = `([], false)`.
    pub fn get_history_log(&self, work_dir: &str, agent_id: &str) -> Option<(Vec<Value>, bool)> {
        let found = HISTORY_ID.match_start(agent_id)?;
        let number: u64 = found[1].parse().ok()?;
        let name = found[2].to_string();
        let record = self.app.loop_files.read_turn(work_dir, number);
        let mut session_id = Self::session_of(&record).to_string();
        if record.is_empty() && name != ORCHESTRATOR_ID {
            // the turn in progress has no record yet: its executors are in the live session
            session_id = self.find_live_session_id(work_dir).unwrap_or_default();
        }
        let (main_path, subagents) = self.app.transcripts.locate_session_files(work_dir, &session_id);
        let path = if name == ORCHESTRATOR_ID { main_path } else { subagents.iter().find(|s| s.0 == name).map(|s| s.1.clone()) };
        if let Some(transcript) = path.and_then(|p| self.app.transcripts.read_transcript(&p)) {
            let log = transcript.get("log").and_then(Value::as_array).cloned().unwrap_or_default();
            return Some((log, notify_truthy(transcript.get("truncated"))));
        }
        if name == ORCHESTRATOR_ID && notify_truthy(record.get("result")) {
            return Some((
                vec![json!({"type": "ai", "text": redact_value(record.get("result"), Some(LOG_TEXT_LIMIT)), "result": "", "t": format_minutes_clock(0)})],
                false,
            ));
        }
        Some((vec![], false))
    }

    /// Body of `/agent/<id>/log`: a history id (with `truncated`) or a `/state` id (events of the last turn).
    pub fn get_agent_log(&self, context: &ProjectContext, agent_id: &str) -> Value {
        let work_dir = context.work_dir();
        match self.get_history_log(&work_dir, agent_id) {
            Some((log, truncated)) => json!({"id": agent_id, "log": log, "truncated": truncated}),
            None => json!({"id": agent_id, "log": self.list_event_log(&work_dir, agent_id)}),
        }
    }

    pub fn list_event_log(&self, work_dir: &str, agent_id: &str) -> Vec<Value> {
        let files = &self.app.loop_files;
        let numbers = files.list_turn_numbers(work_dir);
        let Some(events_turn) = files.get_live_events_turn(work_dir, &numbers) else { return vec![] };
        let events: Vec<Value> = files.read_turn_events(work_dir, events_turn).iter().filter(|e| event_agent(e) == agent_id).map(build_event_log_item).collect();
        match numbers.last() {
            Some(last) if agent_id == ORCHESTRATOR_ID => {
                let mut log = Self::build_orchestrator_log_head(&files.read_turn(work_dir, *last));
                log.extend(events);
                log
            }
            _ => events,
        }
    }

    fn build_orchestrator_log_head(last_turn: &Map<String, Value>) -> Vec<Value> {
        if !notify_truthy(last_turn.get("result")) {
            return vec![];
        }
        let seconds = last_turn.get("duration_ms").filter(|d| d.is_number()).and_then(Value::as_f64).filter(|d| *d != 0.0).map_or(0, |d| (d / 1000.0) as i64);
        vec![json!({"type": "ai", "text": redact_value(last_turn.get("result"), Some(ORCHESTRATOR_RESULT_LIMIT)), "result": "", "t": format_minutes_clock(seconds)})]
    }

    // ------------------------------------------------ agents of /state
    /// Real agents of the last turn (transcript) in the `/state` format. `None` = do not use (loop running or no transcript).
    pub fn list_real_agents(&self, work_dir: &str, numbers: &[u64], running: bool) -> Option<Vec<Value>> {
        if running || numbers.is_empty() {
            return None;
        }
        let last = *numbers.last()?;
        let record = self.app.loop_files.read_turn(work_dir, last);
        let (agents, found) = self.list_turn_agents(work_dir, last, &record);
        if !found {
            return None;
        }
        let mut converted = Vec::new();
        for agent in agents {
            let is_orchestrator = agent["role"] == ORCHESTRATOR_ROLE;
            let log = self.get_history_log(work_dir, agent["id"].as_str().unwrap_or("")).map(|l| l.0).unwrap_or_default();
            let tail: Vec<Value> = log[log.len().saturating_sub(LOG_ITEMS_IN_STATE)..].to_vec();
            converted.push(json!({
                "id": if is_orchestrator { json!(ORCHESTRATOR_ID) } else { agent["id"].clone() }, "role": agent["role"], "feature": agent["feature"],
                "task": agent.get("task").cloned().unwrap_or(Value::Null), "state": if is_orchestrator { json!("idle") } else { agent["state"].clone() },
                "summary": agent["summary"], "tokens": agent["tokens"],
                "cost_usd": if is_orchestrator { number_value(record.get("total_cost_usd")) } else { Value::Null },
                "duration_s": agent["duration_s"], "log": tail, "last_action": agent.get("last_action").cloned().unwrap_or(Value::Null),
                "return_status": agent.get("return_status").cloned().unwrap_or(Value::Null), "decision": agent.get("decision").cloned().unwrap_or(Value::Null),
            }));
        }
        Some(converted)
    }

    pub fn build_state_agents(&self, work_dir: &str, loop_summary: &Value, features: &[Value], numbers: &[u64]) -> Vec<Value> {
        let files = &self.app.loop_files;
        let running = loop_summary["running"] == true;
        let last_turn = numbers.last().map(|n| files.read_turn(work_dir, *n)).unwrap_or_default();
        let real = self.list_real_agents(work_dir, numbers, running);
        let mut events_by_agent: std::collections::HashMap<String, Vec<Map<String, Value>>> = Default::default();
        if let Some(events_turn) = files.get_live_events_turn(work_dir, numbers) {
            for event in files.read_turn_events(work_dir, events_turn) {
                events_by_agent.entry(event_agent(&event)).or_default().push(event);
            }
        }
        let sum_totals = |agent_id: &str, default: [Value; 3]| -> [Value; 3] {
            let own = events_by_agent.get(agent_id).cloned().unwrap_or_default();
            if own.is_empty() {
                return default;
            }
            let sum_field = |key: &str| {
                let numbers: Vec<&Value> = own.iter().filter_map(|e| e.get(key)).filter(|v| v.is_number()).collect();
                if numbers.is_empty() {
                    Value::Null
                } else {
                    sum_numbers(&numbers)
                }
            };
            [sum_field("tokens"), sum_field("cost_usd"), sum_field("duration_s")]
        };
        let mut orchestrator_log = Self::build_orchestrator_log_head(&last_turn);
        orchestrator_log.extend(events_by_agent.get(ORCHESTRATOR_ID).into_iter().flatten().map(build_event_log_item));
        let orchestrator_state = if running {
            "running"
        } else if loop_summary["status"] == LOOP_DONE_STATUS {
            "done"
        } else {
            "idle"
        };
        let summary = redact_value(last_turn.get("result"), Some(ORCHESTRATOR_STATE_SUMMARY_LIMIT)).replace('\n', " ");
        let default_totals = [
            json!(sum_usage_tokens(last_turn.get("usage").unwrap_or(&Value::Null))),
            number_value(last_turn.get("total_cost_usd")),
            json!(ms_to_seconds(last_turn.get("duration_ms").unwrap_or(&Value::Null))),
        ];
        let [tokens, cost, duration] = sum_totals(ORCHESTRATOR_ID, default_totals);
        let mut orchestrator = Map::new();
        orchestrator.insert("id".into(), json!(ORCHESTRATOR_ID));
        orchestrator.insert("role".into(), json!(ORCHESTRATOR_ROLE));
        orchestrator.insert("feature".into(), Value::Null);
        orchestrator.insert("state".into(), json!(orchestrator_state));
        orchestrator.insert("summary".into(), if summary.is_empty() { Value::Null } else { json!(summary) });
        orchestrator.insert("last_action".into(), orchestrator_last_action(&last_turn, None));
        orchestrator.insert("return_status".into(), if running { Value::Null } else { loop_summary["status"].clone() });
        orchestrator.insert("decision".into(), orchestrator_decision(&last_turn, None));
        orchestrator.insert("tokens".into(), tokens);
        orchestrator.insert("cost_usd".into(), cost);
        orchestrator.insert("duration_s".into(), duration);
        let tail = |log: &[Value]| log[log.len().saturating_sub(LOG_ITEMS_IN_STATE)..].to_vec();
        orchestrator.insert("log".into(), json!(tail(&orchestrator_log)));
        if let Some(real) = real {
            if let Some(real_orchestrator) = real.iter().find(|a| a["id"] == ORCHESTRATOR_ID) {
                if !real_orchestrator["tokens"].is_null() {
                    orchestrator.insert("tokens".into(), real_orchestrator["tokens"].clone());
                }
                if notify_truthy(real_orchestrator.get("last_action")) {
                    orchestrator.insert("last_action".into(), real_orchestrator["last_action"].clone());
                }
            }
            let mut agents = vec![Value::Object(orchestrator)];
            agents.extend(real.into_iter().filter(|a| a["id"] != ORCHESTRATOR_ID));
            return agents;
        }
        let mut agents = vec![Value::Object(orchestrator)];
        for feature in features.iter().filter_map(Value::as_object) {
            let status = feature.get("status").and_then(Value::as_str).unwrap_or("");
            if CLOSED_STATUSES.contains(&status) {
                continue;
            }
            let role = get_inferred_role(feature);
            let agent_id = format!("{}:{role}", text_or_empty(feature.get("id")));
            let state = if !running {
                "idle"
            } else if WORKING_STATUSES.contains(&status) {
                "running"
            } else {
                "queued"
            };
            let own_log: Vec<Value> = events_by_agent.get(&agent_id).into_iter().flatten().map(build_event_log_item).collect();
            let [tokens, cost, duration] = sum_totals(&agent_id, [Value::Null, Value::Null, Value::Null]);
            agents.push(json!({
                "id": agent_id, "role": role, "feature": feature.get("id"), "state": state, "summary": feature.get("note"), "last_action": null,
                "return_status": null, "decision": null, "tokens": tokens, "cost_usd": cost, "duration_s": duration, "log": tail(&own_log),
            }));
        }
        agents
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_inferred_from_status_kind_and_note() {
        let feature = |status: &str, kind: &str, note: &str| json!({"status": status, "kind": kind, "note": note}).as_object().cloned().unwrap();
        assert_eq!(get_inferred_role(&feature("pending", "back", "")), "spec-writer");
        assert_eq!(get_inferred_role(&feature("specced", "back", "")), "spec-reviewer");
        assert_eq!(get_inferred_role(&feature("building", "front", "")), "executor");
        assert_eq!(get_inferred_role(&feature("building", "both", "")), "executor");
        assert_eq!(get_inferred_role(&feature("building", "back", "Verifying now")), "verifier");
    }

    #[test]
    fn history_ids_and_logs_without_transcripts() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let work = dir.path().join("w");
        let w = work.display().to_string();
        let turn = app.loop_files.get_turn_path(&w, 2);
        std::fs::create_dir_all(turn.parent().unwrap()).unwrap();
        std::fs::write(
            &turn,
            json!({"result": "- Chose X\nmore", "total_cost_usd": 1, "duration_ms": 61000, "usage": {"input_tokens": 3, "output_tokens": 4}}).to_string(),
        )
        .unwrap();
        let service = app.agent_history();
        assert_eq!(history_id(2, "orch"), "v002:orch");
        assert!(service.get_history_log(&w, "nope").is_none());
        let (log, truncated) = service.get_history_log(&w, "v002:orch").unwrap();
        assert_eq!((log[0]["text"].clone(), truncated), (json!("- Chose X\nmore"), false));
        assert_eq!(service.get_history_log(&w, "v009:abc").unwrap(), (vec![], false));
        let record = app.loop_files.read_turn(&w, 2);
        let agent = service.build_orchestrator_agent(&w, 2, &record, None);
        assert_eq!(
            (agent["tokens"].clone(), agent["decision"].clone(), agent["state"].clone(), agent["duration_s"].clone()),
            (json!(7), json!("Chose X"), json!("done"), json!(61))
        );
        let state_agents =
            service.build_state_agents(&w, &json!({"running": false, "status": "done"}), &[json!({"id": "a", "status": "building", "kind": "back", "note": null})], &[2]);
        assert_eq!(
            (state_agents[0]["state"].clone(), state_agents[0]["cost_usd"].clone(), state_agents[1]["id"].clone(), state_agents[1]["state"].clone()),
            (json!("done"), json!(1), json!("a:executor"), json!("idle"))
        );
        let history = service.build_history(&ProjectContext::new(crate::domain::project::Project {
            id: "x".into(),
            name: "x".into(),
            repo_path: w.clone(),
            opened_at: String::new(),
            last_opened_at: String::new(),
            favorite: false,
        }));
        assert_eq!(history["turns"][0]["agents"][0]["id"], "v002:orch");
    }
}
