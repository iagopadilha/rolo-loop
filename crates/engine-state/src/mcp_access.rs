// SPDX-License-Identifier: MIT
//! MCP access for `claude -p` (planning replies, loop turns): the servers the USER allowed in Settings > MCP servers (`mcp_servers` and
//! `mcp_allow_extra` of `<data dir>/config.json`, grammar and rules in the `mcp-policy` crate, the same code the guard uses). Nothing is
//! allowed by default: with nothing configured a loop gets no MCP server at all.
//!
//! Where the definition of an allowed server comes from: [`crate::mcp_discovery`] (`~/.claude.json`, the project's `.mcp.json`, enabled
//! plugins). One strict MCP config file is written per run with only the allowed definitions (`--mcp-config <file> --strict-mcp-config`),
//! so every other MCP stays out. A server is exposed whole (`--allowedTools mcp__<server>`): what it may do is the guard's job
//! (`mcp-allowlist` read-only/production, `sql-write`, `prod-sql-write`). The one exception is the public Grafana server, whose read tool
//! list and writers are known and kept.
//!
//! The definitions carry credentials, so the file goes under `<data dir>/.secrets/` (the guard's `secrets` rule blocks agent reads there)
//! and its CONTENT never reaches an argv, a log or a response: callers get the path. An unreadable config (or a server without
//! `url`/`command`) = "none found": never a failure, never a guess.

use crate::mcp_discovery;
use cfg::Env;
use mcp_policy::{allowed_server_names, read_policy as read_policy_dir, Policy};
use pycompat::json::py_json_dumps;
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const SECRETS_FOLDER_NAME: &str = ".secrets";
pub const MCP_CONFIG_FILE_NAME: &str = "mcp-access.json";
/// The public Grafana MCP server: its read tools and its writers are known (a loop gets the reads only).
pub const GRAFANA_SERVER: &str = "grafana";
pub const GRAFANA_READ_TOOL_NAMES: [&str; 7] =
    ["query_loki_logs", "list_loki_label_names", "list_loki_label_values", "query_loki_stats", "query_loki_patterns", "find_error_pattern_logs", "list_datasources"];
/// Belt and braces for Grafana: these writers are denied by name too (a deny rule beats an allow rule in any user setting).
pub const GRAFANA_DENIED_WRITE_TOOL_NAMES: [&str; 14] = [
    "create_annotation",
    "update_annotation",
    "create_incident",
    "add_activity_to_incident",
    "update_dashboard",
    "create_folder",
    "create_datasource",
    "update_datasource",
    "create_snapshot",
    "delete_snapshot",
    "install_plugin",
    "alerting_manage_rules",
    "alerting_manage_routing",
    "grafana_api_request",
];

static STRICT_INVALID: LazyLock<Re> = LazyLock::new(|| Re::new(r"[^A-Za-z0-9_-]"));

/// The effective policy of `<data dir>/config.json`; closed and `broken` when unreadable.
pub fn read_policy(env: &Env, data_dir: Option<&Path>) -> Policy {
    read_policy_dir(&data_dir.map(Path::to_path_buf).unwrap_or_else(|| env.user_data_dir()))
}

/// The key of the server in the strict config: the tool prefix is built from it (`plugin:a:b` -> `plugin_a_b`).
pub fn strict_key(server: &str) -> String {
    STRICT_INVALID.replace_lit(server.trim(), "_")
}

pub fn tool_prefix(server: &str) -> String {
    format!("mcp__{}__", strict_key(server))
}

pub fn is_grafana(server: &str) -> bool {
    strict_key(server).to_lowercase() == GRAFANA_SERVER
}

/// Tool patterns of a server for `--allowedTools`: the known read tools of Grafana, else the whole server (`mcp__<server>`).
pub fn read_tools(server: &str) -> Vec<String> {
    if is_grafana(server) {
        return GRAFANA_READ_TOOL_NAMES.iter().map(|n| format!("{}{n}", tool_prefix(server))).collect();
    }
    vec![format!("mcp__{}", strict_key(server))]
}

/// Names of the servers a loop may use (allow = true); empty when nothing is configured.
pub fn server_names(policy: &Policy) -> Vec<String> {
    allowed_server_names(policy)
}

/// Names out of rows (`{"server": ...}`) or plain strings, without repeats.
pub fn servers_to_names(servers: &[Value]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for item in servers {
        let name = match item {
            Value::Object(o) => o.get("server").and_then(Value::as_str),
            Value::String(s) => Some(s.as_str()),
            _ => None,
        };
        if let Some(n) = name.filter(|n| !n.is_empty()) {
            if !names.iter().any(|x| x == n) {
                names.push(n.to_string());
            }
        }
    }
    names
}

/// Writers denied by name (Grafana only; the other servers are kept read only by the guard).
pub fn denied_write_tools(names: &[String]) -> Vec<String> {
    names.iter().filter(|n| is_grafana(n)).flat_map(|n| GRAFANA_DENIED_WRITE_TOOL_NAMES.iter().map(move |w| format!("{}{w}", tool_prefix(n)))).collect()
}

