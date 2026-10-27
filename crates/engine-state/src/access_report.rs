// SPDX-License-Identifier: MIT
//! `harness doctor --access`: what a `claude -p` of this app can actually reach (a report, never a gate).
//!
//! Five parts:
//!   static  MCP servers the user allowed and that exist (`mcp_access`), the effective MCP policy (kinds, production flags, warnings) and the MCP
//!           tool patterns each guard profile may use (computed from config.json `mcp_servers` by the `mcp-policy` crate), the skills allowlist of
//!           the project (the skills bound to the Standards slots are labelled `(slot: <name>)`), the dev toolkit root, the bundle (agents, skills,
//!           skipped skills) and whether isolation is on.
//!   probe   the real isolated argv with a throw-away prompt; stdout is read until the `system/init` event (which lists tools, MCP servers, agents,
//!           skills and plugins the process loaded) and the process is killed. Anything outside the allowlists is flagged. An auth refusal still
//!           prints the init first, so the probe costs nothing in that case.
//!   guard   summary of the last lines of the project's `.specs/project/loop/internal/guard.log`.
//!   network the effective network list of the project with the origin of each entry and the denies (DIA-01: what `egress-host` and the run-time
//!           layer let a loop session reach).
//!   runtime the live self test of the run-time egress layer (RUN-06, `egress_proxy::runtime_self_test`): localhost only, about 2 s, no claude, no
//!           external traffic. A check that failed is a PROBLEM; a skipped one is not.

use crate::app_ctx::AppCtx;
use crate::{claude_binaries, egress_proxy, mcp_access, network_policy, runtime_isolation, standards};
use cfg::Env;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const PROBE_PROMPT: &str = "reply only: ok";
const PROBE_MODEL: &str = "haiku";
const PROBE_BUDGET_USD: &str = "0.05";
pub const PROBE_TIMEOUT_SECONDS: u64 = 60;
const PROBE_PROFILE: &str = "auto";
const EMPTY_MCP_CONFIG: &str = r#"{"mcpServers":{}}"#;
const GUARD_LOG_LINES: usize = 200;
const DENIAL_DECISIONS: [&str; 2] = ["blocked", "deny"];
const LAST_DENIALS: usize = 10;
const SELF_TEST_TIMEOUT_SECONDS: u64 = 30;
const MCP_PROFILES: [&str; 8] = ["orchestrator", "planning", "roadmapmap", "subprojects", "systemmap", "spec", "executor", "verifier"];
/// What Claude Code itself ships (never flagged): the agents/skills below are built into the CLI, the plugin name `builtin` is its own.
const BUILTIN_AGENTS: [&str; 7] = ["general-purpose", "explore", "plan", "statusline-setup", "claude-code-guide", "bash", "claude"];
const BUILTIN_SKILLS: [&str; 27] = [
    "update-config",
    "keybindings-help",
    "code-review",
    "simplify",
    "fewer-permission-prompts",
    "loop",
    "schedule",
    "claude-api",
    "workflow-authoring",
    "run",
    "plugin-authoring",
    "init",
    "security-review",
    "review",
    "debug",
    "batch",
    "verify",
    "compact",
    "deep-research",
    "dataviz",
    "run-skill-generator",
    "context",
    "cost",
    "help",
    "model",
    "clear",
    "usage",
];
const BUILTIN_SKILLS_MORE: [&str; 5] = ["config", "memory", "resume", "status", "doctor"];

fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => pycompat::json::py_json_dumps(other, true),
    }
}

// ---- static ------------------------------------------------------------------------------------------------------------------------------

/// `<plugin root>/bin/guard.exe`.
pub fn guard_path(plugin_root_path: &str) -> String {
    if plugin_root_path.is_empty() {
        String::new()
    } else {
        Path::new(plugin_root_path).join("bin").join("guard.exe").display().to_string()
    }
}

