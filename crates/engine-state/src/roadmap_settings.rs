// SPDX-License-Identifier: MIT
//! Project-wide settings written in the ROADMAP header (not per feature).
//!
//! `**E2E:** yes|no` = the loop ends with the flow-validation phase. Absent or unreadable = no. Decided during planning.

use pycompat::{escape, Re};
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

static HTML_COMMENT: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?s)<!--.*?-->"));
static E2E_LINE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?m)^\s*(?:[-*]\s+)?\*{0,2}E2E\*{0,2}\s*:\s*\*{0,2}\s*(yes|no|true|false)\b"));

/// True/False from the first `E2E: yes|no` line outside HTML comments; `None` when absent.
pub fn parse_roadmap_e2e(text: &str) -> Option<bool> {
    let cleaned = HTML_COMMENT.replace_lit(text, "");
    E2E_LINE.search(&cleaned).map(|m| matches!(m.group(1).unwrap_or("").to_lowercase().as_str(), "yes" | "true"))
}

pub const DEFAULT_DISPATCH: &str = "auto";
const MIN_DISPATCH: i64 = 1;
const MAX_DISPATCH: i64 = 8;
const MIN_TURN_MINUTES: i64 = 1;
const MAX_TURN_MINUTES: i64 = 600;
pub const MIN_PARALLEL_TASKS: i64 = 1;
pub const MAX_PARALLEL_TASKS: i64 = 5;
pub const DEFAULT_MAX_PARALLEL_TASKS: i64 = 3;
const SETUP_LIMIT: usize = 200;
const AUTOMATIC_BRANCH_WORD: &str = "automatic";
pub const DEFAULT_BASE_BRANCH: &str = "developer";
const WHEN_STUCK_VALUES: [&str; 2] = ["pause", "continue"];

static BRANCH: LazyLock<Re> = LazyLock::new(|| Re::new(r"[A-Za-z0-9._/-]{1,80}"));
static TRAILING_NOTE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\s*\([^)]*\)\s*\z"));
static MONEY: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?:US\$|\$)?\s*(\d+(?:[.,]\d+)?)"));
static WHOLE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\d+"));

/// `(key, header name)` exactly as written in the ROADMAP (`**Name:** value`).
pub const LOOP_SETTING_HEADER_NAMES: [(&str, &str); 11] = [
    ("dispatch", "Dispatch"),
    ("e2e", "E2E"),
    ("budget_turn", "Max spend per turn"),
    ("budget_loop", "Max spend for the loop"),
    ("minutes_turn", "Max minutes per turn"),
    ("branch", "Branch"),
    ("base_branch", "Branch base"),
    ("setup", "Setup"),
    ("when_stuck", "When a feature gets stuck"),
    ("push_pr", "Push and PR"),
    ("max_parallel_tasks", "Max parallel tasks"),
];

fn defaults() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("dispatch".into(), json!(DEFAULT_DISPATCH));
    m.insert("e2e".into(), json!(false));
    m.insert("budget_turn".into(), json!(5.0));
    m.insert("budget_loop".into(), json!(50.0));
    m.insert("minutes_turn".into(), json!(60));
    m.insert("branch".into(), json!(""));
    m.insert("base_branch".into(), json!(DEFAULT_BASE_BRANCH));
    m.insert("setup".into(), json!(""));
    m.insert("when_stuck".into(), json!("pause"));
    m.insert("push_pr".into(), json!(true));
    m.insert("max_parallel_tasks".into(), json!(DEFAULT_MAX_PARALLEL_TASKS));
    m
}

fn header_pattern(name: &str) -> Re {
    Re::ci(&format!(r"(?m)^[ \t]*(?:[-*][ \t]+)?\*{{0,2}}{}\*{{0,2}}[ \t]*:[ \t]*\*{{0,2}}[ \t]*(.*?)[ \t]*$", escape(name)))
}

fn describe_default(key: &str) -> String {
    let value = defaults().remove(key).unwrap_or(Value::Null);
    match value {
        Value::Bool(true) => "yes".into(),
        Value::Bool(false) => "no".into(),
        Value::String(s) if s.is_empty() => if key == "branch" { "automatic".into() } else { "empty".into() },
        Value::String(s) => s,
        Value::Number(n) if n.is_f64() => format!("{:.1}", n.as_f64().unwrap_or(0.0)),
        other => other.to_string(),
    }
}

