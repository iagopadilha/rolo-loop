// SPDX-License-Identifier: MIT
//! Files and process facts the guard reads on every call (port of the state half of `guard.py`): loop files in their
//! new or legacy place, the app `.env`, the integration branch, `PERMISSIONS.json`, the turn number, `guard.log`, and the
//! hourly alert for a fail-open.

use crate::paths::G;
use crate::pyre::{nt_dirname, nt_isabs, nt_join, nt_normpath, partition, Re};
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

pub const APP_DATA_DIR_NAME: &str = ".loop-harness";
const LOOP_FOLDER: [&str; 2] = ["project", "loop"];
const LEGACY_LOOP_FOLDER: [&str; 1] = ["project"];
const ATTEMPT_LIMIT: usize = 200;

// ---- Python-style JSON ---------------------------------------------------------------------------------------------------------

fn json_string(text: &str, ensure_ascii: bool, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if ensure_ascii && (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c if c as u32 == 0x7f && ensure_ascii => out.push('\u{7f}'),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `json.dumps(value, ensure_ascii=...)` with Python's default separators (`, ` and `: `).
pub fn py_json_dumps(value: &Value, ensure_ascii: bool) -> String {
    let mut out = String::new();
    write_value(value, ensure_ascii, &mut out);
    out
}

fn write_value(value: &Value, ensure_ascii: bool, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(f) = n.as_f64().filter(|_| n.is_f64()) {
                if f.fract() == 0.0 && f.abs() < 1e16 {
                    out.push_str(&format!("{f:.1}"));
                } else {
                    out.push_str(&format!("{f}"));
                }
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::String(s) => json_string(s, ensure_ascii, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(item, ensure_ascii, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                json_string(k, ensure_ascii, out);
                out.push_str(": ");
                write_value(v, ensure_ascii, out);
            }
            out.push('}');
        }
    }
}

// ---- loop files ------------------------------------------------------------------------------------------------------------------

/// `loop_subfolder`: `""` = the loop folder itself.
pub fn loop_subfolder(name: &str) -> &'static str {
    if name.ends_with(".md") || name == "NOTICE.txt" {
        ""
    } else if name.ends_with(".json") {
        "json"
    } else {
        "internal"
    }
}

/// Path of a loop file: the new layout when the file exists there, else the old flat plugin path when THAT exists, else the new path.
pub fn loop_file(specs_dir: &str, name: &str) -> String {
    let mut current = specs_dir.to_string();
    for part in LOOP_FOLDER {
        current = nt_join(&current, part);
    }
    let sub = loop_subfolder(name);
    if !sub.is_empty() {
        current = nt_join(&current, sub);
    }
    current = nt_join(&current, name);
    if Path::new(&current).exists() {
        return current;
    }
    let mut legacy = specs_dir.to_string();
    for part in LEGACY_LOOP_FOLDER {
        legacy = nt_join(&legacy, part);
    }
    legacy = nt_join(&legacy, name);
    if Path::new(&legacy).exists() {
        return legacy;
    }
    current
}

/// One `KEY=value` of the app's `.env` (`<LOOP_DATA_DIR or ~/.loop-harness>/.env`); empty when absent or unreadable.
pub fn app_env_value(g: &G, name: &str) -> String {
    let data = g
        .env
        .get("LOOP_DATA_DIR")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| nt_join(&g.home(), APP_DATA_DIR_NAME));
    let Ok(bytes) = std::fs::read(nt_join(&data, ".env")) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&bytes).to_string();
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    for raw in text.lines() {
        let mut line = raw.trim();
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim();
        }
        let (key, sep, value) = partition(line, "=");
        if sep && key.trim() == name {
            return value
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string();
        }
    }
    String::new()
}

fn first_non_empty(values: &[String]) -> String {
    values
        .iter()
        .find(|v| !v.is_empty())
        .cloned()
        .unwrap_or_default()
}

/// The branch a push/PR may never target.
pub fn integration_branch(g: &G) -> String {
    let chosen = first_non_empty(&[
        g.env.text("LOOP_PR_BASE"),
        g.env.text("LOOP_INTEGRATION_BRANCH"),
        app_env_value(g, "LOOP_PR_BASE"),
        app_env_value(g, "LOOP_INTEGRATION_BRANCH"),
        "developer".to_string(),
    ]);
    chosen.trim().to_lowercase()
}

