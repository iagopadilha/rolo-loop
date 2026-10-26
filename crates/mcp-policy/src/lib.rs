// SPDX-License-Identifier: MIT
//! MCP policy of the loop: which MCP servers a loop session may use, read from `<data dir>/config.json`.
//!
//! config.json keys (both absent = CLOSED, a loop may use no MCP server):
//! - `mcp_servers`: object keyed by the server name as Claude Code knows it, e.g.
//!   `{"db-dev": {"allow": true, "kind": "sql", "production": false, "read_only": false}}`;
//! - `mcp_allow_extra`: full tool-name patterns `mcp__<server>__<tool>`, a trailing `*` is the only wildcard.
//!
//! Names: Claude Code builds `mcp__<server>__<tool>` with every character of the server name outside `[A-Za-z0-9_-]`
//! turned into `_`. Plugin servers are `plugin:<plugin>:<server>`; claude.ai connectors are `claude.ai <Name>`.
//!
//! Fail closed: a config.json that exists but cannot be used allows nothing and sets `broken`; an entry that cannot be
//! read becomes a server that is NOT allowed and treated as production + read only.

use cfg::app_config::py_repr;
use regex::Regex;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const SERVERS_KEY: &str = "mcp_servers";
pub const EXTRA_KEY: &str = "mcp_allow_extra";
pub const KIND_SQL: &str = "sql";
pub const KIND_OTHER: &str = "other";
pub const SERVER_FIELDS: [&str; 4] = ["allow", "kind", "production", "read_only"];
/// The only guard profiles that may reach a production server (read only).
pub const PRODUCTION_PROFILES: [&str; 2] = ["orchestrator", "planning"];
pub const MAX_SERVERS: usize = 100;
pub const MAX_EXTRA: usize = 100;
pub const NAME_MAX_LENGTH: usize = 100;

static READ_VERB: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:get|list|query|search|find|check|analy[sz]e|suggest|validate|read|describe|show|fetch|select|count|lookup|view|explain|inspect|generate_deeplink)").expect("regex")
});
static NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9 _.:@/-]*\n?$").expect("regex"));
static PATTERN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^mcp__[A-Za-z0-9][A-Za-z0-9_.-]*\*?\n?$").expect("regex"));
static NON_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^A-Za-z0-9_-]").expect("regex"));
static MANY_UNDERSCORES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"_+").expect("regex"));

/// A server name, option or pattern does not follow the grammar (the message says which and why).
#[derive(Debug, Clone, PartialEq)]
pub struct McpPolicyError(pub String);

impl std::fmt::Display for McpPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for McpPolicyError {}

