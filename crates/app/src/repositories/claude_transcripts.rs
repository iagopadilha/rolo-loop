// SPDX-License-Identifier: MIT
//! Claude Code transcripts (`<transcripts_dir>/<work slug>/<session>.jsonl` and `.../<session>/subagents/agent-*.jsonl`).
//!
//! Read only. Each file is parsed once per (mtime, size): the cache avoids re-reading large transcripts on every poll. Credentials come out
//! redacted; whatever cannot be extracted becomes null (a number is never invented).
//!
//! Also holds the lighter `read_transcript_summary` used to build the CONTEXT of an agent's chat.

use super::loop_read_formatting::{compute_seconds_between, format_elapsed_clock, parse_iso_date, redact_credentials, to_number, Moment};
use crate::infrastructure::claude_client::redact_secrets;
use engine_state::loop_state;
use pycompat::json::py_json_dumps;
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

pub const TRANSCRIPT_TEXT_LIMIT: usize = 2000;
pub const LOG_ITEMS_LIMIT: usize = 400;
const SUMMARY_LIMIT: usize = 200;
const ACTION_LIMIT: usize = 160;
pub const TRANSCRIPT_CACHE_LIMIT: usize = 256;
static SESSION_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"\A[A-Za-z0-9_-]{1,80}\z"));
static SUBAGENT_FILE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"\Aagent-([A-Za-z0-9_-]{1,80})\.jsonl\z"));
static PROMPT_FEATURE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"\b(?:SCOPE_)?FEATURE:[ \t]*([A-Za-z0-9][A-Za-z0-9._-]*)"));
static PROMPT_TASK_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\bSCOPE_TASK:[ \t]*[A-Za-z0-9][A-Za-z0-9._-]*/(T\d+)\b"));
static PATH_FEATURE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"\.specs[/\\]features[/\\]([A-Za-z0-9][A-Za-z0-9._-]*)"));
static ANY_CASE_FEATURE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\b(?:SCOPE_)?FEATURE:[ \t]*([A-Za-z0-9][A-Za-z0-9._-]*)"));
const RETURN_KEYWORDS: [&str; 8] = ["DONE", "APPROVED", "BLOCKED", "PASS", "FAIL", "UNVERIFIED", "NEEDS_CONFIRM", "TOO_BIG"];
static RETURN_STATUS_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?im)^[ \t\r\f\v]*(?:STATUS|VERDICT)[ \t\r\f\v]*:[ \t\r\f\v]*\**[ \t\r\f\v]*([A-Za-z_]+)"));
static RETURN_DECISION_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?im)^[ \t\r\f\v]*(?:SUMMARY|DECISION)[ \t\r\f\v]*:[ \t\r\f\v]*(\S.*)$"));
static RETURN_HEADER_LINE: LazyLock<Re> = LazyLock::new(|| {
    Re::ci(r"^\s*(?:FEATURE|SCOPE_FEATURE|FLOW|SCRIPT|STATUS|VERDICT|SCOPE|CITED|FILES|PATHS|DEPS|KIND|RAN|LAYERS|ENVIRONMENT|CHANGED|FAIL|NOTES|STEPS)\s*:")
});
static FINDINGS_EMPTY: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^FINDINGS\s*:\s*\z"));
static FINDINGS_INLINE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^FINDINGS\s*:\s*\S"));
static LEADING_WORD: LazyLock<Re> = LazyLock::new(|| Re::new(r"\A\W*([A-Za-z_]+)"));
static GUARD_BLOCK: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?s)BLOCKED by the orchestrator guard \[([^\]\n]{1,200})\]:\s*(.*)"));
static GUARD_ADVICE: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?s)\.?\s*Do not try to work around it\b.*$"));
static NON_ALNUM: LazyLock<Re> = LazyLock::new(|| Re::new(r"[^A-Za-z0-9]"));
const TOOL_SUMMARY_KEYS: [&str; 8] = ["command", "file_path", "pattern", "url", "query", "description", "prompt", "path"];
const SUBAGENT_RETURN_TOOL: &str = "SubagentHandback";
const END_TURN_REASON: &str = "end_turn";
const SYSTEM_REMINDER_PREFIX: &str = "<system-reminder>";
const GUARD_LOG_TYPE: &str = "guard";
const GUARD_EXIT_LABEL: &str = "exit 2";
const GUARD_REASON_LIMIT: usize = 400;

