// SPDX-License-Identifier: MIT
//! Access of a project is COMPUTED now (see `mcp_access` for the MCP servers and `credential_check` for the `.env` layers): there is no
//! per-project toggle any more. `<project>/.specs/project/ACCESS.json` (`{"grafana_read": ..., "shared_fallback": ...}`) is a legacy file: its
//! keys are ignored and [`migrate_access_file`] drops them (an existing `false` is migrated to the new default: always on).

use crate::loop_io::atomic_write_text;
use pycompat::json::py_json_dumps_indent;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub const GRAFANA_READ_KEY: &str = "grafana_read";
pub const SHARED_FALLBACK_KEY: &str = "shared_fallback";
pub const LEGACY_KEYS: [&str; 2] = [GRAFANA_READ_KEY, SHARED_FALLBACK_KEY];

pub fn access_file_path(root: &str) -> PathBuf {
    Path::new(root).join(".specs").join("project").join("ACCESS.json")
}

/// Effective access: every key true (the file is ignored). Kept so old callers keep working.
pub fn read_access(_root: &str) -> Map<String, Value> {
    LEGACY_KEYS.iter().map(|k| (k.to_string(), Value::Bool(true))).collect()
}

/// Drops the legacy keys from `ACCESS.json`; the file is removed when nothing else is left. Returns true when it changed anything. Tolerant: a
/// missing, broken or odd file is left alone (it is ignored anyway).
pub fn migrate_access_file(root: &str) -> bool {
    let path = access_file_path(root);
    let Some(text) = cfg::env::read_text_sig(&path) else { return false };
    let Ok(Value::Object(content)) = serde_json::from_str::<Value>(&text) else { return false };
    if !LEGACY_KEYS.iter().any(|k| content.contains_key(*k)) {
        return false;
    }
    let remaining: Map<String, Value> = content.into_iter().filter(|(k, _)| !LEGACY_KEYS.contains(&k.as_str())).collect();
    if remaining.is_empty() {
        return std::fs::remove_file(&path).is_ok();
    }
    atomic_write_text(&path, &format!("{}\n", py_json_dumps_indent(&Value::Object(remaining), true, 2))).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_keys_are_dropped_and_empty_file_removed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        assert!(!migrate_access_file(&root));
        let path = access_file_path(&root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"grafana_read": false, "keep": 1}"#).unwrap();
        assert!(migrate_access_file(&root));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\n  \"keep\": 1\n}\n");
        assert!(!migrate_access_file(&root));
        std::fs::write(&path, r#"{"shared_fallback": true}"#).unwrap();
        assert!(migrate_access_file(&root));
        assert!(!path.exists());
        assert_eq!(read_access(&root).len(), 2);
    }
}
