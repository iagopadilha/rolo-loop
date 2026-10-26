// SPDX-License-Identifier: MIT
//! Which `claude` command a headless call runs, and the fallback when an account hits its usage limit.
//!
//! Used by the engine and by the app's panel jobs, so the loop turns and the panel pick the command the same way.
//!
//! The order comes from config.json: `claude_binary` first (empty = `claude`), then `claude_fallback_binaries` (a list or one name). A name
//! like `cc` may be a wrapper script (`cc.cmd`) that signs `claude` in with another account. When the running command fails with an account
//! usage limit, the caller moves to the next one; an `overloaded` API (529) is not an account limit and never switches. An account that
//! cannot be used at all (logged out, revoked or expired token, an organization that blocks subscription access: [`is_account_unusable`])
//! moves the caller to the next command too, with no wait.
//!
//! A Windows batch wrapper (`.cmd`/`.bat`) goes through cmd.exe, which cuts an argument at its first line break and expands `%NAME%`: the
//! prompt of such a call always goes through stdin (`claude -p` with no prompt argument reads it from there).
//!
//! Which account runs: `<command> auth status` (JSON, no secret: method, e-mail, organization, plan) and, for an optional account-switching
//! wrapper (`cc`) in headless mode, the account line the wrapper prints on stderr.

use crate::loop_io::{run_timeout_raw, which, which_in};
use pycompat::Re;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::LazyLock;
use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};

pub const DEFAULT_BINARY_NAME: &str = "claude";
pub const BATCH_EXTENSIONS: [&str; 2] = [".cmd", ".bat"];
pub const PROMPT_FLAG: &str = "-p";
const AUTH_STATUS_TIMEOUT_SECONDS: u64 = 90;
const UNUSABLE_REASON_LIMIT: usize = 300;

// account-side limits: switching to another account helps. `overloaded` (API capacity) is left out on purpose.
static USAGE_LIMIT: LazyLock<Re> = LazyLock::new(|| Re::ci(r"usage limit|limit reached|rate.?limit|too many requests|\b429\b|quota"));
// output of an optional account-switching wrapper, printed on stderr in headless mode
static CC_ACCOUNT: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?m)^\[cc\] conta: *(\S+)"));
// an account that cannot sign in at all (logged out, revoked or expired token, an organization that blocks subscription access): waiting
// never fixes it, only another account does. Only specific signatures: a bare "401"/"403" may be an HTTP code of the app a turn works on.
static ACCOUNT_UNUSABLE: LazyLock<Re> = LazyLock::new(|| {
    Re::ci(concat!(
        r"oauth_org_not_allowed|authentication_error|API Error[^\n]{0,200}permission_error|permission_error[^\n]{0,200}API Error",
        r"|API Error:? *(?:401|403)\b|Invalid API key|OAuth token has expired|OAuth token (?:was )?revoked|Please run /login",
        r"|not logged in|disabled Claude subscription access",
        r"|oauth_not_allowed_for_organization|OAuth authentication is currently not allowed"
    ))
});
// `cc` with no free account: when the first one frees up (ISO, UTC)
static CC_RESET: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?m)^\[cc\] reinicia: *(\S+)"));
// `claude`: "Claude AI usage limit reached|<unix seconds>"
static EPOCH_RESET: LazyLock<Re> = LazyLock::new(|| Re::ci(r"limit reached\|(\d{9,11})"));

fn method_label(method: &str) -> String {
    match method {
        "claude.ai" => "login".into(),
        "oauth_token" => "token".into(),
        "api_key" | "apiKey" => "API key".into(),
        "" => "?".into(),
        other => other.into(),
    }
}

/// `os.path.expandvars` on Windows: `%NAME%` replaced when the variable exists.
fn expand_vars(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => match std::env::var(&after[..end]) {
                Ok(value) => {
                    out.push_str(&value);
                    rest = &after[end + 1..];
                }
                Err(_) => {
                    out.push('%');
                    rest = after;
                }
            },
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn registry_path(key: &str) -> Option<String> {
    let (code, out, _) = run_timeout_raw("reg", &["query", key, "/v", "Path"], None, 15);
    if code != 0 {
        return None;
    }
    // `    Path    REG_EXPAND_SZ    C:\a;C:\b`
    out.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix("Path")?.trim_start();
        let (_, value) = rest.split_once(char::is_whitespace)?;
        Some(value.trim().to_string())
    })
}

