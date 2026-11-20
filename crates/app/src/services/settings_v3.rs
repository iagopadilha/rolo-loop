// SPDX-License-Identifier: MIT
//! Settings v3: AI providers in run order, notification channels, the agent policy, the quality gate checks and the first-run guide.
//!
//! Everything is saved at once in `<data dir>/config.json` (survives a restart and a reboot); tokens, keys, passwords and webhook URLs go to
//! the encrypted secrets store (`cfg::secrets`, DPAPI for the current user) and come back only as `••••abcd`. The engine reads the same
//! values: providers through `engine_state::providers`, notification fields as environment values (`cfg::env::settings_values`).

use super::App;
use crate::domain::errors::{AppError, Kind};
use engine_state::providers::{self, Provider};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::time::Instant;

/// Notification fields the page edits: (variable, secret). Secrets go to the store; the rest to `config.json` `environment`.
pub const NOTIFY_FIELDS: [(&str, bool); 19] = [
    ("LOOP_NOTIFY_CHANNELS", false),
    ("LOOP_NOTIFY_EVENTS", false),
    ("LOOP_NOTIFY_TO", false),
    ("LOOP_NOTIFY_MILESTONES", false),
    ("LOOP_EMAIL_PROVIDER", false),
    ("LOOP_SMTP_HOST", false),
    ("LOOP_SMTP_PORT", false),
    ("LOOP_SMTP_SECURITY", false),
    ("LOOP_SMTP_USER", false),
    ("LOOP_SMTP_PASSWORD", true),
    ("LOOP_SMTP_FROM", false),
    ("LOOP_EMAIL_HTTP_URL", false),
    ("LOOP_EMAIL_HTTP_FROM", false),
    ("LOOP_EMAIL_HTTP_TOKEN", true),
    ("LOOP_WEBHOOK_URL", true),
    ("LOOP_WEBHOOK_METHOD", false),
    ("LOOP_WEBHOOK_PRESET", false),
    ("LOOP_WEBHOOK_BODY", false),
    ("LOOP_WEBHOOK_HEADERS", true),
];
/// Events a notice can be about, as the engine names them, with the words the page shows.
pub const NOTIFY_EVENTS: [(&str, &str); 6] = [
    ("awaiting_user", "The loop needs you"),
    ("done", "The loop finished"),
    ("error", "The loop stopped on an error"),
    ("paused", "Paused at the cost limit"),
    ("stage_closed", "A stage was completed"),
    ("custom", "Waits and usage limits"),
];
pub const AGENTS_KEY: &str = "agents";
pub const QUALITY_KEY: &str = "quality";
pub const ONBOARDING_KEY: &str = "onboarding";
/// Quality gate checks, the default level and what each one looks at.
pub const QUALITY_CHECKS: [(&str, &str, &str); 6] = [
    ("spec", "block", "Every requirement of the feature's spec is implemented and tested; nothing outside the spec was added"),
    ("security", "block", "Injection, secrets in code, missing authorization, unchecked input, path traversal, unsafe deserialization, risky dependencies"),
    ("standards", "block", "The rules of the backend-dev / frontend-dev skills and of the project's standards slots"),
    ("naming", "block", "Descriptive, complete names that follow the project's pattern (no abbreviations, no data/manager/helper/temp)"),
    ("size", "block", "No giant file, class or method; one responsibility each"),
    ("patterns", "warn", "Right layer, existing code reused, the project's design patterns and error handling"),
];
const LEVELS: [&str; 3] = ["block", "warn", "off"];
/// Agents a loop can run without (the rest are the pipeline itself).
pub const OPTIONAL_AGENTS: [&str; 3] = ["spec-reviewer", "verifier-flow", "quality-gate"];
pub const MODELS: [&str; 4] = ["", "opus", "sonnet", "haiku"];
const TEST_PROMPT: &str = "Reply with exactly: OK";
const TEST_TIMEOUT_SECONDS: u64 = 120;