fn truthy(value: Option<&Value>) -> bool {
    engine_state::notify_truthy(value)
}

/// `str(value)` of a JSON value the way Python prints it for the fields used here (strings as they are).
fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => py_json_dumps(other, false),
    }
}

/// `str(block.get(key) or "")`.
fn text_field(block: &Map<String, Value>, key: &str) -> String {
    match block.get(key) {
        Some(v) if truthy(Some(v)) => text_of(v),
        _ => String::new(),
    }
}

/// Possible project folder names for this work dir: non alphanumeric becomes '-'; drive letter as is, lower and upper case.
pub fn list_project_slug_variants(work_dir: &str) -> Vec<String> {
    if work_dir.is_empty() {
        return vec![];
    }
    let path = pycompat::nt_abspath(work_dir).trim_end_matches(['\\', '/']).to_string();
    let mut variants = vec![path.clone()];
    let chars: Vec<char> = path.chars().collect();
    if chars.len() > 1 && chars[1] == ':' {
        let rest: String = chars[1..].iter().collect();
        variants.push(format!("{}{rest}", chars[0].to_lowercase()));
        variants.push(format!("{}{rest}", chars[0].to_uppercase()));
    }
    let mut slugs: Vec<String> = Vec::new();
    for item in variants.iter().filter(|v| !v.is_empty()) {
        let slug = NON_ALNUM.replace_lit(item, "-");
        if !slugs.contains(&slug) {
            slugs.push(slug);
        }
    }
    slugs
}

/// Text of a message/tool_result content (string or list of blocks).
pub fn extract_block_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(Value::as_object)
            .filter_map(|b| b.get("text").filter(|t| !t.is_null()))
            .map(text_of)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub fn summarize_tool_call(name: &str, tool_input: &Value) -> String {
    let empty = Map::new();
    let input = tool_input.as_object().unwrap_or(&empty);
    for key in TOOL_SUMMARY_KEYS {
        if let Some(value) = input.get(key).filter(|v| truthy(Some(v))) {
            let extra = if key == "pattern" && truthy(input.get("path")) { format!(" in {}", text_of(&input["path"])) } else { String::new() };
            return format!("{name} {}{extra}", text_of(value));
        }
    }
    format!("{name} {}", py_json_dumps(&Value::Object(input.clone()), false))
}

/// `(rule, reason)` when the tool result is a block by the orchestrator guard. Only an error result counts: a tool that merely READ the guard
/// source or its log also contains the marker.
pub fn parse_guard_block(result_text: &str, is_error: bool) -> Option<(String, String)> {
    if !is_error {
        return None;
    }
    let found = GUARD_BLOCK.captures(result_text)?;
    let reason = GUARD_ADVICE.replace_lit(&found[2], "");
    let reason = reason.trim();
    Some((found[1].trim().to_string(), redact_credentials(&reason.split_whitespace().collect::<Vec<_>>().join(" "), Some(GUARD_REASON_LIMIT))))
}

pub fn list_message_blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(s) => vec![json!({"type": "text", "text": s})],
        Value::Array(items) => items.clone(),
        _ => vec![],
    }
}

/// Keyword of the final return (`STATUS:`/`VERDICT:` or first word of the handback); `None` when there is none.
pub fn extract_return_status(final_return: &str) -> Option<String> {
    if final_return.is_empty() {
        return None;
    }
    let word = RETURN_STATUS_LINE
        .captures(final_return)
        .map(|c| c[1].to_string())
        .or_else(|| LEADING_WORD.captures(final_return).map(|c| c[1].to_string()))
        .unwrap_or_default()
        .to_uppercase();
    RETURN_KEYWORDS.contains(&word.as_str()).then_some(word)
}

