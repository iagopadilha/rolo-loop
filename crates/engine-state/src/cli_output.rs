// SPDX-License-Identifier: MIT
//! One tolerant parser for the output of `claude -p ... --output-format json|stream-json`.
//!
//! Used by the engine and by the app's panel jobs, so both read the CLI the same way.
//!
//! Accepted shapes, so a change in the CLI output does not silently break a turn:
//!   - one JSON object (the `--output-format json` contract);
//!   - a JSON array of events (what `--verbose` prints): the last event with `type == "result"` wins;
//!   - stream-json (one JSON object per line): the last `result` event wins;
//!   - any of the above with non-JSON noise before or after it (a warning line, an update notice, trailing text).
//!
//! [`parse`] always returns an object and never fails:
//!   recognised:   `{"ok": true, <every field of the result event>, plus the normalised fields below}`
//!   unrecognised: `{"ok": false, "reason": "unrecognized_output", "detail": <why>, "sample": <first 300 chars, redacted>}`
//!
//! Normalised fields (missing, extra or wrongly typed fields never crash the parser): result (str), is_error (bool), subtype (str),
//! total_cost_usd (float or null, falls back to `cost_usd`), duration_ms (int or null), num_turns (int or null), session_id (str),
//! usage (object), permission_denials (list of objects), api_error_status (int or null: the HTTP status of a failed API call),
//! rate_limit (object, see below).
//!
//! `rate_limit` comes from the LAST `rate_limit_event` of a stream-json output (`{}` when there is none or it is unreadable):
//! `{"status", "limit_type", "overage_status", "reset_at", "windows": {"five_hour": {"pct": 60.0, "reset_at": 1792702800}, ...}}`.
//! `pct` is 0-100 (the CLI prints a fraction; a value already above 1 is kept as it is).

use crate::secret_redaction::{redact, REDACTION_MARK};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

/// The newest `claude --version` this contract was verified against. An older CLI only gets a warning in `doctor`, never a failure.
pub const MIN_TESTED_CLAUDE_VERSION: &str = "2.1.301";
pub const UNRECOGNIZED: &str = "unrecognized_output";
const SAMPLE_LIMIT: usize = 300;
/// Redacted BEFORE it is clipped to SAMPLE_LIMIT: a secret cut in half by the clip would no longer match.
const SAMPLE_READ_LIMIT: usize = 20000;
pub const RATE_LIMIT_EVENT: &str = "rate_limit_event";
/// Keys that only a result event carries: an object without `type` is accepted as the result when it has one of them.
const RESULT_KEYS: [&str; 7] = ["result", "is_error", "total_cost_usd", "cost_usd", "session_id", "subtype", "num_turns"];
const MAX_DECODE_ATTEMPTS: usize = 2000;

static VERSION: LazyLock<Re> = LazyLock::new(|| Re::new(r"(\d+)\.(\d+)\.(\d+)"));

/// Removes what looks like a credential (see [`crate::secret_redaction`]).
pub fn redact_text(text: &str, limit: Option<usize>) -> String {
    redact(text, limit, REDACTION_MARK)
}

#[derive(Clone, Copy, PartialEq)]
enum Num {
    Int,
    Float,
}

fn number(value: Option<&Value>, kind: Num) -> Value {
    let f = match value {
        None | Some(Value::Null) | Some(Value::Bool(_)) => return Value::Null,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => match s.trim().parse::<f64>() {
            Ok(f) => f,
            Err(_) => return Value::Null,
        },
        Some(_) => return Value::Null,
    };
    if !f.is_finite() {
        return Value::Null;
    }
    match kind {
        Num::Int => json!(f as i64),
        Num::Float => json!(f),
    }
}