fn first_word_lower(raw: &str) -> String {
    raw.split_whitespace().next().unwrap_or("").to_lowercase()
}

fn convert_yes_no(raw: &str) -> Result<Value, String> {
    match first_word_lower(raw).as_str() {
        "yes" | "true" => Ok(json!(true)),
        "no" | "false" => Ok(json!(false)),
        _ => Err("write yes or no".into()),
    }
}

fn convert_money(raw: &str) -> Result<Value, String> {
    let cleaned = TRAILING_NOTE.replace_lit(raw, "");
    let Some(found) = MONEY.match_start(&cleaned).filter(|m| m.end == cleaned.len()) else {
        return Err("write an amount in US$, for example 5".into());
    };
    let number = found.group(1).unwrap_or("").replace(',', ".");
    Ok(json!(number.parse::<f64>().unwrap_or(0.0)))
}

fn convert_whole_number(raw: &str, minimum: i64, maximum: i64) -> Result<i64, String> {
    let cleaned = TRAILING_NOTE.replace_lit(raw, "");
    let message = format!("write a whole number from {minimum} to {maximum}");
    if !WHOLE.full_match(&cleaned) {
        return Err(message);
    }
    match cleaned.parse::<i64>() {
        Ok(n) if (minimum..=maximum).contains(&n) => Ok(n),
        _ => Err(message),
    }
}

fn convert_dispatch(raw: &str) -> Result<Value, String> {
    let cleaned = TRAILING_NOTE.replace_lit(raw, "").trim().to_string();
    if cleaned.to_lowercase() == DEFAULT_DISPATCH {
        return Ok(json!(DEFAULT_DISPATCH));
    }
    convert_whole_number(&cleaned, MIN_DISPATCH, MAX_DISPATCH).map(|n| json!(n))
}

const BRANCH_MESSAGE: &str = "use only letters, digits, . _ / - (up to 80), without '..' and not starting with '-'";

fn convert_branch(raw: &str) -> Result<Value, String> {
    let cleaned = TRAILING_NOTE.replace_lit(raw, "").trim().to_string();
    if cleaned.is_empty() || cleaned.to_lowercase() == AUTOMATIC_BRANCH_WORD {
        return Ok(json!(""));
    }
    if !BRANCH.full_match(&cleaned) || cleaned.contains("..") || cleaned.starts_with('-') {
        return Err(BRANCH_MESSAGE.into());
    }
    Ok(json!(cleaned))
}

fn convert_base_branch(raw: &str) -> Result<Value, String> {
    let cleaned = TRAILING_NOTE.replace_lit(raw, "").trim().to_string();
    if !BRANCH.full_match(&cleaned) || cleaned.contains("..") || cleaned.starts_with('-') {
        return Err(BRANCH_MESSAGE.into());
    }
    Ok(json!(cleaned))
}

fn convert_setup(raw: &str) -> Result<Value, String> {
    if raw.chars().count() > SETUP_LIMIT {
        return Err(format!("longer than {SETUP_LIMIT} characters"));
    }
    Ok(json!(raw))
}

fn convert_when_stuck(raw: &str) -> Result<Value, String> {
    let first = first_word_lower(raw);
    if !WHEN_STUCK_VALUES.contains(&first.as_str()) {
        return Err("write pause or continue".into());
    }
    Ok(json!(first))
}

fn convert(key: &str, raw: &str) -> Result<Value, String> {
    match key {
        "dispatch" => convert_dispatch(raw),
        "e2e" | "push_pr" => convert_yes_no(raw),
        "budget_turn" | "budget_loop" => convert_money(raw),
        "minutes_turn" => convert_whole_number(raw, MIN_TURN_MINUTES, MAX_TURN_MINUTES).map(|n| json!(n)),
        "branch" => convert_branch(raw),
        "base_branch" => convert_base_branch(raw),
        "setup" => convert_setup(raw),
        "when_stuck" => convert_when_stuck(raw),
        "max_parallel_tasks" => convert_whole_number(raw, MIN_PARALLEL_TASKS, MAX_PARALLEL_TASKS).map(|n| json!(n)),
        _ => Err("unknown setting".into()),
    }
}

