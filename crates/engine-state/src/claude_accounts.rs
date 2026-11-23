// SPDX-License-Identifier: MIT
//! Which `claude` command and account a turn runs on: fallback order, unusable accounts, account labels and usage windows kept in meta.json.
//!
//! The two functions that send a notice (`notify_claude_change`, `alert_main_weekly_limit`) live in the run loop, next to its `notify_wait`.

use crate::loop_io::{iso_seconds, local_now};
use crate::loop_opts::LoopOpts;
use crate::{claude_binaries, cli_output, usage_windows};
use cfg::env::app_env_list;
use cfg::Env;
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;
use time::PrimitiveDateTime;

/// Channels that never leave this machine.
pub const LOCAL_NOTICE_CHANNELS: [&str; 3] = ["console", "toast", "file"];
const UNUSABLE_REASON_LIMIT: usize = 300;

static ACCOUNT_EXTRA: LazyLock<Re> = LazyLock::new(|| Re::new(r"\s*\(([^)]*)\)\s*\z"));

/// Commands of the turns, in fallback order: `--claude` (default `claude`), then each `--claude-fallback`.
pub fn claude_names(opts: &LoopOpts) -> Vec<String> {
    claude_binaries::ordered(&opts.claude, &opts.claude_fallback)
}

/// Re-reads the fallback list from config.json before a turn, so a change made while the loop runs reaches the next turn. Only a list
/// that is present in the file replaces the one from the command line. Returns true when the list changed.
pub fn refresh_fallbacks(opts: &mut LoopOpts, env: &Env) -> bool {
    let config = cfg::locator::read_user_config(&env.user_data_dir());
    let Some(value) = config.get("claude_fallback_binaries") else { return false };
    let fresh = claude_binaries::read_fallbacks(value);
    if fresh == opts.claude_fallback {
        return false;
    }
    opts.claude_fallback = fresh;
    opts.claude_index = opts.claude_index.min(claude_names(opts).len() - 1);
    true
}

pub fn current_claude(opts: &LoopOpts) -> String {
    let names = claude_names(opts);
    names[opts.claude_index.min(names.len() - 1)].clone()
}

/// Commands whose account turned out unusable in this run (they are never picked again in it).
pub fn unusable_names(opts: &LoopOpts) -> Vec<String> {
    opts.unusable_commands.iter().filter_map(|i| i["command"].as_str().map(String::from)).collect()
}

/// Position of the first command that is not unusable (0 = the main command); 0 when every one is.
pub fn first_usable_index(opts: &LoopOpts) -> usize {
    let blocked = unusable_names(opts);
    claude_names(opts).iter().position(|n| !blocked.contains(n)).unwrap_or(0)
}

/// Moves to the next fallback command that is not unusable (sticky until the next wait); false when there is none left.
pub fn switch_claude(opts: &mut LoopOpts) -> bool {
    let (names, blocked) = (claude_names(opts), unusable_names(opts));
    for index in opts.claude_index + 1..names.len() {
        if !blocked.contains(&names[index]) {
            opts.claude_index = index;
            return true;
        }
    }
    false
}

/// Remembers that the command's account cannot be used in this run and returns the record kept in meta.json
/// (`{command, account, reason, turn}`; the reason is redacted and short).
pub fn mark_account_unusable(opts: &mut LoopOpts, name: &str, account: &str, reason: &str, turn: i64) -> Value {
    let record = json!({"command": name, "account": account, "reason": cli_output::redact_text(reason, Some(UNUSABLE_REASON_LIMIT)), "turn": turn});
    opts.unusable_commands.push(record.clone());
    opts.unusable_notice = Some(record.clone());
    record
}

/// Reason of the final error when every command has an unusable account: each command, its account and why, and what to do.
pub fn unusable_stop_message(opts: &LoopOpts) -> String {
    let rows: Vec<String> = opts
        .unusable_commands
        .iter()
        .map(|item| {
            let account = item["account"].as_str().filter(|a| !a.is_empty()).unwrap_or("account unknown");
            let reason = item["reason"].as_str().filter(|a| !a.is_empty()).unwrap_or("no reason in the output");
            format!("`{}` ({account}): {reason}", item["command"].as_str().unwrap_or(""))
        })
        .collect();
    format!(
        "no account can be used: {}. Waiting does not fix a login. Sign in again (`claude` then /login, or `claude setup-token`), ask the admin to enable Claude access for the organization, or switch to another account (`--claude-fallback`), then run again.",
        rows.join("; ")
    )
}