fn boolean(value: &Value) -> bool {
    match value {
        Value::String(s) => matches!(s.trim().to_lowercase().as_str(), "true" | "1" | "yes"),
        other => crate::notify_truthy(Some(other)),
    }
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

fn denials(value: Option<&Value>) -> Vec<Value> {
    let items: Vec<Value> = match value {
        None | Some(Value::Null) => return vec![],
        Some(Value::Array(a)) => a.clone(),
        Some(other) => vec![other.clone()],
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            Value::Object(_) => Some(item),
            Value::Null => None,
            other => Some(json!({"tool_name": "", "tool_input": {"command": text_of(Some(&other))}})),
        })
        .collect()
}

/// Every top-level JSON value found in the text, in order: the whole text when it is JSON, else each object/array that decodes from a
/// `{` or `[` (noise between them is skipped).
fn decoded_values(text: &str) -> Vec<Value> {
    if let Ok(whole) = serde_json::from_str::<Value>(text) {
        return vec![whole];
    }
    let (mut values, mut position, mut attempts) = (Vec::new(), 0usize, 0usize);
    while attempts < MAX_DECODE_ATTEMPTS {
        let Some(found) = text[position..].find(['[', '{']) else { break };
        let start = position + found;
        attempts += 1;
        let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(value)) => {
                position = start + stream.byte_offset();
                values.push(value);
            }
            _ => position = start + 1,
        }
    }
    values
}

fn events(values: Vec<Value>) -> Vec<Map<String, Value>> {
    let mut out = Vec::new();
    for value in values {
        match value {
            Value::Object(m) => out.push(m),
            Value::Array(items) => out.extend(items.into_iter().filter_map(|i| if let Value::Object(m) = i { Some(m) } else { None })),
            _ => {}
        }
    }
    out
}

fn event_type(event: &Map<String, Value>) -> String {
    event.get("type").map(|t| text_of(Some(t))).filter(|t| !t.is_empty()).unwrap_or_default().to_lowercase()
}

fn pick_result(events: &[Map<String, Value>]) -> Option<&Map<String, Value>> {
    if let Some(e) = events.iter().rev().find(|e| event_type(e) == "result") {
        return Some(e);
    }
    events.iter().rev().find(|e| !e.contains_key("type") && RESULT_KEYS.iter().any(|k| e.contains_key(*k)))
}

/// Utilization -> 0-100 float: the CLI prints a fraction (0.6), a value above 1 is taken as already in percent; `None` when unreadable.
fn percent(value: Option<&Value>) -> Option<f64> {
    let n = number(value, Num::Float).as_f64()?;
    if n < 0.0 {
        return None;
    }
    let scaled = if n <= 1.0 { n * 100.0 } else { n };
    Some((scaled * 10000.0).round_ties_even() / 10000.0)
}

/// `rate_limit_event` -> `{"status", "limit_type", "overage_status", "reset_at", "windows"}`; `{}` when the event has no readable
/// `rate_limit_info`. A window without a readable utilization is left out.
pub fn normalize_rate_limit(event: &Map<String, Value>) -> Value {
    let Some(Value::Object(info)) = event.get("rate_limit_info") else { return json!({}) };
    let mut windows = Map::new();
    if let Some(Value::Object(raw)) = info.get("unifiedWindows") {
        for (key, window) in raw {
            let Value::Object(window) = window else { continue };
            if let Some(pct) = percent(window.get("utilization")) {
                windows.insert(key.clone(), json!({"pct": pct, "reset_at": number(window.get("resetsAt"), Num::Int)}));
            }
        }
    }
    let (status, limit_type) = (text_of(info.get("status")), text_of(info.get("rateLimitType")));
    if windows.is_empty() && status.is_empty() {
        return json!({});
    }
    json!({"status": status, "limit_type": limit_type, "overage_status": text_of(info.get("overageStatus")), "reset_at": number(info.get("resetsAt"), Num::Int),
           "windows": windows})
}

fn pick_rate_limit(events: &[Map<String, Value>]) -> Value {
    events.iter().rev().find(|e| event_type(e) == RATE_LIMIT_EVENT).map(normalize_rate_limit).unwrap_or_else(|| json!({}))
}