impl App {
    /// `(--claude, --claude-fallback...)` as the run should use them now: the enabled providers in Settings order (read from config.json at
    /// this moment), else the `claude_binary` / `claude_fallback_binaries` the app started with.
    pub fn claude_chain(&self) -> (String, Vec<String>) {
        let config = self.settings.read_all_or_empty();
        providers::install(&config);
        providers::chain(&config).unwrap_or_else(|| (self.config.claude_binary.clone(), self.config.claude_fallback_binaries.clone()))
    }

    /// [`App::claude_chain`] as loop flags (`--claude` only when it is not the default `claude`).
    pub fn claude_chain_flags(&self) -> Vec<String> {
        let (primary, fallbacks) = self.claude_chain();
        let mut flags = Vec::new();
        if !primary.is_empty() {
            flags.extend(["--claude".to_string(), primary]);
        }
        for fallback in fallbacks {
            flags.extend(["--claude-fallback".to_string(), fallback]);
        }
        flags
    }

    pub fn settings_v3(&self) -> SettingsV3<'_> {
        SettingsV3 { app: self }
    }
}

pub struct SettingsV3<'a> {
    app: &'a App,
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(Kind::InvalidSetting, message)
}

impl SettingsV3<'_> {
    fn data_dir(&self) -> PathBuf {
        self.app.config.user_data_dir.clone()
    }

    fn config(&self) -> Map<String, Value> {
        self.app.settings.read_all_or_empty()
    }

    fn secret_view(&self, name: &str) -> Value {
        match cfg::secrets::get(&self.data_dir(), name) {
            Some(value) => json!({"set": true, "masked": cfg::secrets::masked(&value)}),
            None => json!({"set": false, "masked": ""}),
        }
    }

    fn store_secret(&self, name: &str, value: &str) -> Result<(), AppError> {
        cfg::secrets::set(&self.data_dir(), name, value).map_err(|e| AppError::new(Kind::ConfigFileUnreadable, format!("the secret could not be saved: {e}")))
    }

    // ---------------------------------------------------------------- providers
    /// Providers in run order with what the page shows: status, problems, whether a secret is saved (masked).
    pub fn list_providers(&self) -> Value {
        let config = self.config();
        let configured = config.get(providers::CONFIG_KEY).is_some();
        let data_dir = self.data_dir();
        let items: Vec<Value> = providers::read(&config)
            .iter()
            .map(|provider| {
                let mut item = provider.to_json();
                item["secret"] = self.secret_view(&providers::secret_name(provider));
                item["needs_secret"] = json!(provider.needs_secret());
                item["path"] = json!(engine_state::claude_binaries::resolve(&provider.command_or_default()));
                item["problems"] = json!(providers::problems(provider, &data_dir));
                item
            })
            .collect();
        json!({"providers": items, "configured": configured, "kinds": providers::KINDS, "models": MODELS})
    }

    /// Saves the whole list in the given order (ids cleaned, unknown kinds refused), re-reads the table the runs use and returns the list.
    pub fn save_providers(&self, body: &Value) -> Result<Value, AppError> {
        let Some(list) = body.get("providers").and_then(Value::as_array) else { return Err(invalid("providers must be a list")) };
        let mut cleaned: Vec<Value> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        for raw in list {
            let Some(provider) = providers::from_json(raw) else {
                return Err(invalid(format!("invalid provider (needs an id and a kind among {}): {raw}", providers::KINDS.join(", "))));
            };
            if ids.contains(&provider.id) {
                return Err(invalid(format!("two providers use the id {}", provider.id)));
            }
            if provider.kind == "anthropic-gateway" && !provider.base_url.starts_with("http") {
                return Err(invalid("a gateway needs an http(s) base URL"));
            }
            ids.push(provider.id.clone());
            cleaned.push(provider.to_json());
        }
        if !cleaned.iter().any(|p| p["enabled"] == true) {
            return Err(invalid("keep at least one provider enabled"));
        }
        self.app.settings.set_value(providers::CONFIG_KEY, Value::Array(cleaned))?;
        // a removed provider's secret goes with it
        for name in cfg::secrets::names(&self.data_dir()) {
            if name.starts_with("PROVIDER_") && !ids.iter().any(|id| providers::secret_name(&Provider { id: id.clone(), ..Provider::default() }) == name) {
                let _ = cfg::secrets::remove(&self.data_dir(), &name);
            }
        }
        providers::install(&self.config());
        Ok(self.list_providers())
    }

    /// Saves (or clears, with an empty value) the token or key of one provider.
    pub fn save_provider_secret(&self, id: &str, value: &str) -> Result<Value, AppError> {
        let provider = providers::read(&self.config()).into_iter().find(|p| p.id == id).ok_or_else(|| invalid(format!("unknown provider {id}")))?;
        self.store_secret(&providers::secret_name(&provider), value.trim())?;
        Ok(self.list_providers())
    }

    /// Runs the provider once with a one-line prompt: `{ok, reply, model, cost_usd, seconds, error}`. Costs one tiny call.
    pub fn test_provider(&self, id: &str) -> Result<Value, AppError> {
        let config = self.config();
        providers::install(&config);
        let provider = providers::read(&config).into_iter().find(|p| p.id == id).ok_or_else(|| invalid(format!("unknown provider {id}")))?;
        let problems = providers::problems(&provider, &self.data_dir());
        if !problems.is_empty() {
            return Ok(json!({"ok": false, "error": problems.join("; ")}));
        }
        let mut argv = engine_state::claude_binaries::base_argv(&provider.reference());
        argv.extend(["-p".to_string(), TEST_PROMPT.to_string(), "--output-format".to_string(), "json".to_string(), "--max-turns".to_string(), "1".to_string()]);
        if !provider.model_orchestrator.is_empty() {
            argv.extend(["--model".to_string(), provider.model_orchestrator.clone()]);
        }
        let (argv, stdin_text) = engine_state::claude_binaries::prompt_through_stdin(&argv, "");
        let mut env = self.app.claude.build_env_without_secrets();
        for (key, value) in providers::environment(&provider, &self.data_dir()) {
            env.retain(|(k, _)| !k.eq_ignore_ascii_case(&key));
            env.push((key, value));
        }
        let started = Instant::now();
        let folder = std::env::temp_dir().display().to_string();
        let (code, output, error) = self.app.processes.run_and_wait(&argv, &folder, TEST_TIMEOUT_SECONDS, Some(&stdin_text), &[], Some(&env));
        let seconds = (started.elapsed().as_secs_f64() * 10.0).round() / 10.0;
        let parsed = output.find('{').and_then(|start| serde_json::from_str::<Value>(&output[start..]).ok()).unwrap_or(Value::Null);
        let reply = parsed.get("result").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let model = parsed.get("modelUsage").and_then(Value::as_object).and_then(|m| m.keys().next().cloned()).unwrap_or_default();
        let failed = code != 0 || parsed.get("is_error") == Some(&Value::Bool(true)) || parsed.is_null();
        let why = if failed {
            let source = if !reply.is_empty() {
                reply.clone()
            } else if !error.trim().is_empty() {
                error.trim().to_string()
            } else {
                output.trim().to_string()
            };
            engine_state::cli_output::redact_text(&source, Some(300))
        } else {
            String::new()
        };
        Ok(
            json!({"ok": !failed, "reply": reply.chars().take(80).collect::<String>(), "model": model, "cost_usd": parsed.get("total_cost_usd").cloned().unwrap_or(Value::Null),
                  "seconds": seconds, "error": why}),
        )
    }

    // ---------------------------------------------------------------- notifications
    /// Every notification field: plain values as stored, secrets masked; the events with their words.
    pub fn get_notifications(&self) -> Value {
        let config = self.config();
        let stored = config.get(cfg::env::SETTINGS_ENVIRONMENT_KEY).and_then(Value::as_object).cloned().unwrap_or_default();
        let mut fields = Map::new();
        for (name, secret) in NOTIFY_FIELDS {
            let value = if secret { self.secret_view(name) } else { json!({"value": stored.get(name).cloned().unwrap_or(json!(""))}) };
            fields.insert(name.to_string(), value);
        }
        let mut env = self.app.env.clone();
        cfg::env::load_all(&mut env);
        let channels: Vec<Value> =
            engine_state::notify::CHANNELS.iter().map(|name| json!({"name": name, "missing": engine_state::notify::missing_config(&env, name)})).collect();
        json!({"fields": fields, "events": NOTIFY_EVENTS.iter().map(|(id, words)| json!({"id": id, "label": words})).collect::<Vec<_>>(), "channels": channels})
    }

    /// Saves the fields given (`{"fields": {"LOOP_SMTP_HOST": "...", "LOOP_SMTP_PASSWORD": "..."}}`); an absent field stays, an empty one is
    /// cleared. Only the known fields are accepted.
    pub fn save_notifications(&self, body: &Value) -> Result<Value, AppError> {
        let Some(fields) = body.get("fields").and_then(Value::as_object) else { return Err(invalid("fields must be an object")) };
        let config = self.config();
        let mut environment = config.get(cfg::env::SETTINGS_ENVIRONMENT_KEY).and_then(Value::as_object).cloned().unwrap_or_default();
        for (name, value) in fields {
            let Some((_, secret)) = NOTIFY_FIELDS.iter().find(|(n, _)| n == name) else { return Err(invalid(format!("unknown field {name}"))) };
            let text = match value {
                Value::String(s) => s.trim().to_string(),
                Value::Array(items) => items.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(","),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => {
                    if *b {
                        "on".into()
                    } else {
                        "off".into()
                    }
                }
                Value::Null => String::new(),
                Value::Object(_) => return Err(invalid(format!("{name} must be text"))),
            };
            if name == "LOOP_WEBHOOK_HEADERS" && !text.is_empty() && serde_json::from_str::<Value>(&text).map(|v| !v.is_object()).unwrap_or(true) {
                return Err(invalid("webhook headers must be a JSON object, e.g. {\"Authorization\": \"Bearer ...\"}"));
            }
            if *secret {
                self.store_secret(name, &text)?;
            } else if text.is_empty() {
                environment.remove(name);
            } else {
                environment.insert(name.clone(), json!(text));
            }
        }
        self.app.settings.set_value(cfg::env::SETTINGS_ENVIRONMENT_KEY, Value::Object(environment))?;
        Ok(self.get_notifications())
    }

    // ---------------------------------------------------------------- agents
    /// The plugin's agents with their role, default model and the user's policy (enabled, model).
    pub fn list_agents(&self) -> Value {
        let policy = self.config().get(AGENTS_KEY).and_then(Value::as_object).cloned().unwrap_or_default();
        let folder = self.app.config.ctx.plugin_root.join("agents");
        let mut items: Vec<Value> = std::fs::read_dir(&folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|text| {
                let front = frontmatter(&text);
                let name = front.get("name")?.clone();
                let chosen = policy.get(&name).cloned().unwrap_or(json!({}));
                Some(json!({
                    "name": name, "description": front.get("description").cloned().unwrap_or_default(), "default_model": front.get("model").cloned().unwrap_or_default(),
                    "optional": OPTIONAL_AGENTS.contains(&name.as_str()), "enabled": chosen.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                    "model": chosen.get("model").and_then(Value::as_str).unwrap_or(""),
                }))
            })
            .collect();
        items.sort_by_key(|a| pipeline_position(a["name"].as_str().unwrap_or("")));
        json!({"agents": items, "models": MODELS, "folder_found": folder.is_dir()})
    }

    /// Saves `{"agents": {"<name>": {"enabled": bool, "model": ""}}}` (only optional agents can be turned off).
    pub fn save_agents(&self, body: &Value) -> Result<Value, AppError> {
        let Some(map) = body.get("agents").and_then(Value::as_object) else { return Err(invalid("agents must be an object")) };
        let mut policy = Map::new();
        for (name, choice) in map {
            let enabled = choice.get("enabled").and_then(Value::as_bool).unwrap_or(true);
            let model = choice.get("model").and_then(Value::as_str).unwrap_or("").trim().to_string();
            if !enabled && !OPTIONAL_AGENTS.contains(&name.as_str()) {
                return Err(invalid(format!("{name} is part of the pipeline and cannot be turned off")));
            }
            if !MODELS.contains(&model.as_str()) {
                return Err(invalid(format!("model must be one of: inherit, {}", MODELS[1..].join(", "))));
            }
            policy.insert(name.clone(), json!({"enabled": enabled, "model": model}));
        }
        self.app.settings.set_value(AGENTS_KEY, Value::Object(policy))?;
        Ok(self.list_agents())
    }

    // ---------------------------------------------------------------- quality gate
    pub fn get_quality(&self) -> Value {
        let stored = self.config().get(QUALITY_KEY).and_then(|q| q.get("checks")).and_then(Value::as_object).cloned().unwrap_or_default();
        let checks: Vec<Value> = QUALITY_CHECKS
            .iter()
            .map(|(id, default, text)| json!({"id": id, "level": stored.get(*id).and_then(Value::as_str).unwrap_or(default), "default": default, "text": text}))
            .collect();
        json!({"checks": checks, "levels": LEVELS})
    }

    pub fn save_quality(&self, body: &Value) -> Result<Value, AppError> {
        let Some(map) = body.get("checks").and_then(Value::as_object) else { return Err(invalid("checks must be an object")) };
        let mut checks = Map::new();
        for (id, level) in map {
            if !QUALITY_CHECKS.iter().any(|(known, _, _)| known == id) {
                return Err(invalid(format!("unknown check {id}")));
            }
            let level = level.as_str().unwrap_or("");
            if !LEVELS.contains(&level) {
                return Err(invalid(format!("{id} must be block, warn or off")));
            }
            checks.insert(id.clone(), json!(level));
        }
        self.app.settings.set_value(QUALITY_KEY, json!({"checks": checks}))?;
        Ok(self.get_quality())
    }

    // ---------------------------------------------------------------- first-run guide
    pub fn get_onboarding(&self) -> Value {
        let stored = self.config().get(ONBOARDING_KEY).cloned().unwrap_or(json!({}));
        json!({"done": stored.get("done").and_then(Value::as_bool).unwrap_or(false), "step": stored.get("step").and_then(Value::as_u64).unwrap_or(0)})
    }

    pub fn save_onboarding(&self, body: &Value) -> Result<Value, AppError> {
        let done = body.get("done").and_then(Value::as_bool).unwrap_or(false);
        let step = body.get("step").and_then(Value::as_u64).unwrap_or(0).min(50);
        self.app.settings.set_value(ONBOARDING_KEY, json!({"done": done, "step": step}))?;
        Ok(self.get_onboarding())
    }
}