/// `LOOP_SPECS_DIR`: absolute path of the PRIMARY `.specs` folder, when it is an existing folder named `.specs`.
pub fn specs_dir_from_env(g: &G) -> Option<String> {
    let raw = g.env.text("LOOP_SPECS_DIR");
    let value = raw
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();
    if !value.is_empty() && nt_isabs(&value) && Path::new(&value).is_dir() {
        let normalized = nt_normpath(&value);
        if normalized.rsplit('\\').next() == Some(".specs") {
            return Some(normalized);
        }
    }
    None
}

/// `LOOP_SPECS_DIR`, else walking up from `cwd`: the `.specs` of the git root wins over a nearer one.
pub fn find_specs_dir(g: &G, cwd: Option<&str>) -> Option<String> {
    if let Some(from_env) = specs_dir_from_env(g) {
        return Some(from_env);
    }
    let start = match cwd.filter(|c| !c.is_empty()) {
        Some(c) => c.to_string(),
        None => std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    };
    let mut here = crate::pyre::nt_abspath(&start);
    let mut nearest: Option<String> = None;
    for _ in 0..8 {
        let candidate = nt_join(&here, ".specs");
        if Path::new(&candidate).is_dir() && nearest.is_none() {
            nearest = Some(candidate.clone());
        }
        if Path::new(&nt_join(&here, ".git")).exists() {
            return if Path::new(&candidate).is_dir() {
                Some(candidate)
            } else {
                nearest
            };
        }
        let parent = nt_dirname(&here);
        if parent == here {
            break;
        }
        here = parent;
    }
    nearest
}

// ---- PERMISSIONS.json --------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Permissions {
    pub allow: Vec<String>,
    pub deny_bash_regex: Vec<Value>,
    pub deny_path_regex: Vec<Value>,
    pub cwd: Option<String>,
    pub specs_dir: Option<String>,
    pub slug: Option<String>,
    pub task: Option<String>,
}

/// Loads the user's releases. An unreadable file releases nothing (and says so on stderr, returned as the second value).
pub fn load_permissions(specs_dir: Option<&str>) -> (Permissions, String) {
    let mut permissions = Permissions::default();
    let Some(specs) = specs_dir.filter(|s| !s.is_empty()) else {
        return (permissions, String::new());
    };
    let path = loop_file(specs, "PERMISSIONS.json");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (permissions, String::new()),
        Err(e) => {
            return (
                permissions,
                format!("guard: PERMISSIONS.json unreadable ({e}); no exception applied\n"),
            )
        }
    };
    let parsed = String::from_utf8(bytes)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()));
    match parsed {
        Ok(Value::Object(map)) => {
            if let Some(Value::Array(items)) = map.get("allow") {
                permissions.allow = items
                    .iter()
                    .filter_map(|entry| match entry {
                        Value::Object(o) => o.get("rule").and_then(Value::as_str).map(String::from),
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect();
            }
            for (key, slot) in [("deny_bash_regex", &mut permissions.deny_bash_regex), ("deny_path_regex", &mut permissions.deny_path_regex)] {
                if let Some(Value::Array(items)) = map.get(key) {
                    *slot = items.clone();
                }
            }
            (permissions, String::new())
        }
        Ok(_) => (permissions, "guard: PERMISSIONS.json unreadable (dictionary update sequence element #0 has length 1; 2 is required); no exception applied\n".to_string()),
        Err(error) => (permissions, format!("guard: PERMISSIONS.json unreadable ({error}); no exception applied\n")),
    }
}

// ---- turn, attempt, log ---------------------------------------------------------------------------------------------------------------

static TURN_FILE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\d+\.json\z"));

/// Number of the loop turn in progress, or None when unknown (known only inside the headless loop).
pub fn current_turn(g: &G, specs_dir: Option<&str>) -> Option<u64> {
    let specs = specs_dir.filter(|s| !s.is_empty())?;
    let exported = g.env.text("LOOP_TURN");
    let exported = exported.trim();
    if !exported.is_empty() && exported.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(n) = exported.parse::<u64>() {
            if n > 0 {
                return Some(n);
            }
        }
    }
    if g.env.get("LOOP_HEADLESS").as_deref() != Some("1") {
        return None;
    }
    let mut folder = specs.to_string();
    for part in LOOP_FOLDER {
        folder = nt_join(&folder, part);
    }
    folder = nt_join(&nt_join(&folder, "internal"), "turns");
    if !Path::new(&folder).is_dir() {
        let mut legacy = specs.to_string();
        for part in LEGACY_LOOP_FOLDER {
            legacy = nt_join(&legacy, part);
        }
        folder = nt_join(&legacy, "turns");
    }
    let entries = std::fs::read_dir(&folder).ok()?;
    let numbers: Vec<u64> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| TURN_FILE.is_match(n))
        .filter_map(|n| n[..n.len() - 5].parse::<u64>().ok())
        .collect();
    Some(numbers.into_iter().max().unwrap_or(0) + 1)
}