/// Account text of a turn: `<name> auth status`, asked once per command.
pub fn claude_account(opts: &mut LoopOpts, name: &str) -> String {
    if !opts.claude_auth.iter().any(|(n, _)| n == name) {
        opts.claude_auth.push((name.to_string(), claude_binaries::auth_status(name)));
    }
    let auth = &opts.claude_auth.iter().find(|(n, _)| n == name).expect("cached").1;
    claude_binaries::account_label(auth)
}

/// The configured notice channels that stay on this machine (console, toast, file), in the configured order.
pub fn local_notice_channels(env: &Env) -> Vec<String> {
    let configured = app_env_list(env, "LOOP_NOTIFY_CHANNELS");
    let configured: Vec<String> = if configured.is_empty() { LOCAL_NOTICE_CHANNELS.iter().map(|s| s.to_string()).collect() } else { configured };
    configured.into_iter().filter(|n| LOCAL_NOTICE_CHANNELS.contains(&n.as_str())).collect()
}

/// How each sign-in method of `claude auth status` is billed, in plain words (a sign-in token is still the subscription).
pub fn sign_in_words(method: &str) -> Option<&'static str> {
    match method {
        "login" => Some("subscription, browser login"),
        "token" => Some("subscription, sign-in token"),
        "API key" => Some("API key, billed per token"),
        _ => None,
    }
}

/// `C:/bin/claude.exe` -> `claude`, `backup.cmd` -> `backup`.
pub fn command_base_name(name: &str) -> String {
    let last = pycompat::last_segment(name);
    match last.rfind('.') {
        Some(i) if i > 0 => last[..i].to_string(),
        _ => last.to_string(),
    }
}

/// `(method, who, organization and plan)` of a [`claude_binaries::account_label`] text such as `login, dev@example.com (Acme, team)` or
/// `token`; missing parts come back empty.
pub fn split_account_label(account: &str) -> (String, String, String) {
    let mut text = account.trim().to_string();
    let mut extra = String::new();
    if let Some(found) = ACCOUNT_EXTRA.search(&text) {
        extra = found.group(1).unwrap_or("").to_string();
        text.truncate(found.start);
    }
    let (method, who) = match text.split_once(", ") {
        Some((m, w)) => (m.to_string(), w.to_string()),
        None => (text.clone(), String::new()),
    };
    if sign_in_words(&method).is_none() {
        return (String::new(), text.trim().to_string(), extra);
    }
    (method, who.trim().to_string(), extra)
}

fn auth_method_code(label: &str) -> &'static str {
    match label {
        "login" => "login",
        "token" => "token",
        "API key" => "api_key",
        _ => "",
    }
}

/// Structured sign-in of a turn, taken from its account label (never a secret): `{"auth_method", "auth_email", "auth_plan"}`.
pub fn claude_auth_fields(account: &str) -> Map<String, Value> {
    let (method, who, extra) = split_account_label(account);
    let mut parts: Vec<String> = extra.split(',').map(|p| p.trim().to_string()).collect();
    parts.extend([String::new(), String::new()]);
    let mut fields = Map::new();
    fields.insert("auth_method".into(), json!(auth_method_code(&method)));
    fields.insert("auth_email".into(), json!(if who.contains('@') { who.clone() } else { String::new() }));
    fields.insert("auth_plan".into(), json!(parts[1]));
    fields
}

/// Body text of one command and its account: ``the main command `claude` (dev@example.com, subscription, browser login; Acme, team)``.
pub fn describe_claude_used(name: &str, account: &str, is_main: bool) -> String {
    let (method, who, extra) = split_account_label(account);
    let mut details = [who, sign_in_words(&method).unwrap_or("").to_string()].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(", ");
    if !extra.is_empty() {
        details.push_str(&format!("; {extra}"));
    }
    let role = if is_main { "the main command" } else { "the backup command" };
    format!("{role} `{}`{}", command_base_name(name), if details.is_empty() { " (account unknown)".to_string() } else { format!(" ({details})") })
}

/// Subject that says, in words, which account the loop is on now and how it signs in. `unusable` ("main account unusable") goes in front
/// when the change came from an account that cannot be used.
pub fn claude_change_subject(name: &str, account: &str, is_main: bool, unusable: &str) -> String {
    let subject = claude_change_subject_core(name, account, is_main);
    if unusable.is_empty() {
        subject
    } else {
        format!("{unusable}, {subject}")
    }
}

/// The kind of an account change, as the panel and the notice name it.
pub fn change_kind(unusable: bool, is_main: bool) -> &'static str {
    if unusable {
        "account_unusable"
    } else if is_main {
        "back_to_main"
    } else {
        "usage_limit_switch"
    }
}

/// The part of the subject that names the account the loop is on now.
pub fn claude_change_subject_core(name: &str, account: &str, is_main: bool) -> String {
    let (method, who, _) = split_account_label(account);
    let who = if who.is_empty() { format!("`{}`", command_base_name(name)) } else { who };
    let how = sign_in_words(&method).map(|w| format!(" ({w})")).unwrap_or_default();
    format!("{}: {who}{how}", if is_main { "back on the main account" } else { "now on the backup account" })
}

