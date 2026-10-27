// SPDX-License-Identifier: MIT
//! Turns the timestamped sources of the error center into durable, turn-stamped events.
//!
//! Sources: the guard blocks of `internal/guard.log` and the problem lines of `internal/monitor-relaunch.log`. Each line becomes one event with a
//! stable id (a hash of its own content plus its ordinal among identical lines), so reading the same line again never duplicates it. The turn of an
//! event is, in order: the `turn` the guard hook wrote in the line, the turn the launch log printed it in, else the turn whose time window holds
//! the line's timestamp (`TurnTimeline`). Unknown stays `None`: nothing is invented, and an event whose turn was unknown is filled in by a later
//! sync once the turn's file exists. Events go to `internal/events/errors.jsonl` (append only), so they outlive a rotated or truncated source
//! log. Texts are redacted and clipped BEFORE they are stored.

use super::guard_events::BLOCK_DECISIONS;
use super::loop_parsers::{parse_epoch, Launch, TurnTimeline, LAUNCH_PROBLEM_PATTERN};
use super::App;
use crate::repositories::loop_read_formatting::{clip_redacted_text, text_or_empty};
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use std::collections::HashMap;

pub const KIND_GUARD_EVENT: &str = "guard";
pub const KIND_LAUNCH_EVENT: &str = "launch";
/// The planning chat runs before any turn: its blocks belong to no turn.
const PLANNING_PROFILE: &str = "planning";
const EVENT_DETAIL_LIMIT: usize = 200;
const EVENT_ATTEMPT_LIMIT: usize = 200;
const EVENT_LINE_LIMIT: usize = 300;
const EVENT_FIELD_LIMIT: usize = 60;
const EVENT_ID_LENGTH: usize = 16;

fn make_event_id(prefix: &str, parts: &[String]) -> String {
    let digest = Sha1::digest(parts.join("\u{1f}").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}-{}", &hex[..EVENT_ID_LENGTH])
}

fn as_turn_number(value: Option<&Value>) -> Value {
    match value.and_then(Value::as_i64) {
        Some(n) if n > 0 && value.is_some_and(|v| !v.is_boolean()) => json!(n),
        _ => Value::Null,
    }
}

fn or_null(text: String) -> Value {
    if text.is_empty() { Value::Null } else { json!(text) }
}

pub struct ErrorEventSyncService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn error_event_sync(&self) -> ErrorEventSyncService<'_> {
        ErrorEventSyncService { app: self }
    }
}

