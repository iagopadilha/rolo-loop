// SPDX-License-Identifier: MIT
//! The loop policy the user set in Settings, handed to the orchestrator every turn: which optional agents run, the model of each agent and the
//! quality gate checks. Written to `.specs/project/loop/internal/policy.json` before a turn; the turn prompt carries `--policy` and the
//! orchestrator skill reads the file (it never decides these by itself).
//!
//! `{"agents": {"quality-gate": {"enabled": true, "model": "opus"}}, "quality": {"checks": {"spec": "block", "security": "block", ...}}}`

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "policy.json";
/// Quality gate checks and their default level (the order the agent reports them in).
pub const QUALITY_DEFAULTS: [(&str, &str); 6] =
    [("spec", "block"), ("security", "block"), ("standards", "block"), ("naming", "block"), ("size", "block"), ("patterns", "warn")];

pub fn policy_path(work: &str) -> PathBuf {
    Path::new(work).join(".specs").join("project").join("loop").join("internal").join(FILE_NAME)
}

/// The policy of `config.json` with every default filled in.
pub fn build(config: &Map<String, Value>) -> Value {
    let agents = config.get("agents").and_then(Value::as_object).cloned().unwrap_or_default();
    let stored = config.get("quality").and_then(|q| q.get("checks")).and_then(Value::as_object).cloned().unwrap_or_default();
    let mut checks = Map::new();
    for (id, default) in QUALITY_DEFAULTS {
        let level = stored.get(id).and_then(Value::as_str).filter(|l| ["block", "warn", "off"].contains(l)).unwrap_or(default);
        checks.insert(id.to_string(), json!(level));
    }
    json!({"agents": agents, "quality": {"checks": checks}})
}

/// `spec=block,security=block,...` for the quality gate's prompt.
pub fn quality_line(policy: &Value) -> String {
    QUALITY_DEFAULTS.iter().map(|(id, default)| format!("{id}={}", policy["quality"]["checks"][*id].as_str().unwrap_or(default))).collect::<Vec<_>>().join(",")
}

/// Writes the policy file of the turn; `true` when it was written (the prompt then carries `--policy`).
pub fn write_turn_policy(work: &str, config: &Map<String, Value>) -> bool {
    let path = policy_path(work);
    let policy = build(config);
    let text = format!("{}\n", serde_json::to_string_pretty(&policy).unwrap_or_default());
    path.parent().is_some_and(|dir| std::fs::create_dir_all(dir).is_ok()) && crate::loop_io::atomic_write_text(&path, &text).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fill_and_bad_levels_fall_back() {
        let config: Map<String, Value> =
            serde_json::from_value(json!({"agents": {"quality-gate": {"enabled": false, "model": ""}}, "quality": {"checks": {"security": "warn", "size": "nope"}}}))
                .unwrap();
        let policy = build(&config);
        assert_eq!(policy["agents"]["quality-gate"]["enabled"], false);
        assert_eq!(quality_line(&policy), "spec=block,security=warn,standards=block,naming=block,size=block,patterns=warn");
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        assert!(write_turn_policy(&work, &config));
        let written: Value = serde_json::from_str(&std::fs::read_to_string(policy_path(&work)).unwrap()).unwrap();
        assert_eq!(written["quality"]["checks"]["security"], "warn");
    }
}