fn err<T>(message: String) -> Result<T, McpPolicyError> {
    Err(McpPolicyError(message))
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerOptions {
    pub allow: bool,
    pub kind: String,
    pub production: bool,
    pub read_only: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Policy {
    /// In config order.
    pub servers: Vec<(String, ServerOptions)>,
    pub extra: Vec<String>,
    pub broken: String,
    pub notes: Vec<String>,
}

pub fn data_dir(env: &cfg::Env) -> PathBuf {
    env.user_data_dir()
}

// ---- names ----------------------------------------------------------------------------------------------------------------

/// The server part of a tool name, the way Claude Code builds it (lower case: matching ignores case).
pub fn normalize_name(name: &str) -> String {
    let stripped = name.trim();
    let mut text = NON_NAME.replace_all(stripped, "_").into_owned();
    if stripped.to_lowercase().starts_with("claude.ai ") {
        text = MANY_UNDERSCORES
            .replace_all(&text, "_")
            .trim_matches('_')
            .to_string();
    }
    text.to_lowercase()
}

pub fn tool_prefix(name: &str) -> String {
    format!("mcp__{}__", normalize_name(name))
}

pub fn plugin_server_name(plugin: &str, server: &str) -> String {
    format!("plugin:{plugin}:{server}")
}

fn repr(text: &str) -> String {
    py_repr(&Value::String(text.to_string()))
}

pub fn check_name(name: &Value) -> Result<String, McpPolicyError> {
    let Value::String(raw) = name else {
        return err("a server name must be text".into());
    };
    let text = raw.trim().to_string();
    let bad = text.is_empty()
        || text.chars().count() > NAME_MAX_LENGTH
        || !NAME_RE.is_match(&text)
        || normalize_name(&text)
            .trim_matches(|c| c == '_' || c == '-')
            .is_empty();
    if bad {
        let shown: String = text.chars().take(40).collect();
        return err(format!(
            "{} is not a valid MCP server name (letters, digits, space, dot, dash, underscore, colon, at; at most {} characters)",
            repr(&shown),
            NAME_MAX_LENGTH
        ));
    }
    Ok(text)
}

pub fn check_pattern(pattern: &Value) -> Result<String, McpPolicyError> {
    let Value::String(raw) = pattern else {
        return err("a tool pattern must be text".into());
    };
    let text = raw.trim().to_string();
    if !PATTERN_RE.is_match(&text) || text.chars().count() > 200 {
        let shown: String = text.chars().take(60).collect();
        return err(format!(
            "{} is not a valid tool pattern: use mcp__<server>__<tool>, with a trailing * as the only wildcard",
            repr(&shown)
        ));
    }
    Ok(text)
}

pub fn pattern_matches(pattern: &str, tool_name: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let name = tool_name.to_lowercase();
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == pattern,
    }
}

// ---- validation (strict: for a save) ---------------------------------------------------------------------------------------------

pub fn check_server(name: &str, value: &Value) -> Result<ServerOptions, McpPolicyError> {
    check_name(&Value::String(name.to_string()))?;
    let Value::Object(map) = value else {
        return err(format!("{name}: the options must be an object"));
    };
    let mut unknown: Vec<String> = map
        .keys()
        .filter(|k| !SERVER_FIELDS.contains(&k.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return err(format!("{name}: unknown option(s): {}", unknown.join(", ")));
    }
    let mut clean = ServerOptions {
        allow: false,
        kind: KIND_OTHER.into(),
        production: false,
        read_only: false,
    };
    for key in ["allow", "production", "read_only"] {
        if let Some(v) = map.get(key) {
            let Value::Bool(b) = v else {
                return err(format!("{name}: {key} must be true or false"));
            };
            match key {
                "allow" => clean.allow = *b,
                "production" => clean.production = *b,
                _ => clean.read_only = *b,
            }
        }
    }
    if let Some(kind) = map.get("kind") {
        match kind.as_str() {
            Some(k) if k == KIND_SQL || k == KIND_OTHER => clean.kind = k.to_string(),
            _ => {
                return err(format!(
                    "{name}: kind must be one of: {KIND_SQL}, {KIND_OTHER}"
                ))
            }
        }
    }
    Ok(clean)
}

/// Clean servers and extras of a whole save; the first problem raises.
pub fn check_config(
    servers: &Value,
    extra: &Value,
) -> Result<(Vec<(String, ServerOptions)>, Vec<String>), McpPolicyError> {
    let Value::Object(map) = servers else {
        return err("servers must be an object keyed by server name".into());
    };
    if map.len() > MAX_SERVERS {
        return err(format!("at most {MAX_SERVERS} servers"));
    }
    let mut clean: Vec<(String, ServerOptions)> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for (name, value) in map {
        let text = check_name(&Value::String(name.clone()))?;
        let prefix = normalize_name(&text);
        if let Some((_, other)) = seen.iter().find(|(p, _)| *p == prefix) {
            return err(format!(
                "{} and {} are the same server (both give the tool prefix mcp__{}__)",
                repr(other),
                repr(&text),
                prefix
            ));
        }
        seen.push((prefix, text.clone()));
        let options = check_server(&text, value)?;
        clean.push((text, options));
    }
    let list: Vec<Value> = match extra {
        Value::Null => vec![],
        Value::Array(items) => items.clone(),
        _ => return err("mcp_allow_extra must be a list of tool patterns".into()),
    };
    if list.len() > MAX_EXTRA {
        return err(format!("at most {MAX_EXTRA} extra patterns"));
    }
    let mut patterns: Vec<String> = Vec::new();
    for item in &list {
        let text = check_pattern(item)?;
        if !patterns
            .iter()
            .any(|p| p.to_lowercase() == text.to_lowercase())
        {
            patterns.push(text);
        }
    }
    Ok((clean, patterns))
}

// ---- reading (lenient and fail closed: for the guard and the run) --------------------------------------------------------------

fn restrictive_server(value: &Value) -> ServerOptions {
    let sql = value.get("kind").and_then(Value::as_str) == Some(KIND_SQL);
    ServerOptions {
        allow: false,
        kind: if sql { KIND_SQL } else { KIND_OTHER }.into(),
        production: true,
        read_only: true,
    }
}

/// The effective policy of a parsed config.json.
pub fn policy_from(config: &Value, broken: &str) -> Policy {
    let mut policy = Policy {
        broken: broken.to_string(),
        ..Policy::default()
    };
    let Value::Object(config) = config else {
        return policy;
    };
    let servers: Map<String, Value> = match config.get(SERVERS_KEY) {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => {
            if policy.broken.is_empty() {
                policy.broken = format!("config.json {SERVERS_KEY}");
            }
            Map::new()
        }
    };
    for (name, value) in &servers {
        let outcome = check_name(&Value::String(name.clone()))
            .and_then(|text| check_server(name, value).map(|o| (text, o)));
        match outcome {
            Ok((text, options)) => set_server(&mut policy.servers, text, options),
            Err(error) => {
                policy.notes.push(format!("config.json {SERVERS_KEY} entry ignored (the server stays not allowed): {error}"));
                if !name.trim().is_empty() {
                    set_server(
                        &mut policy.servers,
                        name.trim().to_string(),
                        restrictive_server(value),
                    );
                }
            }
        }
    }
    let extra: Vec<Value> = match config.get(EXTRA_KEY) {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(items)) => items.clone(),
        Some(_) => {
            if policy.broken.is_empty() {
                policy.broken = format!("config.json {EXTRA_KEY}");
            }
            vec![]
        }
    };
    for item in &extra {
        match check_pattern(item) {
            Ok(text) => policy.extra.push(text),
            Err(error) => policy
                .notes
                .push(format!("config.json {EXTRA_KEY} entry ignored: {error}")),
        }
    }
    policy
}

fn set_server(list: &mut Vec<(String, ServerOptions)>, name: String, options: ServerOptions) {
    if let Some(slot) = list.iter_mut().find(|(n, _)| *n == name) {
        slot.1 = options;
    } else {
        list.push((name, options));
    }
}

/// The policy of `<dir>/config.json`. Missing file = nothing configured (closed). A file that exists but is not a
/// JSON object allows nothing and sets `broken`.
pub fn read_policy(dir: &Path) -> Policy {
    let path = dir.join("config.json");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return policy_from(&Value::Object(Map::new()), "")
        }
        Err(_) => return policy_from(&Value::Object(Map::new()), "config.json"),
    };
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(_) => return policy_from(&Value::Object(Map::new()), "config.json"),
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    match serde_json::from_str::<Value>(text) {
        Ok(v @ Value::Object(_)) => policy_from(&v, ""),
        _ => policy_from(&Value::Object(Map::new()), "config.json"),
    }
}

