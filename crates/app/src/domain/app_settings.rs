// SPDX-License-Identifier: MIT
//! The app settings of `~/.loop-harness/config.json` that the Settings screen edits: the catalog (key, text, type, default) and the validation of
//! a new value. Pure: no file, no HTTP. A key the user never saved stays absent from config.json, so every default here equals the one the code
//! already uses.

use crate::domain::errors::{AppError, Kind, Result};
use cfg::Env;
use egress_policy::{parse_deny, parse_entry};
use pycompat::{py_repr_str, Re};
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

pub const BOOL: &str = "bool";
pub const INT: &str = "int";
pub const NUMBER: &str = "number";
pub const TEXT: &str = "text";
pub const LIST: &str = "list";
pub const CHOICE: &str = "choice";
const TEXT_MAX_LENGTH: usize = 500;
const LIST_MAX_ITEMS: usize = 50;
/// `loop_extra_skills` takes the whole skill list of a machine (the Settings screen lists at most this many).
const SKILLS_MAX_ITEMS: usize = 400;
pub const CAVEMAN_NOT_INSTALLED_NOTE: &str =
    "Not installed: no SessionStart hook that mentions caveman was found in your Claude settings (~/.claude/settings.json), so there is nothing to carry.";
pub const CAVEMAN_RECOMMENDATION: &str = "Off by default: loop output is mostly machine-read lines and files, so terse replies save little. Turn it on to measure.";
/// config.json: `{server: {allow, kind, production, read_only}}` (grammar: the `mcp-policy` crate).
pub const MCP_SERVERS_KEY: &str = "mcp_servers";
/// config.json: full tool-name patterns, trailing * allowed.
pub const MCP_EXTRA_KEY: &str = "mcp_allow_extra";
pub const MCP_KEYS: [&str; 2] = [MCP_SERVERS_KEY, MCP_EXTRA_KEY];
pub const NETWORK_KEYS: [&str; 5] = ["egress_hosts", "egress_never", "egress_builtin_read", "env_sources", "loop_max_hours"];

/// A skill is a folder name: never a path.
static SKILL_NAME: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,99}\z"));

#[derive(Clone, Debug)]
pub struct SettingDefinition {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub kind: &'static str,
    pub default: Value,
    pub restart_required: bool,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    /// `(value, label)` pairs of a `choice`.
    pub choices: Vec<(&'static str, &'static str)>,
}

fn def(key: &'static str, label: &'static str, description: &'static str, kind: &'static str, default: Value) -> SettingDefinition {
    SettingDefinition { key, label, description, kind, default, restart_required: false, minimum: None, maximum: None, choices: vec![] }
}

