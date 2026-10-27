// SPDX-License-Identifier: MIT
//! `main()` of the guard as a library function: payload in, exit code / stdout / stderr out.

use crate::check::{is_headless, isolated_or_headless, normalize_skill_name as _n, skill_call_name, unique, Check, Hits};
use crate::net;
use crate::paths::G;
use crate::rules::*;
use crate::scope::{executor_slug, has_rule, load_scope, note_identity_gap};
use crate::state::{alert, describe_attempt, find_specs_dir, load_permissions, log, loop_file, py_json_dumps, truthy, value_text};
use serde_json::{json, Value};

#[derive(Debug, Default, Clone)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// `profile_for_agent`: the profile of the calling agent, or `None` (main session / other agents).
fn profile_for_agent(payload: &Value) -> Option<&'static str> {
    let raw = payload.get("agent_type").filter(|v| truthy(v)).map(value_text).unwrap_or_default();
    agent_profile(raw.rsplit(':').next().unwrap_or("").trim())
}

fn audit_allowed(check: &Check, specs_dir: Option<&str>, profile: &str, payload: &Value) {
    let g = check.g;
    let tool = payload.get("tool_name").filter(|v| truthy(v)).map(value_text).unwrap_or_default();
    let input = payload.get("tool_input").cloned().filter(truthy).unwrap_or(Value::Object(Default::default()));
    let (rule, subject);
    if tool == "Skill" {
        if g.env.text("LOOP_SKILLS_FILE").is_empty() {
            return;
        }
        rule = "skill-allowlist";
        subject = skill_call_name(&input);
    } else if tool.to_lowercase().starts_with("mcp__") {
        if !isolated_or_headless(g) {
            return;
        }
        rule = "mcp-allowlist";
        let mut s = String::new();
        if mcp_policy::is_sql_tool(&mcp_policy::read_policy(&g.env.user_data_dir()), &tool) && input.is_object() {
            let text = ["query", "sql", "statement"].iter().find_map(|k| input.get(*k).and_then(Value::as_str)).unwrap_or("");
            let statements = crate::sql::sql_statements(text);
            let first = statements.first().cloned().unwrap_or_default();
            let words: Vec<String> = crate::pyre::Re::new(r"[a-z_]+").find_all(&first).into_iter().map(|m| m.text().to_string()).collect();
            s = words.first().map(|w| w.to_uppercase()).unwrap_or_default();
        }
        subject = s;
    } else {
        return;
    }
    let agent_raw = payload.get("agent_type").filter(|v| truthy(v)).map(value_text).unwrap_or_default();
    let agent = agent_raw.rsplit(':').next().unwrap_or("").trim().to_string();
    log(
        g,
        specs_dir,
        profile,
        rule,
        "allow",
        &format!("{tool} {subject}"),
        None,
        Some(&[("tool", Value::String(tool.clone())), ("agent", Value::String(agent)), ("subject", Value::String(subject.clone()))]),
    );
}