// ---- queries --------------------------------------------------------------------------------------------------------------------

/// The configured server a tool name belongs to (the longest matching prefix).
pub fn find_server<'a>(
    policy: &'a Policy,
    tool_name: &str,
) -> Option<(&'a str, &'a ServerOptions)> {
    let lowered = tool_name.to_lowercase();
    let mut best: Option<(&str, &ServerOptions, usize)> = None;
    for (name, options) in &policy.servers {
        let prefix = tool_prefix(name);
        if lowered.starts_with(&prefix) && best.is_none_or(|(_, _, len)| prefix.len() > len) {
            best = Some((name.as_str(), options, prefix.len()));
        }
    }
    best.map(|(n, o, _)| (n, o))
}

/// An allowed server may be used by the guard profile (a production server: only [`PRODUCTION_PROFILES`]).
pub fn server_allows(options: &ServerOptions, profile: &str) -> bool {
    options.allow && (!options.production || PRODUCTION_PROFILES.contains(&profile))
}

pub fn extra_allows(policy: &Policy, tool_name: &str) -> bool {
    policy.extra.iter().any(|p| pattern_matches(p, tool_name))
}

pub fn is_sql_tool(policy: &Policy, tool_name: &str) -> bool {
    find_server(policy, tool_name).is_some_and(|(_, o)| o.kind == KIND_SQL)
}