/// The output without its `rate_limit_event` lines: that event's name contains "rate_limit", which a search for a usage-limit message would
/// take for one. Use it before looking for limit or authentication text in a stream-json output.
pub fn strip_rate_limit_events(text: &str) -> String {
    if !text.contains(RATE_LIMIT_EVENT) {
        return text.to_string();
    }
    pycompat::splitlines(text).into_iter().filter(|l| !l.contains(RATE_LIMIT_EVENT)).collect::<Vec<_>>().join("\r\n")
}

/// Result event -> object with every original field plus the normalised ones and `ok: true`.
pub fn normalize(event: &Map<String, Value>, rate_limit: Value) -> Value {
    let mut data = event.clone();
    let mut cost = number(event.get("total_cost_usd"), Num::Float);
    if cost.is_null() {
        cost = number(event.get("cost_usd"), Num::Float);
    }
    let subtype = text_of(event.get("subtype"));
    let is_error = match event.get("is_error") {
        Some(v) => boolean(v),
        None => subtype.to_lowercase().starts_with("error"),
    };
    let usage = match event.get("usage") {
        Some(u @ Value::Object(_)) => u.clone(),
        _ => json!({}),
    };
    let rate_limit = if crate::notify_truthy(Some(&rate_limit)) { rate_limit } else { json!({}) };
    for (key, value) in [
        ("ok", json!(true)),
        ("type", json!("result")),
        ("result", json!(text_of(event.get("result")))),
        ("is_error", json!(is_error)),
        ("subtype", json!(subtype)),
        ("total_cost_usd", cost),
        ("duration_ms", number(event.get("duration_ms"), Num::Int)),
        ("num_turns", number(event.get("num_turns"), Num::Int)),
        ("session_id", json!(text_of(event.get("session_id")))),
        ("usage", usage),
        ("permission_denials", Value::Array(denials(event.get("permission_denials")))),
        ("api_error_status", number(event.get("api_error_status"), Num::Int)),
        ("rate_limit", rate_limit),
    ] {
        data.insert(key.into(), value);
    }
    Value::Object(data)
}

pub fn unrecognized(text: &str, detail: &str) -> Value {
    let head: String = text.trim().chars().take(SAMPLE_READ_LIMIT).collect();
    json!({"ok": false, "reason": UNRECOGNIZED, "detail": detail, "sample": redact_text(&head, Some(SAMPLE_LIMIT))})
}

/// Output of `claude -p --output-format json|stream-json` -> normalised result object (see the module text).
pub fn parse(text: &str) -> Value {
    let text = text.trim().trim_start_matches('\u{feff}');
    if text.is_empty() {
        return unrecognized(text, "empty output");
    }
    let found = events(decoded_values(text));
    if found.is_empty() {
        return unrecognized(text, "no JSON object in the output");
    }
    match pick_result(&found) {
        None => unrecognized(text, "JSON found but no result event (type=result) in it"),
        Some(event) => normalize(event, pick_rate_limit(&found)),
    }
}

/// `2.1.301 (Claude Code)` -> `(2, 1, 301)`; `None` when there is no x.y.z in the text.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let m = VERSION.search(text)?;
    let part = |i| m.group(i).and_then(|s| s.parse::<u64>().ok());
    Some((part(1)?, part(2)?, part(3)?))
}

