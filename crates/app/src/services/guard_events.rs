// SPDX-License-Identifier: MIT
//! Guard events of a project for the panel: the latest blocks of the plugin's PreToolUse hook and how many per rule.
//!
//! Source: `<work>/.specs/project/loop/internal/guard.log`, one JSON line per decision (`{t, turn, profile, rule, decision, detail, attempt}`)
//! written by the guard. Only `blocked` decisions count (info and allowed-by-user lines are left out). The raw file never leaves through the API:
//! each field is clipped and the detail goes through credential redaction.
//!
//! The run-time egress denials of the project (`<data dir>/egress.log`, rule `egress-runtime`) come in through the same repository read
//! (`read_guard_lines` merges them, already in the guard-line shape) and are listed and counted like any other block. A line carries a `turn` when it
//! has one; the item then has a `turn` key too.

use super::App;
use crate::repositories::loop_read_formatting::clip_redacted_text;
use serde_json::{json, Map, Value};

pub const BLOCK_DECISIONS: [&str; 2] = ["blocked", "block"];
pub const GUARD_EVENTS_LIMIT: usize = 20;
const GUARD_DETAIL_LIMIT: usize = 200;
const GUARD_FIELD_LIMIT: usize = 60;
const UNKNOWN_RULE: &str = "unknown";

fn text(line: &Value, key: &str) -> String {
    match line.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

pub fn build_guard_event(line: &Value) -> Value {
    let rule = Some(clip_redacted_text(&text(line, "rule"), GUARD_FIELD_LIMIT)).filter(|r| !r.is_empty()).unwrap_or_else(|| UNKNOWN_RULE.into());
    let optional = |key: &str| Some(clip_redacted_text(&text(line, key), GUARD_FIELD_LIMIT)).filter(|s| !s.is_empty()).map(Value::from).unwrap_or(Value::Null);
    let mut event = Map::new();
    event.insert("t".into(), optional("t"));
    event.insert("profile".into(), optional("profile"));
    event.insert("rule".into(), json!(rule));
    event.insert("detail".into(), json!(clip_redacted_text(&text(line, "detail"), GUARD_DETAIL_LIMIT)));
    if let Some(turn) = line.get("turn").filter(|t| t.is_i64() || t.is_u64()).and_then(Value::as_i64).filter(|t| *t > 0) {
        event.insert("turn".into(), json!(turn));
    }
    Value::Object(event)
}

pub struct GuardEventService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn guard_events(&self) -> GuardEventService<'_> {
        GuardEventService { app: self }
    }
}

impl GuardEventService<'_> {
    /// `{"events": last GUARD_EVENTS_LIMIT blocks, newest first, "counts": {rule: blocks}, "groups": [...]}` (counts and groups cover the whole log).
    /// `groups` is X2: the same refusal repeated 111 times is ONE row `{rule, detail, count, first_t, last_t, caller}` (`caller` = the command or
    /// tool call that made it, redacted), the most frequent first, so the panel can say "blocked 111 times" and who did it.
    pub fn build_guard_summary(&self, work_dir: &str) -> Value {
        let lines = self.app.loop_files.read_guard_lines(work_dir);
        let blocked_lines: Vec<&Value> = lines.iter().filter(|line| BLOCK_DECISIONS.contains(&text(line, "decision").trim().to_lowercase().as_str())).collect();
        let blocks: Vec<Value> = blocked_lines.iter().map(|line| build_guard_event(line)).collect();
        let groups = group_blocks(&blocked_lines);
        let mut counts = Map::new();
        for event in &blocks {
            let rule = text(event, "rule");
            let count = counts.get(&rule).and_then(Value::as_i64).unwrap_or(0) + 1;
            counts.insert(rule, json!(count));
        }
        let recent: Vec<&Value> = blocks.iter().rev().take(GUARD_EVENTS_LIMIT).collect();
        json!({"events": recent, "counts": counts, "groups": groups})
    }
}

const GUARD_GROUPS_LIMIT: usize = 10;