pub fn is_production_tool(policy: &Policy, tool_name: &str) -> bool {
    find_server(policy, tool_name).is_some_and(|(_, o)| o.production)
}

pub fn is_read_only_tool(policy: &Policy, tool_name: &str) -> bool {
    find_server(policy, tool_name).is_some_and(|(_, o)| o.read_only)
}

/// The tool part of the name starts with a read verb.
pub fn is_read_tool_name(tool_name: &str) -> bool {
    let tail = tool_name.rsplit("__").next().unwrap_or("");
    READ_VERB.is_match(tail)
}

/// Why an `mcp__` tool is outside the loop's allowlist for this profile; empty when it may be used.
pub fn deny_reason(policy: &Policy, tool_name: &str, profile: &str) -> String {
    if !policy.broken.is_empty() {
        return format!(
            "{} is unreadable: no MCP server is allowed until it is fixed",
            policy.broken
        );
    }
    let found = find_server(policy, tool_name);
    if let Some((name, options)) = found {
        if options.allow {
            if server_allows(options, profile) {
                if options.read_only && options.kind != KIND_SQL && !is_read_tool_name(tool_name) {
                    return format!("{tool_name} is not a read tool and the server {name} is marked read-only in Settings > MCP servers");
                }
                return String::new();
            }
            return format!(
                "the production server {name} is only for the {} profiles, not {profile}",
                PRODUCTION_PROFILES.join("/")
            );
        }
    }
    if extra_allows(policy, tool_name) {
        return String::new();
    }
    if let Some((name, _)) = found {
        return format!(
            "{tool_name}: the server {name} is configured but not allowed (Settings > MCP servers)"
        );
    }
    format!("{tool_name} is not on the MCP allowlist: a loop may use only the servers allowed in Settings > MCP servers (none by default)")
}

/// Sorted tool-name patterns the profile may use: `mcp__<server>__*` of each allowed server plus the extras.
pub fn allowed_patterns(policy: &Policy, profile: &str) -> Vec<String> {
    if !policy.broken.is_empty() {
        return vec![];
    }
    let mut found: Vec<String> = Vec::new();
    for (name, options) in &policy.servers {
        if server_allows(options, profile) {
            let pattern = format!("{}*", tool_prefix(name));
            if !found.contains(&pattern) {
                found.push(pattern);
            }
        }
    }
    for p in &policy.extra {
        if !found.contains(p) {
            found.push(p.clone());
        }
    }
    found.sort_by_key(|s| s.to_lowercase());
    found
}