/// Short sentence of what the agent concluded: SUMMARY/DECISION line, else 1st FINDINGS item, else the 1st non-header line.
pub fn extract_return_decision(final_return: &str) -> Option<String> {
    if final_return.is_empty() {
        return None;
    }
    if let Some(found) = RETURN_DECISION_LINE.captures(final_return) {
        return Some(redact_credentials(found[1].trim(), Some(SUMMARY_LIMIT)));
    }
    let lines: Vec<&str> = pycompat::splitlines(final_return).into_iter().map(str::trim).filter(|l| !l.is_empty()).collect();
    for (index, line) in lines.iter().enumerate() {
        if FINDINGS_EMPTY.is_match(line) {
            if let Some(item) = lines[index + 1..].iter().find(|f| f.starts_with(['-', '*'])) {
                return Some(redact_credentials(item.trim_start_matches(['-', '*', ' ']).trim(), Some(SUMMARY_LIMIT)));
            }
        } else if FINDINGS_INLINE.is_match(line) {
            return Some(redact_credentials(line.split_once(':').map(|p| p.1).unwrap_or("").trim(), Some(SUMMARY_LIMIT)));
        }
    }
    let free = lines.iter().find(|l| !RETURN_HEADER_LINE.is_match(l))?;
    Some(redact_credentials(free.trim_start_matches(['-', '*', ' ']).trim(), Some(SUMMARY_LIMIT)))
}

struct Item {
    value: Map<String, Value>,
    moment: Option<Moment>,
}

struct LastAction {
    kind: &'static str,
    text: String,
    moment: Option<Moment>,
}

/// Accumulates a .jsonl transcript line by line (tolerates a corrupted line and a missing field).
#[derive(Default)]
struct TranscriptReader {
    first_stamp: Option<String>,
    last_stamp: Option<String>,
    usage_by_message: Vec<(String, BTreeMap<String, f64>)>,
    prompt: Option<String>,
    final_return: Option<String>,
    last_text: Option<String>,
    last_stop_reason: Option<String>,
    items: Vec<Item>,
    pending_tools: BTreeMap<String, usize>,
    total_items: usize,
    last_result_was_error: bool,
    had_api_error: bool,
    last_action: Option<LastAction>,
}

impl TranscriptReader {
    fn add_item(&mut self, item_type: &str, text: &str, moment: Option<Moment>) -> Option<usize> {
        self.total_items += 1;
        if self.items.len() >= LOG_ITEMS_LIMIT {
            return None;
        }
        let mut value = Map::new();
        value.insert("type".into(), json!(item_type));
        value.insert("text".into(), json!(redact_credentials(text, Some(TRANSCRIPT_TEXT_LIMIT))));
        value.insert("result".into(), json!(redact_credentials("", Some(TRANSCRIPT_TEXT_LIMIT))));
        self.items.push(Item { value, moment });
        Some(self.items.len() - 1)
    }

    fn read_line(&mut self, line: &str) {
        let Ok(Value::Object(record)) = serde_json::from_str::<Value>(line) else { return };
        let stamp = record.get("timestamp").and_then(Value::as_str).map(String::from);
        let moment = stamp.as_deref().and_then(parse_iso_date);
        if let Some(stamp) = stamp.filter(|s| !s.is_empty()) {
            if self.first_stamp.is_none() {
                self.first_stamp = Some(stamp.clone());
            }
            self.last_stamp = Some(stamp);
        }
        let Some(message) = record.get("message").and_then(Value::as_object) else { return };
        let record_type = record.get("type").and_then(Value::as_str).unwrap_or("");
        if !matches!(record_type, "assistant" | "user") || truthy(record.get("isMeta")) {
            return;
        }
        let blocks = list_message_blocks(message.get("content").unwrap_or(&Value::Null));
        if record_type == "assistant" {
            self.read_assistant_message(&record, message, &blocks, moment);
        } else {
            self.read_user_message(&blocks, moment);
        }
    }

