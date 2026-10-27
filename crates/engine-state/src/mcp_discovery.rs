// SPDX-License-Identifier: MIT
//! MCP servers present on this machine, found offline (no network): the union of
//!   a. `mcpServers` of `~/.claude.json` (user scope, and the project scope of the selected project when there is one),
//!   b. `<project>/.mcp.json` of the selected project,
//!   c. the `.mcp.json` of every enabled plugin of `~/.claude/plugins/installed_plugins.json` (named `plugin:<plugin>:<server>`, tools
//!      `mcp__plugin_<plugin>_<server>__<tool>`),
//!   d. the servers that `permissions` rules of `~/.claude/settings.json` mention (`mcp__<server>` or `mcp__<server>__<tool>`; the name of
//!      such a rule is already the normalised one, so a server with a dot in its name shows up with an underscore).
//!
//! claude.ai connectors cannot be found offline: the user adds them by name. A definition (command/url/env: it may carry credentials) is
//! kept only for [`definition`], which the strict MCP config of a turn uses; [`list_servers`] never returns it.

use cfg::Env;
use mcp_policy::{normalize_name, plugin_server_name};
use pycompat::{nt_abspath, nt_normcase};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// Tests point this at a fake `.claude.json`.
pub const CLAUDE_CONFIG_VARIABLE: &str = "LOOP_CLAUDE_CONFIG_FILE";
/// Tests point this at a fake `~/.claude`.
pub const CLAUDE_HOME_VARIABLE: &str = "LOOP_CLAUDE_HOME";
pub const SOURCE_USER: &str = "user";
pub const SOURCE_PROJECT: &str = "project";
pub const SOURCE_MCP_FILE: &str = ".mcp.json";
pub const SOURCE_PLUGIN: &str = "plugin";
pub const SOURCE_PERMISSION: &str = "permissions";

/// One discovered server: `name`, where it was seen, and a usable definition when there is one.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub sources: Vec<String>,
    pub definition: Option<Value>,
}

pub fn claude_config_path(env: &Env) -> PathBuf {
    match env.get(CLAUDE_CONFIG_VARIABLE).filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => env.home_dir().join(".claude.json"),
    }
}

pub fn claude_home(env: &Env) -> PathBuf {
    match env.get(CLAUDE_HOME_VARIABLE).filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => env.home_dir().join(".claude"),
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let text = String::from_utf8(std::fs::read(path).ok()?).ok()?;
    serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text)).ok()
}

/// A definition the CLI can start: an object with a `url` or a `command`.
pub fn usable(definition: &Value) -> bool {
    definition.as_object().is_some_and(|o| ["url", "command"].iter().any(|k| o.get(*k).and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty())))
}

/// `{name: definition}` of an `.mcp.json` or the `mcpServers` of a config (a top-level map of servers is accepted too).
fn servers_of(content: &Value) -> Map<String, Value> {
    let Some(object) = content.as_object() else { return Map::new() };
    match object.get("mcpServers") {
        Some(Value::Object(m)) => m.clone(),
        Some(_) => Map::new(),
        None if object.values().all(Value::is_object) => object.clone(),
        None => Map::new(),
    }
}

fn same_folder(left: &str, right: &str) -> bool {
    nt_normcase(&nt_abspath(left)) == nt_normcase(&nt_abspath(right))
}

fn substitute_root(value: &Value, root: &str) -> Value {
    match value {
        Value::String(s) => Value::String(s.replace("${CLAUDE_PLUGIN_ROOT}", root)),
        Value::Array(items) => Value::Array(items.iter().map(|i| substitute_root(i, root)).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), substitute_root(v, root))).collect()),
        other => other.clone(),
    }
}

struct Rows(Vec<(String, Row)>);

impl Rows {
    fn add(&mut self, name: &str, source: &str, definition: Option<&Value>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let key = name.to_lowercase();
        let index = match self.0.iter().position(|(k, _)| *k == key) {
            Some(i) => i,
            None => {
                self.0.push((key, Row { name: name.to_string(), sources: vec![], definition: None }));
                self.0.len() - 1
            }
        };
        let row = &mut self.0[index].1;
        if !row.sources.iter().any(|s| s == source) {
            row.sources.push(source.to_string());
        }
        if row.definition.is_none() {
            if let Some(d) = definition.filter(|d| usable(d)) {
                row.definition = Some(d.clone());
            }
        }
    }
}

