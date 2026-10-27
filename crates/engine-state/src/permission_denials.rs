// SPDX-License-Identifier: MIT
//! Permission denials of a turn: which ones always stop the loop (risky), how a tolerated one is written down, and whether the turn
//! advanced.
//!
//! A denial of a common command (write evidence, read a file) that the agent redoes another way does NOT stop the loop: it becomes a line
//! in VALIDATE.md and the turn carries on. It always stops when the denied command looks risky, or when the turn did not advance.

use crate::decisions;
use crate::loop_io::{load_json, read};
use crate::loop_state::loop_path;
use cfg::env::app_env_option;
use cfg::Env;
use pycompat::json::py_json_dumps;
use pycompat::Re;
use serde_json::Value;
use std::path::Path;
use std::sync::LazyLock;

static RISKY_DENIAL: LazyLock<Re> = LazyLock::new(|| Re::ci(r"git\s+push|--force|push\s+-f|rm\s+-rf|DROP\s+(TABLE|DATABASE)|TRUNCATE|DELETE\s+FROM"));
static SECRET_LIKE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[A-Za-z0-9+/=_-]{24,}"));
static SPACES: LazyLock<Re> = LazyLock::new(|| Re::new(r"\s+"));

/// `GUARD_PROD_HINTS` (the guard's production words, comma-separated; default prod,production) as whole-word regexes.
pub fn prod_hint_patterns(env: &Env) -> Vec<Re> {
    let raw = app_env_option(env, "GUARD_PROD_HINTS").unwrap_or_else(|| "prod,production".into());
    raw.split(',')
        .map(str::trim)
        .filter(|i| !i.is_empty())
        .filter_map(|item| Re::try_build(&format!("(?<![A-Za-z0-9]){}(?![A-Za-z0-9])", pycompat::escape(item)), true))
        .collect()
}

/// The project's `deny_bash_regex` from PERMISSIONS.json (block rules written with `harness rules`): a denied command that matches one of
/// them is never tolerated.
pub fn project_deny_patterns(work: &str) -> Vec<Re> {
    if work.is_empty() {
        return vec![];
    }
    let mut path = loop_path(work, "PERMISSIONS.json");
    if !path.exists() {
        path = Path::new(work).join(".specs").join("project").join("PERMISSIONS.json");
    }
    let data = load_json(&path);
    data.get("deny_bash_regex").and_then(Value::as_array).map(|list| list.iter().filter_map(|p| Re::try_build(&text_of(Some(p)), true)).collect()).unwrap_or_default()
}

fn text_of(v: Option<&Value>) -> String {
    match v {
        None => "None".into(),
        Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => py_json_dumps(other, false),
    }
}

fn truthy_text(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Null) | None | Some(Value::Bool(false)) => None,
        Some(Value::String(_)) => None,
        Some(other) => Some(text_of(Some(other))),
    }
}

/// A denial that always stops the loop: any MCP tool, or a command that looks like push, force, rm -rf, destructive SQL, production
/// (`GUARD_PROD_HINTS`) or something the project forbids (PERMISSIONS.json `deny_bash_regex`).
pub fn is_risky_denial(env: &Env, denial: &Value, work: &str) -> bool {
    if text_of(denial.get("tool_name")).starts_with("mcp__") && denial.get("tool_name").is_some_and(|v| !v.is_null()) {
        return true;
    }
    let input = denial.get("tool_input").filter(|v| crate::notify_truthy(Some(*v))).cloned().unwrap_or_else(|| serde_json::json!({}));
    let text = py_json_dumps(&input, false);
    if RISKY_DENIAL.search(&text).is_some() {
        return true;
    }
    let command = truthy_text(input.get("command")).unwrap_or_else(|| text.clone());
    prod_hint_patterns(env).iter().any(|p| p.search(&text).is_some()) || project_deny_patterns(work).iter().any(|p| p.search(&command).is_some())
}

/// A short line without secrets (a long sequence becomes `***`) for VALIDATE.md, the notice and the summary.
pub fn denial_text(denial: &Value) -> String {
    let input = denial.get("tool_input").filter(|v| crate::notify_truthy(Some(*v))).cloned().unwrap_or_else(|| serde_json::json!({}));
    let text = truthy_text(input.get("command")).or_else(|| truthy_text(input.get("file_path"))).unwrap_or_else(|| py_json_dumps(&input, false));
    let flat = SPACES.replace_lit(&text, " ");
    let masked = SECRET_LIKE.replace_lit(flat.trim(), "***");
    format!("{}: {}", text_of(denial.get("tool_name")), masked.chars().take(140).collect::<String>())
}

/// Notes each tolerated denial in VALIDATE.md; the final summary and the PR show that file as "For you to validate".
pub fn note_tolerated_denials(work: &str, turn: i64, denials: &[Value], progress: &str) {
    for denial in denials {
        decisions::append(
            work,
            "general",
            Some(turn),
            "important",
            &format!("permission denied ({})", denial_text(denial)),
            &format!("carried on, the turn advanced ({progress})"),
            "check whether what was denied was needed",
        );
    }
}