    fn read_assistant_message(&mut self, record: &Map<String, Value>, message: &Map<String, Value>, blocks: &[Value], moment: Option<Moment>) {
        self.last_result_was_error = false;
        if let Some(reason) = message.get("stop_reason").filter(|v| truthy(Some(v))) {
            self.last_stop_reason = Some(text_of(reason));
        }
        self.had_api_error = self.had_api_error || truthy(record.get("isApiErrorMessage"));
        if let Some(usage) = message.get("usage").and_then(Value::as_object) {
            // repeated lines of the same message.id (streaming) count once
            let id = match message.get("id") {
                Some(v) if truthy(Some(v)) => text_of(v),
                _ => format!("line-{}", self.usage_by_message.len()),
            };
            let position = match self.usage_by_message.iter().position(|(k, _)| *k == id) {
                Some(p) => p,
                None => {
                    self.usage_by_message.push((id, BTreeMap::new()));
                    self.usage_by_message.len() - 1
                }
            };
            for (key, value) in usage {
                if let (true, Some(number)) = (key.ends_with("_tokens"), to_number(value)) {
                    let slot = self.usage_by_message[position].1.entry(key.clone()).or_insert(0.0);
                    *slot = slot.max(number);
                }
            }
        }
        for block in blocks.iter().filter_map(Value::as_object) {
            match block.get("type").and_then(Value::as_str) {
                Some("text") if !text_field(block, "text").trim().is_empty() => {
                    let text = text_field(block, "text");
                    self.add_item("ai", &text, moment);
                    self.last_action = Some(LastAction { kind: "ai", text: text.clone(), moment });
                    self.last_text = Some(text);
                }
                Some("tool_use") => {
                    let name = match block.get("name") {
                        Some(v) if truthy(Some(v)) => text_of(v),
                        _ => "tool".into(),
                    };
                    let input = block.get("input").filter(|v| v.is_object()).cloned().unwrap_or_else(|| json!({}));
                    if name == SUBAGENT_RETURN_TOOL {
                        let message = text_field(input.as_object().unwrap_or(&Map::new()), "message");
                        self.add_item("ai", &message, moment);
                        self.final_return = Some(message);
                        continue;
                    }
                    let summary = summarize_tool_call(&name, &input);
                    self.last_action = Some(LastAction { kind: "tool", text: summary.clone(), moment });
                    let item = self.add_item("tool", &summary, moment);
                    if let (Some(index), Some(id)) = (item, block.get("id").filter(|v| truthy(Some(v)))) {
                        self.pending_tools.insert(text_of(id), index);
                    }
                }
                _ => {}
            }
        }
    }

    fn read_user_message(&mut self, blocks: &[Value], moment: Option<Moment>) {
        let mut saw_result = false;
        for block in blocks.iter().filter_map(Value::as_object) {
            let kind = block.get("type").and_then(Value::as_str);
            if kind == Some("tool_result") {
                saw_result = true;
                self.last_result_was_error = truthy(block.get("is_error"));
                let id = block.get("tool_use_id").map(text_of).unwrap_or_default();
                if let Some(index) = self.pending_tools.remove(&id) {
                    let result_text = extract_block_text(block.get("content").unwrap_or(&Value::Null));
                    let item = &mut self.items[index].value;
                    item.insert("result".into(), json!(redact_credentials(&result_text, Some(TRANSCRIPT_TEXT_LIMIT))));
                    if let Some((rule, reason)) = parse_guard_block(&result_text, truthy(block.get("is_error"))) {
                        item.insert("type".into(), json!(GUARD_LOG_TYPE));
                        item.insert("rule".into(), json!(rule));
                        item.insert("reason".into(), json!(reason));
                        item.insert("result".into(), json!(format!("{GUARD_EXIT_LABEL} · [{rule}] {reason}")));
                    }
                }
            } else if kind == Some("text") && !text_field(block, "text").trim().is_empty() && !text_field(block, "text").trim_start().starts_with(SYSTEM_REMINDER_PREFIX) {
                let text = text_field(block, "text");
                self.add_item(if self.prompt.is_some() { "user" } else { "sys" }, &text, moment);
                if self.prompt.is_none() && self.last_action.is_none() {
                    self.last_action = Some(LastAction { kind: "sys", text: text.clone(), moment });
                }
                if self.prompt.is_none() {
                    self.prompt = Some(text);
                }
            }
        }
        if !saw_result {
            self.last_result_was_error = false;
        }
    }

