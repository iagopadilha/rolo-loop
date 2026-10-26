// SPDX-License-Identifier: MIT
//! The 5-hour and weekly usage windows of a Claude account, read from Claude Code's own output.
//!
//! Source: `claude -p ... --output-format stream-json --verbose` prints a `rate_limit_event`; `cli_output::parse(...)["rate_limit"]` is its
//! normalised form: `{"status", "limit_type", "overage_status", "reset_at", "windows": {"five_hour": {"pct": 60.0, "reset_at": 1792702800}, ...}}`.
//! `pct` is 0-100 and `reset_at` is epoch seconds. Nothing here calls the network or reads a credential.
//!
//! Blocking window: the window that stopped the account. A window with `pct >= 100` (and a reset still ahead) blocks; when several are full
//! the one that frees up LAST blocks (the account is usable only when all of them reset). A rejected status with no full window blocks
//! through its `limit_type`.

use crate::loop_io::{iso_seconds, local_from_epoch, local_now};
use serde_json::{json, Map, Value};
use time::PrimitiveDateTime;

pub const FIVE_HOUR: &str = "five_hour";
pub const SEVEN_DAY: &str = "seven_day";
const FULL_PERCENT: f64 = 100.0;
/// Fixed English names: no dependence on the machine's locale.
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

fn window_map(rate_limit: &Value) -> Map<String, Value> {
    match rate_limit.get("windows") {
        Some(Value::Object(w)) => w.iter().filter(|(_, v)| v.is_object()).map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => Map::new(),
    }
}

/// `{key: {"pct", "reset_at"}}` of the parse result's `rate_limit` (`{}` when it has none).
pub fn windows_of(rate_limit: &Value) -> Value {
    Value::Object(window_map(rate_limit))
}

fn epoch_of(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(n)) => n.as_f64().map(|f| f as i64),
        _ => None,
    }
}

/// Local naive datetime of an epoch-seconds value; `None` when it is not a usable number.
pub fn moment_of(epoch: Option<&Value>) -> Option<PrimitiveDateTime> {
    epoch_of(epoch).and_then(local_from_epoch)
}

/// Copy of the windows with `reset_local` (local ISO text of `reset_at`, empty when unknown) added to each: what the turn log keeps.
pub fn with_local_reset(windows: &Value) -> Value {
    let mut out = Map::new();
    if let Some(map) = windows.as_object() {
        for (key, window) in map {
            let local = moment_of(window.get("reset_at")).map(iso_seconds).unwrap_or_default();
            out.insert(key.clone(), json!({"pct": window.get("pct").cloned().unwrap_or(Value::Null), "reset_at": window.get("reset_at").cloned().unwrap_or(Value::Null), "reset_local": local}));
        }
    }
    Value::Object(out)
}

/// True when the rate limit status says the call was refused (`rejected`); `allowed` and `allowed_warning` are not.
pub fn is_rejected(status: &str) -> bool {
    status.trim().to_lowercase().starts_with("reject")
}

/// The window that stops the account right now: `{"key", "pct", "reset_at"}` (`reset_at` may be null) or `None` when nothing blocks. A full
/// window whose reset already passed is stale and does not block.
pub fn blocking_window(rate_limit: &Value, now: Option<PrimitiveDateTime>) -> Option<Value> {
    let now = now.unwrap_or_else(local_now);
    let windows = window_map(rate_limit);
    let mut full: Vec<Value> = Vec::new();
    for (key, window) in &windows {
        let pct = window.get("pct").and_then(Value::as_f64);
        let reset = moment_of(window.get("reset_at"));
        if pct.is_some_and(|p| p >= FULL_PERCENT) && !reset.is_some_and(|r| r <= now) {
            full.push(json!({"key": key, "pct": pct.unwrap_or(FULL_PERCENT), "reset_at": window.get("reset_at").cloned().unwrap_or(Value::Null)}));
        }
    }
    if !full.is_empty() {
        let latest = |v: &Value| v["reset_at"].as_i64().unwrap_or(0);
        return full.into_iter().reduce(|best, item| if latest(&item) > latest(&best) { item } else { best });
    }
    let status = rate_limit.get("status").and_then(Value::as_str).unwrap_or("");
    if is_rejected(status) {
        let key = rate_limit.get("limit_type").and_then(Value::as_str).unwrap_or("").to_string();
        let window = windows.get(&key).cloned().unwrap_or(json!({}));
        let reset_at = match window.get("reset_at") {
            Some(v) if !v.is_null() => v.clone(),
            _ => rate_limit.get("reset_at").cloned().unwrap_or(Value::Null),
        };
        let reset = moment_of(Some(&reset_at));
        if !key.is_empty() && !reset.is_some_and(|r| r <= now) {
            let pct = window.get("pct").and_then(Value::as_f64).unwrap_or(FULL_PERCENT);
            return Some(json!({"key": key, "pct": pct, "reset_at": reset_at}));
        }
    }
    None
}

/// True when the output says the account is out of usage (a blocking window exists).
pub fn is_limited(rate_limit: &Value, now: Option<PrimitiveDateTime>) -> bool {
    blocking_window(rate_limit, now).is_some()
}

/// Local datetime when the blocking window frees up; `None` when unknown.
pub fn reset_moment(blocking: Option<&Value>) -> Option<PrimitiveDateTime> {
    blocking.and_then(|b| moment_of(b.get("reset_at")))
}

/// `five_hour` -> "5-hour", `seven_day` -> "weekly", another key -> its words ("seven_day_opus" -> "seven day opus").
pub fn window_name(key: &str) -> String {
    match key {
        FIVE_HOUR => "5-hour".into(),
        SEVEN_DAY => "weekly".into(),
        "" => "usage".into(),
        other => other.replace('_', " "),
    }
}