/// `{profile: sorted tool patterns the guard's `mcp-allowlist` lets through}`, from config.json; `{}` when there is no guard.
fn mcp_allow_by_profile(env: &Env, guard: &str) -> Value {
    if guard.is_empty() {
        return json!({});
    }
    let policy = mcp_access::read_policy(env, None);
    Value::Object(MCP_PROFILES.iter().map(|profile| (profile.to_string(), json!(mcp_policy::allowed_patterns(&policy, profile)))).collect())
}

/// `{plain skill name: "slot1, slot2"}` for the skills bound to the Standards slots of the project (root and subprojects).
fn slot_skill_labels(project_root: &str) -> BTreeMap<String, String> {
    let mut slots: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (_, bindings) in standards::project_bindings(project_root) {
        for (slot, value) in bindings {
            let name = standards::skill_name(&value);
            if !name.is_empty() {
                let list = slots.entry(name).or_default();
                if !list.contains(&slot) {
                    list.push(slot);
                }
            }
        }
    }
    slots.into_iter().map(|(name, names)| (name, names.join(", "))).collect()
}

fn label_skill(name: &str, labels: &BTreeMap<String, String>) -> String {
    match labels.get(name) {
        Some(label) => format!("{name} (slot: {label})"),
        None => name.to_string(),
    }
}

pub fn build_static(env: &Env, ctx: &AppCtx, project_root: &str, config: &Map<String, Value>) -> Value {
    let root = ctx.plugin_root_text();
    let enabled = runtime_isolation::isolation_enabled(config);
    let data_dir = runtime_isolation::data_dir(env);
    let access = mcp_access::build_access(env, Some(&data_dir), None, "");
    let guard = guard_path(&root);
    let guard_exists = Path::new(&guard).is_file();
    let allowlist = runtime_isolation::skills_allowlist(Some(project_root), config);
    let mut report = Map::new();
    report.insert("project".into(), json!(project_root));
    report.insert("plugin_root".into(), json!(root));
    report.insert("toolkit_root".into(), json!(cfg::Locator::new(env.clone(), ctx.repo_root.clone()).find_toolkit_root(config).display().to_string()));
    report.insert("guard".into(), json!(if guard_exists { guard.clone() } else { String::new() }));
    report.insert("isolation".into(), json!(enabled));
    report.insert("slot_skills".into(), json!(slot_skill_labels(project_root)));
    report.insert(
        "mcp_servers".into(),
        json!(access["servers"].as_array().into_iter().flatten().map(|row| json!({"server": row["server"], "tools": row["tools"]})).collect::<Vec<_>>()),
    );
    report.insert("mcp_config_path".into(), access["config_path"].clone());
    report.insert("mcp_policy".into(), mcp_access::describe_policy(env, None, ""));
    report.insert("mcp_allow_by_profile".into(), mcp_allow_by_profile(env, if guard_exists { &guard } else { "" }));
    report.insert("skills_allowlist".into(), Value::Object(allowlist.into_iter().map(|(profile, skills)| (profile, json!(skills))).collect()));
    report.insert("bundle".into(), Value::Null);
    report.insert("allowlist_file".into(), json!(""));
    if enabled {
        let prepared = runtime_isolation::prepare(env, Some(project_root), config, &root, &ctx.toolkit_text());
        let bundle = &prepared["bundle"];
        let kept: Map<String, Value> =
            ["path", "skills", "skipped", "agents", "rebuilt", "error"].iter().filter_map(|key| bundle.get(*key).map(|v| (key.to_string(), v.clone()))).collect();
        report.insert("bundle".into(), Value::Object(kept));
        report.insert("allowlist_file".into(), prepared["allowlist_file"].clone());
    }
    Value::Object(report)
}

// ---- probe -------------------------------------------------------------------------------------------------------------------------------