/// What `doctor` shows about `claude --version`: `{"version", "min_tested_version", "older_than_tested", "warning"}`. Never a failure.
pub fn describe_version(version_output: &str) -> Value {
    let version = parse_version(version_output);
    let minimum = parse_version(MIN_TESTED_CLAUDE_VERSION);
    let shown = version.map(|(a, b, c)| format!("{a}.{b}.{c}")).unwrap_or_default();
    let older = version.is_some() && version < minimum;
    let warning = if version.is_none() {
        format!("could not read the `claude --version` output; the JSON contract was tested with {MIN_TESTED_CLAUDE_VERSION}")
    } else if older {
        format!("claude {shown} is older than the version this harness was tested with ({MIN_TESTED_CLAUDE_VERSION}); update Claude Code if a turn fails with unrecognized_output")
    } else {
        String::new()
    };
    json!({"version": shown, "min_tested_version": MIN_TESTED_CLAUDE_VERSION, "older_than_tested": older, "warning": warning})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_json_array_and_stream_shapes() {
        let one = parse(r#"{"type":"result","result":"done","is_error":false,"total_cost_usd":0.5,"session_id":"s1","permission_denials":[]}"#);
        assert_eq!((one["ok"].clone(), one["result"].clone(), one["total_cost_usd"].clone()), (json!(true), json!("done"), json!(0.5)));
        let stream = "warning: update available\n{\"type\":\"system\"}\n{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"allowed_warning\",\"rateLimitType\":\"five_hour\",\"resetsAt\":1792702800,\"unifiedWindows\":{\"five_hour\":{\"utilization\":0.6,\"resetsAt\":1792702800},\"seven_day\":{\"utilization\":33,\"resetsAt\":1793000000},\"bad\":{\"utilization\":\"x\"}}}}\n{\"type\":\"result\",\"result\":\"ok\",\"cost_usd\":\"1.25\",\"num_turns\":\"3\"}\ntrailing";
        let data = parse(stream);
        assert_eq!(
            (data["ok"].clone(), data["total_cost_usd"].clone(), data["num_turns"].clone(), data["is_error"].clone()),
            (json!(true), json!(1.25), json!(3), json!(false))
        );
        assert_eq!(data["rate_limit"]["windows"]["five_hour"], json!({"pct": 60.0, "reset_at": 1792702800}));
        assert_eq!(data["rate_limit"]["windows"]["seven_day"]["pct"], json!(33.0));
        assert!(data["rate_limit"]["windows"].get("bad").is_none());
        assert_eq!(data["rate_limit"]["status"], "allowed_warning");
        let array = parse(r#"[{"type":"assistant"},{"type":"result","subtype":"error_max_turns","result":null}]"#);
        assert_eq!((array["is_error"].clone(), array["result"].clone()), (json!(true), json!("")));
        let typeless = parse(r#"{"result":"x","session_id":"a"}"#);
        assert_eq!(typeless["ok"], json!(true));
    }

    #[test]
    fn unrecognised_output_is_described_and_redacted() {
        assert_eq!(parse("")["detail"], "empty output");
        assert_eq!(parse("not json at all")["detail"], "no JSON object in the output");
        let no_result = parse(r#"{"type":"assistant","password":"hunter22x"}"#);
        assert_eq!(no_result["reason"], UNRECOGNIZED);
        assert_eq!(no_result["detail"], "JSON found but no result event (type=result) in it");
        assert!(no_result["sample"].as_str().unwrap().contains("[redacted]") && !no_result["sample"].as_str().unwrap().contains("hunter22x"));
        assert_eq!(parse(r#"{"type":"result","permission_denials":["rm x"]}"#)["permission_denials"], json!([{"tool_name": "", "tool_input": {"command": "rm x"}}]));
    }

    #[test]
    fn helpers() {
        assert_eq!(strip_rate_limit_events("a\n{\"type\":\"rate_limit_event\"}\nb"), "a\r\nb");
        assert_eq!(strip_rate_limit_events("plain"), "plain");
        assert_eq!(parse_version("2.1.301 (Claude Code)"), Some((2, 1, 301)));
        assert_eq!(describe_version("2.0.9")["older_than_tested"], json!(true));
        assert_eq!(describe_version("garbage")["version"], "");
        assert_eq!(describe_version("3.0.0")["warning"], "");
    }
}
