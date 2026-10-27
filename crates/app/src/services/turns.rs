// SPDX-License-Identifier: MIT
//! Loop turns: short list (for `/state`), detail of one turn from its snapshot (`/turn/<n>`) and turns without progress.

use super::claude_accounts::build_turn_claude;
use super::App;
use crate::domain::project::ProjectContext;
use crate::repositories::loop_read_formatting::{ms_to_seconds, redact_credentials, to_text_list};
use engine_state::loop_state;
use pycompat::json::py_json_dumps;
use serde_json::{json, Map, Value};

/// Same scoreboard as the engine's run loop: counts turns without progress from the snapshots.
fn points_by_status(status: &str) -> i64 {
    match status {
        "speccing" => 1,
        "specced" => 2,
        "approved" => 3,
        "building" => 4,
        "verified" => 6,
        _ => 0, // pending, blocked, anything else
    }
}

const TURN_RESULT_LIMIT: usize = 20000;
const PERMISSION_DENIAL_LIMIT: usize = 300;
const TURN_STATUS_RUNNING: &str = "running";
const TURN_STATUS_ERROR: &str = "error";

fn is_zero(value: &Value) -> bool {
    value.as_f64() == Some(0.0)
}

/// Turn status: the one from the snapshot's LOOP.json; without it, `error` when claude -p failed; else None.
pub fn get_turn_status(turn_record: &Map<String, Value>, snapshot_loop: &Map<String, Value>) -> Option<String> {
    if let Some(status) = snapshot_loop.get("status").and_then(Value::as_str).filter(|s| loop_state::LOOP_STATUS.contains(s)) {
        return Some(status.to_string());
    }
    let exit_failed = turn_record.get("exit_code").is_some_and(|c| !c.is_null() && !is_zero(c));
    if engine_state::notify_truthy(turn_record.get("is_error")) || engine_state::notify_truthy(turn_record.get("timeout")) || exit_failed {
        return Some(TURN_STATUS_ERROR.to_string());
    }
    None
}

pub struct TurnService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn turns(&self) -> TurnService<'_> {
        TurnService { app: self }
    }
}

fn head_of(snapshot: &Map<String, Value>) -> Option<&str> {
    snapshot.get("head").and_then(Value::as_str).filter(|h| !h.is_empty())
}