/// `(argv, stdin_text, extra env)` of the isolated probe, built like `ClaudeClient::build_command` does for a job with no hook profile.
pub fn build_probe_argv(env: &Env, static_part: &Value, config: &Map<String, Value>) -> (Vec<String>, String, Vec<(String, String)>) {
    let binary = config.get("claude_binary").and_then(Value::as_str).filter(|b| !b.is_empty()).unwrap_or(claude_binaries::DEFAULT_BINARY_NAME);
    let mut argv = claude_binaries::base_argv(binary);
    argv.extend(["-p", PROBE_PROMPT, "--model", PROBE_MODEL, "--max-budget-usd", PROBE_BUDGET_USD, "--output-format", "stream-json", "--verbose"].map(String::from));
    let bundle = static_part["bundle"]["path"].as_str().unwrap_or("");
    let guard = static_part["guard"].as_str().unwrap_or("");
    argv.extend(runtime_isolation::isolation_argv(env, bundle, &runtime_isolation::guard_settings(env, guard, PROBE_PROFILE, None)));
    let mcp_config = static_part["mcp_config_path"].as_str().filter(|p| !p.is_empty()).unwrap_or(EMPTY_MCP_CONFIG);
    argv.extend(["--strict-mcp-config".to_string(), "--mcp-config".to_string(), mcp_config.to_string()]);
    let (argv, stdin_text) = claude_binaries::prompt_through_stdin(&argv, "");
    let allowlist = static_part["allowlist_file"].as_str().unwrap_or("");
    (argv, stdin_text, runtime_isolation::isolation_env(Path::new(allowlist)))
}

pub fn parse_init_line(line: &str) -> Option<Value> {
    let event: Value = serde_json::from_str(line).ok()?;
    (event.get("type") == Some(&json!("system")) && event.get("subtype") == Some(&json!("init"))).then_some(event)
}

/// Starts the process, reads stdout lines until the `system/init` event, kills it. Returns `(init event or None, error text)`.
pub fn read_until_init(argv: &[String], stdin_text: &str, extra_env: &[(String, String)], cwd: &str, timeout_seconds: u64) -> (Option<Value>, String) {
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).envs(extra_env.iter().cloned());
    if !cwd.is_empty() && Path::new(cwd).is_dir() {
        command.current_dir(cwd);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(failure) => return (None, format!("could not start: {failure}")),
    };
    let (sender, receiver) = mpsc::channel::<Option<Value>>();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(event) = parse_init_line(&line) {
                    let _ = sender.send(Some(event));
                    return;
                }
            }
            let _ = sender.send(None);
        });
    }
    if let Some(mut stdin) = child.stdin.take() {
        if !stdin_text.is_empty() {
            let _ = stdin.write_all(stdin_text.as_bytes());
        }
    }
    let outcome = receiver.recv_timeout(Duration::from_secs(timeout_seconds));
    let _ = child.kill();
    let _ = child.wait();
    match outcome {
        Ok(Some(init)) => (Some(init), String::new()),
        Ok(None) => (None, "the process ended without an init event".into()),
        Err(_) => (None, format!("timeout after {timeout_seconds} s without an init event")),
    }
}

fn short_name(value: &Value) -> String {
    let named = match value {
        Value::Object(map) => map.get("name").filter(|n| crate::notify_truthy(Some(n))).or_else(|| map.get("id")).cloned().unwrap_or(Value::Null),
        other => other.clone(),
    };
    text(&named).rsplit(':').next().unwrap_or("").trim().to_string()
}

fn is_builtin_plugin(entry: &Value) -> bool {
    let shown = match entry {
        Value::Object(_) | Value::Array(_) => pycompat::json::py_json_dumps(entry, true).to_lowercase(),
        other => text(other).to_lowercase(),
    };
    shown.contains("builtin") || shown.contains("built-in")
}