/// `key: value` lines of a Markdown file's `---` front matter.
fn frontmatter(text: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return out;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            out.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    out
}

/// Order of the agents in a loop (unknown ones last).
fn pipeline_position(name: &str) -> usize {
    ["brownfield-mapper", "spec-writer", "spec-reviewer", "executor", "verifier", "verifier-flow", "quality-gate"].iter().position(|n| *n == name).unwrap_or(99)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_save_in_order_and_secrets_stay_masked() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let s = app.settings_v3();
        assert_eq!(s.list_providers()["configured"], false);
        assert_eq!(s.list_providers()["providers"][0]["id"], "main");
        let saved = s
            .save_providers(&json!({"providers": [
                {"id": "api", "name": "Pay per use", "kind": "anthropic-api-key"},
                {"id": "main", "kind": "claude-subscription", "models": {"orchestrator": "opus"}}
            ]}))
            .unwrap();
        assert_eq!(saved["providers"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect::<Vec<_>>(), ["api", "main"]);
        assert!(saved["providers"][0]["problems"].as_array().unwrap().iter().any(|p| p.as_str().unwrap().contains("no token")));
        if cfg!(windows) {
            let with_key = s.save_provider_secret("api", "sk-ant-test-1234567890-abcd").unwrap();
            assert_eq!(with_key["providers"][0]["secret"]["masked"], "••••abcd");
            let on_disk = std::fs::read_to_string(dir.path().join("config.json")).unwrap_or_default();
            assert!(!on_disk.contains("sk-ant-test"));
        }
        assert!(s.save_providers(&json!({"providers": [{"id": "x", "kind": "claude-subscription", "enabled": false}]})).is_err());
        assert!(s.save_providers(&json!({"providers": [{"id": "x", "kind": "bogus"}]})).is_err());
        assert!(s.save_providers(&json!({"providers": [{"id": "g", "kind": "anthropic-gateway", "base_url": "localhost"}]})).is_err());
    }

    #[test]
    fn notifications_split_plain_values_and_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let s = app.settings_v3();
        let saved = s
            .save_notifications(&json!({"fields": {"LOOP_SMTP_HOST": " smtp.example.com ", "LOOP_NOTIFY_CHANNELS": ["toast", "email"], "LOOP_SMTP_PORT": 587}}))
            .unwrap();
        assert_eq!(saved["fields"]["LOOP_SMTP_HOST"]["value"], "smtp.example.com");
        assert_eq!(saved["fields"]["LOOP_NOTIFY_CHANNELS"]["value"], "toast,email");
        assert_eq!(saved["fields"]["LOOP_SMTP_PORT"]["value"], "587");
        assert!(s.save_notifications(&json!({"fields": {"NOPE": "x"}})).is_err());
        assert!(s.save_notifications(&json!({"fields": {"LOOP_WEBHOOK_HEADERS": "[1]"}})).is_err());
        let cleared = s.save_notifications(&json!({"fields": {"LOOP_SMTP_HOST": ""}})).unwrap();
        assert_eq!(cleared["fields"]["LOOP_SMTP_HOST"]["value"], "");
        assert_eq!(saved["events"].as_array().unwrap().len(), NOTIFY_EVENTS.len());
    }

    #[test]
    fn agents_quality_and_onboarding_are_validated_and_saved() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let s = app.settings_v3();
        assert!(s.save_agents(&json!({"agents": {"executor": {"enabled": false}}})).is_err());
        assert!(s.save_agents(&json!({"agents": {"quality-gate": {"enabled": true, "model": "gpt"}}})).is_err());
        assert!(s.save_agents(&json!({"agents": {"quality-gate": {"enabled": false, "model": "opus"}}})).is_ok());
        assert_eq!(app.settings.read_all_or_empty()["agents"]["quality-gate"]["enabled"], false);
        let quality = s.save_quality(&json!({"checks": {"security": "block", "patterns": "off"}})).unwrap();
        let level = |id: &str| quality["checks"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap()["level"].clone();
        assert_eq!((level("security"), level("patterns"), level("naming")), (json!("block"), json!("off"), json!("block")));
        assert!(s.save_quality(&json!({"checks": {"security": "maybe"}})).is_err());
        assert_eq!(s.get_onboarding()["done"], false);
        assert_eq!(s.save_onboarding(&json!({"done": true, "step": 4})).unwrap()["done"], true);
        assert_eq!(frontmatter("---\nname: x\nmodel: sonnet\n---\nbody")["model"], "sonnet");
    }
}