/// What the agent tried, in one short line.
pub fn describe_attempt(payload: &Value) -> String {
    let tool = payload.get("tool_name").map(value_text).unwrap_or_default();
    let input = payload.get("tool_input").cloned().unwrap_or(Value::Null);
    let input = if input.is_null() || input == Value::Bool(false) {
        Value::Object(Default::default())
    } else {
        input
    };
    let text = if tool == "Bash" || tool == "PowerShell" {
        input.get("command").map(value_text).unwrap_or_default()
    } else if ["Write", "Edit", "MultiEdit", "NotebookEdit", "Read"].contains(&tool.as_str()) {
        let path = input
            .get("file_path")
            .filter(|v| truthy(v))
            .or_else(|| input.get("notebook_path"))
            .map(value_text)
            .unwrap_or_default();
        format!("{tool} {path}")
    } else {
        tool.clone()
    };
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(ATTEMPT_LIMIT)
        .collect()
}

/// Python's `str(value)` for the JSON types the payload carries.
pub fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => crate::state::py_json_dumps(other, false),
    }
}

/// Python truthiness of a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn now_local_stamp() -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    now.format(format).unwrap_or_default()
}

/// Appends one JSON line to `guard.log`. Never raises.
pub fn log(
    g: &G,
    specs_dir: Option<&str>,
    profile: &str,
    rule: &str,
    decision: &str,
    detail: &str,
    attempt: Option<&str>,
    extra: Option<&[(&str, Value)]>,
) {
    let Some(specs) = specs_dir.filter(|s| !s.is_empty()) else {
        return;
    };
    let mut line = serde_json::Map::new();
    line.insert("t".into(), Value::String(now_local_stamp()));
    line.insert(
        "turn".into(),
        current_turn(g, Some(specs))
            .map(|n| Value::Number(n.into()))
            .unwrap_or(Value::Null),
    );
    line.insert("profile".into(), Value::String(profile.to_string()));
    line.insert("rule".into(), Value::String(rule.to_string()));
    line.insert("decision".into(), Value::String(decision.to_string()));
    line.insert(
        "detail".into(),
        Value::String(detail.chars().take(200).collect()),
    );
    if let Some(a) = attempt.filter(|a| !a.is_empty()) {
        line.insert("attempt".into(), Value::String(a.to_string()));
    }
    if let Some(extra) = extra {
        for (k, v) in extra {
            line.insert((*k).to_string(), v.clone());
        }
    }
    let path = loop_file(specs, "guard.log");
    if let Some(parent) = Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{}", py_json_dumps(&Value::Object(line), false));
    }
}

/// The notify command of the app (`harness notify`), or empty when no app is installed.
pub fn notify_command(g: &G) -> Vec<String> {
    let app = {
        let home = g.env.text("LOOP_APP_HOME");
        if !home.is_empty() {
            home
        } else {
            let data = g
                .env
                .get("LOOP_DATA_DIR")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| nt_join(&g.home(), APP_DATA_DIR_NAME));
            cfg::locator::read_user_config(Path::new(&data))
                .get("app_root_dir")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        }
    };
    if app.is_empty() {
        return vec![];
    }
    for candidate in [
        nt_join(&app, "harness.exe"),
        nt_join(&nt_join(&app, "bin"), "harness.exe"),
    ] {
        if Path::new(&candidate).is_file() {
            return vec![candidate, "notify".to_string()];
        }
    }
    vec![]
}