/// Summary of an init event plus the flags (anything outside the allowlists).
pub fn analyze_init(init: &Value, static_part: &Value) -> Value {
    let list = |key: &str| init.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
    let tools: Vec<String> = list("tools").iter().map(text).collect();
    let servers: Vec<String> = list("mcp_servers").iter().map(short_name).collect();
    let agents: Vec<String> = list("agents").iter().map(text).collect();
    let skills: Vec<String> = list("skills").iter().map(text).collect();
    let plugins = list("plugins");
    let bundle = &static_part["bundle"];
    let allowed_servers: Vec<&str> = static_part["mcp_servers"].as_array().into_iter().flatten().filter_map(|r| r["server"].as_str()).collect();
    let allowed_skills: Vec<String> = static_part["skills_allowlist"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .flat_map(|v| v.as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .map(str::to_lowercase)
        .collect();
    let bundle_agents: Vec<String> = bundle["agents"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_lowercase).collect();
    let mut flags: Vec<String> = Vec::new();
    for name in &servers {
        if !allowed_servers.contains(&name.as_str()) {
            flags.push(format!("mcp server outside the generated config: {name}"));
        }
    }
    for name in &agents {
        let plain = short_name(&json!(name)).to_lowercase();
        if !bundle_agents.contains(&plain) && !BUILTIN_AGENTS.contains(&plain.as_str()) {
            flags.push(format!("agent outside the bundle: {name}"));
        }
    }
    for name in &skills {
        let plain = short_name(&json!(name)).to_lowercase();
        if !allowed_skills.contains(&plain) && !BUILTIN_SKILLS.contains(&plain.as_str()) && !BUILTIN_SKILLS_MORE.contains(&plain.as_str()) {
            flags.push(format!("skill outside the allowlist: {name}"));
        }
    }
    for entry in &plugins {
        if !is_builtin_plugin(entry) {
            flags.push(format!("plugin that is not builtin: {}", short_name(entry)));
        }
    }
    let mut mcp_tools: Vec<&String> = tools.iter().filter(|t| t.starts_with("mcp__")).collect();
    mcp_tools.sort();
    json!({
        "model": init.get("model").cloned().unwrap_or(Value::Null), "tool_count": tools.len(), "mcp_tools": mcp_tools,
        "mcp_servers": init.get("mcp_servers").filter(|v| crate::notify_truthy(Some(v))).cloned().unwrap_or(json!([])), "agents": agents, "skills": skills,
        "plugins": plugins.iter().filter(|e| !is_builtin_plugin(e)).collect::<Vec<_>>(), "flags": flags,
    })
}

pub fn run_probe(env: &Env, static_part: &Value, config: &Map<String, Value>, project_root: &str, timeout_seconds: u64) -> Value {
    let (argv, stdin_text, extra_env) = build_probe_argv(env, static_part, config);
    match read_until_init(&argv, &stdin_text, &extra_env, project_root, timeout_seconds) {
        (Some(init), _) => {
            let mut analysis = analyze_init(&init, static_part);
            analysis["ok"] = json!(true);
            analysis["error"] = json!("");
            analysis
        }
        (None, error) => json!({"ok": false, "error": error, "flags": []}),
    }
}

// ---- guard.log ---------------------------------------------------------------------------------------------------------------------------

fn read_guard_log(project_root: &str, limit: usize) -> (String, Vec<Map<String, Value>>) {
    let candidates = [
        Path::new(project_root).join(".specs").join("project").join("loop").join("internal").join("guard.log"),
        Path::new(project_root).join(".specs").join("project").join("guard.log"),
    ];
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { return (path.display().to_string(), vec![]) };
        let content = String::from_utf8_lossy(&bytes).into_owned();
        let lines = pycompat::splitlines(&content);
        let tail = &lines[lines.len().saturating_sub(limit)..];
        let entries = tail.iter().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter_map(|v| v.as_object().cloned()).collect();
        return (path.display().to_string(), entries);
    }
    (String::new(), vec![])
}

pub fn summarize_guard_log(project_root: &str, limit: usize) -> Value {
    let (path, entries) = read_guard_log(project_root, limit);
    let mut counts: [BTreeMap<String, i64>; 3] = Default::default();
    for entry in &entries {
        for (index, key) in ["decision", "rule", "agent"].iter().enumerate() {
            let value = entry.get(*key).filter(|v| crate::notify_truthy(Some(v))).map(text).unwrap_or_else(|| "-".into());
            *counts[index].entry(value).or_insert(0) += 1;
        }
    }
    let denials: Vec<Value> = entries
        .iter()
        .filter(|e| e.get("decision").and_then(Value::as_str).is_some_and(|d| DENIAL_DECISIONS.contains(&d)))
        .map(|e| Value::Object(["t", "profile", "rule", "agent", "detail"].iter().map(|k| (k.to_string(), e.get(*k).cloned().unwrap_or(json!("")))).collect()))
        .collect();
    let last: Vec<&Value> = denials.iter().skip(denials.len().saturating_sub(LAST_DENIALS)).collect();
    json!({"path": path, "lines": entries.len(), "by_decision": counts[0], "by_rule": counts[1], "by_agent": counts[2], "last_denials": last})
}

// ---- network list (DIA-01) ---------------------------------------------------------------------------------------------------------------

/// The effective network list of the project: `{allow: [{origin, entry, inactive}], deny, notes, broken, env_names}`. `inactive` names the variable that
/// switches an `if-missing=` entry off (it is defined in the project's `.env.test`). Names only, never a value.
pub fn build_network(env: &Env, project_root: &str) -> Value {
    let effective = network_policy::load(project_root, &runtime_isolation::data_dir(env), None);
    let rows: Vec<Value> = effective
        .allow
        .iter()
        .map(|entry| {
            let inactive = if !entry.if_missing.is_empty() && effective.env_names.contains(&entry.if_missing) { entry.if_missing.clone() } else { String::new() };
            json!({"origin": entry.origin, "entry": network_policy::format_entry(entry), "inactive": inactive})
        })
        .collect();
    json!({"allow": rows, "deny": effective.deny, "notes": effective.notes, "broken": effective.broken, "env_names": effective.env_names})
}

// ---- report ------------------------------------------------------------------------------------------------------------------------------

pub fn run_self_test(env: &Env, timeout_seconds: u64) -> Value {
    egress_proxy::runtime_self_test(env, None, timeout_seconds)
}

pub fn build_report(env: &Env, ctx: &AppCtx, project_root: &str, probe: bool, selftest: bool, timeout_seconds: u64) -> Value {
    let project_root = pycompat::nt_abspath(if project_root.is_empty() { "." } else { project_root });
    let config = runtime_isolation::read_config(env);
    let static_part = build_static(env, ctx, &project_root, &config);
    let probe_result = if probe && static_part["isolation"] == true { run_probe(env, &static_part, &config, &project_root, timeout_seconds) } else { Value::Null };
    json!({
        "static": static_part, "probe": probe_result, "guard_log": summarize_guard_log(&project_root, GUARD_LOG_LINES), "network": build_network(env, &project_root),
        "runtime_self_test": if selftest { run_self_test(env, SELF_TEST_TIMEOUT_SECONDS) } else { Value::Null },
    })
}

fn format_counts(counts: &Value) -> String {
    let mut pairs: Vec<(String, i64)> = counts.as_object().into_iter().flatten().map(|(k, v)| (k.clone(), v.as_i64().unwrap_or(0))).collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if pairs.is_empty() {
        "(none)".into()
    } else {
        pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ")
    }
}

fn format_network(network: &Value) -> Vec<String> {
    if network.is_null() {
        return vec![];
    }
    let mut out = vec![String::new(), "-- network list of a loop session (egress-host + run-time layer; an interactive session is not limited)".to_string()];
    for row in network["allow"].as_array().into_iter().flatten() {
        let inactive = text(&row["inactive"]);
        out.push(format!(
            "  {:<8} {}{}",
            text(&row["origin"]),
            text(&row["entry"]),
            if inactive.is_empty() { String::new() } else { format!("  [inactive: {inactive} is defined in .env.test]") }
        ));
    }
    out.push("  deny (config.json egress_never: any port, path and method; beats every allow):".into());
    let deny: Vec<String> = network["deny"].as_array().into_iter().flatten().map(|p| format!("    {}", text(p))).collect();
    if deny.is_empty() {
        out.push("    (none)".into())
    } else {
        out.extend(deny)
    }
    if crate::notify_truthy(network.get("broken")) {
        out.push(format!("  PROBLEM: {} is unusable: every non-local request fails closed", text(&network["broken"])));
    }
    out.extend(network["notes"].as_array().into_iter().flatten().map(|n| format!("  note: {}", text(n))));
    let names: Vec<String> = network["env_names"].as_array().into_iter().flatten().map(text).collect();
    out.push(format!("  .env.test names (never values): {}", if names.is_empty() { "(none)".to_string() } else { names.join(", ") }));
    out
}

fn format_self_test(result: &Value) -> Vec<String> {
    let mut out = vec![String::new(), "-- run-time egress layer self test (local proxy + Python and Node preloads, localhost only, no external traffic)".to_string()];
    if result.is_null() {
        out.push("  skipped (--no-selftest)".into());
        return out;
    }
    for check in result["checks"].as_array().into_iter().flatten() {
        out.push(format!("  [{}] {:<14} {}", if check["ok"] == true { "ok " } else { "NO " }, text(&check["id"]), text(&check["detail"])));
    }
    for item in result["skipped"].as_array().into_iter().flatten() {
        out.push(format!("  [skip] {:<14} {}", text(&item["id"]), text(&item["reason"])));
    }
    out.push(if result["ok"] == true {
        "  run-time layer works".into()
    } else {
        "  PROBLEM: the run-time layer self test failed: a loop would not be confined as the network list says".into()
    });
    out
}

fn join_names(value: &Value, labels: &BTreeMap<String, String>) -> String {
    value.as_array().into_iter().flatten().map(|n| label_skill(&text(n), labels)).collect::<Vec<_>>().join(", ")
}

pub fn format_report(report: &Value) -> String {
    let static_part = &report["static"];
    let (probe, guard) = (&report["probe"], &report["guard_log"]);
    let labels: BTreeMap<String, String> = static_part["slot_skills"].as_object().into_iter().flatten().map(|(k, v)| (k.clone(), text(v))).collect();
    let or_none = |value: &Value, none: &str| if text(value).is_empty() { none.to_string() } else { text(value) };
    let mut out: Vec<String> = vec![
        format!("== access report: {}", text(&static_part["project"])),
        format!("isolation ........ {}", if static_part["isolation"] == true { "on" } else { "OFF (loop_isolation=false: today's argv, nothing below is enforced)" }),
        format!("plugin root ...... {}", or_none(&static_part["plugin_root"], "(not found)")),
        format!("toolkit root ..... {}", or_none(&static_part["toolkit_root"], "(not found: /plugin install claude-craft-kit@claude-craft-kit)")),
        format!("guard ............ {}", or_none(&static_part["guard"], "(not found)")),
        String::new(),
        "-- MCP servers allowed for loops and present on this machine".into(),
    ];
    let servers = static_part["mcp_servers"].as_array().cloned().unwrap_or_default();
    for row in &servers {
        let tools: Vec<String> = row["tools"].as_array().into_iter().flatten().map(|n| text(n).rsplit("__").next().unwrap_or("").to_string()).collect();
        out.push(format!("  {}: {}", text(&row["server"]), tools.join(", ")));
    }
    if servers.is_empty() {
        out.push("  (none: a loop cannot use any MCP server until you allow one in Settings > MCP servers)".into());
    }
    let policy = &static_part["mcp_policy"];
    for row in policy["servers"].as_array().into_iter().flatten() {
        out.push(format!(
            "  policy: {} ({}{}{}){}{}",
            text(&row["server"]),
            if row["kind"] == "sql" { "database" } else { "other" },
            if row["production"] == true { ", production" } else { "" },
            if row["read_only"] == true { ", read-only" } else { "" },
            if row["allow"] == true { "" } else { " [not allowed]" },
            if row["found"] == true { "" } else { " [not found]" }
        ));
    }
    out.extend(policy["warnings"].as_array().into_iter().flatten().map(|t| format!("  WARNING: {}", text(t))));
    out.push("-- MCP tool patterns allowed by profile (guard mcp-allowlist, from config.json)".into());
    let by_profile = static_part["mcp_allow_by_profile"].as_object().cloned().unwrap_or_default();
    for (profile, tools) in &by_profile {
        let names: Vec<String> = tools.as_array().into_iter().flatten().map(text).collect();
        out.push(format!("  {:<13} {}", profile, if names.is_empty() { "none".to_string() } else { names.join(", ") }));
    }
    if by_profile.is_empty() {
        out.push("  (guard not loaded)".into());
    }
    out.push("-- skills allowlist by profile".into());
    let mut profiles: Vec<(&String, &Value)> = static_part["skills_allowlist"].as_object().into_iter().flatten().collect();
    profiles.sort_by(|a, b| a.0.cmp(b.0));
    for (profile, skills) in profiles {
        out.push(format!("  {:<13} {}", profile, join_names(skills, &labels)));
    }
    let bundle = &static_part["bundle"];
    if !bundle.is_null() {
        out.push(format!("-- bundle {}{}", text(&bundle["path"]), if bundle["rebuilt"] == true { " (rebuilt)" } else { "" }));
        let plain = |value: &Value| value.as_array().into_iter().flatten().map(text).collect::<Vec<_>>().join(", ");
        let or_none_list = |s: String| if s.is_empty() { "(none)".to_string() } else { s };
        out.push(format!("  agents:  {}", or_none_list(plain(&bundle["agents"]))));
        out.push(format!("  skills:  {}", or_none_list(join_names(&bundle["skills"], &labels))));
        out.push(format!("  skipped: {}", or_none_list(plain(&bundle["skipped"]))));
        if crate::notify_truthy(bundle.get("error")) {
            out.push(format!("  ERROR:   {}", text(&bundle["error"])));
        }
        out.push(format!("  allowlist file: {}", text(&static_part["allowlist_file"])));
    }
    out.push(String::new());
    out.push("-- probe (real isolated argv, killed at the init event)".into());
    if probe.is_null() {
        out.push("  skipped (--no-probe or isolation off)".into());
    } else if probe["ok"] != true {
        out.push(format!("  FAILED: {}", text(&probe["error"])));
    } else {
        let joined = |value: &Value, none: &str| {
            let items: Vec<String> = value.as_array().into_iter().flatten().map(text).collect();
            if items.is_empty() {
                none.to_string()
            } else {
                items.join(", ")
            }
        };
        let servers: Vec<String> = probe["mcp_servers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|item| {
                format!(
                    "{}({})",
                    short_name(item),
                    if item.is_object() { item.get("status").map(text).filter(|s| !s.is_empty()).unwrap_or_else(|| "?".into()) } else { "?".into() }
                )
            })
            .collect();
        let plugins: Vec<String> = probe["plugins"].as_array().into_iter().flatten().map(short_name).collect();
        out.push(format!("  model:       {}", text(&probe["model"])));
        out.push(format!("  tools:       {}", probe["tool_count"]));
        out.push(format!("  mcp tools:   {}", joined(&probe["mcp_tools"], "(none)")));
        out.push(format!("  mcp servers: {}", if servers.is_empty() { "(none)".to_string() } else { servers.join(", ") }));
        out.push(format!("  agents:      {}", joined(&probe["agents"], "(none)")));
        out.push(format!("  skills:      {}", joined(&probe["skills"], "(none)")));
        out.push(format!("  plugins:     {}", if plugins.is_empty() { "(none besides builtin)".to_string() } else { plugins.join(", ") }));
        let flags: Vec<String> = probe["flags"].as_array().into_iter().flatten().map(text).collect();
        out.extend(flags.iter().map(|f| format!("  FLAG: {f}")));
        out.push(if flags.is_empty() { "  no flags: everything loaded is inside the allowlists".into() } else { format!("  {} flag(s)", flags.len()) });
    }
    out.extend(format_network(&report["network"]));
    out.extend(format_self_test(&report["runtime_self_test"]));
    out.push(String::new());
    out.push(format!("-- guard.log (last {GUARD_LOG_LINES} lines) {}", or_none(&guard["path"], "(no log)")));
    if guard["lines"].as_i64().unwrap_or(0) > 0 {
        out.push(format!("  lines: {}", guard["lines"]));
        out.push(format!("  by decision: {}", format_counts(&guard["by_decision"])));
        out.push(format!("  by rule:     {}", format_counts(&guard["by_rule"])));
        out.push(format!("  by agent:    {}", format_counts(&guard["by_agent"])));
        out.push("  last denials:".into());
        let denials = guard["last_denials"].as_array().cloned().unwrap_or_default();
        out.extend(denials.iter().map(|d| format!("    {} [{}/{}] {}", text(&d["t"]), text(&d["profile"]), text(&d["rule"]), text(&d["detail"]))));
        if denials.is_empty() {
            out.push("    (none)".into());
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_init_event_is_found_and_checked_against_the_allowlists() {
        assert!(parse_init_line(r#"{"type":"system","subtype":"init","model":"m"}"#).is_some());
        assert!(parse_init_line(r#"{"type":"assistant"}"#).is_none() && parse_init_line("not json").is_none());
        let static_part = json!({"bundle": {"agents": ["spec-writer"]}, "mcp_servers": [{"server": "grafana"}], "skills_allowlist": {"spec": ["tlc-spec-driven"]}});
        let init = json!({
            "model": "haiku", "tools": ["Read", "mcp__grafana__query", "mcp__other__x"], "mcp_servers": [{"name": "grafana"}, {"name": "other"}],
            "agents": ["spec-writer", "general-purpose", "stray"], "skills": ["tlc-spec-driven", "plugin:extra", "help"], "plugins": ["builtin-core", {"name": "evil"}],
        });
        let analysis = analyze_init(&init, &static_part);
        let flags: Vec<&str> = analysis["flags"].as_array().unwrap().iter().map(|f| f.as_str().unwrap()).collect();
        assert_eq!(
            flags,
            [
                "mcp server outside the generated config: other",
                "agent outside the bundle: stray",
                "skill outside the allowlist: plugin:extra",
                "plugin that is not builtin: evil"
            ]
        );
        assert_eq!((analysis["tool_count"].clone(), analysis["mcp_tools"].clone()), (json!(3), json!(["mcp__grafana__query", "mcp__other__x"])));
    }

    #[test]
    fn the_guard_log_is_summarized() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join(".specs/project/loop/internal");
        std::fs::create_dir_all(&log).unwrap();
        std::fs::write(
            log.join("guard.log"),
            "{\"decision\": \"blocked\", \"rule\": \"git-write\", \"agent\": \"a\", \"t\": \"1\", \"profile\": \"p\", \"detail\": \"d\"}\nbroken\n{\"decision\": \"allowed\", \"rule\": \"x\"}\n",
        )
        .unwrap();
        let summary = summarize_guard_log(&dir.path().display().to_string(), 200);
        assert_eq!(
            (summary["lines"].clone(), summary["by_decision"].clone(), summary["last_denials"][0]["rule"].clone()),
            (json!(2), json!({"allowed": 1, "blocked": 1}), json!("git-write"))
        );
        assert_eq!(format_counts(&summary["by_rule"]), "git-write=1, x=1");
    }
}