/// The catalog of the Settings screen, in display order.
pub fn setting_catalog() -> Vec<SettingDefinition> {
    let restart = |mut d: SettingDefinition| {
        d.restart_required = true;
        d
    };
    let min = |mut d: SettingDefinition, m: f64| {
        d.minimum = Some(m);
        d
    };
    vec![
        def("loop_isolation", "Isolate every Claude run", "Runs each claude -p with only the skills, agents and hooks the app allows, instead of your whole profile. Turn it off only to get the old behavior back, for example to debug a missing skill.", BOOL, json!(true)),
        def("loop_caveman", "Caveman mode", "Carries your caveman SessionStart hook into the isolated runs, so the replies come out terse. Needs isolation on and the hook installed in your Claude settings.", BOOL, json!(false)),
        def("loop_extra_skills", "Extra skills", "Skill names (one per line) added to the allowlist of every run, on top of the defaults. Add one when a run needs a skill from your profile that the app does not include.", LIST, json!([])),
        {
            let mut d = restart(def("orchestrator_model", "Orchestrator model", "The model of the loop's orchestrator turns. Subagents and app jobs keep their own ceiling (Sonnet). Pick the account default to send no model flag.", CHOICE, json!("opus")));
            d.choices = vec![("opus", "Opus (latest)"), ("sonnet", "Sonnet (latest)"), ("haiku", "Haiku (latest)"), ("", "Account default (no flag)")];
            d
        },
        restart(def("claude_binary", "Claude command", "The command that starts Claude Code. Empty uses claude from your PATH. Change it to run a wrapper (for example cc) signed in with another account.", TEXT, json!(""))),
        restart(def("claude_fallback_binaries", "Fallback commands", "Commands tried in order, one per line, when the running one hits the account usage limit. A command missing on this machine is skipped. A loop that is already running reads the list again before each turn.", LIST, json!([]))),
        min(restart(def("max_parallel_maps", "Parallel subproject maps", "How many subproject map jobs of a monorepo run at the same time; the rest wait in a queue. Lower it if your machine or account limit struggles.", INT, json!(6))), 1.0),
        min(restart(def("idle_hours", "Idle shutdown (hours)", "The app shuts itself down after this many hours without activity or a running loop. 0 means it never shuts down.", NUMBER, json!(2))), 0.0),
        min(restart(def("inventory_giant_lines", "Giant file (lines)", "A code file with more lines than this counts as giant: the readiness check warns when a feature touches one without a refactor feature before it.", INT, json!(1500))), 1.0),
        min(restart(def("inventory_giant_kb", "Giant file (KB)", "A code file larger than this many KB also counts as giant. Works together with the line limit.", INT, json!(200))), 1.0),
        restart(def("output_dir", "Output folder", "The folder where exported files (reports, summaries) are written. Unset uses your Downloads folder.", TEXT, json!(""))),
        def("egress_hosts", "Allowed hosts (every project)", "Hosts the agents of every project may reach, one entry per line: host[:port][/path] [METHODS] [cred] [if-missing=NAME], for example api.example.com GET,POST cred. Each project adds its own list on top.", LIST, json!([])),
        def("egress_never", "Never-reach hosts", "Host patterns no project may reach, one per line (example.com or *.example.com, any port and method). A deny always wins over every allow.", LIST, json!([])),
        def("egress_builtin_read", "Built-in read list", "Lets the agents read (GET, HEAD) the documentation and package sites of the built-in list. Turn it off to reach only the hosts you list.", BOOL, json!(true)),
        def("env_sources", "Credential sources", "Full paths of the .env files test values can be imported from, one per line (a leading ~ is your home folder). Nothing reads them at run time: the panel copies the names you choose into the project's .env.test. Empty means the app data folder's .env (~/.loop-harness/.env).", LIST, json!(["~/.loop-harness/.env"])),
        {
            let mut d = min(def("loop_max_hours", "Longest loop you expect (hours)", "Used to check that test tokens do not expire mid-loop: a test token that expires within this many hours counts as a problem in the Test environment check. It does not limit how long a loop runs.", INT, json!(24)), 1.0);
            d.maximum = Some(168.0);
            d
        },
    ]
}

pub fn invalid_setting(message: impl Into<String>) -> AppError {
    AppError::new(Kind::InvalidSetting, message)
}

/// `(clean servers, clean extra patterns)` for the MCP servers section, judged by the SAME crate the guard uses: unknown option fields,
/// non-boolean flags, a bad kind, a bad pattern or two names with the same tool prefix are an `InvalidSetting`.
pub fn validate_mcp_settings(servers: &Value, extra: &Value) -> Result<(Value, Value)> {
    match mcp_policy::check_config(servers, extra) {
        Ok((list, patterns)) => {
            let map: Map<String, Value> =
                list.into_iter().map(|(name, o)| (name, json!({"allow": o.allow, "kind": o.kind, "production": o.production, "read_only": o.read_only}))).collect();
            Ok((Value::Object(map), json!(patterns)))
        }
        Err(error) => Err(invalid_setting(error.to_string())),
    }
}

pub fn find_definition(key: &Value) -> Result<SettingDefinition> {
    let name = key.as_str().unwrap_or("");
    setting_catalog()
        .into_iter()
        .find(|d| d.key == name && key.is_string())
        .ok_or_else(|| invalid_setting(format!("unknown setting: {}", key.as_str().map(String::from).unwrap_or_else(|| py_type_name(key)))))
}