/// A fail-open must not be silent: logs it and warns through the notify channels, once an hour. Never raises.
pub fn alert(g: &G, specs_dir: Option<&str>, profile: &str, message: &str) {
    log(
        g,
        specs_dir,
        profile,
        "guard-failopen",
        "error",
        message,
        None,
        None,
    );
    let marker = match specs_dir.filter(|s| !s.is_empty()) {
        Some(specs) => {
            let m = loop_file(specs, ".guard-alert");
            if let Some(parent) = Path::new(&m).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            m
        }
        None => nt_join(
            &std::env::temp_dir().display().to_string(),
            "loop-guard-alert",
        ),
    };
    if let Ok(meta) = std::fs::metadata(&marker) {
        if meta.is_file() {
            if let Ok(age) = meta
                .modified()
                .and_then(|m| m.elapsed().map_err(std::io::Error::other))
            {
                if age.as_secs() < 3600 {
                    return;
                }
            }
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    if std::fs::write(&marker, stamp.to_string()).is_err() {
        return;
    }
    let notify = notify_command(g);
    if notify.is_empty() {
        return;
    }
    let body = format!(
        "The orchestrator guard failed internally and LET THE ACTION THROUGH (fail-open, so the loop is not stalled).\nProfile: {profile}\nReason: {}\n\nSee .specs/project/loop/internal/guard.log and run the guard test suite.",
        message.chars().take(300).collect::<String>()
    );
    let _ = std::process::Command::new(&notify[0])
        .args(&notify[1..])
        .args([
            "--subject",
            "[guard] internal failure: the gatekeeper let it through",
            "--body",
            &body,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// `git -C <cwd> branch --show-current` (5 s limit); empty on any failure.
pub fn current_branch(cwd: Option<&str>) -> String {
    let dir = cwd.filter(|c| !c.is_empty()).unwrap_or(".");
    let child = std::process::Command::new("git")
        .args(["-C", dir, "branch", "--show-current"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return String::new();
    };
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed().as_secs() >= 5 {
                    let _ = child.kill();
                    return String::new();
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => return String::new(),
        }
    }
    match child.wait_with_output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

/// `split_commands`: split on `&&`, `||`, `;`, `|`, newline, and a single `&`.
pub fn split_commands(command: &str) -> Vec<String> {
    static SPLIT: LazyLock<Re> = LazyLock::new(|| Re::new(r"&&|\|\||;|\||\n|&(?!&)"));
    SPLIT
        .split(command)
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_dumps_matches_python_defaults() {
        assert_eq!(
            py_json_dumps(
                &json!({"a": 1, "b": [true, null, "x"], "c": {"d": 1.5}}),
                false
            ),
            r#"{"a": 1, "b": [true, null, "x"], "c": {"d": 1.5}}"#
        );
        assert_eq!(py_json_dumps(&json!("\u{e9}\n"), true), r#""\u00e9\n""#);
        assert_eq!(py_json_dumps(&json!("\u{e9}"), false), "\"\u{e9}\"");
        assert_eq!(
            py_json_dumps(&json!("\u{1F600}"), true),
            r#""\ud83d\ude00""#
        );
        assert_eq!(py_json_dumps(&json!("C:\\x"), true), r#""C:\\x""#);
    }

    #[test]
    fn split_commands_matches_python() {
        assert_eq!(
            split_commands("a && b || c; d | e\nf & g"),
            vec!["a", "b", "c", "d", "e", "f", "g"]
        );
    }

    #[test]
    fn describe_attempt_shapes() {
        assert_eq!(
            describe_attempt(
                &json!({"tool_name": "Bash", "tool_input": {"command": "git   push\norigin"}})
            ),
            "git push origin"
        );
        assert_eq!(
            describe_attempt(&json!({"tool_name": "Write", "tool_input": {"file_path": "a.txt"}})),
            "Write a.txt"
        );
        assert_eq!(
            describe_attempt(&json!({"tool_name": "WebFetch", "tool_input": {}})),
            "WebFetch"
        );
    }

    #[test]
    fn integration_branch_precedence() {
        let g = G::new(cfg::Env::from_pairs([(
            "LOOP_DATA_DIR",
            "C:\\nonexistent-data-dir",
        )]));
        assert_eq!(integration_branch(&g), "developer");
        let g = G::new(cfg::Env::from_pairs([
            ("LOOP_DATA_DIR", "C:\\nonexistent-data-dir"),
            ("LOOP_INTEGRATION_BRANCH", " Release/Next "),
        ]));
        assert_eq!(integration_branch(&g), "release/next");
    }
}