/// Every server found, in discovery order, one row per name (case-insensitive).
pub fn collect(env: &Env, project_root: &str, claude_json: Option<&str>) -> Vec<Row> {
    let mut rows = Rows(Vec::new());
    let path = claude_json.filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(|| claude_config_path(env));
    let config = read_json(&path).filter(Value::is_object).unwrap_or(json!({}));
    // user scope: the `mcpServers` of the config
    if let Some(Value::Object(servers)) = config.get("mcpServers") {
        for (name, definition) in servers {
            rows.add(name, SOURCE_USER, Some(definition));
        }
    }
    if let Some(Value::Object(projects)) = config.get("projects") {
        for (path, entry) in projects {
            if !project_root.is_empty() && !same_folder(path, project_root) {
                continue;
            }
            if let Some(Value::Object(servers)) = entry.get("mcpServers") {
                for (name, definition) in servers {
                    rows.add(name, SOURCE_PROJECT, Some(definition));
                }
            }
        }
    }
    if !project_root.is_empty() {
        if let Some(content) = read_json(&Path::new(project_root).join(".mcp.json")) {
            for (name, definition) in &servers_of(&content) {
                rows.add(name, SOURCE_MCP_FILE, Some(definition));
            }
        }
    }
    let home = claude_home(env);
    let settings = read_json(&home.join("settings.json")).filter(Value::is_object).unwrap_or(json!({}));
    let enabled = settings.get("enabledPlugins").and_then(Value::as_object);
    let installed = read_json(&home.join("plugins").join("installed_plugins.json"));
    let plugins = installed.as_ref().and_then(|i| i.get("plugins")).and_then(Value::as_object).cloned().unwrap_or_default();
    for (plugin_id, installs) in &plugins {
        if enabled.is_some_and(|e| e.get(plugin_id) != Some(&Value::Bool(true))) {
            continue;
        }
        let plugin = plugin_id.split('@').next().unwrap_or("");
        for install in installs.as_array().into_iter().flatten() {
            let Some(root) = install.get("installPath").and_then(Value::as_str).filter(|r| Path::new(r).is_dir()) else { continue };
            let mut found = read_json(&Path::new(root).join(".mcp.json")).map(|c| servers_of(&c)).unwrap_or_default();
            let manifest = read_json(&Path::new(root).join(".claude-plugin").join("plugin.json"));
            if let Some(Value::Object(from_manifest)) = manifest.as_ref().and_then(|m| m.get("mcpServers")) {
                let mut merged = from_manifest.clone();
                for (k, v) in found {
                    merged.insert(k, v);
                }
                found = merged;
            }
            for (name, definition) in &found {
                rows.add(&plugin_server_name(plugin, name), SOURCE_PLUGIN, Some(&substitute_root(definition, root)));
            }
            break;
        }
    }
    if let Some(Value::Object(permissions)) = settings.get("permissions") {
        for key in ["allow", "deny", "ask"] {
            for rule in permissions.get(key).and_then(Value::as_array).into_iter().flatten() {
                let rule_text = rule.as_str().map(String::from).unwrap_or_else(|| rule.to_string());
                let text = rule_text.split('(').next().unwrap_or("").trim();
                if text.starts_with("mcp__") && text.len() > 5 {
                    rows.add(text[5..].split("__").next().unwrap_or(""), SOURCE_PERMISSION, None);
                }
            }
        }
    }
    rows.0.into_iter().map(|(_, r)| r).collect()
}

/// What the Settings screen lists: `[{"name", "sources", "defined"}]` (no definition, no credential).
pub fn list_servers(env: &Env, project_root: &str, claude_json: Option<&str>) -> Vec<Value> {
    collect(env, project_root, claude_json).into_iter().map(|r| json!({"name": r.name, "sources": r.sources, "defined": r.definition.is_some()})).collect()
}

/// The usable definition of a server by its name (any spelling that normalises to the same tool prefix), or `None`. Never printed.
pub fn definition(env: &Env, name: &str, project_root: &str, claude_json: Option<&str>) -> Option<Value> {
    let wanted = normalize_name(name);
    collect(env, project_root, claude_json).into_iter().find(|r| r.definition.is_some() && normalize_name(&r.name) == wanted).and_then(|r| r.definition)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn finds_every_source_once_per_name() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("claude-home");
        let plugin_root = dir.path().join("plug");
        let project = dir.path().join("proj");
        write(
            dir.path(),
            "claude.json",
            &format!(
                r#"{{"mcpServers": {{"Db": {{"command": "npx"}}, "empty": {{}}}}, "projects": {{"{}": {{"mcpServers": {{"local": {{"url": "http://x"}}}}}}}}}}"#,
                project.display().to_string().replace('\\', "\\\\")
            ),
        );
        write(&project, ".mcp.json", r#"{"mcpServers": {"db": {"command": "other"}, "fileonly": {"command": "f"}}}"#);
        write(&home, "settings.json", r#"{"enabledPlugins": {"p@m": true, "off@m": false}, "permissions": {"allow": ["mcp__db__query(x)", "mcp__ghost", "Bash(ls)"]}}"#);
        write(&plugin_root, ".mcp.json", r#"{"mcpServers": {"srv": {"command": "${CLAUDE_PLUGIN_ROOT}/run"}}}"#);
        write(
            &home,
            "plugins/installed_plugins.json",
            &format!(
                r#"{{"plugins": {{"p@m": [{{"installPath": "{}"}}], "off@m": [{{"installPath": "{}"}}]}}}}"#,
                plugin_root.display().to_string().replace('\\', "\\\\"),
                plugin_root.display().to_string().replace('\\', "\\\\")
            ),
        );
        let env = Env::from_pairs([(CLAUDE_HOME_VARIABLE, home.display().to_string()), (CLAUDE_CONFIG_VARIABLE, dir.path().join("claude.json").display().to_string())]);
        let rows = collect(&env, &project.display().to_string(), None);
        let names: Vec<(&str, Vec<&str>, bool)> =
            rows.iter().map(|r| (r.name.as_str(), r.sources.iter().map(String::as_str).collect(), r.definition.is_some())).collect();
        assert_eq!(
            names,
            vec![
                ("Db", vec!["user", ".mcp.json", "permissions"], true),
                ("empty", vec!["user"], false),
                ("local", vec!["project"], true),
                ("fileonly", vec![".mcp.json"], true),
                ("plugin:p:srv", vec!["plugin"], true),
                ("ghost", vec!["permissions"], false),
            ]
        );
        let plugin_def = definition(&env, "plugin:p:srv", "", None).unwrap();
        assert!(plugin_def["command"].as_str().unwrap().ends_with("/run") && !plugin_def["command"].as_str().unwrap().contains("${"));
        assert_eq!(definition(&env, "DB", &project.display().to_string(), None).unwrap()["command"], "npx");
        assert!(definition(&env, "ghost", "", None).is_none());
        assert_eq!(list_servers(&env, "", None).len(), 5);
    }
}
