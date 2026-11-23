// SPDX-License-Identifier: MIT
//! AI providers of the loop, in run order (`config.json` `providers`). The first enabled one runs every turn and panel job; on an account
//! usage limit or an unusable account the engine moves to the next one (the fallback rules of `claude_binaries` / `run_loop`).
//!
//! Every provider drives a Claude Code compatible command (the engine reads its `stream-json`); what changes is how it signs in and which
//! models it uses:
//!   - `claude-subscription`: the `claude` CLI signed in with the user's plan (no key reaches it: an API key in the environment is blanked, or
//!     the CLI would bill it instead);
//!   - `claude-oauth-token`: `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token` (subscription, no browser);
//!   - `anthropic-api-key`: `ANTHROPIC_API_KEY`, billed per token;
//!   - `anthropic-gateway`: `ANTHROPIC_BASE_URL` (+ `ANTHROPIC_AUTH_TOKEN`): LiteLLM, a Bedrock or Vertex proxy, or local models behind a
//!     gateway;
//!   - `custom-cli`: another Claude Code compatible command (a local agent wrapper), with extra arguments such as
//!     `--dangerously-skip-permissions`.
//!
//! A provider is named in the fallback chain as `provider:<id>`: [`crate::claude_binaries`] resolves that name to the provider's command and
//! the turn adds [`environment`]. Its secret (token, key) lives in the encrypted secrets store under [`secret_name`]; it is read only when
//! a process starts and never written to `config.json`, a log or the panel.

use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::{LazyLock, RwLock};

pub const CONFIG_KEY: &str = "providers";
pub const REF_PREFIX: &str = "provider:";
pub const KINDS: [&str; 5] = ["claude-subscription", "claude-oauth-token", "anthropic-api-key", "anthropic-gateway", "custom-cli"];
const DEFAULT_COMMAND: &str = "claude";
const ID_MAX: usize = 32;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    /// The command (empty = `claude`).
    pub command: String,
    /// Extra arguments right after the command (a `custom-cli`'s own flags).
    pub args: Vec<String>,
    /// `anthropic-gateway` only.
    pub base_url: String,
    /// Model of the orchestrator turn (alias or id; empty = the app's `orchestrator_model`).
    pub model_orchestrator: String,
    /// Model of the subagents (empty = each agent's own model).
    pub model_agents: String,
}

impl Provider {
    pub fn reference(&self) -> String {
        format!("{REF_PREFIX}{}", self.id)
    }

    pub fn command_or_default(&self) -> String {
        if self.command.trim().is_empty() {
            DEFAULT_COMMAND.to_string()
        } else {
            self.command.trim().to_string()
        }
    }

    /// The kinds that need a secret before they can run.
    pub fn needs_secret(&self) -> bool {
        matches!(self.kind.as_str(), "claude-oauth-token" | "anthropic-api-key")
    }

    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "name": self.name, "kind": self.kind, "enabled": self.enabled, "command": self.command, "args": self.args,
               "base_url": self.base_url, "models": {"orchestrator": self.model_orchestrator, "agents": self.model_agents}})
    }
}

fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// `[a-z0-9-]` of at most 32 characters; anything else is dropped.
pub fn clean_id(raw: &str) -> String {
    raw.to_lowercase().chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-').take(ID_MAX).collect()
}

/// One provider from its JSON; `None` without an id or with an unknown kind.
pub fn from_json(value: &Value) -> Option<Provider> {
    let id = clean_id(&text(value.get("id")));
    let kind = text(value.get("kind"));
    if id.is_empty() || !KINDS.contains(&kind.as_str()) {
        return None;
    }
    let models = value.get("models");
    Some(Provider {
        name: Some(text(value.get("name"))).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone()),
        id,
        kind,
        enabled: value.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        command: text(value.get("command")),
        args: value
            .get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(String::from).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default(),
        base_url: text(value.get("base_url")),
        model_orchestrator: text(models.and_then(|m| m.get("orchestrator"))),
        model_agents: text(models.and_then(|m| m.get("agents"))),
    })
}

/// The providers of `config.json` in run order; when none is configured, the ones the old keys describe (`claude_binary`, then
/// `claude_fallback_binaries`), so an existing setup keeps its order.
pub fn read(config: &Map<String, Value>) -> Vec<Provider> {
    if let Some(list) = config.get(CONFIG_KEY).and_then(Value::as_array) {
        let mut seen: Vec<String> = Vec::new();
        return list
            .iter()
            .filter_map(from_json)
            .filter(|p| {
                let fresh = !seen.contains(&p.id);
                seen.push(p.id.clone());
                fresh
            })
            .collect();
    }
    migrated(config)
}