impl TurnService<'_> {
    pub fn list_turns(&self, work_dir: &str, running_number: Option<u64>) -> Vec<Value> {
        let files = &self.app.loop_files;
        let mut rows = Vec::new();
        for number in files.list_turn_numbers(work_dir) {
            let record = files.read_turn(work_dir, number);
            let snapshot = files.load_snapshot(work_dir, number);
            let snapshot_loop = files.read_snapshot_loop_json(snapshot.as_ref());
            let previous = if number > 0 { files.load_snapshot(work_dir, number - 1) } else { None };
            let commits = match (&snapshot, &previous) {
                (Some(current), Some(before)) => match (head_of(current), head_of(before)) {
                    (Some(now), Some(then)) => files.count_commits_between(work_dir, then, now),
                    _ => None,
                },
                _ => None,
            };
            let status = if Some(number) == running_number { Some(TURN_STATUS_RUNNING.to_string()) } else { get_turn_status(&record, &snapshot_loop) };
            let mut row = Map::new();
            row.insert("n".into(), json!(number));
            row.insert("cost_usd".into(), crate::repositories::loop_read_formatting::number_value(record.get("total_cost_usd")));
            row.insert("duration_s".into(), json!(ms_to_seconds(record.get("duration_ms").unwrap_or(&Value::Null))));
            row.insert("status".into(), json!(status));
            row.insert("feature".into(), json!(to_text_list(snapshot_loop.get("feature").unwrap_or(&Value::Null))));
            row.insert("commits".into(), json!(commits));
            row.insert("has_snapshot".into(), json!(snapshot.is_some()));
            if let Value::Object(claude) = build_turn_claude(&Value::Object(record)) {
                row.extend(claude);
            }
            rows.push(Value::Object(row));
        }
        rows
    }

    /// Consecutive turns, up to the last one, in which the ROADMAP score did not go up (same count as the run loop, redone from the snapshots).
    pub fn count_turns_without_progress(&self, work_dir: &str) -> usize {
        let files = &self.app.loop_files;
        let last = files.list_turn_numbers(work_dir).last().copied().unwrap_or(0);
        let mut scores: Vec<i64> = Vec::new();
        for number in 0..=last {
            let Some(snapshot) = files.load_snapshot(work_dir, number) else { continue };
            let (parsed, _) = loop_state::parse_roadmap(&files.read_snapshot_roadmap(Some(&snapshot)));
            scores.push(parsed.iter().map(|(_, f)| points_by_status(f.status())).sum());
        }
        let mut without_progress = 0;
        for index in (1..scores.len()).rev() {
            if scores[index] > scores[index - 1] {
                break;
            }
            without_progress += 1;
        }
        without_progress
    }

    /// `{"n","turn_log","roadmap","features"}` from the turn snapshot; `None` without a snapshot.
    pub fn get_turn_detail(&self, context: &ProjectContext, number: u64) -> Option<Value> {
        let work_dir = context.work_dir();
        let files = &self.app.loop_files;
        let snapshot = files.load_snapshot(&work_dir, number)?;
        let mut turn_log = files.read_turn(&work_dir, number);
        turn_log.remove("raw");
        if let Some(Value::String(result)) = turn_log.get("result") {
            let redacted = redact_credentials(result, Some(TURN_RESULT_LIMIT));
            turn_log.insert("result".into(), json!(redacted));
        }
        if let Some(Value::Array(denials)) = turn_log.get("permission_denials") {
            let shown: Vec<Value> = denials.iter().map(|item| json!(redact_credentials(&py_json_dumps(item, false), Some(PERMISSION_DENIAL_LIMIT)))).collect();
            turn_log.insert("permission_denials".into(), Value::Array(shown));
        }
        let roadmap = files.read_snapshot_roadmap(Some(&snapshot));
        let features = self.app.roadmap_view().build_roadmap_view(&work_dir, &roadmap, Some(number)).1;
        Some(json!({"n": number, "turn_log": turn_log, "roadmap": roadmap, "features": features}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_turn(work: &std::path::Path, app: &App, number: u64, record: Value, roadmap: &str, loop_json: Value) {
        let path = app.loop_files.get_turn_path(&work.display().to_string(), number);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, record.to_string()).unwrap();
        let snap = loop_state::snap_dir(&work.display().to_string(), number);
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::write(snap.join("snapshot.json"), "{}").unwrap();
        std::fs::write(snap.join("ROADMAP.md"), roadmap).unwrap();
        std::fs::write(snap.join("LOOP.json"), loop_json.to_string()).unwrap();
    }

    #[test]
    fn rows_status_and_stalled_turns() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let work = dir.path().join("w");
        std::fs::create_dir_all(&work).unwrap();
        let w = work.display().to_string();
        write_turn(&work, &app, 1, json!({"total_cost_usd": 0.5, "duration_ms": 2500}), "- [a] status=building\n", json!({"status": "continue", "feature": "a"}));
        write_turn(&work, &app, 2, json!({"is_error": true}), "- [a] status=building\n", json!({}));
        write_turn(&work, &app, 3, json!({"exit_code": 0, "result": "ok", "raw": "x", "permission_denials": [{"tool": "Bash"}]}), "- [a] status=building\n", json!({}));
        let rows = app.turns().list_turns(&w, Some(3));
        assert_eq!((rows[0]["status"].clone(), rows[0]["duration_s"].clone(), rows[0]["feature"].clone()), (json!("continue"), json!(2), json!(["a"])));
        assert_eq!((rows[1]["status"].clone(), rows[2]["status"].clone(), rows[0]["has_snapshot"].clone()), (json!("error"), json!("running"), json!(true)));
        assert_eq!(app.turns().count_turns_without_progress(&w), 2);
        write_turn(&work, &app, 4, json!({}), "- [a] status=verified\n", json!({}));
        assert_eq!(app.turns().count_turns_without_progress(&w), 0);
    }
}