fn py_type_name(v: &Value) -> String {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
    .to_string()
}

/// The value shown as default (the output folder shows the real path of Downloads).
pub fn get_default_value(definition: &SettingDefinition, env: &Env) -> Value {
    if definition.key == "output_dir" {
        return json!(env.home_dir().join("Downloads").display().to_string());
    }
    definition.default.clone()
}

fn is_whole_number(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.is_i64() || n.is_u64())
}

fn is_finite_number(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.as_f64().is_some_and(f64::is_finite))
}

fn read_text_value(definition: &SettingDefinition, value: &Value) -> Result<String> {
    let Value::String(raw) = value else { return Err(invalid_setting(format!("{} must be text", definition.key))) };
    let text = raw.trim().to_string();
    if text.chars().count() > TEXT_MAX_LENGTH || text.chars().any(|c| (c as u32) < 32) {
        return Err(invalid_setting(format!("{} must be one line of at most {TEXT_MAX_LENGTH} characters", definition.key)));
    }
    Ok(text)
}

fn read_list_value(definition: &SettingDefinition, value: &Value) -> Result<Value> {
    let Value::Array(raw) = value else { return Err(invalid_setting(format!("{} must be a list", definition.key))) };
    let mut items: Vec<String> = Vec::new();
    for item in raw {
        if !item.is_string() {
            return Err(invalid_setting(format!("{} must be a list of text", definition.key)));
        }
        let text = read_text_value(definition, item)?;
        if !text.is_empty() && !items.contains(&text) {
            items.push(text);
        }
    }
    let max_items = if definition.key == "loop_extra_skills" { SKILLS_MAX_ITEMS } else { LIST_MAX_ITEMS };
    if items.len() > max_items {
        return Err(invalid_setting(format!("{} accepts at most {max_items} items", definition.key)));
    }
    if definition.key == "loop_extra_skills" {
        for name in &items {
            if SKILL_NAME.match_start(name).is_none() {
                return Err(invalid_setting(format!("{} is not a valid skill name (letters, digits, dot, dash and underscore only)", py_repr_str(name))));
            }
        }
    }
    check_network_list(definition.key, &items)?;
    Ok(json!(items))
}

/// The list keys of the network feature: every item must be valid for its grammar (the error names the item's position and why).
fn check_network_list(key: &str, items: &[String]) -> Result<()> {
    for (index, text) in items.iter().enumerate() {
        let position = index + 1;
        let outcome: std::result::Result<(), String> = match key {
            "egress_hosts" => parse_entry(&json!(text), "project").map(|_| ()),
            "egress_never" => parse_deny(&json!(text)).map(|_| ()),
            "env_sources" => {
                let expanded = if let Some(rest) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
                    format!("{}\\{rest}", cfg::Env::process().home_dir().display())
                } else if text == "~" {
                    cfg::Env::process().home_dir().display().to_string()
                } else {
                    text.clone()
                };
                if pycompat::nt_isabs(&expanded) {
                    Ok(())
                } else {
                    Err("use a full path (or one that starts with ~)".to_string())
                }
            }
            _ => Ok(()),
        };
        if let Err(error) = outcome {
            return Err(invalid_setting(format!("{key} item {position} ({text}): {error}")));
        }
    }
    Ok(())
}

/// The value the app uses for a plain stored key (bool, whole number or list of text): the stored one when it fits, else the default. A list
/// keeps its text items (the engine ignores an invalid entry by itself and says so in its notes).
pub fn read_stored_or_default(definition: &SettingDefinition, stored: &Map<String, Value>) -> Value {
    let value = stored.get(definition.key).cloned().unwrap_or_else(|| definition.default.clone());
    match definition.kind {
        BOOL => {
            if value.is_boolean() {
                value
            } else {
                definition.default.clone()
            }
        }
        INT => {
            let n = value.as_f64();
            let fits = is_whole_number(&value) && n.is_some_and(|n| definition.minimum.is_none_or(|m| n >= m) && definition.maximum.is_none_or(|m| n <= m));
            if fits {
                value
            } else {
                definition.default.clone()
            }
        }
        LIST => match &value {
            Value::Array(items) => json!(items.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>()),
            _ => json!([]),
        },
        _ => value,
    }
}

