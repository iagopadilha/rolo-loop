// SPDX-License-Identifier: MIT
//! The "MCP servers" section of the Settings screen (`GET/POST /api/app/settings/mcp-servers`): which MCP servers a loop may use.
//!
//! Reads: the servers found on this machine (`mcp_discovery`: ~/.claude.json, the project's .mcp.json, enabled plugins, settings permissions) joined
//! with what config.json saved (`mcp_servers`, `mcp_allow_extra`), plus the servers the user added by name (a claude.ai connector cannot be found
//! offline). Writes: ONE validated replace of both keys (the grammar is the `mcp-policy` crate, the same one the guard uses). Nothing is allowed by
//! default. Never returns a server definition (command, url, env, headers): it may carry credentials; a row has names only.

use super::App;
use crate::domain::app_settings::{validate_mcp_settings, MCP_EXTRA_KEY, MCP_SERVERS_KEY};
use crate::domain::errors::AppError;
use crate::domain::errors::Kind;
use engine_state::mcp_discovery;
use serde_json::{json, Map, Value};

const SOURCE_MANUAL: &str = "manual";

pub struct McpServersService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn mcp_servers(&self) -> McpServersService<'_> {
        McpServersService { app: self }
    }
}

impl McpServersService<'_> {
    fn project_root(&self, project_id: &str) -> String {
        if project_id.is_empty() {
            return String::new();
        }
        self.app.project_service().get_project_context(project_id).map(|c| c.repo_path().to_string()).unwrap_or_default()
        // an unknown project just means "no project scope"
    }

    fn read_policy(&self) -> mcp_policy::Policy {
        match self.app.settings.read_all() {
            Err(_) => mcp_policy::policy_from(&json!({}), "config.json"),
            Ok(config) => mcp_policy::policy_from(&Value::Object(config), ""),
        }
    }

    /// `{ok, servers: [{name, sources, defined, saved, allow, kind, production, read_only, prefix, tools, tools_known}], extra, allowed, none_allowed, broken, notes}`.
    pub fn get_servers(&self, project_id: &str) -> Value {
        let policy = self.read_policy();
        let mut rows: Vec<Map<String, Value>> = Vec::new();
        let mut by_prefix: Vec<(String, usize)> = Vec::new();
        for found in mcp_discovery::list_servers(&self.app.env, &self.project_root(project_id), None) {
            let name = found["name"].as_str().unwrap_or("").to_string();
            let mut row = Map::new();
            row.insert("name".into(), json!(name));
            row.insert("sources".into(), found["sources"].clone());
            row.insert("defined".into(), found["defined"].clone());
            row.insert("saved".into(), json!(false));
            row.insert("allow".into(), json!(false));
            row.insert("kind".into(), json!(mcp_policy::KIND_OTHER));
            row.insert("production".into(), json!(false));
            row.insert("read_only".into(), json!(false));
            rows.push(row);
            by_prefix.retain(|(p, _)| *p != mcp_policy::normalize_name(&name));
            by_prefix.push((mcp_policy::normalize_name(&name), rows.len() - 1));
        }
        for (name, options) in &policy.servers {
            let key = mcp_policy::normalize_name(name);
            let index = match by_prefix.iter().find(|(p, _)| *p == key) {
                Some((_, index)) => *index,
                None => {
                    let mut row = Map::new();
                    row.insert("name".into(), json!(name));
                    row.insert("sources".into(), json!([SOURCE_MANUAL]));
                    row.insert("defined".into(), json!(false));
                    rows.push(row);
                    by_prefix.push((key, rows.len() - 1));
                    rows.len() - 1
                }
            };
            let row = &mut rows[index];
            row.insert("name".into(), json!(name));
            row.insert("saved".into(), json!(true));
            row.insert("allow".into(), json!(options.allow));
            row.insert("kind".into(), json!(options.kind));
            row.insert("production".into(), json!(options.production));
            row.insert("read_only".into(), json!(options.read_only));
        }
        let known = mcp_discovery::known_tools(&self.app.env);
        for row in &mut rows {
            let prefix = mcp_policy::normalize_name(row["name"].as_str().unwrap_or(""));
            let mut tools: Vec<String> = known.iter().filter(|(server, _)| mcp_policy::normalize_name(server) == prefix).map(|(_, tool)| tool.clone()).collect();
            for pattern in &policy.extra {
                if let Some((server, tool)) = pattern.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")) {
                    if mcp_policy::normalize_name(server) == prefix && tool != "*" && !tools.iter().any(|t| t == tool) {
                        tools.push(tool.to_string());
                    }
                }
            }
            tools.sort();
            row.insert("prefix".into(), json!(prefix));
            row.insert("tools_known".into(), json!(!tools.is_empty()));
            row.insert("tools".into(), json!(tools));
        }
        let allowed = mcp_policy::allowed_server_names(&policy);
        json!({
            "ok": true, "servers": rows, "extra": policy.extra, "allowed": allowed, "none_allowed": allowed.is_empty() && policy.extra.is_empty(),
            "broken": policy.broken, "notes": policy.notes,
            "kinds": [{"value": mcp_policy::KIND_SQL, "label": "Database"}, {"value": mcp_policy::KIND_OTHER, "label": "Other"}],
        })
    }

    /// Replaces `mcp_servers` and `mcp_allow_extra`. `servers` is `{name: {allow, kind, production, read_only}}` (every field optional); errors with
    /// `InvalidSetting` (400) for anything the grammar refuses, `ConfigFileUnreadable` (409) for a config.json that is not an object.
    pub fn save_servers(&self, servers: &Value, extra: &Value) -> Result<Value, AppError> {
        let (clean_servers, clean_extra) = validate_mcp_settings(servers, extra)?;
        let (mut values, mut remove): (Vec<(String, Value)>, Vec<&str>) = (vec![], vec![]);
        for (key, value) in [(MCP_SERVERS_KEY, clean_servers), (MCP_EXTRA_KEY, clean_extra)] {
            if engine_state::notify_truthy(Some(&value)) {
                values.push((key.to_string(), value));
            } else {
                remove.push(key);
            }
        }
        self.app.settings.set_values(&values, &remove).map_err(|e| if e.kind == Kind::ConfigFileUnreadable { e } else { e })?;
        Ok(self.get_servers(""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_servers_are_listed_and_the_file_is_never_overwritten_when_odd() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let service = app.mcp_servers();
        let none = service.get_servers("");
        assert_eq!((none["none_allowed"].clone(), none["allowed"].clone()), (json!(true), json!([])));
        let saved = service.save_servers(&json!({"grafana": {"allow": true, "kind": "other", "read_only": true}}), &json!(["mcp__grafana__query*"])).unwrap();
        assert_eq!(
            (saved["allowed"].clone(), saved["none_allowed"].clone(), saved["extra"].clone()),
            (json!(["grafana"]), json!(false), json!(["mcp__grafana__query*"]))
        );
        let row = saved["servers"].as_array().unwrap().iter().find(|s| s["name"] == "grafana").unwrap();
        assert_eq!((row["saved"].clone(), row["sources"].clone(), row["read_only"].clone()), (json!(true), json!(["manual"]), json!(true)));
        assert_eq!(service.save_servers(&json!({"x": {"bogus": 1}}), &json!([])).unwrap_err().kind, Kind::InvalidSetting);
        let cleared = service.save_servers(&json!({}), &json!([])).unwrap();
        assert_eq!(cleared["none_allowed"], true);
        std::fs::write(app.settings.path(), "[1]").unwrap();
        assert_eq!(service.save_servers(&json!({}), &json!([])).unwrap_err().kind, Kind::ConfigFileUnreadable);
        assert_eq!(service.get_servers("")["broken"], "config.json");
    }

    #[test]
    fn tools_come_from_patterns_and_permissions_and_no_secret_leaves() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let home = dir.path().join("data");
        std::fs::write(home.join("settings.json"), r#"{"permissions": {"allow": ["mcp__db__read(secret-arg-1)", "mcp__db__*", "mcp__db"]}}"#).unwrap();
        std::fs::write(
            home.join(".claude.json"),
            r#"{"mcpServers": {"db": {"command": "run-db", "env": {"TOKEN": "tok-SECRET-value"}, "headers": {"Authorization": "Bearer hdr-SECRET-value"}}, "lonely": {"url": "https://example.test/${API_KEY}"}}}"#,
        )
        .unwrap();
        let service = app.mcp_servers();
        service.save_servers(&json!({}), &json!(["mcp__db__write*", "mcp__db__list"])).unwrap();
        let env = cfg::Env::from_pairs([
            ("LOOP_DATA_DIR", home.to_str().unwrap()),
            ("LOOP_CLAUDE_HOME", home.to_str().unwrap()),
            ("LOOP_CLAUDE_CONFIG_FILE", home.join(".claude.json").to_str().unwrap()),
        ]);
        let app = App::new(&env, &Default::default(), std::sync::Arc::new(crate::infrastructure::process_runner::SystemProcesses));
        let listed = app.mcp_servers().get_servers("");
        let text = listed.to_string();
        for secret in ["SECRET", "run-db", "Bearer", "API_KEY", "example.test", "secret-arg"] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
        let rows = listed["servers"].as_array().unwrap();
        let db = rows.iter().find(|r| r["name"] == "db").unwrap();
        assert_eq!((db["tools"].clone(), db["tools_known"].clone()), (json!(["list", "read", "write*"]), json!(true)));
        let lonely = rows.iter().find(|r| r["name"] == "lonely").unwrap();
        assert_eq!((lonely["tools"].clone(), lonely["tools_known"].clone()), (json!([]), json!(false)));
    }
}