    fn build_result(self) -> Value {
        let start = self.first_stamp.as_deref().and_then(parse_iso_date);
        let log: Vec<Value> = self
            .items
            .iter()
            .map(|item| {
                let mut value = item.value.clone();
                value.insert("t".into(), json!(format_elapsed_clock(start.as_ref(), item.moment.as_ref())));
                Value::Object(value)
            })
            .collect();
        let totals: Vec<f64> = self.usage_by_message.iter().filter(|(_, slot)| !slot.is_empty()).map(|(_, slot)| slot.values().sum()).collect();
        let finished = self.final_return.is_some() || self.last_stop_reason.as_deref() == Some(END_TURN_REASON);
        let final_return: Option<String> = self.final_return.clone().or_else(|| if finished { self.last_text.clone() } else { None });
        let last_action = self.last_action.as_ref().map(|action| {
            let text: String = redact_credentials(&action.text, None).split_whitespace().collect::<Vec<_>>().join(" ").chars().take(ACTION_LIMIT).collect();
            let clock = format_elapsed_clock(start.as_ref(), action.moment.as_ref());
            json!({"type": action.kind, "text": text, "t": if clock.is_empty() { Value::Null } else { json!(clock) }})
        });
        let source = self.final_return.clone().filter(|f| !f.is_empty()).or_else(|| self.last_text.clone()).unwrap_or_default();
        let first_line = pycompat::splitlines(&source).into_iter().map(str::trim).find(|l| !l.is_empty());
        let (mut feature, mut task) = (Value::Null, Value::Null);
        if let Some(prompt) = self.prompt.as_deref().filter(|p| !p.is_empty()) {
            let found = PROMPT_FEATURE_PATTERN.captures(prompt).or_else(|| ANY_CASE_FEATURE_PATTERN.captures(prompt)).or_else(|| PATH_FEATURE_PATTERN.captures(prompt));
            if let Some(found) = found {
                feature = json!(found[1].trim_end_matches('.'));
            }
            if let Some(found) = PROMPT_TASK_PATTERN.captures(prompt) {
                task = json!(found[1].to_uppercase());
            }
        }
        let duration = match (&self.first_stamp, &self.last_stamp) {
            (Some(a), Some(b)) => compute_seconds_between(a, b),
            _ => None,
        };
        let optional = |value: Option<String>| value.map(Value::String).unwrap_or(Value::Null);
        json!({
            "started_at": optional(self.first_stamp.clone()),
            "finished_at": optional(self.last_stamp.clone()),
            "duration_s": duration,
            "tokens": if totals.is_empty() { Value::Null } else { json!(totals.iter().sum::<f64>() as i64) },
            "feature": feature,
            "task": task,
            "summary": first_line.map(|l| redact_credentials(l, Some(SUMMARY_LIMIT))),
            "final": finished,
            "failed": (self.last_result_was_error || self.had_api_error) && self.final_return.is_none(),
            "last_action": last_action,
            "return_status": final_return.as_deref().and_then(extract_return_status),
            "decision": final_return.as_deref().and_then(extract_return_decision),
            "initial_prompt": self.prompt.as_deref().filter(|p| !p.is_empty()).map(|p| redact_credentials(p, Some(TRANSCRIPT_TEXT_LIMIT))),
            "final_return": final_return.as_deref().filter(|f| !f.is_empty()).map(|f| redact_credentials(f, Some(TRANSCRIPT_TEXT_LIMIT))),
            "log": log,
            "truncated": self.total_items > LOG_ITEMS_LIMIT,
        })
    }
}

/// Reads a .jsonl transcript. `None` = unreadable file.
pub fn parse_transcript_file(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    let mut reader = TranscriptReader::default();
    for line in String::from_utf8_lossy(&bytes).split_inclusive('\n') {
        reader.read_line(line);
    }
    Some(reader.build_result())
}

type Subagent = (String, PathBuf, PathBuf);