fn emit_runtime_input(g: &G, payload: &Value, specs_dir: Option<&str>, profile: &str) -> String {
    let tool = payload.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let Some(specs) = specs_dir.filter(|s| !s.is_empty()) else {
        return String::new();
    };
    if !isolated_or_headless(g) || (tool != "Bash" && tool != "PowerShell") {
        return String::new();
    }
    let Some(settings) = net::runtime_settings(specs, &g.env.text("LOOP_APP_HOME")) else {
        return String::new();
    };
    let input = payload.get("tool_input").cloned().filter(truthy).unwrap_or(Value::Object(Default::default()));
    let Some(updated) = net::runtime_input(tool, &input, &settings) else {
        return String::new();
    };
    if settings.degraded {
        log(g, Some(specs), profile, "runtime-layer", "warn", "no network-runtime.json: proxy variables point at a closed port (HTTP clients fail closed)", None, None);
    }
    py_json_dumps(&json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": updated}}), true)
}

/// Runs the guard for one hook payload. `profile_arg` is the value of `--profile` (default `executor`).
pub fn run(g: &G, profile_arg: &str, stdin_text: &str) -> Outcome {
    let mut profile = profile_arg.to_string();
    let payload: Value = match serde_json::from_str(stdin_text) {
        Ok(v) => v,
        Err(_) => {
            alert(g, None, &profile, "hook payload unreadable (JSONDecodeError)");
            return Outcome::default();
        }
    };
    if !payload.is_object() {
        return Outcome { code: 1, stdout: String::new(), stderr: "AttributeError: the hook payload is not an object\n".into() };
    }
    if profile == "auto" {
        match profile_for_agent(&payload) {
            Some(p) => profile = p.to_string(),
            None => {
                let has_agent = payload.get("agent_type").filter(|v| truthy(v)).map(|v| !value_text(v).trim().is_empty()).unwrap_or(false);
                if !is_headless(g) && !(g.env.get("LOOP_ISOLATED").as_deref() == Some("1") && !has_agent) {
                    return Outcome::default();
                }
                profile = if !has_agent && !is_headless(g) {
                    "planning".into()
                } else if has_agent {
                    "executor".into()
                } else {
                    "orchestrator".into()
                };
            }
        }
    }
    let mut rules = profile_rules(&profile);
    if profile == "planning" {
        rules.push("network-suggest".into());
    }
    for rule in net::RULES {
        if !has_rule(&rules, rule) {
            rules.push(rule.to_string());
        }
    }
    if is_headless(g) {
        for rule in HEADLESS_RULES {
            if !has_rule(&rules, rule) {
                rules.push(rule.to_string());
            }
        }
    }
    let cwd_value = payload.get("cwd").filter(|v| truthy(v)).map(value_text);
    let specs_dir = find_specs_dir(g, cwd_value.as_deref());
    let mut stderr = String::new();

    let (mut permissions, warning) = load_permissions(specs_dir.as_deref());
    stderr.push_str(&warning);
    permissions.specs_dir = specs_dir.clone();
    permissions.cwd = cwd_value.clone();
    let tool_name = payload.get("tool_name").filter(|v| truthy(v)).map(value_text).unwrap_or_default();
    let input = payload.get("tool_input").cloned().filter(truthy).unwrap_or(Value::Object(Default::default()));
    if let Some(specs) = specs_dir.as_deref().filter(|s| !s.is_empty()) {
        if has_rule(&rules, "path-scope") {
            permissions.slug = executor_slug(&payload, false);
            permissions.task = executor_slug(&payload, true);
            let writes = ["Write", "Edit", "MultiEdit"].contains(&tool_name.as_str());
            if writes && !load_scope(specs, None, None).is_empty() {
                let mut keys: Vec<String> = payload.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
                keys.sort();
                log(
                    g,
                    Some(specs),
                    &profile,
                    "scope-trace",
                    "info",
                    &format!("slug={} fields={}", permissions.slug.clone().unwrap_or_else(|| "None".into()), keys.join(",")),
                    None,
                    None,
                );
            }
            if permissions.slug.is_none() && writes {
                let features = std::fs::read(loop_file(specs, "SCOPE.json"))
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .and_then(|v| v.get("features").and_then(Value::as_object).map(|m| m.len()))
                    .unwrap_or(0);
                if features > 1 {
                    note_identity_gap(g, specs, &profile, &payload);
                }
            }
        }
    }
    let allowed = permissions.allow.clone();
    let mut check = Check::new(g, permissions);
    let hits_result = check.check_tool(&tool_name, &input, &rules, &mut stderr);
    let mut hits: Hits = match hits_result {
        Ok(h) => unique(h),
        Err(message) => {
            alert(g, specs_dir.as_deref(), &profile, &message);
            return Outcome { code: 0, stdout: String::new(), stderr };
        }
    };
    let reason = check.mcp_allowlist_reason(&tool_name, &input, &profile);
    if !reason.is_empty() {
        hits.push(("mcp-allowlist".into(), reason));
    }
    let reason = check.skill_allowlist_reason(&tool_name, &input, &profile);
    if !reason.is_empty() {
        hits.push(("skill-allowlist".into(), reason));
    }
    let is_net_rule = |r: &str| net::RULES.contains(&r);
    let blocked: Hits = hits.iter().filter(|(r, _)| HARD.contains(&r.as_str()) || is_net_rule(r) || !allowed.contains(r)).cloned().collect();
    for (rule, why) in &hits {
        if allowed.contains(rule) && !HARD.contains(&rule.as_str()) {
            log(g, specs_dir.as_deref(), &profile, rule, "allowed-by-user", why, None, None);
        }
    }
    if blocked.is_empty() {
        audit_allowed(&check, specs_dir.as_deref(), &profile, &payload);
        check.log_egress(specs_dir.as_deref(), &profile);
        let stdout = emit_runtime_input(g, &payload, specs_dir.as_deref(), &profile);
        return Outcome { code: 0, stdout, stderr };
    }
    let attempt = describe_attempt(&payload);
    for (rule, why) in &blocked {
        log(g, specs_dir.as_deref(), &profile, rule, "blocked", why, Some(&attempt), None);
    }
    let mut names: Vec<&str> = blocked.iter().map(|(r, _)| r.as_str()).collect();
    names.sort();
    names.dedup();
    stderr.push_str(&format!(
        "BLOCKED by the orchestrator guard [{}]: {}. Do not try to work around it. Return STATUS: NEEDS_CONFIRM with the reason; the orchestrator takes it to the user. This block is final: do not retry this command or a variant of it. Continue without it, or report it in your final return.\n",
        names.join(", "),
        blocked.iter().map(|(_, w)| w.as_str()).collect::<Vec<_>>().join("; ")
    ));
    Outcome { code: 2, stdout: String::new(), stderr }
}

#[allow(dead_code)]
fn _unused(_: &str) -> String {
    _n("")
}