fn number_text(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// The value to store, or an `InvalidSetting`. Bool must be a JSON boolean, int a whole number (no 6.5, no true), number finite.
pub fn validate_setting_value(definition: &SettingDefinition, value: &Value) -> Result<Value> {
    match definition.kind {
        BOOL => {
            if !value.is_boolean() {
                return Err(invalid_setting(format!("{} must be true or false", definition.key)));
            }
            Ok(value.clone())
        }
        INT | NUMBER => {
            let usable = if definition.kind == INT { is_whole_number(value) } else { is_finite_number(value) };
            if !usable {
                return Err(invalid_setting(format!("{} must be a {}", definition.key, if definition.kind == INT { "whole number" } else { "number" })));
            }
            let n = value.as_f64().unwrap_or(0.0);
            if let Some(m) = definition.minimum.filter(|m| n < *m) {
                return Err(invalid_setting(format!("{} must be at least {}", definition.key, number_text(m))));
            }
            if let Some(m) = definition.maximum.filter(|m| n > *m) {
                return Err(invalid_setting(format!("{} must be at most {}", definition.key, number_text(m))));
            }
            Ok(value.clone())
        }
        CHOICE => {
            let allowed: Vec<&str> = definition.choices.iter().map(|(c, _)| *c).collect();
            match value.as_str().map(str::trim) {
                Some(t) if allowed.contains(&t) => Ok(json!(t)),
                _ => Err(invalid_setting(format!("{} must be one of: {}", definition.key, allowed.iter().map(|c| py_repr_str(c)).collect::<Vec<_>>().join(", ")))),
            }
        }
        LIST => read_list_value(definition, value),
        _ => {
            let text = read_text_value(definition, value)?;
            if definition.key == "output_dir" && text.is_empty() {
                return Err(invalid_setting("output_dir cannot be empty: use Reset to default instead"));
            }
            Ok(json!(text))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def_of(key: &str) -> SettingDefinition {
        find_definition(&json!(key)).unwrap()
    }

    #[test]
    fn values_are_judged_by_type() {
        assert_eq!(setting_catalog().len(), 16);
        assert_eq!(validate_setting_value(&def_of("loop_isolation"), &json!(false)).unwrap(), json!(false));
        assert_eq!(validate_setting_value(&def_of("loop_isolation"), &json!("yes")).unwrap_err().message, "loop_isolation must be true or false");
        assert_eq!(validate_setting_value(&def_of("max_parallel_maps"), &json!(6.5)).unwrap_err().message, "max_parallel_maps must be a whole number");
        assert_eq!(validate_setting_value(&def_of("max_parallel_maps"), &json!(0)).unwrap_err().message, "max_parallel_maps must be at least 1");
        assert_eq!(validate_setting_value(&def_of("loop_max_hours"), &json!(200)).unwrap_err().message, "loop_max_hours must be at most 168");
        assert_eq!(validate_setting_value(&def_of("idle_hours"), &json!(0.5)).unwrap(), json!(0.5));
        assert_eq!(validate_setting_value(&def_of("orchestrator_model"), &json!(" sonnet ")).unwrap(), json!("sonnet"));
        assert_eq!(
            validate_setting_value(&def_of("orchestrator_model"), &json!("gpt")).unwrap_err().message,
            "orchestrator_model must be one of: 'opus', 'sonnet', 'haiku', ''"
        );
        assert_eq!(validate_setting_value(&def_of("claude_binary"), &json!("  cc ")).unwrap(), json!("cc"));
        assert_eq!(validate_setting_value(&def_of("output_dir"), &json!(" ")).unwrap_err().message, "output_dir cannot be empty: use Reset to default instead");
        assert_eq!(validate_setting_value(&def_of("claude_binary"), &json!("a\nb")).unwrap_err().message, "claude_binary must be one line of at most 500 characters");
        assert_eq!(find_definition(&json!("nope")).unwrap_err().message, "unknown setting: nope");
        assert_eq!(find_definition(&json!(3)).unwrap_err().message, "unknown setting: int");
    }

    #[test]
    fn lists_are_cleaned_and_checked() {
        assert_eq!(validate_setting_value(&def_of("loop_extra_skills"), &json!([" a ", "a", "", "b-c"])).unwrap(), json!(["a", "b-c"]));
        assert_eq!(
            validate_setting_value(&def_of("loop_extra_skills"), &json!(["../x"])).unwrap_err().message,
            "'../x' is not a valid skill name (letters, digits, dot, dash and underscore only)"
        );
        assert_eq!(validate_setting_value(&def_of("loop_extra_skills"), &json!("a")).unwrap_err().message, "loop_extra_skills must be a list");
        assert_eq!(validate_setting_value(&def_of("loop_extra_skills"), &json!([1])).unwrap_err().message, "loop_extra_skills must be a list of text");
        assert_eq!(validate_setting_value(&def_of("egress_hosts"), &json!(["api.example.com POST cred"])).unwrap(), json!(["api.example.com POST cred"]));
        assert!(validate_setting_value(&def_of("egress_hosts"), &json!(["ok.example.com", "http://bad"]))
            .unwrap_err()
            .message
            .starts_with("egress_hosts item 2 (http://bad): "));
        assert!(validate_setting_value(&def_of("egress_never"), &json!(["*.prod.example.com", "x.com:80"]))
            .unwrap_err()
            .message
            .starts_with("egress_never item 2 (x.com:80): "));
        assert_eq!(
            validate_setting_value(&def_of("env_sources"), &json!(["relative/.env"])).unwrap_err().message,
            "env_sources item 1 (relative/.env): use a full path (or one that starts with ~)"
        );
        assert!(validate_setting_value(&def_of("env_sources"), &json!(["C:\\a\\.env", "~/x/.env"])).is_ok());
        let many: Vec<Value> = (0..401).map(|i| json!(format!("s{i}"))).collect();
        assert_eq!(validate_setting_value(&def_of("loop_extra_skills"), &Value::Array(many)).unwrap_err().message, "loop_extra_skills accepts at most 400 items");
        let fifty_one: Vec<Value> = (0..51).map(|i| json!(format!("s{i}"))).collect();
        assert!(validate_setting_value(&def_of("loop_extra_skills"), &Value::Array(fifty_one.clone())).is_ok());
        assert_eq!(validate_setting_value(&def_of("egress_hosts"), &Value::Array(fifty_one)).unwrap_err().message, "egress_hosts accepts at most 50 items");
    }

    #[test]
    fn stored_values_fall_back_to_the_default() {
        let stored: Map<String, Value> =
            serde_json::from_value(json!({"loop_isolation": "x", "max_parallel_maps": 0, "loop_extra_skills": [" a ", 3, ""], "claude_binary": "cc"})).unwrap();
        assert_eq!(read_stored_or_default(&def_of("loop_isolation"), &stored), json!(true));
        assert_eq!(read_stored_or_default(&def_of("max_parallel_maps"), &stored), json!(6));
        assert_eq!(read_stored_or_default(&def_of("loop_extra_skills"), &stored), json!(["a"]));
        assert_eq!(read_stored_or_default(&def_of("claude_binary"), &stored), json!("cc"));
        let env = Env::from_pairs([("USERPROFILE", "C:\\Users\\Me")]);
        assert_eq!(get_default_value(&def_of("output_dir"), &env), json!("C:\\Users\\Me\\Downloads"));
        let (servers, extra) = validate_mcp_settings(&json!({"db": {"allow": true}}), &json!(["mcp__db__query"])).unwrap();
        assert_eq!(servers["db"]["allow"], json!(true));
        assert_eq!(extra, json!(["mcp__db__query"]));
        assert!(validate_mcp_settings(&json!({"db": {"allow": "yes"}}), &json!([])).is_err());
    }
}