/// `(orchestrator transcript or None, [(agentId, jsonl, meta.json)])` of a session. A `session_id` outside the pattern = nothing.
pub fn locate_session_files(transcripts_dir: &Path, work_dir: &str, session_id: &str) -> (Option<PathBuf>, Vec<Subagent>) {
    if !SESSION_PATTERN.is_match(session_id) {
        return (None, vec![]);
    }
    for slug in list_project_slug_variants(work_dir) {
        let folder = transcripts_dir.join(slug);
        let main_path = folder.join(format!("{session_id}.jsonl"));
        let subagents_dir = folder.join(session_id).join("subagents");
        let (has_main, has_subagents) = (main_path.is_file(), subagents_dir.is_dir());
        if !(has_main || has_subagents) {
            continue;
        }
        let mut names: Vec<String> = if has_subagents {
            std::fs::read_dir(&subagents_dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default()
        } else {
            vec![]
        };
        names.sort();
        let subagents = names
            .iter()
            .filter_map(|name| {
                let found = SUBAGENT_FILE_PATTERN.captures(name)?;
                let id = found[1].to_string();
                Some((id.clone(), subagents_dir.join(name), subagents_dir.join(format!("agent-{id}.meta.json"))))
            })
            .collect();
        return (has_main.then_some(main_path), subagents);
    }
    (None, vec![])
}

/// `agentType` of the subagent's `.meta.json`; unreadable/missing = `None`.
pub fn read_agent_type(meta_path: &Path) -> Option<String> {
    let meta = super::loop_command_file::read_json_object(meta_path);
    meta.get("agentType").and_then(Value::as_str).filter(|t| !t.is_empty()).map(String::from)
}

/// `session_id` of `turns/NNN.json` when it has a safe format; otherwise `None`.
pub fn read_turn_session_id(work_dir: &str, turn_number: u64) -> Option<String> {
    let turn = super::loop_command_file::read_json_object(&loop_state::turns_dir(work_dir).join(format!("{turn_number:03}.json")));
    turn.get("session_id").and_then(Value::as_str).filter(|s| SESSION_PATTERN.is_match(s)).map(String::from)
}

/// `{"initial_prompt","actions":[{"type","text"}],"final_return","feature"}` redacted; `None` = missing/unreadable. Tolerates a corrupted line
/// and a missing field.
pub fn read_transcript_summary(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    let (mut prompt, mut handback_return, mut last_text, mut last_stop_reason): (Option<String>, Option<String>, Option<String>, Option<String>) = (None, None, None, None);
    let mut actions: Vec<Value> = Vec::new();
    let append_action = |actions: &mut Vec<Value>, kind: &str, text: &str| {
        if actions.len() < LOG_ITEMS_LIMIT {
            actions.push(json!({"type": kind, "text": redact_secrets(text, Some(TRANSCRIPT_TEXT_LIMIT))}));
        }
    };
    for line in String::from_utf8_lossy(&bytes).split_inclusive('\n') {
        let Ok(Value::Object(record)) = serde_json::from_str::<Value>(line) else { continue };
        let Some(message) = record.get("message").and_then(Value::as_object) else { continue };
        let record_type = record.get("type").and_then(Value::as_str).unwrap_or("");
        if !matches!(record_type, "assistant" | "user") || truthy(record.get("isMeta")) {
            continue;
        }
        let blocks = list_message_blocks(message.get("content").unwrap_or(&Value::Null));
        if record_type == "assistant" {
            if let Some(reason) = message.get("stop_reason").filter(|v| truthy(Some(v))) {
                last_stop_reason = Some(text_of(reason));
            }
            for block in blocks.iter().filter_map(Value::as_object) {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") if !text_field(block, "text").trim().is_empty() => {
                        let text = text_of(&block["text"]);
                        append_action(&mut actions, "ai", &text);
                        last_text = Some(text);
                    }
                    Some("tool_use") => {
                        let name = match block.get("name") {
                            Some(v) if truthy(Some(v)) => text_of(v),
                            _ => "tool".into(),
                        };
                        let input = block.get("input").filter(|v| v.is_object()).cloned().unwrap_or_else(|| json!({}));
                        if name == SUBAGENT_RETURN_TOOL {
                            let message = text_field(input.as_object().unwrap_or(&Map::new()), "message");
                            append_action(&mut actions, "ai", &message);
                            handback_return = Some(message);
                            continue;
                        }
                        append_action(&mut actions, "tool", &summarize_tool_call(&name, &input));
                    }
                    _ => {}
                }
            }
        } else if prompt.is_none() {
            for block in blocks.iter().filter_map(Value::as_object) {
                let text = if block.get("type").and_then(Value::as_str) == Some("text") { text_field(block, "text") } else { String::new() };
                if !text.trim().is_empty() && !text.trim_start().starts_with(SYSTEM_REMINDER_PREFIX) {
                    prompt = Some(text);
                    break;
                }
            }
        }
    }
    let finished = handback_return.is_some() || last_stop_reason.as_deref() == Some(END_TURN_REASON);
    let final_return = handback_return.or(if finished { last_text } else { None });
    let found = prompt
        .as_deref()
        .filter(|p| !p.is_empty())
        .and_then(|p| PROMPT_FEATURE_PATTERN.captures(p).or_else(|| ANY_CASE_FEATURE_PATTERN.captures(p)).or_else(|| PATH_FEATURE_PATTERN.captures(p)))
        .map(|c| c[1].trim_end_matches('.').to_string());
    Some(json!({
        "initial_prompt": prompt.as_deref().filter(|p| !p.is_empty()).map(|p| redact_secrets(p, Some(TRANSCRIPT_TEXT_LIMIT))),
        "actions": actions,
        "final_return": final_return.as_deref().filter(|f| !f.is_empty()).map(|f| redact_secrets(f, Some(TRANSCRIPT_TEXT_LIMIT))),
        "feature": found,
    }))
}