/// When the loop is on a fallback and the main command's remembered reset moment has passed, goes back to the main command (index 0).
/// Returns true when it went back; the next `notify_claude_change` announces it.
pub fn return_to_main_command(opts: &mut LoopOpts) -> bool {
    let Some(moment) = opts.main_reset else { return false };
    let names = claude_names(opts);
    if opts.claude_index > 0 && local_now() >= moment && !unusable_names(opts).contains(&names[0]) {
        opts.claude_index = 0;
        opts.main_reset = None;
        println!("[loop] the main command `{}` should be free again (reset {}): back on it", names[0], day_month_clock(moment));
        return true;
    }
    false
}

fn day_month_clock(moment: PrimitiveDateTime) -> String {
    format!("{:02}/{:02} {:02}:{:02}", moment.day(), u8::from(moment.month()), moment.hour(), moment.minute())
}

/// Keeps a known reset time of a limited command (for the wait when every command is limited), with the words that name the window that
/// ran out ("5-hour window, back at 17:05"; empty when unknown) for the notice of that wait.
pub fn remember_reset(opts: &mut LoopOpts, moment: Option<PrimitiveDateTime>, phrase: &str) {
    if let Some(moment) = moment {
        opts.limit_resets.push(moment);
        if !phrase.is_empty() {
            opts.limit_reset_phrases.retain(|(m, _)| *m != moment);
            opts.limit_reset_phrases.push((moment, phrase.to_string()));
        }
    }
}

/// ` (5-hour window, back at 17:05)` for a notice; empty when the window is unknown.
pub fn limit_suffix(phrase: &str) -> String {
    if phrase.is_empty() {
        String::new()
    } else {
        format!(" ({phrase})")
    }
}

/// Key of a command in meta.json `claude_usage`: its base name.
pub fn usage_key(name: &str) -> String {
    command_base_name(name)
}

/// Keeps the last usage windows read from a command's output in meta.json: `{"<key>": {"windows", "status", "read_at"}}`. A turn whose
/// output had no rate limit event leaves the earlier reading as it was.
pub fn record_usage(meta: &mut Value, key: &str, windows: &Value, status: &str) {
    let has_windows = windows.as_object().is_some_and(|w| !w.is_empty());
    if !has_windows && status.is_empty() {
        return;
    }
    if let Some(map) = meta.as_object_mut() {
        let slot = map.entry("claude_usage").or_insert_with(|| json!({}));
        if let Some(usage) = slot.as_object_mut() {
            usage.insert(key.to_string(), json!({"windows": windows, "status": status, "read_at": iso_seconds(local_now())}));
        }
    }
}

/// The command a limited main command would move to (the next one that is not unusable); empty when there is none.
pub fn next_fallback_name(opts: &LoopOpts) -> String {
    let (names, blocked) = (claude_names(opts), unusable_names(opts));
    names.iter().skip(opts.claude_index + 1).find(|n| !blocked.contains(n)).cloned().unwrap_or_default()
}