/// Names of the servers with `allow = true`, in config order (empty when the file is broken).
pub fn allowed_server_names(policy: &Policy) -> Vec<String> {
    if !policy.broken.is_empty() {
        return vec![];
    }
    policy
        .servers
        .iter()
        .filter(|(_, o)| o.allow)
        .map(|(n, _)| n.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(config: Value) -> Policy {
        policy_from(&config, "")
    }

    #[test]
    fn names_normalise_like_claude_code() {
        assert_eq!(normalize_name("db-dev"), "db-dev");
        assert_eq!(normalize_name("plugin:data:hex"), "plugin_data_hex");
        assert_eq!(normalize_name("claude.ai My  Tool"), "claude_ai_my_tool");
        assert_eq!(tool_prefix("DB.Dev"), "mcp__db_dev__");
    }

    #[test]
    fn closed_by_default() {
        let p = policy(json!({}));
        assert!(
            deny_reason(&p, "mcp__db-dev__query", "executor").contains("not on the MCP allowlist")
        );
    }

    #[test]
    fn allowed_server_passes_and_production_is_restricted() {
        let p = policy(
            json!({"mcp_servers": {"db-dev": {"allow": true, "kind": "sql"}, "db-prod": {"allow": true, "kind": "sql", "production": true}}}),
        );
        assert_eq!(deny_reason(&p, "mcp__db-dev__query", "executor"), "");
        assert!(deny_reason(&p, "mcp__db-prod__query", "executor")
            .contains("only for the orchestrator/planning profiles"));
        assert_eq!(deny_reason(&p, "mcp__db-prod__query", "planning"), "");
        assert!(is_sql_tool(&p, "mcp__db-dev__query"));
        assert!(is_production_tool(&p, "mcp__db-prod__x"));
    }

    #[test]
    fn read_only_non_sql_passes_only_read_tools() {
        let p = policy(json!({"mcp_servers": {"graf": {"allow": true, "read_only": true}}}));
        assert_eq!(deny_reason(&p, "mcp__graf__get_dashboard", "executor"), "");
        assert!(
            deny_reason(&p, "mcp__graf__create_dashboard", "executor").contains("not a read tool")
        );
        assert!(is_read_tool_name("mcp__graf__listAlerts"));
    }

    #[test]
    fn unreadable_entry_is_restrictive_and_noted() {
        let p = policy(json!({"mcp_servers": {"x": {"allow": true, "bogus": 1}}}));
        let (_, o) = &p.servers[0];
        assert!(!o.allow && o.production && o.read_only);
        assert_eq!(p.notes.len(), 1);
    }

    #[test]
    fn broken_config_allows_nothing() {
        let p = policy_from(&json!({}), "config.json");
        assert!(deny_reason(&p, "mcp__a__b", "executor").contains("config.json is unreadable"));
        assert!(allowed_patterns(&p, "executor").is_empty());
        let wrong = policy(json!({"mcp_servers": []}));
        assert_eq!(wrong.broken, "config.json mcp_servers");
    }

    #[test]
    fn extras_open_tools() {
        let p = policy(json!({"mcp_allow_extra": ["mcp__x__*", "bad"]}));
        assert_eq!(deny_reason(&p, "mcp__x__anything", "executor"), "");
        assert_eq!(p.notes.len(), 1);
    }

    #[test]
    fn save_validation() {
        assert!(check_config(&json!({"a b": {"allow": true}}), &json!([])).is_ok());
        assert!(check_config(&json!({"a": {"allow": "yes"}}), &json!([])).is_err());
        assert!(check_config(&json!({"a:b": {}, "a_b": {}}), &json!([])).is_err());
        assert!(check_config(&json!({"a": {"kind": "weird"}}), &json!([])).is_err());
        assert_eq!(
            check_config(&json!({}), &json!(["mcp__a__*", "MCP__A__*"]))
                .unwrap()
                .1
                .len(),
            1
        );
    }

    #[test]
    fn reads_policy_from_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_policy(dir.path()).broken, "");
        std::fs::write(dir.path().join("config.json"), "{nope").unwrap();
        assert_eq!(read_policy(dir.path()).broken, "config.json");
        std::fs::write(
            dir.path().join("config.json"),
            "\u{feff}{\"mcp_servers\": {\"a\": {\"allow\": true}}}",
        )
        .unwrap();
        assert_eq!(
            allowed_server_names(&read_policy(dir.path())),
            vec!["a".to_string()]
        );
    }
}
