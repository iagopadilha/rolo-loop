// SPDX-License-Identifier: MIT
//! Writes the Claude Code transcripts of a finished demo turn, in the format the panel's history reads:
//! `<transcripts_dir>/<work slug>/<session>.jsonl` (orchestrator) and `<session>/subagents/agent-<id>.jsonl` + `.meta.json`.
//!
//! A guard block is written the way Claude Code records a PreToolUse hook that exited 2: the tool call, then a `tool_result` with `is_error: true`
//! whose text is the hook's stderr (`BLOCKED by the orchestrator guard [<rule>]: <reason>. ...`).

use super::catalog::{GUARD_ADVICE, GUARD_HOOK_COMMAND, GUARD_MARKER};
use super::workspace::write_text_atomically;
use crate::repositories::claude_transcripts::list_project_slug_variants;
use serde_json::{json, Value};
use std::path::PathBuf;

const OUTPUT_SHARE: f64 = 0.08;
const CACHE_SHARE: f64 = 0.7;
const RETURN_TOOL: &str = "SubagentHandback";

/// A moment as milliseconds since the Unix epoch (UTC).
pub type Moment = i64;

pub fn format_transcript_time(moment: Moment) -> String {
    let seconds = moment.div_euclid(1000);
    let millis = moment.rem_euclid(1000);
    let date = time::OffsetDateTime::from_unix_timestamp(seconds).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{millis:03}Z", date.year(), u8::from(date.month()), date.day(), date.hour(), date.minute(), date.second())
}

/// Spreads a token count over the usage fields a real message has (they sum back to `tokens`).
pub fn split_usage(tokens: i64) -> Value {
    let output_tokens = (tokens as f64 * OUTPUT_SHARE) as i64;
    let cache_tokens = (tokens as f64 * CACHE_SHARE) as i64;
    json!({"input_tokens": tokens - output_tokens - cache_tokens, "cache_read_input_tokens": cache_tokens, "output_tokens": output_tokens})
}

fn hook_block_text(tool: &str, rule: &str, reason: &str) -> String {
    format!("PreToolUse:{tool} hook error: [{GUARD_HOOK_COMMAND}]: {GUARD_MARKER} [{rule}]: {reason}. {GUARD_ADVICE}\n")
}

/// One recorded action of an agent or of the orchestrator.
#[derive(Clone, Debug)]
pub enum Recorded {
    Ai { text: String },
    Tool { tool: String, input: Value, result: String },
    Guard { tool: String, input: Value, rule: String, reason: String },
    Return { text: String },
}

#[derive(Clone, Debug)]
pub struct RecordedAction {
    pub at: Moment,
    pub tokens: i64,
    pub what: Recorded,
}

/// Turns recorded actions into transcript lines (one assistant message per action, tool results as user lines).
struct TranscriptRowBuilder {
    prefix: String,
    rows: Vec<Value>,
    counter: u32,
}

impl TranscriptRowBuilder {
    fn new(prefix: &str) -> Self {
        TranscriptRowBuilder { prefix: prefix.to_string(), rows: vec![], counter: 0 }
    }

    fn next_id(&mut self, kind: &str) -> String {
        self.counter += 1;
        format!("{kind}_{}_{:03}", self.prefix, self.counter)
    }

    fn add_prompt(&mut self, moment: Moment, text: &str) {
        self.rows.push(json!({"type": "user", "timestamp": format_transcript_time(moment), "message": {"role": "user", "content": text}}));
    }

    fn add_assistant(&mut self, moment: Moment, blocks: Value, tokens: i64, stop_reason: Option<&str>) {
        let id = self.next_id("msg");
        self.rows.push(json!({
            "type": "assistant", "timestamp": format_transcript_time(moment),
            "message": {"id": id, "role": "assistant", "content": blocks, "stop_reason": stop_reason, "usage": split_usage(tokens)},
        }));
    }

    fn add_tool_call(&mut self, moment: Moment, tool: &str, input: &Value, result: &str, tokens: i64, is_error: bool) {
        let tool_id = self.next_id("toolu");
        self.add_assistant(moment, json!([{"type": "tool_use", "id": tool_id, "name": tool, "input": input}]), tokens, Some("tool_use"));
        self.rows.push(json!({
            "type": "user", "timestamp": format_transcript_time(moment),
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool_id, "content": [{"type": "text", "text": result}], "is_error": is_error}]},
        }));
    }

    fn add_action(&mut self, action: &RecordedAction) {
        let (moment, tokens) = (action.at, action.tokens);
        match &action.what {
            Recorded::Ai { text } => self.add_assistant(moment, json!([{"type": "text", "text": text}]), tokens, None),
            Recorded::Tool { tool, input, result } => self.add_tool_call(moment, tool, input, result, tokens, false),
            Recorded::Guard { tool, input, rule, reason } => self.add_tool_call(moment, tool, input, &hook_block_text(tool, rule, reason), tokens, true),
            Recorded::Return { text } => self.add_tool_call(moment, RETURN_TOOL, &json!({"message": text}), "{\"success\":true}", tokens, false),
        }
    }
}

pub struct OrchestratorRecord {
    pub prompt: String,
    pub started_at: Moment,
    pub actions: Vec<RecordedAction>,
    pub result: String,
    pub finished_at: Moment,
}

#[derive(Clone, Debug)]
pub struct AgentRecord {
    pub agent_id: String,
    pub role: String,
    pub slug: String,
    pub prompt: String,
    pub started_at: Moment,
    pub actions: Vec<RecordedAction>,
}

pub struct DemoTranscriptWriter {
    project_dir: PathBuf,
}

impl DemoTranscriptWriter {
    pub fn new(transcripts_dir: &std::path::Path, work_dir: &str) -> Self {
        let slug = list_project_slug_variants(work_dir).into_iter().next().unwrap_or_default();
        DemoTranscriptWriter { project_dir: transcripts_dir.join(slug) }
    }

    pub fn write_turn(&self, session_id: &str, orchestrator: &OrchestratorRecord, agents: &[&AgentRecord]) {
        let mut main = TranscriptRowBuilder::new("orch");
        main.add_prompt(orchestrator.started_at, &orchestrator.prompt);
        for action in &orchestrator.actions {
            main.add_action(action);
        }
        main.add_assistant(orchestrator.finished_at, json!([{"type": "text", "text": orchestrator.result}]), 0, Some("end_turn"));
        Self::write_rows(&self.project_dir.join(format!("{session_id}.jsonl")), &main.rows);
        let subagents_dir = self.project_dir.join(session_id).join("subagents");
        for agent in agents {
            let tail: String = agent.agent_id.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
            let mut builder = TranscriptRowBuilder::new(&tail);
            builder.add_prompt(agent.started_at, &agent.prompt);
            for action in &agent.actions {
                builder.add_action(action);
            }
            Self::write_rows(&subagents_dir.join(format!("agent-{}.jsonl", agent.agent_id)), &builder.rows);
            let _ = write_text_atomically(&subagents_dir.join(format!("agent-{}.meta.json", agent.agent_id)), &json!({"agentType": agent.role}).to_string());
        }
    }

    fn write_rows(path: &std::path::Path, rows: &[Value]) {
        let text: String = rows.iter().map(|row| format!("{}\n", pycompat::json::py_json_dumps(row, false))).collect();
        let _ = write_text_atomically(path, &text);
    }
}
