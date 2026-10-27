// SPDX-License-Identifier: MIT
//! Pipeline of the loop for the panel (`GET /pipeline`): the six fixed steps of every feature, the features with their wave, and, per turn, the
//! events that turn produced (who ran which step and how it ended).
//!
//! Everything is derived from what the loop already wrote: the agents of the turn (Claude Code transcripts, through the agent history), the
//! feature status changes between two consecutive snapshots and the commits of the turn. Never invents anything: what cannot be derived is `null`
//! or an empty list. The body is cached per project by the turn files, the ROADMAP and the loop state, so repeated reads while nothing changed
//! cost nothing.

use super::agent_history::{get_inferred_role, ORCHESTRATOR_ROLE, VERIFIER_NOTE_MARK, WORKING_STATUSES};
use super::claude_accounts::build_turn_claude;
use super::loop_panel::compute_current_turn;
use super::turns::{get_turn_status, TURN_STATUS_RUNNING};
use super::App;
use crate::domain::project::ProjectContext;
use crate::repositories::loop_read_formatting::{clip_redacted_text, ms_to_seconds, number_value, redact_value, text_or_empty};
use engine_state::{loop_state, notify_truthy};
use pycompat::{splitlines, Re};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

struct Step {
    key: &'static str,
    agent: &'static str,
    role: &'static str,
}

const STEPS: [Step; 6] = [
    Step { key: "spec", agent: "spec-writer", role: "writes the spec (PATHS + criteria)" },
    Step { key: "review", agent: "spec-reviewer", role: "reviews the spec" },
    Step { key: "gate", agent: "gate", role: "approves for execution" },
    Step { key: "exec", agent: "executor", role: "implements" },
    Step { key: "verify", agent: "verifier", role: "build · types · lint · tests" },
    Step { key: "commit", agent: "commit", role: "git checkpoint" },
];
const STEP_SPEC: usize = 0;
const STEP_REVIEW: usize = 1;
const STEP_GATE: usize = 2;
const STEP_EXEC: usize = 3;
const STEP_VERIFY: usize = 4;
const STEP_COMMIT: usize = 5;
const STEP_LABELS: [&str; 6] = ["spec writing", "spec review", "gate", "execution", "verification", "commit"];
const LABEL_STEP_ORDER: [usize; 4] = [STEP_SPEC, STEP_REVIEW, STEP_EXEC, STEP_VERIFY];
const LABEL_MAX_PARTS: usize = 3;
const EVENT_DONE: &str = "DONE";
const EVENT_RUNNING: &str = "RUNNING";
const EVENT_FAIL: &str = "FAIL";
const EVENT_BLOCKED: &str = "BLOCKED";
const EVENT_OUT_LIMIT: usize = 300;
/// The whole orchestrator return of a turn (the panel renders it as Markdown); clipped only when huge.
const DECISION_LIMIT: usize = 6000;
const FILES_PER_TURN_LIMIT: usize = 200;
const SHORT_COMMIT_LENGTH: usize = 7;
const GATE_NOTE_PREFIX: &str = "gate";

static COMMIT_IN_STATE: LazyLock<Re> = LazyLock::new(|| Re::new(r"commit\s+([0-9a-f]{7,40})"));

fn step_by_role(role: &str) -> Option<usize> {
    match role {
        "spec-writer" => Some(STEP_SPEC),
        "spec-reviewer" => Some(STEP_REVIEW),
        "verifier" | "verifier-flow" => Some(STEP_VERIFY),
        _ => None,
    }
}

fn step_by_status_in_progress(status: &str) -> Option<usize> {
    match status {
        "pending" | "speccing" => Some(STEP_SPEC),
        "specced" => Some(STEP_REVIEW),
        "approved" | "building" => Some(STEP_EXEC),
        _ => None,
    }
}

