// SPDX-License-Identifier: MIT
//! Single app configuration (paths, ports, times). Built once in the composition and injected where needed.
//!
//! Precedence: command-line argument > environment variable > `~/.loop-harness/config.json` > default.

use cfg::Env;
use engine_state::app_ctx::AppCtx;
use engine_state::claude_binaries;
use serde_json::{Map, Value};
use std::path::PathBuf;

pub const DEFAULT_PORT_RANGE: (u16, u16) = (47800, 47899);
pub const DEFAULT_IDLE_HOURS: f64 = 2.0;
pub const DEFAULT_INVENTORY_GIANT_LINES: i64 = 1500;
pub const DEFAULT_INVENTORY_GIANT_KB: i64 = 200;
/// Subproject map jobs of a monorepo that run at the same time (config.json: `max_parallel_maps`).
pub const DEFAULT_MAX_PARALLEL_MAPS: i64 = 6;
/// The Claude Code alias of the latest Opus; "" in config.json = the account default (no `--model`).
pub const DEFAULT_ORCHESTRATOR_MODEL: &str = "opus";

/// A positive whole number from config.json, else `default` and a note (absent key = default, no note).
pub fn read_positive_threshold(user_config: &Map<String, Value>, key: &str, default: i64, notes: &mut Vec<String>) -> i64 {
    let Some(value) = user_config.get(key) else { return default };
    let usable = value.is_number() && value.as_f64().is_some_and(|n| n.is_finite() && n > 0.0);
    if !usable || value.as_f64().map(|n| n as i64).unwrap_or(0) <= 0 {
        notes.push(format!("{key} in config.json is not a positive number ({}): using {default}", py_repr_value(value)));
        return default;
    }
    value.as_f64().map(|n| n as i64).unwrap_or(default)
}

/// Python `repr` of a JSON value, for the notes.
fn py_repr_value(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => pycompat::py_repr_str(s),
        Value::Number(n) => n.to_string(),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr_value).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!("{{{}}}", map.iter().map(|(k, v)| format!("{}: {}", pycompat::py_repr_str(k), py_repr_value(v))).collect::<Vec<_>>().join(", ")),
    }
}

/// A number from config.json (a number or numeric text such as "8"); anything else (text, null, a list) gives `default`.
pub fn read_float(value: Option<&Value>, default: f64) -> f64 {
    match value {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(default),
        _ => default,
    }
}

/// `orchestrator_model` of config.json: a model alias or id, "" = no `--model` flag; absent or not text = the default.
pub fn read_orchestrator_model(user_config: &Map<String, Value>) -> String {
    match user_config.get("orchestrator_model") {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => DEFAULT_ORCHESTRATOR_MODEL.to_string(),
    }
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    /// Where the plugin, the skill, the guard and the toolkit were found.
    pub ctx: AppCtx,
    /// `<repo root>/app`: holds `frontend/` and `templates/`.
    pub app_root_dir: PathBuf,
    pub user_data_dir: PathBuf,
    pub frontend_dir: PathBuf,
    pub templates_dir: PathBuf,
    pub claude_transcripts_dir: String,
    pub output_dir: String,
    /// Empty = `claude` from PATH; a name such as `claude-backup` (wrapper of another account) works; tests inject a fake `.py`.
    pub claude_binary: String,
    /// Tried in order when the running command hits the account usage limit.
    pub claude_fallback_binaries: Vec<String>,
    pub port_range: (u16, u16),
    pub fixed_port: u16,
    /// 0 = never shuts down on its own.
    pub idle_hours: f64,
    pub open_browser: bool,
    /// The approval needs the cached `loop --check --planning` to be READY (gate `ready`).
    pub planning_readiness_enforced: bool,
    pub inventory_giant_lines: i64,
    pub inventory_giant_kb: i64,
    pub orchestrator_model: String,
    pub max_parallel_maps: i64,
    /// Why a configured threshold was ignored (empty = none).
    pub inventory_threshold_note: String,
}

/// Command-line values that win over everything else (only non-empty values count).
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub user_data_dir: String,
    pub claude_binary: String,
    pub fixed_port: u16,
    pub idle_hours: Option<f64>,
    pub open_browser: Option<bool>,
    /// Where the Claude Code transcripts are read from (the demo points it at its own fake ones).
    pub claude_transcripts_dir: String,
}

impl AppConfig {
    /// `<user data dir>/app.json`: pid, port, url and administrative token of the live app (only the local user reads it).
    pub fn app_registry_path(&self) -> PathBuf {
        self.user_data_dir.join("app.json")
    }

    pub fn projects_registry_path(&self) -> PathBuf {
        self.user_data_dir.join("projects.json")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.user_data_dir.join("logs")
    }

    pub fn env(&self) -> &Env {
        &self.ctx.env
    }

    pub fn guard_path(&self) -> PathBuf {
        self.ctx.guard.clone()
    }
}

/// Builds the config. `overrides` come from the command line (only non-empty values count).
pub fn load_app_config(env: &Env, overrides: &Overrides) -> AppConfig {
    let mut env = env.clone();
    if !overrides.user_data_dir.is_empty() {
        env.set("LOOP_DATA_DIR", &overrides.user_data_dir);
    }
    let ctx = AppCtx::discover(&env);
    from_ctx(ctx, overrides)
}