/// Latest modification (epoch seconds) of a session: its orchestrator transcript and every subagent transcript.
pub fn get_session_modified_time(folder: &Path, session_id: &str) -> Option<f64> {
    let mut paths = vec![folder.join(format!("{session_id}.jsonl"))];
    if let Ok(entries) = std::fs::read_dir(folder.join(session_id).join("subagents")) {
        paths.extend(entries.flatten().map(|e| e.path()));
    }
    paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .fold(None, |best: Option<f64>, stamp| Some(best.map_or(stamp, |b| b.max(stamp))))
}

type Cache = Vec<(PathBuf, ((u128, u64), Option<Value>))>;

/// Transcript reader with a cache by (mtime, size).
pub struct ClaudeTranscriptRepository {
    pub transcripts_dir: PathBuf,
    cache_limit: usize,
    cache: Mutex<Cache>,
    /// How many times a file was actually parsed (cache tests).
    pub parse_count: std::sync::atomic::AtomicUsize,
}

impl ClaudeTranscriptRepository {
    pub fn new(transcripts_dir: &str) -> Self {
        Self::with_cache_limit(transcripts_dir, TRANSCRIPT_CACHE_LIMIT)
    }

    pub fn with_cache_limit(transcripts_dir: &str, cache_limit: usize) -> Self {
        ClaudeTranscriptRepository { transcripts_dir: PathBuf::from(pycompat::nt_abspath(transcripts_dir)), cache_limit, cache: Mutex::new(Vec::new()), parse_count: 0.into() }
    }