/// `{slug: wave}`: 1 + the highest wave of its slug dependencies (none = 1). `dir:` dependencies and unknown slugs are ignored; a cycle (the
/// engine's validation reports it) does not recurse forever.
pub fn compute_waves(features: &[Value]) -> HashMap<String, i64> {
    let order: Vec<String> = features.iter().map(|f| f["id"].as_str().unwrap_or("").to_string()).collect();
    let dependencies: HashMap<String, Vec<String>> = features
        .iter()
        .map(|f| {
            let deps = f["deps"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|d| !d.starts_with(loop_state::DIR_PREFIX)).map(String::from).collect();
            (f["id"].as_str().unwrap_or("").to_string(), deps)
        })
        .collect();
    fn resolve(slug: &str, dependencies: &HashMap<String, Vec<String>>, waves: &mut HashMap<String, i64>, visiting: &mut HashSet<String>) -> i64 {
        if let Some(wave) = waves.get(slug) {
            return *wave;
        }
        if visiting.contains(slug) {
            return 0;
        }
        visiting.insert(slug.to_string());
        let mut highest = 0;
        for dep in dependencies.get(slug).into_iter().flatten().filter(|d| dependencies.contains_key(*d)) {
            highest = highest.max(resolve(dep, dependencies, waves, visiting));
        }
        visiting.remove(slug);
        waves.insert(slug.to_string(), 1 + highest);
        1 + highest
    }
    let mut waves = HashMap::new();
    for slug in &order {
        resolve(slug, &dependencies, &mut waves, &mut HashSet::new());
    }
    waves
}

/// Pipeline step of an agent role; `None` for roles outside the six steps (the orchestrator, mappers...).
pub fn get_step_for_role(role: &str) -> Option<usize> {
    if role.starts_with("executor") { Some(STEP_EXEC) } else { step_by_role(role) }
}

/// RUNNING while it runs; FAIL when it failed or returned FAIL/BLOCKED (BLOCKED for the reviewer, which blocks a spec); else DONE.
fn event_status_for_agent(agent: &Value, step: usize) -> &'static str {
    if agent["state"] == "running" {
        return EVENT_RUNNING;
    }
    let return_status = agent["return_status"].as_str().unwrap_or("");
    if (step == STEP_REVIEW || step == STEP_GATE) && return_status == "BLOCKED" {
        return EVENT_BLOCKED;
    }
    if agent["state"] == "failed" || return_status == "FAIL" || return_status == "BLOCKED" {
        return EVENT_FAIL;
    }
    EVENT_DONE
}

fn make_event(feature: &str, step: usize, status: &str, out: &str, agent_id: Option<&str>, agent: &str) -> Value {
    let clipped = clip_redacted_text(out, EVENT_OUT_LIMIT);
    json!({"feature": feature, "step": step, "status": status, "out": if clipped.is_empty() { Value::Null } else { json!(clipped) }, "agent_id": agent_id, "agent": agent})
}

/// Short deterministic text from the steps the turn's agents ran ("spec review", "execution + verification"), plus the wave when all its features
/// share one.
fn build_turn_label(events: &[Value], waves: &HashMap<String, i64>) -> String {
    let step_of = |e: &Value| e["step"].as_u64().unwrap_or(0) as usize;
    let mut steps: BTreeSet<usize> = events.iter().filter(|e| notify_truthy(e.get("agent_id"))).map(step_of).collect(); // agent runs only: gate/commit marks are derived
    if steps.is_empty() {
        steps = events.iter().map(step_of).collect();
    }
    let mut ordered: Vec<usize> = LABEL_STEP_ORDER.iter().copied().filter(|s| steps.contains(s)).collect();
    ordered.extend([STEP_GATE, STEP_COMMIT].into_iter().filter(|s| steps.contains(s)));
    let mut label = if ordered.is_empty() {
        "orchestration".to_string()
    } else if ordered.len() > LABEL_MAX_PARTS {
        "mixed steps".to_string()
    } else {
        ordered.iter().map(|s| STEP_LABELS[*s]).collect::<Vec<_>>().join(" + ")
    };
    let wave_numbers: BTreeSet<Option<i64>> = events.iter().map(|e| waves.get(e["feature"].as_str().unwrap_or("")).copied()).collect();
    if wave_numbers.len() == 1 {
        if let Some(Some(wave)) = wave_numbers.iter().next() {
            label.push_str(&format!(" · wave {wave}"));
        }
    }
    label
}