/// PATH folders saved for the user and the machine in the Windows registry (`[]` elsewhere or on error): a process started before the user
/// added a folder (an editor, the panel) still finds a wrapper such as `cc` there.
pub fn user_path_entries() -> Vec<String> {
    if !cfg!(windows) {
        return vec![];
    }
    let mut entries = Vec::new();
    for key in [r"HKCU\Environment", r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment"] {
        if let Some(value) = registry_path(key) {
            entries.extend(value.split(';').filter(|i| !i.trim().is_empty()).map(expand_vars));
        }
    }
    entries
}

/// Full path of `name` (a path, or a command looked up on PATH and then on the registry PATH); empty when not found.
pub fn resolve(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    if Path::new(name).is_file() {
        return name.to_string();
    }
    if let Some(found) = which(name) {
        return found.display().to_string();
    }
    let entries = user_path_entries();
    if entries.is_empty() {
        return String::new();
    }
    let joined = std::env::join_paths(entries.iter()).unwrap_or_default();
    which_in(name, &joined).map(|p| p.display().to_string()).unwrap_or_default()
}

/// Command names in the order they are tried: the primary (empty = `claude`), then each fallback found on this machine, without repeats.
/// A fallback that is not installed is skipped (another machine may not have `cc`): it never blocks a call.
pub fn ordered(primary: &str, fallbacks: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let first = if primary.is_empty() { DEFAULT_BINARY_NAME } else { primary };
    for (position, name) in std::iter::once(first).chain(fallbacks.iter().map(String::as_str)).enumerate() {
        let name = name.trim();
        if !name.is_empty() && !names.iter().any(|n| n == name) && (position == 0 || !resolve(name).is_empty()) {
            names.push(name.to_string());
        }
    }
    names
}

/// `claude_fallback_binaries` of config.json: a list of names or one name; anything else = none.
pub fn read_fallbacks(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => vec![s.trim().to_string()],
        Value::Array(items) => items.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
        _ => vec![],
    }
}

/// What goes in argv[0]: the resolved path, else the name as given (the OS then reports it missing).
pub fn executable(name: &str) -> String {
    let resolved = resolve(name);
    if resolved.is_empty() { name.to_string() } else { resolved }
}

/// Start of the argv: a `.py` (the tests' fake claude) runs with `python`.
pub fn base_argv(name: &str) -> Vec<String> {
    let binary = executable(name);
    if binary.ends_with(".py") { vec!["python".into(), binary] } else { vec![binary] }
}

pub fn is_batch(argv: &[String]) -> bool {
    argv.first().is_some_and(|a| { let l = a.to_lowercase(); BATCH_EXTENSIONS.iter().any(|e| l.ends_with(e)) })
}

/// `(argv, stdin_text)` safe for the command: for a batch wrapper the prompt argument right after `-p` moves to stdin (joined after any
/// stdin text already there). Anything else comes back unchanged.
pub fn prompt_through_stdin(argv: &[String], stdin_text: &str) -> (Vec<String>, String) {
    let mut argv = argv.to_vec();
    if !is_batch(&argv) {
        return (argv, stdin_text.to_string());
    }
    let Some(flag) = argv.iter().position(|a| a == PROMPT_FLAG) else { return (argv, stdin_text.to_string()) };
    let index = flag + 1;
    if index >= argv.len() || argv[index].starts_with("--") {
        return (argv, stdin_text.to_string());
    }
    let prompt = argv.remove(index);
    let text = if stdin_text.is_empty() { prompt } else { format!("{prompt}\n\n{stdin_text}") };
    (argv, text)
}

/// The same argv with the base of `old_name` replaced by the base of `new_name` (unchanged when it does not start with it).
pub fn swap_binary(argv: &[String], old_name: &str, new_name: &str) -> Vec<String> {
    let old = base_argv(old_name);
    if argv.len() < old.len() || argv[..old.len()] != old[..] {
        return argv.to_vec();
    }
    let mut out = base_argv(new_name);
    out.extend_from_slice(&argv[old.len()..]);
    out
}

/// True when the output names an account limit (switching account helps); false for anything else, `overloaded` included.
pub fn is_usage_limit(text: &str) -> bool {
    USAGE_LIMIT.search(text).is_some()
}

/// True when the output says this account cannot be used (authentication or authorization failure): switching account helps, waiting
/// does not. Call it only on the text of a turn or call that failed.
pub fn is_account_unusable(text: &str) -> bool {
    ACCOUNT_UNUSABLE.search(text).is_some()
}