/// One line per command or account other than the main one with its last known usage, for the weekly alert.
pub fn other_usage_lines(opts: &LoopOpts, meta: &Value, main_name: &str) -> Vec<String> {
    let main_base = command_base_name(main_name);
    let usage = meta.get("claude_usage").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut lines = Vec::new();
    for (key, entry) in &usage {
        if key.split(':').next().unwrap_or("") != main_base {
            let read_at: String = entry.get("read_at").and_then(Value::as_str).unwrap_or("").chars().skip(11).take(5).collect();
            lines.push(format!(
                "- `{key}`: {}{}",
                usage_windows::describe_windows(entry.get("windows")),
                if read_at.is_empty() { String::new() } else { format!(", read at {read_at}") }
            ));
        }
    }
    let known: Vec<&str> = usage.keys().map(|k| k.split(':').next().unwrap_or("")).collect();
    for name in claude_names(opts) {
        let base = command_base_name(&name);
        if base != main_base && !known.contains(&base.as_str()) {
            lines.push(format!("- `{base}`: usage unknown (not run yet)"));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_account_change_has_a_kind_for_the_panel_and_the_notice() {
        assert_eq!(change_kind(true, false), "account_unusable");
        assert_eq!(change_kind(true, true), "account_unusable");
        assert_eq!(change_kind(false, true), "back_to_main");
        assert_eq!(change_kind(false, false), "usage_limit_switch");
    }

    #[test]
    fn labels_split_and_describe() {
        assert_eq!(split_account_label("login, dev@example.com (Acme, team)"), ("login".to_string(), "dev@example.com".to_string(), "Acme, team".to_string()));
        assert_eq!(split_account_label("token"), ("token".to_string(), String::new(), String::new()));
        assert_eq!(split_account_label("weird text"), (String::new(), "weird text".to_string(), String::new()));
        let fields = claude_auth_fields("login, dev@example.com (Acme, team)");
        assert_eq!((fields["auth_method"].as_str(), fields["auth_email"].as_str(), fields["auth_plan"].as_str()), (Some("login"), Some("dev@example.com"), Some("team")));
        assert_eq!(
            describe_claude_used("C:/bin/claude.exe", "login, dev@example.com (Acme, team)", true),
            "the main command `claude` (dev@example.com, subscription, browser login; Acme, team)"
        );
        assert_eq!(describe_claude_used("backup.cmd", "", false), "the backup command `backup` (account unknown)");
        assert_eq!(claude_change_subject("backup", "token", false, ""), "now on the backup account: `backup` (subscription, sign-in token)");
        assert_eq!(claude_change_subject("claude", "login, a@x.com", true, ""), "back on the main account: a@x.com (subscription, browser login)");
        assert_eq!(
            claude_change_subject("backup", "token", false, "main account unusable"),
            "main account unusable, now on the backup account: `backup` (subscription, sign-in token)"
        );
        assert_eq!(command_base_name("C:/bin/claude.exe"), "claude");
        assert_eq!(usage_key("backup.cmd"), "backup");
    }

    #[test]
    fn fallbacks_are_read_again_from_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::from_pairs([("LOOP_DATA_DIR", dir.path().display().to_string())]);
        let mut opts = LoopOpts::new("x");
        opts.claude_fallback = vec!["claude-backup".into()];
        assert!(!refresh_fallbacks(&mut opts, &env), "no key in the file: the command line list stays");
        std::fs::write(dir.path().join("config.json"), r#"{"claude_fallback_binaries": ["claude-backup2", "claude-backup3"]}"#).unwrap();
        assert!(refresh_fallbacks(&mut opts, &env));
        assert_eq!(opts.claude_fallback, vec!["claude-backup2", "claude-backup3"]);
        assert!(!refresh_fallbacks(&mut opts, &env));
        std::fs::write(dir.path().join("config.json"), r#"{"claude_fallback_binaries": []}"#).unwrap();
        assert!(refresh_fallbacks(&mut opts, &env) && opts.claude_fallback.is_empty() && opts.claude_index == 0);
    }

    #[test]
    fn fallback_order_and_unusable_accounts() {
        let mut opts = LoopOpts::new("x");
        opts.claude = "claude".into();
        assert_eq!(claude_names(&opts), vec!["claude"]);
        assert_eq!(current_claude(&opts), "claude");
        assert!(!switch_claude(&mut opts));
        let record = mark_account_unusable(&mut opts, "claude", "login, a@x.com", "Invalid API key sk-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 4);
        assert!(record["reason"].as_str().unwrap().contains("[redacted]") && opts.unusable_notice.is_some());
        assert_eq!(unusable_names(&opts), vec!["claude"]);
        assert_eq!(first_usable_index(&opts), 0);
        assert!(unusable_stop_message(&opts).starts_with("no account can be used: `claude` (login, a@x.com): Invalid API key"));
        assert_eq!(next_fallback_name(&opts), "");
    }

    #[test]
    fn usage_is_kept_per_command() {
        let mut meta = json!({});
        record_usage(&mut meta, "claude", &json!({}), "");
        assert!(meta.get("claude_usage").is_none());
        record_usage(&mut meta, "claude", &json!({"five_hour": {"pct": 60.0, "reset_at": null}}), "allowed");
        assert_eq!(meta["claude_usage"]["claude"]["status"], "allowed");
        let mut opts = LoopOpts::new("x");
        opts.claude = "claude".into();
        record_usage(&mut meta, "backup:one", &json!({"seven_day": {"pct": 33.0, "reset_at": null}}), "");
        let lines = other_usage_lines(&opts, &meta, "claude");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("- `backup:one`: weekly 33%, read at "));
        let mut moment_opts = LoopOpts::new("x");
        let now = local_now();
        remember_reset(&mut moment_opts, Some(now), "5-hour window, back at 17:05");
        remember_reset(&mut moment_opts, None, "ignored");
        assert_eq!((moment_opts.limit_resets.len(), moment_opts.limit_reset_phrases.len()), (1, 1));
        assert_eq!(limit_suffix("x"), " (x)");
        assert_eq!(local_notice_channels(&Env::from_pairs([("LOOP_NOTIFY_CHANNELS", "email, console ,toast")])), vec!["console", "toast"]);
    }
}