/// The step the feature was in when it got blocked: the last agent that ran for it in the turn, else the gate when the note says so, else the one
/// the status before shows.
fn find_step_in_progress(old_status: &str, note: &str, own_events: &[&Value]) -> usize {
    if let Some(last) = own_events.last() {
        return last["step"].as_u64().unwrap_or(0) as usize;
    }
    if note.starts_with(GATE_NOTE_PREFIX) {
        return STEP_GATE;
    }
    if old_status == "building" && note.to_lowercase().contains(VERIFIER_NOTE_MARK) {
        return STEP_VERIFY;
    }
    step_by_status_in_progress(old_status).unwrap_or(STEP_EXEC)
}

pub struct PipelineService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn pipeline(&self) -> PipelineService<'_> {
        PipelineService { app: self }
    }
}

impl PipelineService<'_> {
    pub fn build_pipeline(&self, context: &ProjectContext) -> Value {
        let work_dir = context.work_dir();
        let summary = self.app.loop_status().get_loop_summary(context);
        let numbers = self.app.loop_files.list_turn_numbers(&work_dir);
        let key = self.build_cache_key(&work_dir, &summary, &numbers);
        if let Some((stored_key, body)) = self.app.pipeline_cache.lock().unwrap_or_else(|e| e.into_inner()).get(&work_dir) {
            if *stored_key == key {
                return body.clone();
            }
        }
        let body = self.compute_pipeline(&work_dir, &summary, &numbers);
        self.app.pipeline_cache.lock().unwrap_or_else(|e| e.into_inner()).insert(work_dir, (key, body.clone()));
        body
    }

    /// `[{"turn": n, ...event}]` of one feature, oldest first (the quarantine detail reads its attempts from here).
    pub fn list_feature_events(&self, context: &ProjectContext, slug: &str) -> Vec<Value> {
        let body = self.build_pipeline(context);
        let mut rows = Vec::new();
        for turn in body["turns"].as_array().into_iter().flatten() {
            for event in turn["events"].as_array().into_iter().flatten().filter(|e| e["feature"] == slug) {
                let mut row = event.as_object().cloned().unwrap_or_default();
                row.insert("turn".into(), turn["n"].clone());
                rows.push(Value::Object(row));
            }
        }
        rows
    }

    fn build_cache_key(&self, work_dir: &str, summary: &Value, numbers: &[u64]) -> String {
        let files = &self.app.loop_files;
        let turn_stamps: Vec<Value> = numbers.iter().map(|n| json!([n, files.get_mtime(&files.get_turn_path(work_dir, *n))])).collect();
        let roadmap_stamp = files.get_mtime(&Path::new(work_dir).join(".specs").join("project").join("ROADMAP.md"));
        let loop_stamp = files.get_mtime(&loop_state::state_file(work_dir, "LOOP.json"));
        json!([turn_stamps, roadmap_stamp, loop_stamp, summary["status"], summary["running"] == true, summary["meta"].get("turns")]).to_string()
    }

    fn compute_pipeline(&self, work_dir: &str, summary: &Value, numbers: &[u64]) -> Value {
        let roadmap_text = self.app.loop_files.read_roadmap(work_dir);
        let (stage_rows, feature_rows, _, _) = self.app.roadmap_view().build_roadmap_view(work_dir, &roadmap_text, None);
        let waves = compute_waves(&feature_rows);
        let features: Vec<Value> = feature_rows
            .iter()
            .map(|row| {
                let id = row["id"].as_str().unwrap_or("");
                let deps: Vec<&Value> = row["deps"].as_array().into_iter().flatten().filter(|d| !d.as_str().unwrap_or("").starts_with(loop_state::DIR_PREFIX)).collect();
                json!({"id": row["id"], "name": row["name"], "stage": row["stage"], "wave": waves.get(id).copied().unwrap_or(1), "deps": deps, "kind": row["kind"], "status": row["status"]})
            })
            .collect();
        let mut turns: Vec<Value> = numbers.iter().map(|n| self.build_turn(work_dir, *n, &waves)).collect();
        let running = summary["running"] == true;
        let loop_json = summary["json"].as_object().cloned().unwrap_or_default();
        let live_number = if running { compute_current_turn(&loop_json, &summary["meta"], numbers, true) } else { None };
        if let Some(live) = &live_number {
            if !live.as_u64().is_some_and(|n| numbers.contains(&n)) {
                turns.push(self.build_live_turn(live, &feature_rows, &waves));
            }
        }
        let paused = summary["status"] == "awaiting_user" && !running;
        json!({
            "steps": STEPS.iter().map(|s| json!({"key": s.key, "agent": s.agent, "role": s.role})).collect::<Vec<_>>(),
            "features": features,
            "stages": stage_rows.iter().map(|s| json!({"n": s["n"], "title": s["title"]})).collect::<Vec<_>>(),
            "turns": turns,
            "live_turn": if running { live_number.unwrap_or(Value::Null) } else { json!(numbers.last()) },
            "paused": paused,
            "quarantine": feature_rows.iter().filter(|r| r["status"] == "blocked").map(|r| r["id"].clone()).collect::<Vec<_>>(),
        })
    }

    fn build_turn(&self, work_dir: &str, number: u64, waves: &HashMap<String, i64>) -> Value {
        let files = &self.app.loop_files;
        let record = files.read_turn(work_dir, number);
        let snapshot = files.load_snapshot(work_dir, number);
        let previous = if number > 0 { files.load_snapshot(work_dir, number - 1) } else { None };
        let mut events = self.list_agent_events(work_dir, number, &record);
        let transitions = self.list_transition_events(snapshot.as_ref(), previous.as_ref(), &events);
        events.extend(transitions);
        let decision = redact_value(record.get("result"), Some(DECISION_LIMIT)).trim().to_string();
        let mut turn = Map::new();
        turn.insert("n".into(), json!(number));
        turn.insert("label".into(), json!(build_turn_label(&events, waves)));
        turn.insert("status".into(), json!(get_turn_status(&record, &files.read_snapshot_loop_json(snapshot.as_ref()))));
        turn.insert("cost_usd".into(), number_value(record.get("total_cost_usd")));
        turn.insert("duration_s".into(), json!(ms_to_seconds(record.get("duration_ms").unwrap_or(&Value::Null))));
        turn.insert("decision".into(), if decision.is_empty() { Value::Null } else { json!(decision) });
        turn.insert("events".into(), json!(events));
        turn.insert("files".into(), json!(self.list_turn_files(work_dir, snapshot.as_ref(), previous.as_ref())));
        turn.extend(build_turn_claude(&Value::Object(record)).as_object().cloned().unwrap_or_default());
        Value::Object(turn)
    }

    /// The turn in progress has no file yet: its RUNNING events are the features the loop is working on (agent inferred from the status, like
    /// `/state` does).
    fn build_live_turn(&self, number: &Value, feature_rows: &[Value], waves: &HashMap<String, i64>) -> Value {
        let mut events = Vec::new();
        for row in feature_rows.iter().filter_map(Value::as_object) {
            if !WORKING_STATUSES.contains(&row.get("status").and_then(Value::as_str).unwrap_or("")) {
                continue;
            }
            let role = get_inferred_role(row);
            if let Some(step) = get_step_for_role(&role) {
                events.push(make_event(row["id"].as_str().unwrap_or(""), step, EVENT_RUNNING, &text_or_empty(row.get("note")), None, &role));
            }
        }
        let label = if events.is_empty() { "in progress".to_string() } else { build_turn_label(&events, waves) };
        json!({"n": number, "label": label, "status": TURN_STATUS_RUNNING, "cost_usd": null, "duration_s": null, "decision": null, "events": events, "files": []})
    }

    // ------------------------------------------------ events
    fn list_agent_events(&self, work_dir: &str, number: u64, record: &Map<String, Value>) -> Vec<Value> {
        let (agents, _) = self.app.agent_history().list_turn_agents(work_dir, number, record);
        let mut events = Vec::new();
        for agent in &agents {
            let role = agent["role"].as_str().unwrap_or("");
            let step = if role != ORCHESTRATOR_ROLE { get_step_for_role(role) } else { None };
            let (Some(step), true) = (step, notify_truthy(agent.get("feature"))) else { continue };
            let out = if notify_truthy(agent.get("decision")) { agent.get("decision") } else { agent.get("summary") };
            events.push(make_event(&text_or_empty(agent.get("feature")), step, event_status_for_agent(agent, step), &text_or_empty(out), agent["id"].as_str(), role));
        }
        events
    }

    /// Marks only the status changes between two snapshots can show: approved (gate), verified with a commit (commit), blocked (the step that was
    /// in progress). Without both snapshots there is nothing to compare.
    fn list_transition_events(&self, snapshot: Option<&Map<String, Value>>, previous: Option<&Map<String, Value>>, agent_events: &[Value]) -> Vec<Value> {
        let (Some(snapshot), Some(previous)) = (snapshot, previous) else { return vec![] };
        let now = self.read_snapshot_features(snapshot);
        let before = self.read_snapshot_features(previous);
        let mut events = Vec::new();
        for (slug, fields) in now.iter() {
            let (old_status, new_status) = (before.get(slug).and_then(|f| f.get("status")), fields.get("status"));
            let Some(old_status) = old_status else { continue };
            if Some(old_status) == new_status {
                continue;
            }
            let own: Vec<&Value> = agent_events.iter().filter(|e| e["feature"] == slug.as_str()).collect();
            match new_status {
                Some("approved") => {
                    let reviewer = own.iter().rev().find(|e| e["step"] == STEP_REVIEW);
                    events.push(make_event(slug, STEP_GATE, EVENT_DONE, &text_or_empty(reviewer.and_then(|e| e.get("out"))), None, "gate"));
                }
                Some("verified") => {
                    if let Some(commit) = self.read_snapshot_commit(snapshot, slug, fields) {
                        events.push(make_event(slug, STEP_COMMIT, EVENT_DONE, &format!("commit {commit}"), None, "commit"));
                    }
                }
                Some("blocked") => {
                    let step = find_step_in_progress(old_status, fields.text("note"), &own);
                    let from_agent = own.last().filter(|e| e["step"] == step).map(|e| text_or_empty(e.get("out"))).filter(|o| !o.is_empty());
                    let out = from_agent.unwrap_or_else(|| fields.text("note").to_string());
                    events.push(make_event(slug, step, EVENT_BLOCKED, &out, None, STEPS[step].agent));
                }
                _ => {}
            }
        }
        events
    }

    fn read_snapshot_features(&self, snapshot: &Map<String, Value>) -> loop_state::Features {
        loop_state::parse_roadmap(&self.app.loop_files.read_snapshot_roadmap(Some(snapshot))).0
    }

    /// Short commit of a feature that became verified: the ROADMAP `commit=` field, else `<slug> ... commit <sha>` in the snapshot's STATE.md.
    fn read_snapshot_commit(&self, snapshot: &Map<String, Value>, slug: &str, fields: &loop_state::Fields) -> Option<String> {
        if let Some(commit) = fields.get("commit").filter(|c| !c.is_empty()) {
            return Some(commit.chars().take(SHORT_COMMIT_LENGTH).collect());
        }
        let dir = snapshot.get("dir").and_then(Value::as_str).unwrap_or("");
        let state_text = self.app.loop_files.read_cached_text(&Path::new(dir).join("STATE.md"));
        for line in splitlines(&state_text) {
            if let Some(found) = COMMIT_IN_STATE.search(line) {
                if line.contains(slug) {
                    return Some(found[1].chars().take(SHORT_COMMIT_LENGTH).collect());
                }
            }
        }
        None
    }

    // ------------------------------------------------ files
    /// Per-file numstat between the two snapshots' HEADs; empty when either HEAD is unknown or git cannot tell.
    fn list_turn_files(&self, work_dir: &str, snapshot: Option<&Map<String, Value>>, previous: Option<&Map<String, Value>>) -> Vec<Value> {
        let head = |s: Option<&Map<String, Value>>| s.and_then(|m| m.get("head")).and_then(Value::as_str).filter(|h| !h.is_empty()).map(String::from);
        let (Some(now), Some(before)) = (head(snapshot), head(previous)) else { return vec![] };
        self.app.loop_files.read_numstat_between(work_dir, &before, &now).map(|rows| rows.into_iter().take(FILES_PER_TURN_LIMIT).collect()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waves_follow_dependencies_and_survive_cycles() {
        let features = vec![
            json!({"id": "a", "deps": []}),
            json!({"id": "b", "deps": ["a", "dir:x", "ghost"]}),
            json!({"id": "c", "deps": ["b"]}),
            json!({"id": "x", "deps": ["y"]}),
            json!({"id": "y", "deps": ["x"]}),
        ];
        let waves = compute_waves(&features);
        assert_eq!((waves["a"], waves["b"], waves["c"]), (1, 2, 3));
        assert!(waves["x"] >= 1 && waves["y"] >= 1);
    }

    #[test]
    fn labels_and_roles() {
        let waves = HashMap::from([("a".to_string(), 2i64)]);
        let events = vec![make_event("a", STEP_REVIEW, EVENT_DONE, "ok", Some("v001:x"), "spec-reviewer"), make_event("a", STEP_GATE, EVENT_DONE, "", None, "gate")];
        assert_eq!(build_turn_label(&events, &waves), "spec review · wave 2");
        assert_eq!(build_turn_label(&[], &waves), "orchestration");
        assert_eq!((get_step_for_role("executor-back"), get_step_for_role("verifier-flow"), get_step_for_role("orchestrator")), (Some(STEP_EXEC), Some(STEP_VERIFY), None));
        assert_eq!(event_status_for_agent(&json!({"state": "done", "return_status": "BLOCKED"}), STEP_REVIEW), EVENT_BLOCKED);
        assert_eq!(event_status_for_agent(&json!({"state": "done", "return_status": "BLOCKED"}), STEP_EXEC), EVENT_FAIL);
    }

    #[test]
    fn pipeline_for_a_project_with_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        std::fs::write(repo.join(".specs/project/ROADMAP.md"), "## Stage 1\n- [a] status=blocked note=gate-closed\n- [b] status=pending deps=a\n").unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let w = context.work_dir();
        for (n, roadmap) in [(1u64, "- [a] status=approved\n"), (2u64, "- [a] status=blocked note=gate-closed\n")] {
            let path = app.loop_files.get_turn_path(&w, n);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, json!({"result": "did it"}).to_string()).unwrap();
            let snap = loop_state::snap_dir(&w, n);
            std::fs::create_dir_all(&snap).unwrap();
            std::fs::write(snap.join("snapshot.json"), "{}").unwrap();
            std::fs::write(snap.join("ROADMAP.md"), roadmap).unwrap();
        }
        let body = app.pipeline().build_pipeline(&context);
        assert_eq!((body["turns"].as_array().unwrap().len(), body["quarantine"].clone(), body["live_turn"].clone()), (2, json!(["a"]), json!(2)));
        assert_eq!(body["features"][1]["wave"], 2);
        let second = &body["turns"][1];
        assert_eq!((second["events"][0]["status"].clone(), second["events"][0]["step"].clone(), second["events"][0]["agent"].clone()), (json!("BLOCKED"), json!(STEP_GATE), json!("gate")));
        assert_eq!(app.pipeline().list_feature_events(&context, "a")[0]["turn"], 2);
        assert_eq!(app.pipeline().build_pipeline(&context), body);
    }
}