fn short_name(key: &str) -> String {
    match key {
        FIVE_HOUR => "5h".into(),
        SEVEN_DAY => "weekly".into(),
        other => window_name(other),
    }
}

/// `Mon 26/10 14:00` (fixed English weekday).
pub fn format_moment(moment: PrimitiveDateTime) -> String {
    format!(
        "{} {:02}/{:02} {:02}:{:02}",
        WEEKDAYS[moment.weekday().number_days_from_monday() as usize],
        moment.day(),
        u8::from(moment.month()),
        moment.hour(),
        moment.minute()
    )
}

fn clock(moment: PrimitiveDateTime) -> String {
    format!("{:02}:{:02}", moment.hour(), moment.minute())
}

/// What a notice says about the blocking window: "5-hour window, back at 17:05", "weekly window, back on Mon 26/10 14:00"; "weekly window"
/// alone when the reset is unknown; empty when nothing blocks.
pub fn phrase(blocking: Option<&Value>) -> String {
    let Some(blocking) = blocking else { return String::new() };
    let key = blocking.get("key").and_then(Value::as_str).unwrap_or("");
    let name = format!("{} window", window_name(key));
    match reset_moment(Some(blocking)) {
        None => name,
        Some(moment) if key == FIVE_HOUR => format!("{name}, back at {}", clock(moment)),
        Some(moment) => format!("{name}, back on {}", format_moment(moment)),
    }
}

pub fn is_weekly_block(blocking: Option<&Value>) -> bool {
    blocking.is_some_and(|b| b.get("key").and_then(Value::as_str) == Some(SEVEN_DAY))
}

/// Short text of known windows: "5h 60% (back 17:05), weekly 33% (back Mon 26/10 14:00)"; "usage unknown" when there are none.
pub fn describe_windows(windows: Option<&Value>) -> String {
    let Some(map) = windows.and_then(Value::as_object) else { return "usage unknown".into() };
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_by_key(|k| (k.as_str() != FIVE_HOUR, k.as_str() != SEVEN_DAY, k.to_string()));
    let parts: Vec<String> = keys
        .into_iter()
        .map(|key| {
            let window = &map[key];
            let pct = match window.get("pct").and_then(Value::as_f64) {
                Some(p) => format!("{}", p.round_ties_even() as i64),
                None => "?".into(),
            };
            let mut text = format!("{} {pct}%", short_name(key));
            if let Some(moment) = moment_of(window.get("reset_at")) {
                text.push_str(&format!(" (back {})", if key == FIVE_HOUR { clock(moment) } else { format_moment(moment) }));
            }
            text
        })
        .collect();
    if parts.is_empty() { "usage unknown".into() } else { parts.join(", ") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn epoch_of_local(moment: PrimitiveDateTime) -> i64 {
        let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
        moment.assume_offset(offset).unix_timestamp()
    }

    #[test]
    fn blocking_picks_the_window_that_frees_last() {
        let now = datetime!(2026-10-26 12:00);
        let five = epoch_of_local(datetime!(2026-10-26 17:05));
        let week = epoch_of_local(datetime!(2026-10-28 09:00));
        let rl = json!({"status": "rejected", "limit_type": "five_hour", "windows": {"five_hour": {"pct": 100.0, "reset_at": five}, "seven_day": {"pct": 100.0, "reset_at": week}}});
        let blocking = blocking_window(&rl, Some(now)).unwrap();
        assert_eq!(blocking["key"], "seven_day");
        assert_eq!(phrase(Some(&blocking)), "weekly window, back on Wed 28/10 09:00");
        assert!(is_weekly_block(Some(&blocking)) && is_limited(&rl, Some(now)));
        let only_five = json!({"windows": {"five_hour": {"pct": 100.0, "reset_at": five}, "seven_day": {"pct": 40.0, "reset_at": week}}});
        assert_eq!(phrase(blocking_window(&only_five, Some(now)).as_ref()), "5-hour window, back at 17:05");
        let stale = json!({"windows": {"five_hour": {"pct": 100.0, "reset_at": epoch_of_local(datetime!(2026-10-26 11:00))}}});
        assert!(blocking_window(&stale, Some(now)).is_none());
        let rejected_only = json!({"status": "rejected", "limit_type": "seven_day", "reset_at": week});
        assert_eq!(blocking_window(&rejected_only, Some(now)).unwrap()["pct"], json!(100.0));
        assert!(blocking_window(&json!({"status": "allowed"}), Some(now)).is_none());
        assert_eq!(phrase(None), "");
    }

    #[test]
    fn describes_windows() {
        let five = epoch_of_local(datetime!(2026-10-26 17:05));
        let week = epoch_of_local(datetime!(2026-10-26 14:00));
        let text = describe_windows(Some(&json!({"seven_day": {"pct": 33.4, "reset_at": week}, "five_hour": {"pct": 60, "reset_at": five}, "x_y": {"pct": null}})));
        assert_eq!(text, "5h 60% (back 17:05), weekly 33% (back Mon 26/10 14:00), x y ?%");
        assert_eq!(describe_windows(Some(&json!({}))), "usage unknown");
        assert_eq!(describe_windows(None), "usage unknown");
        assert_eq!(window_name("seven_day_opus"), "seven day opus");
        let local = with_local_reset(&json!({"five_hour": {"pct": 1.0, "reset_at": five}, "none": {"pct": 2.0, "reset_at": null}}));
        assert_eq!(local["five_hour"]["reset_local"], "2026-10-26T17:05:00");
        assert_eq!(local["none"]["reset_local"], "");
    }
}
