// SPDX-License-Identifier: MIT
//! Formatting rules shared by the loop read side: credential redaction, text clipping, numbers and clocks.
//!
//! The front end and the tests depend on the exact text (e.g. `[redacted]`).

use engine_state::secret_redaction;
use pycompat::Re;
use serde_json::Value;
use std::sync::LazyLock;
use time::{OffsetDateTime, PrimitiveDateTime};

pub const REDACTED_MARK: &str = secret_redaction::REDACTION_MARK;
const SECONDS_PER_MINUTE: i64 = 60;
const SECONDS_PER_HOUR: i64 = 3600;
static LIST_SPLIT: LazyLock<Re> = LazyLock::new(|| Re::new(r"[,\s]+"));

pub use engine_state::loop_io::now_iso;

/// Removes from the text whatever looks like a credential before it leaves through the API (turn results and transcripts may quote commands).
pub fn redact_credentials(text: &str, limit: Option<usize>) -> String {
    secret_redaction::redact(text, limit, REDACTED_MARK)
}

/// Redacts, joins the whitespace into a single line and clips at `limit` characters.
pub fn clip_redacted_text(text: &str, limit: usize) -> String {
    let single = redact_credentials(text, None).split_whitespace().collect::<Vec<_>>().join(" ");
    single.chars().take(limit).collect()
}

pub fn to_text_list(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().map(|i| match i { Value::String(s) => s.clone(), other => pycompat::json::py_json_dumps(other, false) }).collect(),
        Value::String(s) => LIST_SPLIT.split(s).into_iter().filter(|p| !p.is_empty()).map(String::from).collect(),
        _ => vec![],
    }
}

/// A JSON number (never a bool) as `f64`.
pub fn to_number(value: &Value) -> Option<f64> {
    if value.is_number() { value.as_f64() } else { None }
}

/// Python `round()` (banker's rounding) of a number of milliseconds into seconds.
pub fn ms_to_seconds(value: &Value) -> Option<i64> {
    to_number(value).map(|ms| (ms / 1000.0).round_ties_even() as i64)
}

pub fn format_minutes_clock(seconds: i64) -> String {
    format!("{:02}:{:02}", seconds / SECONDS_PER_MINUTE, seconds % SECONDS_PER_MINUTE)
}

/// HH:MM:SS between two instants; either side missing = ''.
pub fn format_elapsed_clock(start: Option<&OffsetDateTime>, moment: Option<&OffsetDateTime>) -> String {
    let (Some(start), Some(moment)) = (start, moment) else { return String::new() };
    let seconds = (*moment - *start).whole_seconds().max(0);
    format!("{:02}:{:02}:{:02}", seconds / SECONDS_PER_HOUR, seconds % SECONDS_PER_HOUR / SECONDS_PER_MINUTE, seconds % SECONDS_PER_MINUTE)
}

/// `(naive?, instant)`: an ISO date with or without offset (`Z` accepted). A naive one is read as local time.
fn parse_iso(value: &str) -> Option<(bool, PrimitiveDateTime, Option<time::UtcOffset>)> {
    let text = value.trim();
    let text = text.strip_suffix('Z').map(|t| format!("{t}+00:00")).unwrap_or_else(|| text.to_string());
    let (body, offset) = match text.rfind(['+', '-']).filter(|i| *i > 10) {
        Some(index) => {
            let (b, o) = text.split_at(index);
            let sign = if o.starts_with('-') { -1 } else { 1 };
            let digits: Vec<&str> = o[1..].split(':').collect();
            let hours: i8 = digits.first()?.parse().ok()?;
            let minutes: i8 = digits.get(1).and_then(|m| m.parse().ok()).unwrap_or(0);
            (b.to_string(), Some(time::UtcOffset::from_hms(sign * hours, sign * minutes, 0).ok()?))
        }
        None => (text.clone(), None),
    };
    let body = body.replace(' ', "T");
    let (date, clock) = body.split_once('T').map(|(d, c)| (d.to_string(), c.to_string())).unwrap_or((body.clone(), "00:00:00".into()));
    let date = time::Date::parse(&date, time::macros::format_description!("[year]-[month]-[day]")).ok()?;
    let mut parts = clock.split(':');
    let hour: u8 = parts.next()?.parse().ok()?;
    let minute: u8 = parts.next().map(|m| m.parse().ok()).unwrap_or(Some(0))?;
    let second_text = parts.next().unwrap_or("0");
    let second: u8 = second_text.split('.').next()?.parse().ok()?;
    let moment = PrimitiveDateTime::new(date, time::Time::from_hms(hour, minute, second).ok()?);
    Some((offset.is_none(), moment, offset))
}

/// Seconds between two ISO dates; a side missing/unparseable, or one with a zone and the other without, = `None`.
pub fn compute_seconds_between(start: &str, end: &str) -> Option<i64> {
    if start.is_empty() || end.is_empty() {
        return None;
    }
    let (first, last) = (parse_iso(start)?, parse_iso(end)?);
    if first.0 != last.0 {
        return None;
    }
    let as_utc = |p: &(bool, PrimitiveDateTime, Option<time::UtcOffset>)| p.1.assume_offset(p.2.unwrap_or(time::UtcOffset::UTC));
    Some((as_utc(&last) - as_utc(&first)).whole_seconds().max(0))
}

/// Sum of the `*_tokens` fields of a `usage`; none of them = `None` (never invents a zero).
pub fn sum_usage_tokens(usage: &Value) -> Option<i64> {
    let map = usage.as_object()?;
    let values: Vec<f64> = map.iter().filter(|(key, _)| key.ends_with("_tokens")).filter_map(|(_, v)| to_number(v)).collect();
    if values.is_empty() { None } else { Some(values.iter().sum::<f64>() as i64) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clipping_numbers_and_clocks() {
        assert_eq!(clip_redacted_text("a   b\n c token=abcdef123456789", 100), "a b c token=[redacted]");
        assert_eq!(clip_redacted_text("abcdef", 3), "abc");
        assert_eq!(to_text_list(&json!("a, b  c")), vec!["a", "b", "c"]);
        assert_eq!(to_text_list(&json!([1, "x"])), vec!["1", "x"]);
        assert_eq!(to_number(&json!(true)), None);
        assert_eq!((ms_to_seconds(&json!(2500)), ms_to_seconds(&json!(1500)), ms_to_seconds(&json!("x"))), (Some(2), Some(2), None));
        assert_eq!(format_minutes_clock(125), "02:05");
        assert_eq!(compute_seconds_between("2026-01-01T10:00:00", "2026-01-01T10:01:30"), Some(90));
        assert_eq!(compute_seconds_between("2026-01-01T10:00:00Z", "2026-01-01T07:00:30-03:00"), Some(30));
        assert_eq!(compute_seconds_between("2026-01-01T10:00:00Z", "2026-01-01T10:00:30"), None);
        assert_eq!(compute_seconds_between("", "2026-01-01T10:00:30"), None);
        assert_eq!(sum_usage_tokens(&json!({"input_tokens": 3, "output_tokens": 4, "x": 9})), Some(7));
        assert_eq!(sum_usage_tokens(&json!({"x": 9})), None);
    }
}