/// Why the turn counts as advanced (empty text = it did not advance): new commit, ROADMAP changed or it stopped asking you.
pub fn denial_progress(prev: Option<&serde_json::Map<String, Value>>, specs: &str, state: &Value, new_commits: i64) -> String {
    if new_commits != 0 {
        return "new commit".into();
    }
    let before = prev.map(|p| read(&Path::new(p.get("dir").and_then(Value::as_str).unwrap_or("")).join("ROADMAP.md"))).unwrap_or_default();
    if before != read(&Path::new(specs).join("ROADMAP.md")) {
        return "ROADMAP changed".into();
    }
    match state.get("status").and_then(Value::as_str) {
        Some("done") | Some("awaiting_user") => "stopped asking you".into(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env() -> Env {
        Env::from_pairs([("LOOP_DATA_DIR", "C:\\no-such-dir")])
    }

    #[test]
    fn risky_denials_always_stop() {
        let work = tempfile::tempdir().unwrap();
        let w = work.path().display().to_string();
        let denial = |tool: &str, input: Value| json!({"tool_name": tool, "tool_input": input});
        assert!(is_risky_denial(&env(), &denial("mcp__db__query", json!({})), &w));
        assert!(is_risky_denial(&env(), &denial("Bash", json!({"command": "git push origin x"})), &w));
        assert!(is_risky_denial(&env(), &denial("Bash", json!({"command": "psql -c 'drop table x'"})), &w));
        assert!(is_risky_denial(&env(), &denial("Bash", json!({"command": "ssh prod-db"})), &w));
        assert!(!is_risky_denial(&env(), &denial("Write", json!({"file_path": "evidence/a.png"})), &w));
        assert!(!is_risky_denial(&env(), &denial("Bash", json!({"command": "echo producer"})), &w));
        std::fs::create_dir_all(work.path().join(".specs/project")).unwrap();
        std::fs::write(work.path().join(".specs/project/PERMISSIONS.json"), r#"{"deny_bash_regex": ["terraform\\s+apply", "(bad"]}"#).unwrap();
        assert!(is_risky_denial(&env(), &denial("Bash", json!({"command": "terraform  apply -auto-approve"})), &w));
        let custom = Env::from_pairs([("GUARD_PROD_HINTS", "")]);
        assert!(!is_risky_denial(&custom, &denial("Bash", json!({"command": "ssh prod-db"})), ""));
    }

    #[test]
    fn denial_lines_hide_secrets_and_clip() {
        let d = json!({"tool_name": "Bash", "tool_input": {"command": format!("curl  -H  {}  x", "a".repeat(30))}});
        assert_eq!(denial_text(&d), "Bash: curl -H *** x");
        assert_eq!(denial_text(&json!({"tool_name": "Write", "tool_input": {"file_path": "a/b.txt"}})), "Write: a/b.txt");
        assert_eq!(denial_text(&json!({"tool_input": {"x": 1}})), "None: {\"x\": 1}");
        assert!(denial_text(&json!({"tool_name": "T", "tool_input": {"command": "x".repeat(300)}})).chars().count() <= 143);
    }

    #[test]
    fn progress_reasons() {
        let dir = tempfile::tempdir().unwrap();
        let specs = dir.path().join("specs");
        let prev_dir = dir.path().join("prev");
        std::fs::create_dir_all(&specs).unwrap();
        std::fs::create_dir_all(&prev_dir).unwrap();
        std::fs::write(specs.join("ROADMAP.md"), "a").unwrap();
        std::fs::write(prev_dir.join("ROADMAP.md"), "a").unwrap();
        let prev = json!({"dir": prev_dir.display().to_string()});
        let s = specs.display().to_string();
        assert_eq!(denial_progress(prev.as_object(), &s, &json!({"status": "continue"}), 2), "new commit");
        assert_eq!(denial_progress(prev.as_object(), &s, &json!({"status": "continue"}), 0), "");
        assert_eq!(denial_progress(prev.as_object(), &s, &json!({"status": "done"}), 0), "stopped asking you");
        std::fs::write(specs.join("ROADMAP.md"), "b").unwrap();
        assert_eq!(denial_progress(prev.as_object(), &s, &json!({"status": "continue"}), 0), "ROADMAP changed");
        let work = dir.path().display().to_string();
        note_tolerated_denials(&work, 4, &[json!({"tool_name": "Bash", "tool_input": {"command": "ls"}})], "new commit");
        assert!(decisions::read_file(&work).contains("- [general] turn=4 level=important permission denied (Bash: ls) | chose: carried on, the turn advanced (new commit) | revert: check whether what was denied was needed"));
    }
}