/// Providers equivalent to the old keys: the main command and each fallback, all `claude-subscription` (or `custom-cli` for a non-claude
/// command).
pub fn migrated(config: &Map<String, Value>) -> Vec<Provider> {
    let main = text(config.get("claude_binary"));
    let mut commands = vec![if main.is_empty() { DEFAULT_COMMAND.to_string() } else { main }];
    commands.extend(crate::claude_binaries::read_fallbacks(config.get("claude_fallback_binaries").unwrap_or(&Value::Null)));
    commands
        .into_iter()
        .enumerate()
        .map(|(position, command)| {
            let base = crate::claude_accounts::command_base_name(&command);
            Provider {
                id: if position == 0 { "main".into() } else { clean_id(&format!("cli-{base}")) },
                name: if position == 0 { "Claude (subscription)".into() } else { format!("`{base}`") },
                kind: if base.eq_ignore_ascii_case("claude") { "claude-subscription".into() } else { "custom-cli".into() },
                enabled: true,
                command: if command == DEFAULT_COMMAND { String::new() } else { command },
                ..Provider::default()
            }
        })
        .collect()
}

/// Name of the secret of a provider in the secrets store.
pub fn secret_name(provider: &Provider) -> String {
    format!("PROVIDER_{}_SECRET", provider.id.to_uppercase().replace('-', "_"))
}

static TABLE: LazyLock<RwLock<Vec<Provider>>> = LazyLock::new(|| RwLock::new(Vec::new()));

/// Makes the providers of `config` known to [`by_ref`] (the engine and the app call it once they read `config.json`, and again when the
/// Settings change).
pub fn install(config: &Map<String, Value>) {
    *TABLE.write().unwrap_or_else(|e| e.into_inner()) = read(config);
}

/// The provider a `provider:<id>` name stands for.
pub fn by_ref(name: &str) -> Option<Provider> {
    let id = name.strip_prefix(REF_PREFIX)?;
    TABLE.read().unwrap_or_else(|e| e.into_inner()).iter().find(|p| p.id == id).cloned()
}

/// `(--claude, --claude-fallback...)` of the enabled providers in order, as `provider:<id>` names; `None` when the user never configured
/// providers (the old keys keep driving the flags).
pub fn chain(config: &Map<String, Value>) -> Option<(String, Vec<String>)> {
    config.get(CONFIG_KEY)?.as_array()?;
    let enabled: Vec<String> = read(config).into_iter().filter(|p| p.enabled).map(|p| p.reference()).collect();
    let (first, rest) = enabled.split_first()?;
    Some((first.clone(), rest.to_vec()))
}

/// Variables the process of this provider starts with, on top of its normal environment. An empty value blanks a variable (a subscription
/// must not see an API key the user has in the environment).
pub fn environment(provider: &Provider, data_dir: &Path) -> Vec<(String, String)> {
    let secret = || crate::secret_store_value(data_dir, &secret_name(provider)).unwrap_or_default();
    let mut env: Vec<(String, String)> = match provider.kind.as_str() {
        "claude-subscription" => vec![("ANTHROPIC_API_KEY".into(), String::new()), ("ANTHROPIC_AUTH_TOKEN".into(), String::new())],
        "claude-oauth-token" => vec![("CLAUDE_CODE_OAUTH_TOKEN".into(), secret()), ("ANTHROPIC_API_KEY".into(), String::new())],
        "anthropic-api-key" => vec![("ANTHROPIC_API_KEY".into(), secret())],
        "anthropic-gateway" => {
            let mut gateway = vec![("ANTHROPIC_BASE_URL".into(), provider.base_url.clone())];
            let token = secret();
            if !token.is_empty() {
                gateway.push(("ANTHROPIC_AUTH_TOKEN".into(), token));
            }
            gateway
        }
        _ => vec![],
    };
    if !provider.model_agents.is_empty() {
        env.push(("CLAUDE_CODE_SUBAGENT_MODEL".into(), provider.model_agents.clone()));
    }
    env
}

static ACTIVE: LazyLock<RwLock<String>> = LazyLock::new(|| RwLock::new(String::new()));

/// Remembers the chain name the next turn runs with (set when the turn's command is built; an engine runs one loop).
pub fn set_active(name: &str) {
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = name.to_string();
}

/// [`environment_of`] the name set by [`set_active`].
pub fn active_environment(data_dir: &Path) -> Vec<(String, String)> {
    environment_of(&ACTIVE.read().unwrap_or_else(|e| e.into_inner()), data_dir)
}