/// Server parts (as written) of the `mcp_allow_extra` patterns that name a tool (`mcp__db__query`): their servers must load too.
pub fn extra_server_keys(policy: &Policy) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for pattern in &policy.extra {
        let rest = pattern.strip_prefix("mcp__").unwrap_or(pattern).trim_end_matches('*');
        if let Some((server, _)) = rest.split_once("__") {
            if !server.is_empty() && !keys.iter().any(|k| k == server) {
                keys.push(server.to_string());
            }
        }
    }
    keys
}

/// `{name: definition}` of the given names that the machine defines with a `url` or a `command`. Empty when nothing is readable.
pub fn read_server_definitions(env: &Env, claude_json: Option<&str>, names: &[String], project_root: &str) -> Vec<(String, Value)> {
    names.iter().filter_map(|name| mcp_discovery::definition(env, name, project_root, claude_json).map(|d| (name.clone(), d))).collect()
}

/// One row per allowed server: `[{"server", "tools", "configured", "kind", "production", "read_only"}]`. No credentials.
pub fn describe_servers(env: &Env, claude_json: Option<&str>, project_root: &str, policy: &Policy) -> Vec<Value> {
    let names = server_names(policy);
    let definitions = read_server_definitions(env, claude_json, &names, project_root);
    names
        .iter()
        .map(|name| {
            let options = &policy.servers.iter().find(|(n, _)| n == name).expect("allowed server is configured").1;
            json!({"server": name, "tools": read_tools(name), "configured": definitions.iter().any(|(n, _)| n == name), "kind": options.kind,
                   "production": options.production, "read_only": options.read_only})
        })
        .collect()
}

/// The effective MCP policy for `doctor` and the Settings API: `{"allowed", "servers", "extra", "broken", "warnings"}`. No credential.
pub fn describe_policy(env: &Env, claude_json: Option<&str>, project_root: &str) -> Value {
    let policy = read_policy(env, None);
    let mut names: Vec<String> = policy.servers.iter().map(|(n, _)| n.clone()).collect();
    names.extend(extra_server_keys(&policy));
    let definitions = read_server_definitions(env, claude_json, &names, project_root);
    let found = |name: &str| definitions.iter().any(|(n, _)| n == name);
    let rows: Vec<Value> = policy
        .servers
        .iter()
        .map(|(name, o)| json!({"server": name, "allow": o.allow, "kind": o.kind, "production": o.production, "read_only": o.read_only, "found": found(name)}))
        .collect();
    let mut warnings: Vec<String> = policy.notes.clone();
    if !policy.broken.is_empty() {
        warnings.push(format!("{} is unreadable: no MCP server is allowed for loops until it is fixed", policy.broken));
    }
    for (name, o) in &policy.servers {
        if o.allow && !found(name) {
            warnings.push(format!("MCP server `{name}` is configured in Settings > MCP servers but no definition was found on this machine (~/.claude.json, the project's .mcp.json, enabled plugins): loops will not have it"));
        }
    }
    let defined_keys: Vec<String> = definitions.iter().map(|(n, _)| strict_key(n)).collect();
    for pattern in &policy.extra {
        let rest = pattern.strip_prefix("mcp__").unwrap_or(pattern);
        let server = rest.trim_end_matches('*').split("__").next().unwrap_or("").to_string();
        if !defined_keys.contains(&server) && rest.contains("__") {
            warnings.push(format!("MCP pattern `{pattern}` names a server with no definition on this machine"));
        }
    }
    json!({"allowed": server_names(&policy), "servers": rows, "extra": policy.extra, "broken": policy.broken, "warnings": warnings})
}

/// Only the allowed servers that this machine defines: a missing one is just absent.
pub fn available_servers(env: &Env, claude_json: Option<&str>, project_root: &str) -> Vec<Value> {
    let policy = read_policy(env, None);
    describe_servers(env, claude_json, project_root, &policy).into_iter().filter(|r| r["configured"] == true).collect()
}

/// `--allowedTools` entries: the tools of the given servers (rows of [`available_servers`] or plain names) plus the `mcp_allow_extra` patterns.
pub fn allowed_tool_names(servers: &[Value], policy: &Policy) -> Vec<String> {
    let mut tools: Vec<String> = Vec::new();
    for name in servers_to_names(servers) {
        tools.extend(read_tools(&name));
    }
    for pattern in &policy.extra {
        if !tools.contains(pattern) && policy.broken.is_empty() {
            tools.push(pattern.clone());
        }
    }
    tools
}

pub fn strict_config_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SECRETS_FOLDER_NAME).join(MCP_CONFIG_FILE_NAME)
}