/// Raw value of the first header line of that setting outside HTML comments; `None` when the line is absent.
fn read_header_value(cleaned: &str, key: &str) -> Option<String> {
    let name = LOOP_SETTING_HEADER_NAMES.iter().find(|(k, _)| *k == key).map(|(_, n)| *n)?;
    let found = header_pattern(name).search(cleaned)?;
    Some(found.group(1).unwrap_or("").trim().trim_matches('*').trim().to_string())
}

pub struct ParsedSettings {
    pub settings: Map<String, Value>,
    /// `{"name", "value", "message"}` each.
    pub problems: Vec<Value>,
    pub missing: Vec<String>,
    pub defaults: Map<String, Value>,
}

impl ParsedSettings {
    pub fn to_json(&self) -> Value {
        json!({"settings": self.settings, "problems": self.problems, "missing": self.missing, "defaults": self.defaults})
    }
}

/// The loop settings written in the ROADMAP header. A missing line = the default (no problem); an invalid value = the default + a problem.
pub fn parse_loop_settings(text: &str) -> ParsedSettings {
    let cleaned = HTML_COMMENT.replace_lit(text, "");
    let mut settings = defaults();
    let mut problems = Vec::new();
    let mut missing = Vec::new();
    for (key, header_name) in LOOP_SETTING_HEADER_NAMES {
        let raw = read_header_value(&cleaned, key);
        if raw.as_deref() == Some("") && key == "base_branch" {
            problems.push(json!({"name": header_name, "value": "", "message": format!("write a branch name: using the default ({})", describe_default(key))}));
            continue;
        }
        match raw {
            None => {
                missing.push(key.to_string());
                continue;
            }
            Some(r) if r.is_empty() && key != "setup" => {
                missing.push(key.to_string());
                continue;
            }
            Some(raw) => match convert(key, &raw) {
                Ok(v) => {
                    settings.insert(key.to_string(), v);
                }
                Err(failure) => {
                    problems.push(json!({"name": header_name, "value": raw, "message": format!("{failure}: using the default ({})", describe_default(key))}));
                }
            },
        }
    }
    ParsedSettings { settings, problems, missing, defaults: defaults() }
}

/// The branch written in `**Branch base:**` when the line is there and valid; empty otherwise.
pub fn read_roadmap_base_branch(text: &str) -> String {
    let parsed = parse_loop_settings(text);
    let base_name = "Branch base";
    if parsed.missing.iter().any(|k| k == "base_branch") || parsed.problems.iter().any(|p| p["name"] == base_name) {
        String::new()
    } else {
        parsed.settings.get("base_branch").and_then(Value::as_str).unwrap_or("").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e2e_line_ignores_comments() {
        assert_eq!(parse_roadmap_e2e("**E2E:** yes"), Some(true));
        assert_eq!(parse_roadmap_e2e("- E2E: no"), Some(false));
        assert_eq!(parse_roadmap_e2e("<!-- E2E: yes -->\n"), None);
        assert_eq!(parse_roadmap_e2e(""), None);
    }

    #[test]
    fn header_settings_and_problems() {
        let text = "**Dispatch:** 4\n**E2E:** yes\n**Max spend per turn:** US$ 7,5 (cap)\n**Max minutes per turn:** 9999\n**Branch:** feature/x\n**Branch base:**\n**When a feature gets stuck:** later\n**Max parallel tasks:** 2\n";
        let p = parse_loop_settings(text);
        assert_eq!(p.settings["dispatch"], 4);
        assert_eq!(p.settings["e2e"], true);
        assert_eq!(p.settings["budget_turn"], 7.5);
        assert_eq!(p.settings["branch"], "feature/x");
        assert_eq!(p.settings["max_parallel_tasks"], 2);
        let names: Vec<&str> = p.problems.iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Max minutes per turn", "Branch base", "When a feature gets stuck"]);
        assert_eq!(p.problems[0]["message"], "write a whole number from 1 to 600: using the default (60)");
        assert!(p.missing.contains(&"budget_loop".to_string()));
        assert_eq!(read_roadmap_base_branch(text), "");
        assert_eq!(read_roadmap_base_branch("Branch base: main"), "main");
    }
}
