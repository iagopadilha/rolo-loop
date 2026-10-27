// SPDX-License-Identifier: MIT
//! Applies the demo story one step at a time to the files the panel reads, the way the loop and a real turn would: ROADMAP.md statuses, live agent
//! events (`turns/NNN.events.jsonl`), `guard.log`, then per finished turn `turns/NNN.json`, `meta.json`, `LOOP.json`, a snapshot, `STATE.md` and the
//! Claude Code transcripts.
//!
//! Time is virtual: each action moves a story clock forward so turn durations and transcript timestamps look like a real loop (minutes per turn)
//! while the animator plays a whole turn in a few seconds. Nothing here calls `claude`.

use super::catalog::{
    feature, spec_file_path, Action, TurnPlan, FEATURES, GOAL, INTEGRATION_BRANCH, LOOP_BRANCH, PROJECT_NAME, STAGES, STATE_HEADER, TURN_COSTS_USD, WAVE,
};
use super::transcripts::{AgentRecord, DemoTranscriptWriter, Moment, OrchestratorRecord, Recorded, RecordedAction};
use super::workspace::{append_json_line, remove_file, write_json_atomically, write_text_atomically, DemoGitRepository, DemoWorkspace};
use crate::repositories::claude_transcripts::summarize_tool_call;
use crate::services::agent_history::get_inferred_role;
use engine_state::{graph_view, loop_io, loop_state};
use pycompat::py_repr_json;
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ACTION_VIRTUAL_MILLISECONDS: i64 = 11_000;
const STEP_VIRTUAL_MILLISECONDS: i64 = 25_000;
const TOKENS_BASE: i64 = 1400;
const TOKENS_SPREAD: i64 = 5200;
const ORCHESTRATOR_TOKENS_BASE: i64 = 9000;
const USD_PER_TOKEN: f64 = 0.0000045;
const EVENT_RESULT_LIMIT: usize = 300;
const SESSION_NAMESPACE: [u8; 16] = [0x6f, 0x1f, 0x3e, 0x0a, 0x5a, 0x2b, 0x4c, 0x1e, 0x9d, 0x3e, 0x1d, 0x0a, 0x7c, 0x9b, 0x2e, 0x55];
const ORCHESTRATOR_EVENT_AGENT: &str = "orch";
const LOCK_FILE: &str = "LOOP.lock";
const ANSWER_PENDING_MARK: &str = "[pending]";
const ANSWER_APPLIED_MARK: &str = "[applied]";
const STOPPED_STATUSES: [&str; 2] = ["awaiting_user", "done"];
const FIELD_ORDER: [&str; 8] = ["status", "scope", "kind", "paths", "deps", "tries", "commit", "note"];

pub fn stable_number(parts: &[String]) -> i64 {
    let digest = Sha1::digest(parts.join(":").as_bytes());
    let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    i64::from_str_radix(&hex, 16).unwrap_or(0)
}

