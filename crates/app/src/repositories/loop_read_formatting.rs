// SPDX-License-Identifier: MIT
//! Formatting rules shared by the loop read side: credential redaction, text clipping, numbers and clocks.
//!
//! The front end and the tests depend on the exact text (e.g. `[redacted]`).

use engine_state::secret_redaction;
use pycompat::Re;
use serde_json::Value;
use std::sync::LazyLock;
use time::PrimitiveDateTime;

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
        Value::Array(items) => items
            .iter()
            .map(|i| match i {
                Value::String(s) => s.clone(),
                other => pycompat::json::py_json_dumps(other, false),
            })
            .collect(),
        Value::String(s) => LIST_SPLIT.split(s).into_iter().filter(|p| !p.is_empty()).collect(),
        _ => vec![],
    }
}

/// A JSON number (never a bool) as `f64`.
pub fn to_number(value: &Value) -> Option<f64> {
    if value.is_number() {
        value.as_f64()
    } else {
        None
    }
}

/// Python `round()` (banker's rounding) of a number of milliseconds into seconds.
pub fn ms_to_seconds(value: &Value) -> Option<i64> {
    to_number(value).map(|ms| (ms / 1000.0).round_ties_even() as i64)
}

pub fn format_minutes_clock(seconds: i64) -> String {
    format!("{:02}:{:02}", seconds / SECONDS_PER_MINUTE, seconds % SECONDS_PER_MINUTE)
}

/// An ISO date read the way `datetime.fromisoformat` does: `naive` when it carries no zone; `seconds` since the epoch (a naive one is taken as UTC,
/// only differences between two moments of the same kind are meaningful).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moment {
    pub naive: bool,
    pub seconds: f64,
}

/// `datetime.fromisoformat(str(value).replace("Z", "+00:00"))`; `None` when it does not parse.
pub fn parse_iso_date(value: &str) -> Option<Moment> {
    let text = value.trim().replace('Z', "+00:00");
    let (body, offset_seconds) = match text.rfind(['+', '-']).filter(|i| *i > 10) {
        Some(index) => {
            let (b, o) = text.split_at(index);
            let sign = if o.starts_with('-') { -1i64 } else { 1 };
            let digits: Vec<&str> = o[1..].split(':').collect();
            let hours: i64 = digits.first()?.parse().ok()?;
            let minutes: i64 = digits.get(1).map(|m| m.parse().ok()).unwrap_or(Some(0))?;
            (b.to_string(), Some(sign * (hours * 3600 + minutes * 60)))
        }
        None => (text.clone(), None),
    };
    let body = body.replace(' ', "T");
    let (date, clock) = match body.split_once('T') {
        Some((d, c)) => (d.to_string(), c.to_string()),
        None => (body.clone(), "00:00:00".into()),
    };
    let date = time::Date::parse(&date, time::macros::format_description!("[year]-[month]-[day]")).ok()?;
    let mut parts = clock.split(':');
    let hour: u8 = parts.next()?.parse().ok()?;
    let minute: u8 = parts.next().map(|m| m.parse().ok()).unwrap_or(Some(0))?;
    let second_text = parts.next().unwrap_or("0");
    let (whole, fraction) = second_text.split_once('.').unwrap_or((second_text, ""));
    let second: u8 = whole.parse().ok()?;
    let fraction: f64 = if fraction.is_empty() { 0.0 } else { format!("0.{fraction}").parse().ok()? };
    let clock_time = time::Time::from_hms(hour, minute, second).ok()?;
    let epoch = PrimitiveDateTime::new(date, clock_time).assume_utc().unix_timestamp() as f64 + fraction;
    Some(Moment { naive: offset_seconds.is_none(), seconds: epoch - offset_seconds.unwrap_or(0) as f64 })
}

/// HH:MM:SS between two moments; either side missing or with an incompatible time zone = ''.
pub fn format_elapsed_clock(start: Option<&Moment>, moment: Option<&Moment>) -> String {
    let (Some(start), Some(moment)) = (start, moment) else { return String::new() };
    if start.naive != moment.naive {
        return String::new();
    }
    let seconds = ((moment.seconds - start.seconds) as i64).max(0);
    format!("{:02}:{:02}:{:02}", seconds / SECONDS_PER_HOUR, seconds % SECONDS_PER_HOUR / SECONDS_PER_MINUTE, seconds % SECONDS_PER_MINUTE)
}

/// Seconds between two ISO dates (Python `round`); a side missing/unparseable, or one with a zone and the other without, = `None`.
pub fn compute_seconds_between(start: &str, end: &str) -> Option<i64> {
    if start.is_empty() || end.is_empty() {
        return None;
    }
    let (first, last) = (parse_iso_date(start)?, parse_iso_date(end)?);
    if first.naive != last.naive {
        return None;
    }
    Some(((last.seconds - first.seconds).round_ties_even() as i64).max(0))
}

/// Sum of the `*_tokens` fields of a `usage`; none of them = `None` (never invents a zero).
pub fn sum_usage_tokens(usage: &Value) -> Option<i64> {
    let map = usage.as_object()?;
    let values: Vec<f64> = map.iter().filter(|(key, _)| key.ends_with("_tokens")).filter_map(|(_, v)| to_number(v)).collect();
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() as i64)
    }
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

/// Python `str(value or "")` of a JSON value.
pub fn text_or_empty(value: Option<&Value>) -> String {
    match value {
        Some(v) if engine_state::notify_truthy(Some(v)) => match v {
            Value::String(s) => s.clone(),
            Value::Bool(true) => "True".into(),
            other => pycompat::json::py_json_dumps(other, false),
        },
        _ => String::new(),
    }
}

/// `redact_credentials(value, limit)` for a value that may be missing or not text.
pub fn redact_value(value: Option<&Value>, limit: Option<usize>) -> String {
    redact_credentials(&text_or_empty(value), limit)
}

/// `to_number(value)` keeping the JSON number as it was read (a whole number stays whole): `null` for anything else.
pub fn number_value(value: Option<&Value>) -> Value {
    match value {
        Some(v) if v.is_number() => v.clone(),
        _ => Value::Null,
    }
}