/// Writes `{"mcpServers": {...}}` with ONLY the given servers (definitions found by discovery, keyed by [`strict_key`]) and returns the
/// path, or `None` when nothing can be written (no server, no definition, or an OS error). For `--mcp-config <path> --strict-mcp-config`.
pub fn write_strict_config(env: &Env, path: &Path, servers: &[Value], claude_json: Option<&str>, project_root: &str) -> Option<PathBuf> {
    let names = servers_to_names(servers);
    let definitions = read_server_definitions(env, claude_json, &names, project_root);
    let mut chosen = Map::new();
    for name in &names {
        if let Some((_, d)) = definitions.iter().find(|(n, _)| n == name) {
            chosen.insert(strict_key(name), d.clone());
        }
    }
    if chosen.is_empty() || path.as_os_str().is_empty() {
        return None;
    }
    crate::loop_io::atomic_write_text(path, &py_json_dumps(&json!({"mcpServers": chosen}), true)).ok()?;
    Some(path.to_path_buf())
}

/// What a `claude -p` needs: `{"servers", "allowed_tools", "denied_tools", "config_path"}`. `config_path` is empty when no allowed server
/// exists on this machine (then no MCP flags should be passed).
pub fn build_access(env: &Env, data_dir: Option<&Path>, claude_json: Option<&str>, project_root: &str) -> Value {
    let policy = read_policy(env, data_dir);
    let rows: Vec<Value> = describe_servers(env, claude_json, project_root, &policy).into_iter().filter(|r| r["configured"] == true).collect();
    let mut names: Vec<String> = rows.iter().filter_map(|r| r["server"].as_str().map(String::from)).collect();
    for key in extra_server_keys(&policy) {
        // a server that only an extra pattern names must load too
        if policy.broken.is_empty() && names.iter().all(|n| strict_key(n).to_lowercase() != strict_key(&key).to_lowercase()) {
            names.push(key);
        }
    }
    let name_values: Vec<Value> = names.iter().map(|n| json!(n)).collect();
    let config_path = match data_dir {
        Some(dir) if !names.is_empty() => write_strict_config(env, &strict_config_path(dir), &name_values, claude_json, project_root),
        _ => None,
    };
    let Some(config_path) = config_path else {
        return json!({"servers": [], "allowed_tools": [], "denied_tools": [], "config_path": ""});
    };
    let row_names: Vec<String> = servers_to_names(&rows);
    json!({"servers": rows, "allowed_tools": allowed_tool_names(&rows, &policy), "denied_tools": denied_write_tools(&row_names), "config_path": config_path.display().to_string()})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Env, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            data.join("config.json"),
            r#"{"mcp_servers": {"grafana": {"allow": true}, "db": {"allow": true, "kind": "sql"}, "off": {"allow": false}}, "mcp_allow_extra": ["mcp__tool__query"]}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("claude.json"), r#"{"mcpServers": {"grafana": {"url": "http://g.example.com"}, "tool": {"command": "t"}}}"#).unwrap();
        let env = Env::from_pairs([
            ("LOOP_CLAUDE_CONFIG_FILE", dir.path().join("claude.json").display().to_string()),
            ("LOOP_CLAUDE_HOME", dir.path().join("none").display().to_string()),
            ("LOOP_DATA_DIR", data.display().to_string()),
        ]);
        (dir, env, data)
    }

    #[test]
    fn names_and_tools() {
        assert_eq!(strict_key(" plugin:a:b "), "plugin_a_b");
        assert_eq!(read_tools("db"), vec!["mcp__db"]);
        assert_eq!(read_tools("Grafana")[0], "mcp__Grafana__query_loki_logs");
        assert_eq!(denied_write_tools(&["grafana".into(), "db".into()]).len(), 14);
        assert_eq!(servers_to_names(&[json!("a"), json!({"server": "b"}), json!("a"), json!(3)]), vec!["a", "b"]);
    }

    #[test]
    fn builds_the_strict_access_with_only_defined_allowed_servers() {
        let (_d, env, data) = setup();
        let policy = read_policy(&env, None);
        assert_eq!(server_names(&policy), vec!["grafana", "db"]);
        assert_eq!(extra_server_keys(&policy), vec!["tool"]);
        let access = build_access(&env, Some(&data), None, "");
        assert_eq!(access["servers"].as_array().unwrap().len(), 1); // db has no definition on this machine
        assert_eq!(access["servers"][0]["server"], "grafana");
        let allowed: Vec<&str> = access["allowed_tools"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(allowed.contains(&"mcp__grafana__query_loki_logs") && allowed.contains(&"mcp__tool__query"));
        assert_eq!(access["denied_tools"].as_array().unwrap().len(), 14);
        let written = std::fs::read_to_string(access["config_path"].as_str().unwrap()).unwrap();
        assert_eq!(written, r#"{"mcpServers": {"grafana": {"url": "http://g.example.com"}, "tool": {"command": "t"}}}"#);
        let described = describe_policy(&env, None, "");
        assert!(described["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().starts_with("MCP server `db` is configured")));
    }

    #[test]
    fn nothing_configured_means_no_flags() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::from_pairs([("LOOP_DATA_DIR", dir.path().display().to_string()), ("LOOP_CLAUDE_CONFIG_FILE", dir.path().join("x.json").display().to_string())]);
        let access = build_access(&env, Some(dir.path()), None, "");
        assert_eq!(access, json!({"servers": [], "allowed_tools": [], "denied_tools": [], "config_path": ""}));
    }
}