    /// Parsed transcript with a cache by (mtime, size). `None` = missing or unreadable.
    pub fn read_transcript(&self, path: &Path) -> Option<Value> {
        let info = std::fs::metadata(path).ok()?;
        let mtime = info.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
        let stamp = (mtime, info.len());
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, (stored, parsed))) = cache.iter().find(|(p, _)| p == path) {
                if *stored == stamp {
                    return parsed.clone();
                }
            }
        }
        let parsed = parse_transcript_file(path);
        self.parse_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|(p, _)| p != path);
        while !cache.is_empty() && cache.len() >= self.cache_limit {
            cache.remove(0);
        }
        cache.push((path.to_path_buf(), (stamp, parsed.clone())));
        parsed
    }

    pub fn locate_session_files(&self, work_dir: &str, session_id: &str) -> (Option<PathBuf>, Vec<Subagent>) {
        locate_session_files(&self.transcripts_dir, work_dir, session_id)
    }

    /// Id of the most recent session of the project whose orchestrator transcript or subagent transcripts were modified after `modified_after`
    /// (epoch seconds); `None` when there is none. Best effort: any file system problem is "not found".
    pub fn find_live_session_id(&self, work_dir: &str, modified_after: f64) -> Option<String> {
        let (mut newest_id, mut newest_stamp): (Option<String>, f64) = (None, modified_after);
        for slug in list_project_slug_variants(work_dir) {
            let folder = self.transcripts_dir.join(slug);
            let Ok(entries) = std::fs::read_dir(&folder) else { continue };
            let mut ids: Vec<String> = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let id = name.strip_suffix(".jsonl").map(String::from).unwrap_or(name);
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            for session_id in ids.iter().filter(|i| SESSION_PATTERN.is_match(i)) {
                if let Some(stamp) = get_session_modified_time(&folder, session_id).filter(|s| *s > newest_stamp) {
                    newest_id = Some(session_id.clone());
                    newest_stamp = stamp;
                }
            }
            if newest_id.is_some() {
                return newest_id;
            }
        }
        None
    }

    pub fn read_agent_type(&self, meta_path: &Path) -> Option<String> {
        read_agent_type(meta_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(value: Value) -> String {
        format!("{}\n", py_json_dumps(&value, false))
    }

    fn sample() -> String {
        [
            line(json!({"type": "user", "timestamp": "2026-01-01T10:00:00Z", "message": {"content": "SCOPE_FEATURE: login-page. do it"}})),
            line(json!({"type": "assistant", "timestamp": "2026-01-01T10:00:05Z", "message": {"id": "m1", "usage": {"input_tokens": 10, "output_tokens": 2}, "content": [{"type": "text", "text": "plan"}, {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}}]}})),
            line(json!({"type": "assistant", "timestamp": "2026-01-01T10:00:06Z", "message": {"id": "m1", "usage": {"input_tokens": 10, "output_tokens": 5}, "content": []}})),
            line(json!({"type": "user", "timestamp": "2026-01-01T10:00:07Z", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1", "is_error": true, "content": "BLOCKED by the orchestrator guard [push-block]: no pushing. Do not try to work around it, ask."}]}})),
            "not json\n".to_string(),
            line(json!({"type": "assistant", "timestamp": "2026-01-01T10:01:00Z", "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": "STATUS: DONE\nSUMMARY: all good token=abcdef123456789"}]}})),
        ]
        .concat()
    }

    #[test]
    fn transcript_is_summarised_and_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, sample()).unwrap();
        let repository = ClaudeTranscriptRepository::new(&dir.path().display().to_string());
        let result = repository.read_transcript(&path).unwrap();
        assert_eq!((result["feature"].clone(), result["tokens"].clone(), result["duration_s"].clone(), result["final"].clone()), (json!("login-page"), json!(15), json!(60), json!(true)));
        assert_eq!((result["return_status"].clone(), result["failed"].clone()), (json!("DONE"), json!(false)));
        assert_eq!(result["decision"], "all good token=[redacted]");
        let log = result["log"].as_array().unwrap();
        assert_eq!(log.iter().map(|i| i["type"].as_str().unwrap()).collect::<Vec<_>>(), ["sys", "ai", "guard", "ai"]);
        assert_eq!((log[2]["rule"].clone(), log[2]["reason"].clone(), log[2]["t"].clone()), (json!("push-block"), json!("no pushing"), json!("00:00:05")));
        assert_eq!(log[2]["result"], "exit 2 · [push-block] no pushing");
        repository.read_transcript(&path);
        assert_eq!(repository.parse_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(repository.read_transcript(&dir.path().join("none.jsonl")).is_none());
        let summary = read_transcript_summary(&path).unwrap();
        assert_eq!((summary["feature"].clone(), summary["actions"].as_array().unwrap().len()), (json!("login-page"), 3));
    }

    #[test]
    fn sessions_are_located_by_slug_and_formats_are_strict() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("proj").display().to_string();
        let slug = list_project_slug_variants(&work)[0].clone();
        let sub = dir.path().join(&slug).join("abc").join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("agent-x1.jsonl"), "").unwrap();
        std::fs::write(sub.join("agent-x1.meta.json"), r#"{"agentType": "builder"}"#).unwrap();
        std::fs::write(sub.join("other.txt"), "").unwrap();
        let repository = ClaudeTranscriptRepository::new(&dir.path().display().to_string());
        let (main, subagents) = repository.locate_session_files(&work, "abc");
        assert!(main.is_none());
        assert_eq!(subagents.len(), 1);
        assert_eq!(repository.read_agent_type(&subagents[0].2).as_deref(), Some("builder"));
        assert_eq!(repository.locate_session_files(&work, "../abc"), (None, vec![]));
        assert_eq!(repository.find_live_session_id(&work, 0.0).as_deref(), Some("abc"));
        assert_eq!(repository.find_live_session_id(&work, 1e12), None);
        assert_eq!(extract_return_status("blocked: nope").as_deref(), Some("BLOCKED"));
        assert_eq!(extract_return_decision("FILES: a\nFINDINGS:\n- first\n- second").as_deref(), Some("first"));
        assert_eq!(extract_return_decision("FILES: a\njust text").as_deref(), Some("just text"));
    }
}
