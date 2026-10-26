// SPDX-License-Identifier: MIT
//! Loop control files that the commands module reads and writes inside the `work_dir` of ONE project.
//!
//! Reads: `LOOP-CMD.json` (command to relaunch), `meta.json`, ROADMAP (open features). Writes: `PAUSE`, `meta.json` (pr/push after a
//! delivery), `internal/actions.log`. The `.specs/project/loop/` layout comes from the engine's `loop_state` (never rewritten here).

use engine_state::loop_io::{atomic_write_text, now_iso as engine_now_iso};
use engine_state::loop_state;
use pycompat::json::{py_json_dumps, py_json_dumps_indent};
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const LOOP_COMMAND_FILE_NAME: &str = "LOOP-CMD.json";
pub const META_FILE_NAME: &str = "meta.json";
pub const PAUSE_FILE_NAME: &str = "PAUSE";
pub const RELAUNCH_LOG_FILE_NAME: &str = "monitor-relaunch.log";
pub const ACTIONS_LOG_FILE_NAME: &str = "actions.log";
pub const OPEN_FEATURE_STATUSES: [&str; 5] = ["pending", "speccing", "specced", "approved", "building"];

pub fn now_iso() -> String {
    engine_now_iso()
}

/// JSON object of the file; missing, unreadable or not an object = `{}`.
pub fn read_json_object(path: &Path) -> Map<String, Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| if let Value::Object(map) = value { Some(map) } else { None })
        .unwrap_or_default()
}

/// Writes to a temporary file and swaps it in (a reader never sees a half-written file).
pub fn write_json_atomic(path: &Path, data: &Value) -> std::io::Result<()> {
    atomic_write_text(path, &py_json_dumps_indent(data, false, 2))
}

/// `.specs/project/loop/internal` (chat, whiteboard, actions.log).
pub fn get_loop_internal_dir(work_dir: &str) -> PathBuf {
    loop_state::internal_dir(work_dir)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LoopCommandFileRepository;

impl LoopCommandFileRepository {
    /// `LOOP-CMD.json` written by the loop: `{"argv":[...],"cwd":"..."}`; missing = `{}`.
    pub fn read_loop_command(&self, work_dir: &str) -> Map<String, Value> {
        read_json_object(&loop_state::loop_path(work_dir, LOOP_COMMAND_FILE_NAME))
    }

    pub fn get_relaunch_log_path(&self, work_dir: &str) -> PathBuf {
        loop_state::loop_path(work_dir, RELAUNCH_LOG_FILE_NAME)
    }

    /// Empty `PAUSE` file: the loop checks it at the top of every turn and stops with `awaiting_user`.
    pub fn create_pause_file(&self, work_dir: &str) {
        let path = loop_state::loop_path(work_dir, PAUSE_FILE_NAME);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(path);
    }

    pub fn read_meta(&self, work_dir: &str) -> Map<String, Value> {
        read_json_object(&loop_state::loop_path(work_dir, META_FILE_NAME))
    }

    /// After a delivery: pr/pr_status/push in `meta.json` (as the loop does) so the state shows the link.
    pub fn write_delivery_to_meta(&self, work_dir: &str, push: &Value, pr: &Value, pr_status: &Value) -> std::io::Result<()> {
        let path = loop_state::loop_path(work_dir, META_FILE_NAME);
        let mut meta = read_json_object(&path);
        meta.insert("push".into(), push.clone());
        meta.insert("pr".into(), pr.clone());
        meta.insert("pr_status".into(), pr_status.clone());
        write_json_atomic(&path, &Value::Object(meta))
    }

    /// One JSON line in `internal/actions.log` (`{"action","t","ok","summary"}`). A deleted worktree is not recreated.
    pub fn record_executed_action(&self, work_dir: &str, action: &str, ok: bool, summary: &str) {
        if !Path::new(work_dir).is_dir() {
            return;
        }
        let path = loop_state::loop_path(work_dir, ACTIONS_LOG_FILE_NAME);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let line = py_json_dumps(&json!({"action": action, "t": now_iso(), "ok": ok, "summary": summary}), false);
        if let Ok(mut handle) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = handle.write_all(format!("{line}\n").as_bytes());
        }
    }

    /// ROADMAP slugs whose status is still open (pending..building), in file order.
    pub fn list_open_features(&self, work_dir: &str) -> Vec<String> {
        let text = loop_state::read(&Path::new(work_dir).join(".specs").join("project").join("ROADMAP.md"));
        let (features, _) = loop_state::parse_roadmap(&text);
        features.iter().filter(|(_, fields)| OPEN_FEATURE_STATUSES.contains(&fields.status())).map(|(slug, _)| slug.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_pause_actions_and_open_features() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let repo = LoopCommandFileRepository;
        assert!(repo.read_loop_command(&work).is_empty());
        repo.create_pause_file(&work);
        assert!(loop_state::loop_path(&work, PAUSE_FILE_NAME).is_file());
        repo.write_delivery_to_meta(&work, &json!("pushed"), &json!("http://x/pr/1"), &json!("open")).unwrap();
        assert_eq!(repo.read_meta(&work)["pr"], "http://x/pr/1");
        repo.record_executed_action(&work, "pause", true, "ok é");
        let log = std::fs::read_to_string(loop_state::loop_path(&work, ACTIONS_LOG_FILE_NAME)).unwrap();
        assert!(log.contains(r#""action": "pause""#) && log.contains("ok é"));
        repo.record_executed_action(&dir.path().join("gone").display().to_string(), "x", true, "");
        assert!(!dir.path().join("gone").exists());
        let roadmap = dir.path().join(".specs/project/ROADMAP.md");
        std::fs::create_dir_all(roadmap.parent().unwrap()).unwrap();
        std::fs::write(&roadmap, "- [a] status=building title=\"A\" scope=Small\n- [b] status=verified title=\"B\" scope=Small\n").unwrap();
        assert_eq!(repo.list_open_features(&work), vec!["a"]);
    }
}