pub fn from_ctx(ctx: AppCtx, overrides: &Overrides) -> AppConfig {
    let user_config = ctx.config.clone();
    let data_dir = ctx.data_dir();
    let home = ctx.env.home_dir();
    let mut notes = Vec::new();
    let text_or = |key: &str, default: PathBuf| match user_config.get(key).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => default.display().to_string(),
    };
    let app_root_dir = ctx.repo_root.join("app");
    let mut config = AppConfig {
        inventory_giant_lines: read_positive_threshold(&user_config, "inventory_giant_lines", DEFAULT_INVENTORY_GIANT_LINES, &mut notes),
        inventory_giant_kb: read_positive_threshold(&user_config, "inventory_giant_kb", DEFAULT_INVENTORY_GIANT_KB, &mut notes),
        max_parallel_maps: read_positive_threshold(&user_config, "max_parallel_maps", DEFAULT_MAX_PARALLEL_MAPS, &mut notes),
        frontend_dir: app_root_dir.join("frontend"),
        templates_dir: app_root_dir.join("templates"),
        app_root_dir,
        user_data_dir: PathBuf::from(pycompat::nt_abspath(&data_dir.display().to_string())),
        claude_transcripts_dir: text_or("claude_transcripts_dir", home.join(".claude").join("projects")),
        output_dir: text_or("output_dir", home.join("Downloads")),
        claude_binary: user_config.get("claude_binary").and_then(Value::as_str).unwrap_or("").to_string(),
        claude_fallback_binaries: claude_binaries::read_fallbacks(user_config.get("claude_fallback_binaries").unwrap_or(&Value::Null)),
        port_range: DEFAULT_PORT_RANGE,
        fixed_port: 0,
        idle_hours: read_float(user_config.get("idle_hours"), DEFAULT_IDLE_HOURS),
        open_browser: true,
        planning_readiness_enforced: true,
        orchestrator_model: read_orchestrator_model(&user_config),
        inventory_threshold_note: notes.join("; "),
        ctx,
    };
    if !overrides.claude_binary.is_empty() {
        config.claude_binary = overrides.claude_binary.clone();
    }
    if overrides.fixed_port != 0 {
        config.fixed_port = overrides.fixed_port;
    }
    if let Some(hours) = overrides.idle_hours {
        config.idle_hours = hours;
    }
    if !overrides.claude_transcripts_dir.is_empty() {
        config.claude_transcripts_dir = overrides.claude_transcripts_dir.clone();
    }
    if let Some(open) = overrides.open_browser {
        config.open_browser = open;
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn thresholds_fall_back_with_a_note() {
        let mut notes = Vec::new();
        let config: Map<String, Value> = json!({"a": 5.9, "b": -1, "c": "x", "d": true, "e": 0}).as_object().unwrap().clone();
        assert_eq!(read_positive_threshold(&config, "a", 1, &mut notes), 5);
        assert_eq!(read_positive_threshold(&config, "missing", 7, &mut notes), 7);
        assert!(notes.is_empty());
        for key in ["b", "c", "d", "e"] {
            assert_eq!(read_positive_threshold(&config, key, 9, &mut notes), 9);
        }
        assert_eq!(notes[0], "b in config.json is not a positive number (-1): using 9");
        assert_eq!(notes[1], "c in config.json is not a positive number ('x'): using 9");
        assert_eq!(notes[2], "d in config.json is not a positive number (True): using 9");
        assert_eq!(
            (read_float(Some(&json!("8")), 1.0), read_float(Some(&json!(true)), 1.0), read_float(None, 2.0), read_float(Some(&json!([1])), 3.0)),
            (8.0, 1.0, 2.0, 3.0)
        );
        assert_eq!(read_orchestrator_model(&Map::new()), "opus");
        assert_eq!(read_orchestrator_model(json!({"orchestrator_model": " sonnet "}).as_object().unwrap()), "sonnet");
        assert_eq!(read_orchestrator_model(json!({"orchestrator_model": 3}).as_object().unwrap()), "opus");
    }

    #[test]
    fn config_reads_the_data_dir_and_honours_overrides() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"claude_binary": "claude-backup", "idle_hours": "0", "max_parallel_maps": 2, "output_dir": "D:\\out"}"#)
            .unwrap();
        let env = Env::from_pairs([("LOOP_DATA_DIR", dir.path().to_str().unwrap()), ("LOOP_CLAUDE_HOME", dir.path().to_str().unwrap())]);
        let config = load_app_config(&env, &Overrides::default());
        assert_eq!((config.claude_binary.as_str(), config.idle_hours, config.max_parallel_maps, config.output_dir.as_str()), ("claude-backup", 0.0, 2, "D:\\out"));
        assert!(config.projects_registry_path().ends_with("projects.json") && config.frontend_dir.ends_with("frontend"));
        let over = load_app_config(&env, &Overrides { claude_binary: "fake.py".into(), fixed_port: 47801, ..Default::default() });
        assert_eq!((over.claude_binary.as_str(), over.fixed_port), ("fake.py", 47801));
    }
}