fn sha1_hex(text: &str) -> String {
    Sha1::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// `uuid.uuid5(SESSION_NAMESPACE, name)`.
fn uuid5(name: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(SESSION_NAMESPACE);
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

pub fn format_clock(seconds: i64) -> String {
    let seconds = seconds.max(0);
    format!("{:02}:{:02}:{:02}", seconds / 3600, seconds % 3600 / 60, seconds % 60)
}

/// Local naive time, ISO, seconds.
pub fn to_local_iso(moment: Moment) -> String {
    loop_io::local_from_epoch(moment.div_euclid(1000)).map(loop_io::iso_seconds).unwrap_or_default()
}

#[derive(Clone, Debug)]
pub struct FeatureState {
    pub status: String,
    pub tries: u32,
    pub commit: Option<String>,
    pub note: Option<String>,
}

pub fn pending_features() -> Vec<FeatureState> {
    FEATURES.iter().map(|_| FeatureState { status: "pending".into(), tries: 0, commit: None, note: None }).collect()
}

pub fn render_roadmap(features: &[FeatureState]) -> String {
    let mut lines = vec![format!("# Roadmap: {PROJECT_NAME}"), String::new(), format!("Goal: {GOAL}"), String::new()];
    for stage in &STAGES {
        lines.extend([format!("## Stage {} - {}", stage.n, stage.title), String::new(), stage.description.to_string(), String::new()]);
        for (index, item) in FEATURES.iter().enumerate().filter(|(_, f)| f.stage == stage.n) {
            let current = &features[index];
            let values: [(&str, String); 8] = [
                ("status", current.status.clone()),
                ("scope", item.scope.into()),
                ("kind", item.kind.into()),
                ("paths", item.paths.into()),
                ("deps", item.deps.into()),
                ("tries", if current.tries > 0 { current.tries.to_string() } else { String::new() }),
                ("commit", current.commit.clone().unwrap_or_default()),
                ("note", current.note.clone().unwrap_or_default()),
            ];
            let fields: Vec<String> =
                FIELD_ORDER.iter().filter_map(|key| values.iter().find(|(k, _)| k == key)).filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={v}")).collect();
            lines.push(format!("- [{}] {}", item.slug, fields.join(" ")));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

fn render_spec(slug: &str) -> String {
    let item = feature(slug);
    let rows: Vec<String> = item.requirements.iter().enumerate().map(|(i, text)| format!("| {}-{:02} | {text} | Must |", item.prefix, i + 1)).collect();
    let mut lines = vec![
        format!("# {}", item.title),
        String::new(),
        format!("SCOPE: {}", item.scope),
        String::new(),
        "## Goal".into(),
        String::new(),
        item.goal.to_string(),
        String::new(),
        "## Requirements".into(),
        String::new(),
        "| ID | Requirement | Priority |".into(),
        "|---|---|---|".into(),
    ];
    lines.extend(rows);
    lines.extend([String::new(), "## Paths".into(), String::new(), format!("- `{}`", item.paths), String::new()]);
    lines.join("\n")
}

fn render_stage_closing(number: u32) -> String {
    let mut lines = vec![format!("# Stage {number} closing"), String::new()];
    for (title, text) in super::catalog::stage_closing(number) {
        lines.extend([format!("## {title}"), String::new(), text.to_string(), String::new()]);
    }
    lines.join("\n")
}

fn render_feature_file(slug: &str) -> String {
    let item = feature(slug);
    let comment = if item.file.ends_with(".ts") || item.file.ends_with(".tsx") { "//" } else { "#" };
    format!("{comment} {} ({slug}): written by the demo loop.\n{comment} {}\n", item.title, item.goal)
}

pub fn build_initial_meta(started_at: Moment) -> Value {
    json!({"turns": 0, "cost_usd": 0, "branch": LOOP_BRANCH, "base": INTEGRATION_BRANCH, "started_at": to_local_iso(started_at)})
}

/// Returns the answer to the loop's question (the animator may hold here).
pub type AnswerProvider = Arc<dyn Fn(&str, f64, &str) -> String + Send + Sync>;

struct TurnState {
    number: usize,
    session_id: String,
    started_at: Moment,
    touched: Vec<&'static str>,
    orchestrator_actions: Vec<RecordedAction>,
    agents: Vec<(String, AgentRecord)>,
    events_path: PathBuf,
}

pub struct DemoStoryPlayer {
    work_dir: String,
    git: DemoGitRepository,
    turn_plans: Vec<TurnPlan>,
    generation: u32,
    pub keeps_lock: bool,
    pub answer_provider: AnswerProvider,
    virtual_now: Moment,
    transcript_writer: DemoTranscriptWriter,
    features: Vec<FeatureState>,
    plan_index: usize,
    step_index: usize,
    turn: Option<TurnState>,
    total_cost: f64,
}

impl DemoStoryPlayer {
    /// `story_start` = virtual time of the first action; `keeps_lock` = write LOOP.lock (own PID) while a turn runs, so the panel shows `running`.
    pub fn new(workspace: &DemoWorkspace, git: &DemoGitRepository, turn_plans: Vec<TurnPlan>, story_start: Moment, generation: u32) -> Self {
        let work_dir = workspace.repo_dir.display().to_string();
        DemoStoryPlayer {
            transcript_writer: DemoTranscriptWriter::new(&workspace.transcripts_dir, &work_dir),
            work_dir,
            git: git.clone(),
            turn_plans,
            generation,
            keeps_lock: false,
            answer_provider: Arc::new(|_, _, answer| answer.to_string()),
            virtual_now: story_start,
            features: pending_features(),
            plan_index: 0,
            step_index: 0,
            turn: None,
            total_cost: 0.0,
        }
    }

    // ------------------------------------------------ position
    pub fn is_finished(&self) -> bool {
        self.plan_index >= self.turn_plans.len()
    }

    pub fn get_completed_turns(&self) -> usize {
        self.plan_index
    }

    /// Applies one step of the current turn (opening the turn first when needed). Returns `false` when the story is over.
    pub fn apply_next_step(&mut self) -> bool {
        if self.is_finished() {
            return false;
        }
        let (number, steps) = (self.turn_plans[self.plan_index].0, self.turn_plans[self.plan_index].1.clone());
        if self.turn.is_none() && self.step_index == 0 {
            self.begin_turn(number);
        }
        for action in &steps[self.step_index] {
            self.apply_action(action);
        }
        self.virtual_now += STEP_VIRTUAL_MILLISECONDS;
        self.step_index += 1;
        if self.step_index >= steps.len() {
            self.plan_index += 1;
            self.step_index = 0;
        }
        true
    }

    pub fn play_until_turn(&mut self, completed_turns: usize) {
        while !self.is_finished() && self.get_completed_turns() < completed_turns {
            self.apply_next_step();
        }
    }

    // ------------------------------------------------ paths
    fn loop_file(&self, name: &str) -> PathBuf {
        loop_state::loop_path(&self.work_dir, name)
    }

    fn project_file(&self, name: &str) -> PathBuf {
        Path::new(&self.work_dir).join(".specs").join("project").join(name)
    }

    fn write_roadmap(&self) {
        let _ = write_text_atomically(&self.project_file("ROADMAP.md"), &render_roadmap(&self.features));
    }

    fn write_lock(&self) {
        let _ = write_text_atomically(&self.loop_file(LOCK_FILE), &std::process::id().to_string());
    }

    pub fn release_lock(&self) {
        remove_file(&self.loop_file(LOCK_FILE));
    }

    // ------------------------------------------------ turn lifecycle
    fn begin_turn(&mut self, number: usize) {
        let session_id = uuid5(&format!("{PROJECT_NAME}-{}-{number}", self.generation));
        let events_path = loop_state::turns_dir(&self.work_dir).join(format!("{number:03}.events.jsonl"));
        let _ = write_text_atomically(&events_path, "");
        self.turn = Some(TurnState { number, session_id, started_at: self.virtual_now, touched: vec![], orchestrator_actions: vec![], agents: vec![], events_path });
        if self.keeps_lock {
            self.write_lock();
        }
    }

    fn tick(&mut self) -> Moment {
        let moment = self.virtual_now;
        self.virtual_now += ACTION_VIRTUAL_MILLISECONDS;
        moment
    }

    fn turn(&self) -> &TurnState {
        self.turn.as_ref().expect("a turn is open")
    }

    fn elapsed_clock(&self, moment: Moment) -> String {
        format_clock((moment - self.turn().started_at) / 1000)
    }

    fn touch(&mut self, slug: &'static str) {
        let turn = self.turn.as_mut().expect("a turn is open");
        if !turn.touched.contains(&slug) {
            turn.touched.push(slug);
        }
    }

    /// The agent key the panel infers for the feature's current status, and its transcript record (created on first use).
    fn agent_index(&mut self, slug: &str) -> usize {
        let item = feature(slug);
        let index = FEATURES.iter().position(|f| f.slug == slug).expect("a feature of the story");
        let current = &self.features[index];
        let feature_row: Map<String, Value> = json!({"id": slug, "status": current.status, "kind": item.kind, "note": current.note}).as_object().cloned().unwrap();
        let role = get_inferred_role(&feature_row);
        let key = format!("{slug}:{role}");
        if let Some(position) = self.turn().agents.iter().position(|(k, _)| *k == key) {
            return position;
        }
        let agent_id = format!("a{}", &sha1_hex(&format!("{}:{}:{key}", self.generation, self.turn().number))[..16]);
        let prompt = format!(
            "Role: {role}\nSCOPE_FEATURE: {slug}\nPATHS: {}\nSpec: {}\n\nWork only inside the PATHS; end with STATUS or VERDICT and a SUMMARY line.",
            item.paths,
            spec_file_path(slug)
        );
        let record = AgentRecord { agent_id, role, slug: slug.to_string(), prompt, started_at: self.virtual_now, actions: vec![] };
        let turn = self.turn.as_mut().unwrap();
        turn.agents.push((key, record));
        turn.agents.len() - 1
    }

    #[allow(clippy::too_many_arguments)]
    fn record_event(&self, agent_key: &str, event_type: &str, text: &str, result: &str, moment: Moment, tokens: i64, extra: &[(&str, Value)]) {
        let mut event = Map::new();
        event.insert("agent".into(), json!(agent_key));
        event.insert("type".into(), json!(event_type));
        event.insert("text".into(), json!(text));
        event.insert("result".into(), json!(result.chars().take(EVENT_RESULT_LIMIT).collect::<String>()));
        event.insert("t".into(), json!(self.elapsed_clock(moment)));
        event.insert("tokens".into(), json!(tokens));
        event.insert("cost_usd".into(), json!((tokens as f64 * USD_PER_TOKEN * 1e4).round_ties_even() / 1e4));
        event.insert("duration_s".into(), json!(ACTION_VIRTUAL_MILLISECONDS / 1000));
        for (key, value) in extra {
            event.insert((*key).to_string(), value.clone());
        }
        let _ = append_json_line(&self.turn().events_path, &Value::Object(event));
    }

    fn count_tokens(&self, parts: &[String]) -> i64 {
        let mut all = vec![self.turn().number.to_string()];
        all.extend(parts.iter().cloned());
        TOKENS_BASE + stable_number(&all) % TOKENS_SPREAD
    }

    // ------------------------------------------------ actions
    fn apply_action(&mut self, action: &Action) {
        match action {
            Action::Orchestrator { text } => {
                let (moment, tokens) = (self.tick(), self.count_tokens(&["orch".into(), text.clone()]));
                self.turn.as_mut().unwrap().orchestrator_actions.push(RecordedAction { at: moment, tokens, what: Recorded::Ai { text: text.clone() } });
                self.record_event(ORCHESTRATOR_EVENT_AGENT, "ai", text, "", moment, tokens, &[]);
            }
            Action::OrchestratorTool { tool, input, result } => {
                let (moment, tokens) = (self.tick(), self.count_tokens(&["orch".into(), (*tool).into(), py_repr_json(input)]));
                self.turn.as_mut().unwrap().orchestrator_actions.push(RecordedAction {
                    at: moment,
                    tokens,
                    what: Recorded::Tool { tool: (*tool).into(), input: input.clone(), result: result.clone() },
                });
                self.record_event(ORCHESTRATOR_EVENT_AGENT, "tool", &summarize_tool_call(tool, input), result, moment, tokens, &[]);
            }
            Action::Status { slug, status, note } => {
                let index = FEATURES.iter().position(|f| f.slug == *slug).expect("a feature of the story");
                self.features[index].status = (*status).into();
                self.features[index].note = note.map(String::from);
                self.touch(slug);
                self.write_roadmap();
            }
            Action::AgentTool { slug, tool, input, result } => {
                let position = self.agent_index(slug);
                let key = self.turn().agents[position].0.clone();
                let (moment, tokens) = (self.tick(), self.count_tokens(&[key.clone(), (*tool).into(), py_repr_json(input)]));
                self.turn.as_mut().unwrap().agents[position].1.actions.push(RecordedAction {
                    at: moment,
                    tokens,
                    what: Recorded::Tool { tool: (*tool).into(), input: input.clone(), result: result.clone() },
                });
                self.record_event(&key, "tool", &summarize_tool_call(tool, input), result, moment, tokens, &[]);
            }
            Action::AgentText { slug, text } => {
                let position = self.agent_index(slug);
                let key = self.turn().agents[position].0.clone();
                let (moment, tokens) = (self.tick(), self.count_tokens(&[key.clone(), text.clone()]));
                self.turn.as_mut().unwrap().agents[position].1.actions.push(RecordedAction { at: moment, tokens, what: Recorded::Ai { text: text.clone() } });
                self.record_event(&key, "ai", text, "", moment, tokens, &[]);
            }
            Action::AgentReturn { slug, text } => {
                let position = self.agent_index(slug);
                let key = self.turn().agents[position].0.clone();
                let (moment, tokens) = (self.tick(), self.count_tokens(&[key.clone(), "return".into()]));
                self.turn.as_mut().unwrap().agents[position].1.actions.push(RecordedAction { at: moment, tokens, what: Recorded::Return { text: text.clone() } });
                self.record_event(&key, "ai", text, "", moment, tokens, &[]);
            }
            Action::GuardBlock { slug, tool, input, rule, reason, profile } => self.apply_guard_block(slug, tool, input, rule, reason, profile),
            Action::Spec { slug } => {
                let _ = write_text_atomically(&Path::new(&self.work_dir).join(spec_file_path(slug).replace('/', "\\")), &render_spec(slug));
            }
            Action::Commit { slug } => self.apply_commit(slug),
            Action::StageClosing { stage } => {
                let path = Path::new(&self.work_dir).join(".specs").join("project").join("phases").join(format!("stage-{stage:02}.md"));
                let _ = write_text_atomically(&path, &render_stage_closing(*stage));
            }
            Action::WaitForAnswer { question, answer, hold_seconds } => self.apply_wait_for_answer(question, answer, *hold_seconds),
            Action::EndTurn { status, result, reason, next, options, to_check } => self.apply_end_turn(status, result, reason, next, options, to_check),
        }
    }

    /// The executor's tool call the PreToolUse guard refused: live `guard` event, `guard.log` line, transcript tool error.
    fn apply_guard_block(&mut self, slug: &str, tool: &str, input: &Value, rule: &str, reason: &str, profile: &str) {
        let position = self.agent_index(slug);
        let key = self.turn().agents[position].0.clone();
        let (moment, tokens) = (self.tick(), self.count_tokens(&[key.clone(), "guard".into(), rule.into()]));
        self.turn.as_mut().unwrap().agents[position].1.actions.push(RecordedAction {
            at: moment,
            tokens,
            what: Recorded::Guard { tool: tool.into(), input: input.clone(), rule: rule.into(), reason: reason.into() },
        });
        self.record_event(
            &key,
            "guard",
            &summarize_tool_call(tool, input),
            &format!("exit 2 · [{rule}] {reason}"),
            moment,
            tokens,
            &[("rule", json!(rule)), ("reason", json!(reason))],
        );
        let _ = append_json_line(
            &self.loop_file("guard.log"),
            &json!({"t": to_local_iso(moment), "profile": profile, "rule": rule, "decision": "blocked", "detail": reason.chars().take(200).collect::<String>()}),
        );
    }

    fn apply_commit(&mut self, slug: &'static str) {
        let item = feature(slug);
        let _ = write_text_atomically(&Path::new(&self.work_dir).join(item.file.replace('/', "\\")), &render_feature_file(slug));
        let commit = self.git.commit_paths(&[item.file.to_string()], &format!("feat({slug}): {}", item.title.to_lowercase())).unwrap_or_default();
        let index = FEATURES.iter().position(|f| f.slug == slug).expect("a feature of the story");
        self.features[index] = FeatureState { status: "verified".into(), tries: self.features[index].tries, commit: Some(commit), note: None };
        self.touch(slug);
        self.write_roadmap();
    }

    /// The user answers the loop's question (the animator may hold here); the next turn reads it from ANSWERS.md.
    fn apply_wait_for_answer(&mut self, question: &str, answer: &str, hold_seconds: f64) {
        let given = (self.answer_provider)(question, hold_seconds, answer);
        let block = format!(
            "\n## Answer {} {ANSWER_PENDING_MARK}\nQuestion: {}\nUser answer: {given}\n",
            to_local_iso(self.virtual_now),
            if question.is_empty() { "the loop's question" } else { question }
        );
        let path = loop_state::state_file(&self.work_dir, "ANSWERS.md");
        let _ = write_text_atomically(&path, &format!("{}{block}", loop_state::read(&path)));
        if self.keeps_lock {
            self.write_lock(); // the answer relaunches the loop: running again until the next turn ends
        }
    }

    fn apply_end_turn(&mut self, status: &str, result: &str, reason: &str, next: &str, options: &[String], to_check: &[String]) {
        let finished_at = self.tick();
        let (number, started_at) = (self.turn().number, self.turn().started_at);
        let duration_ms = finished_at - started_at;
        let cost = TURN_COSTS_USD[(number - 1) % TURN_COSTS_USD.len()];
        self.total_cost = (((self.total_cost + cost) * 1e4).round_ties_even()) / 1e4;
        let orchestrator_tokens = ORCHESTRATOR_TOKENS_BASE + self.turn().orchestrator_actions.iter().map(|a| a.tokens).sum::<i64>();
        let _ = write_json_atomically(
            &loop_state::turns_dir(&self.work_dir).join(format!("{number:03}.json")),
            &json!({
                "type": "result", "subtype": "success", "is_error": false, "total_cost_usd": cost, "duration_ms": duration_ms,
                "num_turns": 4 + self.turn().orchestrator_actions.len(), "result": result, "session_id": self.turn().session_id,
                "usage": {"input_tokens": orchestrator_tokens / 10, "cache_read_input_tokens": orchestrator_tokens * 8 / 10, "output_tokens": orchestrator_tokens / 10},
                "permission_denials": [], "exit_code": 0, "timeout": false, "turn": number,
            }),
        );
        self.mark_answers_applied();
        let mut loop_json = Map::new();
        loop_json.insert("status".into(), json!(status));
        loop_json.insert("turn".into(), json!(number));
        loop_json.insert("feature".into(), json!(self.turn().touched.join(",")));
        loop_json.insert("reason".into(), json!(reason));
        loop_json.insert("next".into(), json!(next));
        loop_json.insert("at".into(), json!(to_local_iso(finished_at)));
        if !options.is_empty() {
            loop_json.insert("options".into(), json!(options));
        }
        if !to_check.is_empty() {
            loop_json.insert("to_check".into(), json!(to_check));
        }
        let _ = write_json_atomically(&self.loop_file("LOOP.json"), &Value::Object(loop_json));
        let mut meta = self.read_meta();
        meta.insert("turns".into(), json!(number));
        meta.insert("cost_usd".into(), json!(self.total_cost));
        if status == "done" {
            meta.insert("stopped".into(), json!("done"));
            meta.insert("reason".into(), json!(reason));
            meta.insert("finished_at".into(), json!(to_local_iso(finished_at)));
        }
        let _ = write_json_atomically(&self.loop_file("meta.json"), &Value::Object(meta));
        let state_path = self.project_file("STATE.md");
        let existing = loop_state::read(&state_path);
        let _ = write_text_atomically(&state_path, &format!("{}- turn {number}: {result}\n", if existing.is_empty() { STATE_HEADER.to_string() } else { existing }));
        loop_state::snapshot(&self.work_dir, number as u64, &self.git.get_head(false).unwrap_or_default(), None);
        graph_view::write(&self.work_dir);
        self.write_turn_transcripts(result, finished_at);
        if STOPPED_STATUSES.contains(&status) {
            self.release_lock();
        }
        self.turn = None;
    }

    fn mark_answers_applied(&self) {
        let path = loop_state::state_file(&self.work_dir, "ANSWERS.md");
        let text = loop_state::read(&path);
        if text.contains(ANSWER_PENDING_MARK) {
            let _ = write_text_atomically(&path, &text.replace(ANSWER_PENDING_MARK, ANSWER_APPLIED_MARK));
        }
    }

    fn read_meta(&self) -> Map<String, Value> {
        std::fs::read(self.loop_file("meta.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
    }

    fn write_turn_transcripts(&self, result: &str, finished_at: Moment) {
        let turn = self.turn();
        let orchestrator = OrchestratorRecord {
            prompt: format!("/orchestrator continue --headless --turn {} --wave {WAVE} --branch {LOOP_BRANCH}", turn.number),
            started_at: turn.started_at,
            actions: turn.orchestrator_actions.clone(),
            result: result.to_string(),
            finished_at,
        };
        let agents: Vec<&AgentRecord> = turn.agents.iter().map(|(_, record)| record).collect();
        self.transcript_writer.write_turn(&turn.session_id, &orchestrator, &agents);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_uuid5_and_tokens_are_stable() {
        let id = uuid5("acme-checkout-0-1");
        assert_eq!((id.len(), id.as_bytes()[14] as char), (36, '5'));
        assert_eq!(uuid5("acme-checkout-0-1"), id);
        assert_ne!(uuid5("acme-checkout-0-2"), id);
        assert_eq!(stable_number(&["1".into(), "orch".into()]), stable_number(&["1".into(), "orch".into()]));
        assert_eq!(format_clock(3725), "01:02:05");
    }

    #[test]
    fn the_roadmap_renders_the_fields_in_the_grammar_order() {
        let mut features = pending_features();
        features[0] = FeatureState { status: "verified".into(), tries: 2, commit: Some("abc1234".into()), note: Some("ok".into()) };
        let text = render_roadmap(&features);
        assert!(text.contains("- [payments-api] status=verified scope=Medium kind=back paths=services/payments-api tries=2 commit=abc1234 note=ok"));
        assert!(text.contains("- [checkout-ui] status=pending scope=Large kind=front paths=web/checkout-ui deps=cart-service,dir:web/shared"));
        let (parsed, errors) = loop_state::parse_roadmap(&text);
        assert_eq!((parsed.len(), errors.len()), (9, 0));
    }
}