/// One row per (rule, detail), most repeated first; the first caller seen is kept.
fn group_blocks(lines: &[&Value]) -> Vec<Value> {
    let mut rows: Vec<(String, String, i64, Value, Value, String)> = Vec::new();
    for line in lines {
        let event = build_guard_event(line);
        let (rule, detail) = (text(&event, "rule"), text(&event, "detail"));
        let caller = clip_redacted_text(&text(line, "attempt"), GUARD_DETAIL_LIMIT);
        match rows.iter_mut().find(|r| r.0 == rule && r.1 == detail) {
            Some(row) => {
                row.2 += 1;
                row.4 = event["t"].clone();
            }
            None => rows.push((rule, detail, 1, event["t"].clone(), event["t"].clone(), caller)),
        }
    }
    rows.sort_by(|a, b| b.2.cmp(&a.2));
    rows.into_iter().take(GUARD_GROUPS_LIMIT).map(|(rule, detail, count, first_t, last_t, caller)| json!({"rule": rule, "detail": detail, "count": count, "first_t": first_t, "last_t": last_t, "caller": caller})).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_blocks_count_and_the_newest_come_first() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let work = dir.path().display().to_string();
        let log = engine_state::loop_state::loop_path(&work, "guard.log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        let lines: Vec<String> = (0..25)
            .map(|i| json!({"t": format!("2026-01-01T10:00:{i:02}"), "turn": if i == 3 { json!(2) } else { Value::Null }, "profile": "orchestrator", "rule": if i % 2 == 0 { "push-block" } else { "state-write" }, "decision": if i == 1 { "allowed" } else { "blocked" }, "detail": format!("token=abcdefghij12345 {i}")}).to_string())
            .collect();
        std::fs::write(&log, lines.join("\n")).unwrap();
        let summary = app.guard_events().build_guard_summary(&work);
        let events = summary["events"].as_array().unwrap();
        assert_eq!(events.len(), 20);
        assert_eq!((events[0]["t"].clone(), events[0]["detail"].clone()), (json!("2026-01-01T10:00:24"), json!("token=[redacted] 24")));
        assert_eq!(summary["counts"], json!({"push-block": 13, "state-write": 11}));
        assert_eq!(build_guard_event(&json!({"turn": 4, "rule": ""}))["turn"], 4);
    }

    #[test]
    fn repeats_are_one_row_with_a_count_and_the_caller() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let work = dir.path().display().to_string();
        let log = engine_state::loop_state::loop_path(&work, "guard.log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        let mut lines: Vec<String> = (0..5)
            .map(|i| json!({"t": format!("2026-01-01T10:00:0{i}"), "rule": "egress-runtime", "decision": "blocked", "detail": "host.example.com is not in the network list", "attempt": "Bash tool-x --password=abcdefghijklmnopqrstuvwxyz1234"}).to_string())
            .collect();
        lines.push(
            json!({"t": "2026-01-01T10:01:00", "rule": "egress-host", "decision": "blocked", "detail": "other.example.com", "attempt": "curl other.example.com"})
                .to_string(),
        );
        std::fs::write(
            &log,
            lines.join(
                "
",
            ),
        )
        .unwrap();
        let groups = app.guard_events().build_guard_summary(&work)["groups"].clone();
        assert_eq!(groups.as_array().unwrap().len(), 2);
        assert_eq!(
            (groups[0]["count"].clone(), groups[0]["first_t"].clone(), groups[0]["last_t"].clone()),
            (json!(5), json!("2026-01-01T10:00:00"), json!("2026-01-01T10:00:04"))
        );
        assert!(
            groups[0]["caller"].as_str().unwrap().starts_with("Bash tool-x") && !groups[0]["caller"].as_str().unwrap().contains("abcdefghijklmnopqrstuvwxyz1234"),
            "{}",
            groups[0]["caller"]
        );
        assert_eq!((groups[1]["count"].clone(), groups[1]["caller"].clone()), (json!(1), json!("curl other.example.com")));
        assert_eq!(build_guard_event(&json!({"turn": true}))["rule"], "unknown");
    }
}