/// [`environment`] of a `provider:<id>` name (nothing for a plain command).
pub fn environment_of(name: &str, data_dir: &Path) -> Vec<(String, String)> {
    by_ref(name).map(|p| environment(&p, data_dir)).unwrap_or_default()
}

/// The orchestrator model of a `provider:<id>` name, when the provider sets one.
pub fn orchestrator_model_of(name: &str) -> Option<String> {
    by_ref(name).map(|p| p.model_orchestrator).filter(|m| !m.is_empty())
}

/// Display name of a chain name: the provider's name, or the command as given.
pub fn display_name(name: &str) -> String {
    by_ref(name).map(|p| p.name).unwrap_or_else(|| name.to_string())
}

/// Problems that keep a provider from running (shown next to it and by the preflight): an unknown command, a missing secret, a gateway
/// without URL.
pub fn problems(provider: &Provider, data_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if crate::claude_binaries::resolve(&provider.command_or_default()).is_empty() {
        out.push(format!("`{}` was not found on this machine", provider.command_or_default()));
    }
    if provider.needs_secret() && crate::secret_store_value(data_dir, &secret_name(provider)).unwrap_or_default().is_empty() {
        out.push("no token or key saved".into());
    }
    if provider.kind == "anthropic-gateway" && !(provider.base_url.starts_with("http://") || provider.base_url.starts_with("https://")) {
        out.push("the gateway needs an http(s) base URL".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn providers_are_read_in_order_and_cleaned() {
        let c = config(json!({"providers": [
            {"id": "Main!", "kind": "claude-subscription", "models": {"orchestrator": "opus"}},
            {"id": "api", "name": "Pay per use", "kind": "anthropic-api-key", "enabled": false},
            {"id": "gw", "kind": "anthropic-gateway", "base_url": "http://localhost:4000", "models": {"agents": "local-coder"}},
            {"id": "x", "kind": "nope"},
            {"id": "api", "kind": "custom-cli"}
        ]}));
        let list = read(&c);
        assert_eq!(list.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["main", "api", "gw"]);
        assert_eq!((list[0].name.as_str(), list[0].model_orchestrator.as_str(), list[1].enabled), ("main", "opus", false));
        assert_eq!(chain(&c), Some(("provider:main".to_string(), vec!["provider:gw".to_string()])));
        assert_eq!(secret_name(&list[2]), "PROVIDER_GW_SECRET");
    }

    #[test]
    fn old_keys_become_providers_and_drive_no_chain() {
        let c = config(json!({"claude_binary": "", "claude_fallback_binaries": ["claude-backup"]}));
        let list = read(&c);
        assert_eq!(list.iter().map(|p| (p.id.as_str(), p.kind.as_str())).collect::<Vec<_>>(), [("main", "claude-subscription"), ("cli-claude-backup", "custom-cli")]);
        assert_eq!(chain(&c), None);
    }

    #[test]
    fn environment_per_kind() {
        let dir = tempfile::tempdir().unwrap();
        let p = |kind: &str| Provider { id: "t".into(), kind: kind.into(), base_url: "http://gw".into(), ..Provider::default() };
        assert_eq!(environment(&p("claude-subscription"), dir.path())[0], ("ANTHROPIC_API_KEY".to_string(), String::new()));
        assert!(environment(&p("anthropic-gateway"), dir.path()).contains(&("ANTHROPIC_BASE_URL".to_string(), "http://gw".to_string())));
        let mut agents = p("custom-cli");
        agents.model_agents = "sonnet".into();
        assert_eq!(environment(&agents, dir.path()), vec![("CLAUDE_CODE_SUBAGENT_MODEL".to_string(), "sonnet".to_string())]);
        assert!(problems(&p("anthropic-api-key"), dir.path()).iter().any(|m| m.contains("no token")));
        let mut bad = p("anthropic-gateway");
        bad.base_url = "localhost".into();
        assert!(problems(&bad, dir.path()).iter().any(|m| m.contains("base URL")));
    }

    #[test]
    fn references_resolve_after_install() {
        install(&config(json!({"providers": [{"id": "zz-test-ref", "name": "Z", "kind": "custom-cli", "command": "agy", "args": ["--dangerously-skip-permissions"]}]})));
        let found = by_ref("provider:zz-test-ref").unwrap();
        assert_eq!((found.command_or_default(), found.args.clone()), ("agy".to_string(), vec!["--dangerously-skip-permissions".to_string()]));
        assert_eq!(display_name("provider:zz-test-ref"), "Z");
        assert_eq!(display_name("claude"), "claude");
        assert!(by_ref("claude").is_none());
    }
}
