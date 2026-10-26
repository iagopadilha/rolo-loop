// SPDX-License-Identifier: MIT
//! Pause request and waiting: the PAUSE file the panel writes, the wait until a usage window resets, and the sleep that a pause cuts short.

use crate::loop_io::{iso_seconds, load_json, local_now, save_json};
use crate::loop_opts::LoopOpts;
use crate::loop_state::{loop_path, state_file};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use time::PrimitiveDateTime;

pub const PAUSE_REASON: &str = "paused by the user in the panel; click Resume in the panel or run the command again";
pub const PAUSE_NEXT: &str = "resume when the user wants";
/// The weekly window is the longest one.
pub const MAX_LIMIT_WAIT_SECONDS: i64 = 8 * 24 * 3600;
/// Wake up a little after the reset, not on it.
pub const RESET_MARGIN_SECONDS: i64 = 120;
pub const LIMIT_WAIT_STEP_SECONDS: u64 = 60;

/// PAUSE file (created by the panel): deletes it and returns the stop text; empty if there is no request. Spends no turn.
pub fn pause_requested(work: &str, turn: i64) -> String {
    let path = loop_path(work, "PAUSE");
    if !path.exists() {
        return String::new();
    }
    let _ = std::fs::remove_file(&path);
    let mut state = load_json(&state_file(work, "LOOP.json"));
    if let Value::Object(map) = &mut state {
        map.insert("status".into(), json!("awaiting_user"));
        map.insert("turn".into(), json!(turn));
        map.insert("reason".into(), json!(PAUSE_REASON));
        map.insert("next".into(), json!(PAUSE_NEXT));
        map.insert("at".into(), json!(iso_seconds(local_now())));
        map.shift_remove("options");
    }
    let _ = save_json(&state_file(work, "LOOP.json"), &state);
    PAUSE_REASON.into()
}

/// `(seconds, datetime of the reset or None)`: until the first known reset among the limited commands plus a margin, else `--limit-wait`.
pub fn limit_wait_seconds(opts: &LoopOpts) -> (i64, Option<PrimitiveDateTime>) {
    let now = local_now();
    let mut future: Vec<PrimitiveDateTime> = opts.limit_resets.iter().copied().filter(|m| *m > now).collect();
    future.sort();
    let Some(first) = future.first().copied() else { return (opts.limit_wait * 60, None) };
    let seconds = (first - now).whole_seconds() + RESET_MARGIN_SECONDS;
    (seconds.clamp(60, MAX_LIMIT_WAIT_SECONDS.max(60)), Some(first))
}

/// When the loop tries the main command again while a fallback keeps the turns: the reset of the main command's blocking window (the weekly
/// one when that is what ran out, never an earlier moment) plus a margin; an unknown or already past reset waits `--limit-wait` minutes
/// instead, so the loop never bounces between the commands.
pub fn main_reset_moment(opts: &LoopOpts, reset: Option<PrimitiveDateTime>) -> PrimitiveDateTime {
    let now = local_now();
    let known = match reset {
        Some(r) if r > now => r,
        _ => now + time::Duration::minutes(opts.limit_wait),
    };
    known + time::Duration::seconds(RESET_MARGIN_SECONDS)
}

/// `1d 2h 5min`, `45min`, `0min`.
pub fn wait_text(seconds: i64) -> String {
    let minutes_total = seconds / 60;
    let (hours_total, minutes) = (minutes_total / 60, minutes_total % 60);
    let (days, hours) = (hours_total / 24, hours_total % 24);
    let mut parts: Vec<String> = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 || (days == 0 && hours == 0) {
        parts.push(format!("{minutes}min"));
    }
    parts.join(" ")
}

/// Sleeps `seconds` in steps, stopping early when the panel asks for a pause; returns the pause text (empty = slept the whole time).
pub fn sleep_unless_paused(work: &str, turn: i64, seconds: i64) -> String {
    let deadline = Instant::now() + Duration::from_secs(seconds.max(0) as u64);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return String::new();
        }
        std::thread::sleep(left.min(Duration::from_secs(LIMIT_WAIT_STEP_SECONDS)));
        let paused = pause_requested(work, turn);
        if !paused.is_empty() {
            return paused;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_text_forms() {
        assert_eq!(wait_text(0), "0min");
        assert_eq!(wait_text(45 * 60), "45min");
        assert_eq!(wait_text(3600), "1h");
        assert_eq!(wait_text(26 * 3600 + 5 * 60), "1d 2h 5min");
        assert_eq!(wait_text(59), "0min");
    }

    #[test]
    fn waits_follow_the_known_reset() {
        let mut opts = LoopOpts::new("x");
        assert_eq!(limit_wait_seconds(&opts), (30 * 60, None));
        let soon = local_now() + time::Duration::minutes(10);
        opts.limit_resets = vec![soon, local_now() - time::Duration::minutes(5)];
        let (seconds, at) = limit_wait_seconds(&opts);
        assert_eq!(at, Some(soon));
        assert!((seconds - (600 + RESET_MARGIN_SECONDS)).abs() <= 2);
        let moment = main_reset_moment(&opts, None);
        assert!(moment > local_now() + time::Duration::minutes(31));
    }

    #[test]
    fn pause_file_stops_once() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        assert_eq!(pause_requested(&work, 3), "");
        let path = loop_path(&work, "PAUSE");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        save_json(&state_file(&work, "LOOP.json"), &json!({"status": "continue", "options": ["a"]})).unwrap();
        assert_eq!(pause_requested(&work, 3), PAUSE_REASON);
        assert!(!path.exists());
        let state = load_json(&state_file(&work, "LOOP.json"));
        assert_eq!((state["status"].as_str(), state["turn"].as_i64(), state.get("options").is_none()), (Some("awaiting_user"), Some(3), true));
        assert_eq!(sleep_unless_paused(&work, 3, 0), "");
    }
}