impl ErrorEventSyncService<'_> {
    /// Stores what the sources hold now and returns every known event (oldest first), each with its best-known turn.
    pub fn sync_events(&self, work_dir: &str, timeline: &TurnTimeline, launches: &[Launch]) -> Vec<Map<String, Value>> {
        let mut events: Vec<Map<String, Value>> = self.app.error_events.read_events(work_dir);
        let mut index: HashMap<String, usize> = events.iter().enumerate().filter_map(|(i, e)| e.get("id").and_then(Value::as_str).map(|id| (id.to_string(), i))).collect();
        let mut writes: Vec<Value> = Vec::new();
        let derived = self.derive_guard_events(work_dir).into_iter().chain(self.derive_launch_events(launches));
        for mut event in derived {
            let id = event.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            if index.contains_key(&id) {
                continue;
            }
            if event.get("turn").is_none_or(Value::is_null) {
                event.insert("turn".into(), Self::find_turn(&event, timeline).map_or(Value::Null, |t| json!(t)));
            }
            writes.push(Value::Object(event.clone()));
            index.insert(id, events.len());
            events.push(event);
        }
        for event in events.iter_mut() {
            // history whose turn was unknown (the turn file did not exist yet)
            if event.get("turn").is_none_or(Value::is_null) {
                if let Some(turn) = Self::find_turn(event, timeline) {
                    event.insert("turn".into(), json!(turn));
                    writes.push(json!({"id": event.get("id"), "turn": turn}));
                }
            }
        }
        self.app.error_events.append_events(work_dir, &writes);
        events
    }

    fn find_turn(event: &Map<String, Value>, timeline: &TurnTimeline) -> Option<i64> {
        if event.get("profile").and_then(Value::as_str) == Some(PLANNING_PROFILE) {
            return None;
        }
        let when = text_or_empty(event.get("t"));
        if when.is_empty() { None } else { timeline.turn_at(parse_epoch(&when)) }
    }

    fn derive_guard_events(&self, work_dir: &str) -> Vec<Map<String, Value>> {
        let mut events = Vec::new();
        let mut seen: HashMap<Vec<String>, i64> = HashMap::new();
        for line in self.app.loop_files.read_guard_lines(work_dir) {
            let Some(line) = line.as_object() else { continue };
            if !BLOCK_DECISIONS.contains(&text_or_empty(line.get("decision")).trim().to_lowercase().as_str()) {
                continue;
            }
            let fields: Vec<String> = ["t", "profile", "rule", "detail", "attempt"].iter().map(|name| text_or_empty(line.get(*name))).collect();
            let ordinal = seen.entry(fields.clone()).and_modify(|n| *n += 1).or_insert(0);
            let mut id_parts = fields.clone();
            id_parts.push(ordinal.to_string());
            let rule = clip_redacted_text(&fields[2], EVENT_FIELD_LIMIT);
            let mut event = Map::new();
            event.insert("id".into(), json!(make_event_id("g", &id_parts)));
            event.insert("kind".into(), json!(KIND_GUARD_EVENT));
            event.insert("t".into(), or_null(clip_redacted_text(&fields[0], EVENT_FIELD_LIMIT)));
            event.insert("turn".into(), as_turn_number(line.get("turn")));
            event.insert("profile".into(), or_null(clip_redacted_text(&fields[1], EVENT_FIELD_LIMIT)));
            event.insert("rule".into(), json!(if rule.is_empty() { "unknown".to_string() } else { rule }));
            event.insert("detail".into(), json!(clip_redacted_text(&fields[3], EVENT_DETAIL_LIMIT)));
            event.insert("attempt".into(), or_null(clip_redacted_text(&fields[4], EVENT_ATTEMPT_LIMIT)));
            events.push(event);
        }
        events
    }

    fn derive_launch_events(&self, launches: &[Launch]) -> Vec<Map<String, Value>> {
        let mut events = Vec::new();
        for launch in launches {
            let mut seen: HashMap<&str, i64> = HashMap::new();
            for (text, line_turn) in &launch.lines {
                if !LAUNCH_PROBLEM_PATTERN.is_match(text) {
                    continue;
                }
                let ordinal = seen.entry(text.as_str()).and_modify(|n| *n += 1).or_insert(0);
                let turn = line_turn.or(launch.first_turn);
                let mut event = Map::new();
                event.insert("id".into(), json!(make_event_id("l", &[launch.marker.clone(), text.clone(), ordinal.to_string()])));
                event.insert("kind".into(), json!(KIND_LAUNCH_EVENT));
                event.insert("t".into(), json!(launch.time));
                event.insert("turn".into(), json!(turn));
                event.insert("launch".into(), json!(launch.marker));
                event.insert("line".into(), json!(clip_redacted_text(text, EVENT_LINE_LIMIT)));
                events.push(event);
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn guard_lines_become_stable_events_stamped_with_their_turn() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let work = dir.path().join("w");
        let w = work.display().to_string();
        let guard = crate::repositories::loop_command_file::get_loop_internal_dir(&w).join("guard.log");
        std::fs::create_dir_all(guard.parent().unwrap()).unwrap();
        let line = json!({"t": "2026-01-01T10:00:05", "profile": "loop", "rule": "git-write", "decision": "blocked", "detail": "git commit", "turn": 0});
        let other = json!({"t": "2026-01-01T10:00:06", "profile": "planning", "rule": "specs-only", "decision": "allowed", "detail": "x"});
        std::fs::write(&guard, format!("{line}\n{line}\n{other}\n")).unwrap();
        let start = parse_epoch("2026-01-01T10:00:00").unwrap();
        let timeline = TurnTimeline::build(&BTreeMap::from([(1, start + 60.0)]), &BTreeMap::from([(1, start)]), &BTreeMap::new(), None);
        let service = app.error_event_sync();
        let events = service.sync_events(&w, &timeline, &[]);
        assert_eq!(events.len(), 2); // two identical lines = two events (ordinal); the allowed line is no event
        assert_eq!((events[0]["turn"].clone(), events[0]["rule"].clone()), (json!(1), json!("git-write")));
        assert_ne!(events[0]["id"], events[1]["id"]);
        let again = service.sync_events(&w, &timeline, &[]);
        assert_eq!(again.len(), 2);
        assert_eq!(app.error_events.read_events(&w).len(), 2);
    }
}