/// Short text of why the account is unusable: the output line (or the window of a long JSON line) around the first signature; empty when
/// the output has none. It is not redacted: the caller redacts it before it leaves the machine.
pub fn unusable_reason(text: &str) -> String {
    let Some(found) = ACCOUNT_UNUSABLE.search(text) else { return String::new() };
    let line_start = text[..found.start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = text[found.end..].find('\n').map(|i| found.end + i).unwrap_or(text.len());
    let line = &text[line_start..line_end];
    let chars: Vec<char> = line.chars().collect();
    let line: String = if chars.len() > UNUSABLE_REASON_LIMIT {
        let begin = text[line_start..found.start].chars().count();
        chars[begin..(begin + UNUSABLE_REASON_LIMIT).min(chars.len())].iter().collect()
    } else {
        line.to_string()
    };
    line.trim().to_string()
}

fn local_offset() -> UtcOffset {
    UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
}

fn to_local_naive(moment: OffsetDateTime) -> PrimitiveDateTime {
    let local = moment.to_offset(local_offset());
    PrimitiveDateTime::new(local.date(), local.time())
}

fn parse_iso(text: &str) -> Option<PrimitiveDateTime> {
    use time::format_description::well_known::Iso8601;
    let fixed = text.replacen('Z', "+00:00", 1);
    if let Ok(with_offset) = OffsetDateTime::parse(&fixed, &Iso8601::DEFAULT) {
        return Some(to_local_naive(with_offset));
    }
    PrimitiveDateTime::parse(&fixed.replacen(' ', "T", 1), &Iso8601::DEFAULT).ok()
}

/// Local naive datetime of the limit reset named in the output (`[cc] reinicia: <ISO>` or `limit reached|<epoch>`); `None` when absent.
pub fn reset_time(text: &str) -> Option<PrimitiveDateTime> {
    if let Some(found) = CC_RESET.search(text) {
        if let Some(moment) = parse_iso(found.group(1).unwrap_or("")) {
            return Some(moment);
        }
    }
    let found = EPOCH_RESET.search(text)?;
    let seconds: i64 = found.group(1)?.parse().ok()?;
    OffsetDateTime::from_unix_timestamp(seconds).ok().map(to_local_naive)
}

/// Account name an optional account-switching wrapper chose (the line it prints on stderr); empty for any other command.
pub fn account_from_stderr(text: &str) -> String {
    CC_ACCOUNT.search(text).and_then(|m| m.group(1).map(String::from)).unwrap_or_default()
}

fn s(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Null) | None | Some(Value::Bool(false)) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// `{"method", "email", "org", "plan", "account", "logged_in", "error"}` of `<name> auth status` (no secret in it); `{}` for a test `.py`
/// fake. Never fails: a missing command or an unreadable answer comes back as `error`.
pub fn auth_status(name: &str) -> Value {
    let argv = base_argv(name);
    if argv[0] == "python" {
        return json!({});
    }
    let mut args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    args.extend(["auth", "status"]);
    let (code, stdout, stderr) = run_timeout_raw(&argv[0], &args, None, AUTH_STATUS_TIMEOUT_SECONDS);
    if code == 1 && stdout.is_empty() && !stderr.is_empty() && !stderr.contains('{') && stderr.contains("os error") {
        return json!({"error": stderr.chars().take(200).collect::<String>()});
    }
    let data = stdout.find('{').and_then(|start| serde_json::from_str::<Value>(&stdout[start..]).ok());
    let Some(Value::Object(data)) = data.filter(|d| d.as_object().is_some_and(|o| !o.is_empty())) else {
        return json!({"error": format!("`{name} auth status` gave no JSON (code {code})")});
    };
    let method = s(data.get("authMethod"));
    json!({
        "method": method_label(&method), "email": s(data.get("email")), "org": s(data.get("orgName")), "plan": s(data.get("subscriptionType")),
        "account": account_from_stderr(&stderr), "logged_in": crate::notify_truthy(data.get("loggedIn")), "error": ""
    })
}

/// Short text of the account: `login dev@example.com (Acme, team)`, `token, cc account claude`; empty when nothing is known.
pub fn account_label(auth: &Value, account: &str) -> String {
    let account = if account.is_empty() { s(auth.get("account")) } else { account.to_string() };
    if !s(auth.get("error")).is_empty() && account.is_empty() {
        return String::new();
    }
    let who = { let e = s(auth.get("email")); if !e.is_empty() { e } else if !account.is_empty() { format!("cc account {account}") } else { String::new() } };
    let extra: Vec<String> = ["org", "plan"].iter().map(|k| s(auth.get(*k))).filter(|v| !v.is_empty()).collect();
    let text: Vec<String> = [s(auth.get("method")), who].into_iter().filter(|v| !v.is_empty()).collect();
    format!("{}{}", text.join(", "), if extra.is_empty() { String::new() } else { format!(" ({})", extra.join(", ")) })
}

/// One row per configured command, missing ones included: `{"name", "role", "path", "found", "auth", "label"}`. Runs `auth status` of every
/// command found (the `cc` picker queries each account's usage: a few seconds).
pub fn describe(primary: &str, fallbacks: &[String]) -> Vec<Value> {
    let mut names: Vec<String> = Vec::new();
    let first = if primary.is_empty() { DEFAULT_BINARY_NAME.to_string() } else { primary.to_string() };
    for name in std::iter::once(first).chain(read_fallbacks(&json!(fallbacks))) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
        .iter()
        .enumerate()
        .map(|(position, name)| {
            let path = resolve(name);
            let auth = if path.is_empty() { json!({}) } else { auth_status(name) };
            json!({"name": name, "role": if position == 0 { "main" } else { "fallback" }, "path": path, "found": !path.is_empty(), "label": account_label(&auth, ""), "auth": auth})
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_and_unusable_accounts() {
        assert!(is_usage_limit("Claude AI usage limit reached|1700000000") && is_usage_limit("HTTP 429 Too Many Requests") && is_usage_limit("Rate-limit"));
        assert!(!is_usage_limit("API Error: 529 overloaded") && !is_usage_limit("port 4290"));
        assert!(is_account_unusable("API Error: 401 {\"type\":\"error\"}") && is_account_unusable("Please run /login") && is_account_unusable("OAuth token has expired"));
        assert!(!is_account_unusable("GET /x 401 Unauthorized"));
        assert_eq!(unusable_reason("first\n  Invalid API key · fix it \nlast"), "Invalid API key · fix it");
        assert_eq!(unusable_reason("fine"), "");
        let long = format!("{}Invalid API key{}", "a".repeat(400), "b".repeat(400));
        assert_eq!(unusable_reason(&long).chars().count(), 300);
        assert!(unusable_reason(&long).starts_with("Invalid API key"));
    }

    #[test]
    fn reset_times() {
        assert!(reset_time("nothing").is_none());
        let epoch = reset_time("Claude AI usage limit reached|1700000000").unwrap();
        assert_eq!(to_local_naive(OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()), epoch);
        let iso = reset_time("[cc] reinicia: 2026-01-02T03:04:05Z").unwrap();
        assert_eq!(to_local_naive(OffsetDateTime::parse("2026-01-02T03:04:05+00:00", &time::format_description::well_known::Iso8601::DEFAULT).unwrap()), iso);
        assert!(reset_time("[cc] reinicia: garbage").is_none());
    }

    #[test]
    fn batch_prompt_goes_through_stdin() {
        let argv: Vec<String> = ["C:\\bin\\cc.cmd", "-p", "do it", "--model", "x"].map(String::from).to_vec();
        let (new_argv, stdin) = prompt_through_stdin(&argv, "ctx");
        assert_eq!((new_argv.join(" "), stdin.as_str()), ("C:\\bin\\cc.cmd -p --model x".to_string(), "do it\n\nctx"));
        let exe: Vec<String> = ["claude.exe", "-p", "do it"].map(String::from).to_vec();
        assert_eq!(prompt_through_stdin(&exe, "in"), (exe.clone(), "in".to_string()));
        let flagged: Vec<String> = ["a.bat", "-p", "--x"].map(String::from).to_vec();
        assert_eq!(prompt_through_stdin(&flagged, ""), (flagged.clone(), String::new()));
    }

    #[test]
    fn account_text() {
        assert_eq!(account_from_stderr("x\n[cc] conta: claude2\n"), "claude2");
        assert_eq!(account_label(&json!({"method": "login", "email": "dev@example.com", "org": "Acme", "plan": "team", "error": ""}), ""), "login, dev@example.com (Acme, team)");
        assert_eq!(account_label(&json!({"method": "token", "error": ""}), "claude"), "token, cc account claude");
        assert_eq!(account_label(&json!({"error": "boom"}), ""), "");
        assert_eq!(read_fallbacks(&json!([" a ", 3, ""])), vec!["a"]);
        assert_eq!(read_fallbacks(&json!("cc")), vec!["cc"]);
        assert!(read_fallbacks(&json!(5)).is_empty());
        assert_eq!(ordered("", &["definitely-not-installed-xyz".to_string()]), vec!["claude"]);
    }
}
