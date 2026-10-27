// SPDX-License-Identifier: MIT
//! "What is running now": the processes the engine itself starts (the Claude turn), kept in `project/loop/json/processes.json` so the panel
//! (`GET state` -> `processes`) can list kind, feature, step, start time and pid. It holds labels and numbers only, never a command line or an
//! environment value.

use crate::loop_io::{load_json, now_iso, save_json};
use crate::loop_state::loop_path;
use serde_json::{json, Value};

const FILE: &str = "processes.json";

/// One lane of a turn: a feature at its own step.
pub fn lanes_of_prompt(prompt: &str) -> Vec<(String, String)> {
    let mut words = prompt.split_whitespace();
    while let Some(word) = words.next() {
        if word == "--lanes" {
            return words.next().unwrap_or("").split(',').filter_map(|lane| lane.split_once(':')).map(|(slug, step)| (slug.to_string(), step.to_string())).collect();
        }
    }
    vec![]
}

/// One-line title for a process (also usable as a window title): `Loop turn 7 · feature a, b · step execute`.
pub fn label_of(kind: &str, turn: i64, lanes: &[(String, String)]) -> String {
    let mut parts = vec![if kind == "claude_turn" { format!("Loop turn {turn}") } else { format!("{kind} {turn}") }];
    if !lanes.is_empty() {
        let slugs: Vec<&str> = lanes.iter().map(|(s, _)| s.as_str()).collect();
        parts.push(format!("feature {}", slugs.join(", ")));
        let mut steps: Vec<&str> = Vec::new();
        for (_, step) in lanes {
            if !steps.contains(&step.as_str()) {
                steps.push(step);
            }
        }
        parts.push(format!("step {}", steps.join(", ")));
    }
    parts.join(" \u{b7} ")
}

fn read_all(work: &str) -> Vec<Value> {
    load_json(&loop_path(work, FILE)).get("processes").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn write_all(work: &str, rows: Vec<Value>) {
    let path = loop_path(work, FILE);
    let _ = std::fs::create_dir_all(path.parent().expect("has parent"));
    let _ = save_json(&path, &json!({"processes": rows}));
}

/// Records a started process and returns its id. `claude` and `account` say which command and sign-in the process runs on.
#[allow(clippy::too_many_arguments)]
pub fn register(work: &str, kind: &str, turn: i64, lanes: &[(String, String)], pid: i64, claude: &str, account: &str) -> String {
    let id = format!("{kind}-{turn}-{pid}");
    let mut rows: Vec<Value> = read_all(work).into_iter().filter(|r| r["id"] != id.as_str()).collect();
    rows.push(json!({
        "id": id, "kind": kind, "turn": turn, "pid": pid, "started_at": now_iso(), "label": label_of(kind, turn, lanes),
        "feature": lanes.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>().join(","),
        "step": lanes.first().map(|(_, step)| step.as_str()).unwrap_or(""),
        "lanes": lanes.iter().map(|(s, st)| json!({"feature": s, "step": st})).collect::<Vec<_>>(),
        "claude": claude, "claude_account": account,
    }));
    write_all(work, rows);
    id
}

/// Forgets a process that ended.
pub fn unregister(work: &str, id: &str) {
    let rows: Vec<Value> = read_all(work).into_iter().filter(|r| r["id"] != id).collect();
    write_all(work, rows);
}

/// Forgets every process (the loop ended or starts over: nothing it started is running any more).
pub fn clear(work: &str) {
    if loop_path(work, FILE).is_file() {
        write_all(work, vec![]);
    }
}

/// The processes to show. A pid cannot be checked here, so `running` (the loop itself is alive) gates the list: a crashed loop leaves none.
pub fn list(work: &str, running: bool) -> Vec<Value> {
    if running {
        read_all(work)
    } else {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_and_label() {
        let lanes = lanes_of_prompt("/orchestrator continue --turn 7 --phase x --lanes a-api:execute,b-ui:spec --ready a-api");
        assert_eq!(lanes, vec![("a-api".to_string(), "execute".to_string()), ("b-ui".to_string(), "spec".to_string())]);
        assert_eq!(label_of("claude_turn", 7, &lanes), "Loop turn 7 \u{b7} feature a-api, b-ui \u{b7} step execute, spec");
        assert_eq!(label_of("claude_turn", 2, &[]), "Loop turn 2");
        assert!(lanes_of_prompt("/orchestrator continue").is_empty());
    }

    #[test]
    fn register_list_unregister() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let lanes = vec![("a".to_string(), "gate".to_string())];
        let id = register(&work, "claude_turn", 3, &lanes, 4242, "claude", "dev@example.com");
        let rows = list(&work, true);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0]["pid"].as_i64(), rows[0]["feature"].as_str(), rows[0]["step"].as_str()), (Some(4242), Some("a"), Some("gate")));
        assert!(rows[0]["started_at"].as_str().is_some_and(|s| s.len() >= 19));
        assert!(list(&work, false).is_empty());
        unregister(&work, &id);
        assert!(list(&work, true).is_empty());
        register(&work, "claude_turn", 4, &[], 1, "claude", "");
        clear(&work);
        assert!(list(&work, true).is_empty());
    }
}
