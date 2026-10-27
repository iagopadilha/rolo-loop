// SPDX-License-Identifier: MIT
//! Structured log (one JSON per line) in `~/.loop-harness/logs/app.jsonl`. Never writes a token, password or .env body.

use engine_state::loop_io::now_iso;
use engine_state::secret_redaction::redact;
use pycompat::json::py_json_dumps;
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub const SECRET_MARK: &str = "***";
const MAX_FILE_SIZE_BYTES: u64 = 5 * 1024 * 1024;

pub fn redact_secrets(text: &str) -> String {
    redact(text, None, SECRET_MARK)
}

pub struct StructuredLog {
    file_path: Option<PathBuf>,
    mirror_to_console: bool,
    write_lock: Mutex<()>,
}

impl StructuredLog {
    /// `logs_dir` empty = no file (tests).
    pub fn new(logs_dir: &str, file_name: &str, mirror_to_console: bool) -> Self {
        StructuredLog { file_path: (!logs_dir.is_empty()).then(|| PathBuf::from(logs_dir).join(file_name)), mirror_to_console, write_lock: Mutex::new(()) }
    }

    pub fn app(logs_dir: &str) -> Self {
        Self::new(logs_dir, "app.jsonl", false)
    }

    pub fn record(&self, level: &str, event: &str, fields: &[(&str, Value)]) {
        let mut entry = Map::new();
        entry.insert("t".into(), json!(now_iso()));
        entry.insert("level".into(), json!(level));
        entry.insert("event".into(), json!(event));
        for (key, value) in fields {
            entry.insert(
                (*key).to_string(),
                match value {
                    Value::String(s) => json!(redact_secrets(s)),
                    other => other.clone(),
                },
            );
        }
        let line = py_json_dumps(&Value::Object(entry), false);
        if self.mirror_to_console {
            println!("{line}");
        }
        let Some(path) = &self.file_path else { return };
        let _guard = self.write_lock.lock();
        let _ = std::fs::create_dir_all(path.parent().expect("has parent"));
        if std::fs::metadata(path).map(|m| m.len() > MAX_FILE_SIZE_BYTES).unwrap_or(false) {
            let _ = std::fs::rename(path, format!("{}.1", path.display()));
        }
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{line}"); // the log never breaks the app
        }
    }

    pub fn info(&self, event: &str, fields: &[(&str, Value)]) {
        self.record("info", event, fields);
    }

    pub fn warn(&self, event: &str, fields: &[(&str, Value)]) {
        self.record("warning", event, fields);
    }

    pub fn error(&self, event: &str, fields: &[(&str, Value)]) {
        self.record("error", event, fields);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_json_and_secrets_are_masked() {
        let dir = tempfile::tempdir().unwrap();
        let log = StructuredLog::app(&dir.path().display().to_string());
        log.info("started", &[("port", json!(47801)), ("note", json!("DB_PASSWORD=hunter22 ok"))]);
        log.error("route_failed", &[("route", json!("/api/x"))]);
        let text = std::fs::read_to_string(dir.path().join("app.jsonl")).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!((lines[0]["event"].as_str(), lines[0]["level"].as_str(), lines[0]["port"].as_i64()), (Some("started"), Some("info"), Some(47801)));
        assert_eq!(lines[0]["note"], "DB_PASSWORD=*** ok");
        assert_eq!(lines[1]["level"], "error");
        assert_eq!(StructuredLog::new("", "x.jsonl", false).file_path, None);
        assert_eq!(redact_secrets("token=abc123456"), "token=***");
    }
}
