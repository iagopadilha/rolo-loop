// SPDX-License-Identifier: MIT
//! The network list of a loop: entry grammar, matching and judgement.
//!
//! Entry grammar: `host[:port][/path] [METHODS] [cred] [if-missing=NAME]`. `host` is a name, `*.domain`, or an IPv4/IPv6
//! literal (brackets for IPv6); METHODS is `GET,POST,...`, `*` (any HTTP method) or `tcp`; default `GET,HEAD`. A deny
//! pattern is a host only. Order of judgement: parse, deny, always-local hosts, broken list (fail closed), allow entries
//! (`if-missing` honoured), `cred` needs a matching entry that carries it. A target that cannot be judged safely is blocked.
//!
//! The shared contract fixture (`tests/network_contract.json`: 112 judged requests + 35 invalid entries) pins this crate.

use regex::Regex;
use serde_json::Value;
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::Path;
use std::sync::LazyLock;

pub const HTTP_METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];
pub const ANY_METHOD: &str = "*";
pub const TCP: &str = "tcp";
/// The method of a URL literal found in a script.
pub const UNKNOWN_METHOD: &str = "*";
pub const ORIGIN_LOCAL: &str = "local";
pub const ORIGIN_BUILTIN: &str = "builtin";
pub const ORIGIN_GLOBAL: &str = "global";
pub const ORIGIN_PROJECT: &str = "project";
pub const LOCAL_ENTRIES: [&str; 5] = ["localhost * cred", "127.0.0.1 * cred", "[::1] * cred", "0.0.0.0 * cred", "*.localhost * cred"];
pub const BUILTIN_READ_HOSTS: [&str; 12] = [
    "learn.microsoft.com",
    "developer.mozilla.org",
    "registry.npmjs.org",
    "www.npmjs.com",
    "pypi.org",
    "files.pythonhosted.org",
    "api.nuget.org",
    "www.nuget.org",
    "github.com",
    "raw.githubusercontent.com",
    "codeload.github.com",
    "objects.githubusercontent.com",
];
pub const NETWORK_LABEL: &str = ".specs/project/NETWORK.json";
const IF_MISSING_PREFIX: &str = "if-missing=";

static LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9_](?:[a-z0-9_-]*[a-z0-9_])?$").expect("regex"));
static ENV_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("regex"));
static SCHEME_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9+.\-]*://").expect("regex"));
static CONTROL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\x00-\x1f\x7f]").expect("regex"));

pub fn default_port(scheme: &str) -> Option<u32> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub host: String,
    pub port: Option<u32>,
    pub path: String,
    pub methods: BTreeSet<String>,
    pub cred: bool,
    pub if_missing: String,
    pub origin: String,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Effective {
    pub allow: Vec<Entry>,
    pub deny: Vec<String>,
    pub notes: Vec<String>,
    pub broken: String,
    pub env_names: Vec<String>,
}

/// Kind of a judgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ok,
    Unjudgeable,
    Denied,
    Broken,
    NotListed,
    NoCred,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Ok => "ok",
            Kind::Unjudgeable => "unjudgeable",
            Kind::Denied => "denied",
            Kind::Broken => "broken",
            Kind::NotListed => "not_listed",
            Kind::NoCred => "no_cred",
        }
    }
}

fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h = (bytes[i + 1] as char).to_digit(16);
            let l = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (h, l) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// URL path -> `''` or `/a/b`: percent-decoded once, `\` = `/`, `//` collapsed, `.` skipped, `..` resolved.
pub fn normalise_path(raw: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for segment in unquote(raw).replace('\\', "/").split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s.to_string()),
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("/{}", parts.join("/"))
    }
}

fn parse_host(text: &str) -> Result<String, String> {
    let mut host = text.to_lowercase();
    if host.ends_with('.') && !host.ends_with("..") {
        host.pop();
    }
    if host.is_empty() {
        return Err("host is empty".into());
    }
    let labels: Vec<&str> = if let Some(rest) = host.strip_prefix("*.") { rest.split('.').collect() } else { host.split('.').collect() };
    if labels.concat().contains('*') {
        return Err(format!("wildcard only as a leading `*.domain` (got {text})"));
    }
    for label in labels {
        if !LABEL.is_match(label) {
            return Err(format!("invalid host name {text}"));
        }
    }
    Ok(host)
}

fn parse_port(text: &str) -> Result<u32, String> {
    match text.parse::<u32>() {
        Ok(n) if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) && (1..=65535).contains(&n) => Ok(n),
        _ => Err(format!("invalid port {text} (1-65535)")),
    }
}

type Target = (String, Option<u32>, String);

fn parse_target(target: &str, allow_path: bool) -> Result<Target, String> {
    if target.contains("://") {
        return Err("write host[:port][/path] without a scheme".into());
    }
    for c in ['?', '#', '\\'] {
        if target.contains(c) {
            return Err(format!("`{c}` is not allowed in the target"));
        }
    }
    let (hostport, raw_path) = match target.find('/') {
        None => (target, ""),
        Some(i) => (&target[..i], &target[i..]),
    };
    let mut port: Option<u32> = None;
    let host;
    if hostport.starts_with('[') {
        let close = hostport.find(']').ok_or("unclosed `[` in the IPv6 host")?;
        let literal = &hostport[1..close];
        host = literal.parse::<IpAddr>().map(|ip| ip.to_string()).map_err(|_| format!("invalid IPv6 address {}", &hostport[..=close]))?;
        let rest = &hostport[close + 1..];
        if !rest.is_empty() {
            let Some(p) = rest.strip_prefix(':') else {
                return Err("unexpected text after the IPv6 host".into());
            };
            port = Some(parse_port(p)?);
        }
    } else {
        if hostport.matches(':').count() > 1 {
            return Err("a second `:` in the host (an IPv6 literal needs brackets)".into());
        }
        let (name, colon, port_text) = match hostport.find(':') {
            Some(i) => (&hostport[..i], true, &hostport[i + 1..]),
            None => (hostport, false, ""),
        };
        host = parse_host(name)?;
        if colon {
            port = Some(parse_port(port_text)?);
        }
    }
    let mut path = String::new();
    if !raw_path.is_empty() {
        if !allow_path {
            return Err("a path is not allowed here".into());
        }
        if unquote(raw_path).replace('\\', "/").split('/').any(|s| s == "." || s == "..") {
            return Err("path prefix cannot contain `.` or `..` segments".into());
        }
        path = normalise_path(raw_path);
    }
    Ok((host, port, path))
}

fn parse_methods(token: &str) -> Result<BTreeSet<String>, String> {
    if token == ANY_METHOD {
        return Ok([ANY_METHOD.to_string()].into());
    }
    if token.eq_ignore_ascii_case(TCP) {
        return Ok([TCP.to_string()].into());
    }
    let mut methods = BTreeSet::new();
    for item in token.split(',') {
        let method = item.trim().to_uppercase();
        if method.is_empty() {
            return Err(format!("empty method in {token}"));
        }
        if method == ANY_METHOD || method.eq_ignore_ascii_case(TCP) {
            return Err(format!("`{item}` must stand alone in METHODS"));
        }
        if !HTTP_METHODS.contains(&method.as_str()) {
            return Err(format!("unknown method {item} (use {}, * or tcp)", HTTP_METHODS.join(",")));
        }
        methods.insert(method);
    }
    Ok(methods)
}

/// Entry text -> [`Entry`]; `Err(reason)` when invalid.
pub fn parse_entry(text: &Value, origin: &str) -> Result<Entry, String> {
    let Value::String(raw) = text else {
        return Err("entry must be a string".into());
    };
    let original = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if original.is_empty() {
        return Err("entry is empty".into());
    }
    let tokens: Vec<&str> = original.split(' ').collect();
    let (host, port, path) = parse_target(tokens[0], true)?;
    let mut methods: Option<BTreeSet<String>> = None;
    let mut cred = false;
    let mut if_missing = String::new();
    for token in &tokens[1..] {
        let lower = token.to_lowercase();
        if lower == "cred" {
            if cred {
                return Err("duplicate `cred`".into());
            }
            cred = true;
        } else if lower.starts_with(IF_MISSING_PREFIX) {
            let name = &token[IF_MISSING_PREFIX.len()..];
            if !if_missing.is_empty() {
                return Err("duplicate `if-missing`".into());
            }
            if !ENV_NAME.is_match(name) {
                return Err(format!("if-missing needs a variable NAME (letters, digits, `_`), got '{name}'"));
            }
            if_missing = name.to_string();
        } else if let Some(m) = &methods {
            let listed: Vec<&str> = m.iter().map(String::as_str).collect();
            return Err(format!("more than one METHODS token ({}) or unknown option {token}", listed.join(",")));
        } else {
            match parse_methods(token) {
                Ok(m) => methods = Some(m),
                Err(error) => return Err(format!("invalid METHODS or unknown option '{token}': {error}")),
            }
        }
    }
    let methods = methods.unwrap_or_else(|| ["GET".to_string(), "HEAD".to_string()].into());
    if methods.contains(TCP) && !path.is_empty() {
        return Err("a tcp entry cannot have a path prefix".into());
    }
    Ok(Entry { host, port, path, methods, cred, if_missing, origin: origin.to_string(), text: original })
}

/// Deny host pattern (`360.example.com`, `*.prod.example.com`): host only.
pub fn parse_deny(text: &Value) -> Result<String, String> {
    let Value::String(raw) = text else {
        return Err("deny pattern must be a non-empty string".into());
    };
    if raw.trim().is_empty() {
        return Err("deny pattern must be a non-empty string".into());
    }
    let (host, port, _) = parse_target(raw.trim(), false)?;
    if port.is_some() {
        return Err("a deny pattern is a host only (any port)".into());
    }
    Ok(host)
}

/// The text of an entry, or the Python type name of a non-string (`int`, `list`, `NoneType`...).
fn entry_label(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "NoneType".into(),
        Value::Bool(_) => "bool".into(),
        Value::Number(n) if n.is_f64() => "float".into(),
        Value::Number(_) => "int".into(),
        Value::Array(_) => "list".into(),
        Value::Object(_) => "dict".into(),
    }
}

/// Python `repr` of a scalar JSON value (enough for the `version` note).
fn scalar_repr(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{s}'"),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => other.to_string(),
    }
}

fn entries_from(texts: &Value, origin: &str, label: &str, notes: &mut Vec<String>) -> Vec<Entry> {
    let Value::Array(list) = texts else {
        notes.push(format!("{label}: expected a list of entries, ignored"));
        return vec![];
    };
    let mut entries = Vec::new();
    for (index, text) in list.iter().enumerate() {
        match parse_entry(text, origin) {
            Ok(e) => entries.push(e),
            Err(error) => notes.push(format!("{label} entry {index} ignored ({}): {error}", entry_label(text))),
        }
    }
    entries
}

/// Effective list from entry texts: local + built-in read + global + project, plus the denies.
pub fn effective_from(
    project_entries: &Value,
    global_entries: &Value,
    deny: &[Value],
    builtin_read: bool,
    env_names: &[String],
    mut notes: Vec<String>,
    broken: &str,
) -> Effective {
    let mut allow: Vec<Entry> = LOCAL_ENTRIES.iter().map(|t| parse_entry(&Value::String((*t).to_string()), ORIGIN_LOCAL).expect("local entries are valid")).collect();
    if builtin_read {
        allow.extend(BUILTIN_READ_HOSTS.iter().map(|h| parse_entry(&Value::String((*h).to_string()), ORIGIN_BUILTIN).expect("built-in hosts are valid")));
    }
    allow.extend(entries_from(global_entries, ORIGIN_GLOBAL, "config.json egress_hosts", &mut notes));
    allow.extend(entries_from(project_entries, ORIGIN_PROJECT, NETWORK_LABEL, &mut notes));
    let mut patterns = Vec::new();
    for text in deny {
        match parse_deny(text) {
            Ok(p) => patterns.push(p),
            Err(error) => notes.push(format!("config.json egress_never {} ignored: {error}", crate_repr(text))),
        }
    }
    let mut names: Vec<String> = env_names.to_vec();
    names.sort();
    names.dedup();
    Effective { allow, deny: patterns, notes, broken: broken.to_string(), env_names: names }
}

fn crate_repr(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{s}'"),
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

fn read_json(path: &Path) -> (Option<Value>, String) {
    match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => {
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_string();
                match serde_json::from_str::<Value>(&text) {
                    Ok(v) => (Some(v), String::new()),
                    Err(_) => (None, "JSONDecodeError".into()),
                }
            }
            Err(_) => (None, "UnicodeDecodeError".into()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, String::new()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => (None, "PermissionError".into()),
        Err(_) => (None, "OSError".into()),
    }
}

fn as_bool(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) if matches!(s.trim().to_lowercase().as_str(), "true" | "false") => s.trim().to_lowercase() == "true",
        _ => default,
    }
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The raw `allow` list of NETWORK.json and why it is unusable (`""` when missing or fine).
pub fn read_project_entries(project_root: &Path) -> (Vec<Value>, String) {
    let (content, error) = read_json(&project_root.join(".specs").join("project").join("NETWORK.json"));
    if !error.is_empty() {
        return (vec![], format!("unreadable JSON ({error})"));
    }
    match content {
        None => (vec![], String::new()),
        Some(Value::Object(m)) => match m.get("allow") {
            None => (vec![], String::new()),
            Some(Value::Array(a)) => (a.clone(), String::new()),
            Some(_) => (vec![], "unexpected shape (need an object with an `allow` list)".into()),
        },
        Some(_) => (vec![], "unexpected shape (need an object with an `allow` list)".into()),
    }
}

/// Effective list of a project from `<data dir>/config.json` and `<project>/.specs/project/NETWORK.json`. A list file that
/// exists but cannot be used sets `broken`: every non-local request then fails closed.
pub fn load_effective(project_root: &Path, data_dir: &Path, env_names: &[String]) -> Effective {
    let (mut notes, mut broken) = (Vec::<String>::new(), String::new());
    let (config, error) = read_json(&data_dir.join("config.json"));
    let config = match (config, error.is_empty()) {
        (Some(Value::Object(m)), true) => Value::Object(m),
        (None, true) => Value::Object(Default::default()),
        (other, ok) => {
            broken = "config.json".to_string();
            let why = if !ok {
                error
            } else if other.is_some() {
                "not a JSON object".to_string()
            } else {
                "not a JSON object".to_string()
            };
            notes.push(format!("config.json is unusable ({why}): every non-local request fails closed"));
            Value::Object(Default::default())
        }
    };
    let (texts, error) = read_project_entries(project_root);
    if !error.is_empty() {
        if broken.is_empty() {
            broken = NETWORK_LABEL.to_string();
        }
        notes.push(format!("{NETWORK_LABEL} is unusable ({error}): every non-local request fails closed"));
    }
    let (content, _) = read_json(&project_root.join(".specs").join("project").join("NETWORK.json"));
    if let Some(Value::Object(m)) = &content {
        if let Some(version) = m.get("version") {
            if version.as_i64() != Some(1) {
                notes.push(format!("{NETWORK_LABEL} has version {} (expected 1), read anyway", scalar_repr(version)));
            }
        }
    }
    let denies: Vec<Value> = match config.get("egress_never") {
        Some(Value::Array(a)) => a.clone(),
        other if !truthy(other) => vec![],
        Some(_) => {
            // losing the denies would fail open
            if broken.is_empty() {
                broken = "config.json".to_string();
            }
            notes.push("config.json egress_never must be a list: every non-local request fails closed".into());
            vec![]
        }
        None => vec![],
    };
    let global = match config.get("egress_hosts") {
        v if !truthy(v) => Value::Array(vec![]),
        Some(v) => v.clone(),
        None => Value::Array(vec![]),
    };
    effective_from(&Value::Array(texts), &global, &denies, as_bool(config.get("egress_builtin_read"), true), env_names, notes, &broken)
}

/// Exact or `*.domain` (needs at least one extra label, never the bare domain).
pub fn host_matches(pattern: &str, host: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix('*') {
        if !host.ends_with(suffix) || host.len() <= suffix.len() {
            return false;
        }
        return host[..host.len() - suffix.len()].split('.').all(|label| !label.is_empty());
    }
    pattern == host
}

pub fn path_matches(prefix: &str, path: &str) -> bool {
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// `(scheme, host, port, path)` of a request URL, normalised as the contract says; `Err(reason)` when it cannot be judged safely.
pub fn parse_request_url(url: &str) -> Result<(String, String, Option<u32>, String), String> {
    if url.trim().is_empty() {
        return Err("empty url".into());
    }
    let mut text = url.trim().to_string();
    if CONTROL.is_match(&text) {
        return Err("control character in the url".into());
    }
    if text.starts_with("//") {
        text = format!("http:{text}");
    } else if !SCHEME_PREFIX.is_match(&text) {
        text = format!("http://{text}");
    }
    let rest = &text[text.find("://").map(|i| i + 3).unwrap_or(0)..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('\\') {
        return Err("backslash in the authority (parsers disagree on the host)".into());
    }
    let (scheme, hostname, port, path) = split_url(&text).map_err(|e| format!("invalid url ({e})"))?;
    let Some(mut host) = hostname else {
        return Err("no host in the url".into());
    };
    host = host.to_lowercase();
    if host.ends_with('.') && !host.ends_with("..") {
        host.pop();
    }
    let port = port.or_else(|| default_port(&scheme));
    Ok((scheme, host, port, normalise_path(&path)))
}

type Split = (String, Option<String>, Option<u32>, String);

/// The slice of `urllib.parse.urlsplit` the contract relies on.
fn split_url(url: &str) -> Result<Split, String> {
    let mut rest = url.to_string();
    let mut scheme = String::new();
    if let Some(i) = rest.find(':') {
        let cand = &rest[..i];
        let valid = cand.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && cand.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        if valid {
            scheme = cand.to_ascii_lowercase();
            rest = rest[i + 1..].to_string();
        }
    }
    let mut netloc = String::new();
    if let Some(body) = rest.strip_prefix("//") {
        let end = body.find(['/', '?', '#']).unwrap_or(body.len());
        netloc = body[..end].to_string();
        rest = body[end..].to_string();
        if (netloc.contains('[') && !netloc.contains(']')) || (netloc.contains(']') && !netloc.contains('[')) {
            return Err("Invalid IPv6 URL".into());
        }
    }
    if let Some(i) = rest.find('#') {
        rest.truncate(i);
    }
    if let Some(i) = rest.find('?') {
        rest.truncate(i);
    }
    let hostinfo = netloc.rsplit('@').next().unwrap_or("").to_string();
    let (mut hostname, mut port_text): (Option<String>, Option<String>) = (None, None);
    if netloc.is_empty() {
    } else if hostinfo.starts_with('[') && hostinfo.contains(']') {
        let close = hostinfo.find(']').unwrap_or(0);
        hostname = Some(hostinfo[1..close].to_ascii_lowercase());
        if let Some(p) = hostinfo[close + 1..].strip_prefix(':') {
            port_text = Some(p.to_string());
        }
    } else {
        match hostinfo.split_once(':') {
            Some((h, p)) => {
                hostname = Some(h.to_ascii_lowercase());
                port_text = Some(p.to_string());
            }
            None => hostname = Some(hostinfo.to_ascii_lowercase()),
        }
    }
    if hostname.as_deref() == Some("") {
        hostname = None;
    }
    let mut port = None;
    if let Some(p) = port_text.filter(|p| !p.is_empty()) {
        if p.chars().all(|c| c.is_ascii_digit()) {
            let n: u32 = p.parse().map_err(|_| "Port out of range 0-65535".to_string())?;
            if n > 65535 {
                return Err("Port out of range 0-65535".into());
            }
            port = Some(n);
        } else {
            return Err("Port could not be cast to integer value".into());
        }
    }
    Ok((scheme, hostname, port, rest))
}

fn normalise_method(method: &str) -> String {
    let text = method.trim();
    if text == UNKNOWN_METHOD {
        return UNKNOWN_METHOD.to_string();
    }
    if text.eq_ignore_ascii_case(TCP) {
        TCP.to_string()
    } else {
        text.to_uppercase()
    }
}

fn method_matches(entry: &Entry, method: &str, scheme: &str) -> bool {
    let entry_tcp = entry.methods.contains(TCP);
    let request_tcp = method == TCP || (method == UNKNOWN_METHOD && scheme == TCP);
    if entry_tcp != request_tcp {
        return false;
    }
    if request_tcp || method == UNKNOWN_METHOD {
        return true;
    }
    entry.methods.contains(ANY_METHOD) || entry.methods.contains(method)
}

/// `(allowed, kind, reason)`. Method `*` = unknown method: any HTTP entry matches.
pub fn judge(effective: &Effective, method: &str, url: &str, cred: bool, env_names: Option<&[String]>) -> (bool, Kind, String) {
    let method = normalise_method(method);
    let (scheme, host, port, path) = match parse_request_url(url) {
        Ok(parts) => parts,
        Err(error) => return (false, Kind::Unjudgeable, format!("cannot judge the url: {error}")),
    };
    let shown_port = match port {
        Some(p) if default_port(&scheme) != Some(p) => format!(":{p}"),
        _ => String::new(),
    };
    let target = format!("{host}{shown_port}{path}");
    for pattern in &effective.deny {
        if host_matches(pattern, &host) {
            return (false, Kind::Denied, format!("host {host} is denied (egress_never {pattern})"));
        }
    }
    for entry in &effective.allow {
        if entry.origin == ORIGIN_LOCAL && host_matches(&entry.host, &host) {
            return (true, Kind::Ok, "local host".into());
        }
    }
    if !effective.broken.is_empty() {
        return (false, Kind::Broken, format!("{} is unreadable: every non-local request is blocked until it is fixed", effective.broken));
    }
    let defined: Vec<&String> = match env_names {
        Some(names) => names.iter().collect(),
        None => effective.env_names.iter().collect(),
    };
    let mut matching: Vec<&Entry> = Vec::new();
    for entry in &effective.allow {
        if !entry.if_missing.is_empty() && defined.iter().any(|n| **n == entry.if_missing) {
            continue;
        }
        if !host_matches(&entry.host, &host) || entry.port.is_some_and(|p| Some(p) != port) {
            continue;
        }
        if !path_matches(&entry.path, &path) || !method_matches(entry, &method, &scheme) {
            continue;
        }
        matching.push(entry);
    }
    if matching.is_empty() {
        return (false, Kind::NotListed, format!("{method} {target}"));
    }
    if cred {
        let carrying: Vec<&Entry> = matching.iter().copied().filter(|e| e.cred).collect();
        if carrying.is_empty() {
            return (false, Kind::NoCred, format!("{method} {target} is listed ({}) but without `cred`: credentials may not be sent there", matching[0].text));
        }
        matching = carrying;
    }
    (true, Kind::Ok, format!("matches {} ({})", matching[0].text, matching[0].origin))
}

/// The contract call the shared fixture exercises.
pub fn allows(effective: &Effective, method: &str, url: &str, cred: bool, env_names: Option<&[String]>) -> (bool, String) {
    let (allowed, _, reason) = judge(effective, method, url, cred, env_names);
    (allowed, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIXTURE: &str = include_str!("../tests/network_contract.json");

    fn texts(case: &Value) -> Value {
        if let Some(e) = case.get("entries") {
            return e.clone();
        }
        match case.get("entry") {
            Some(e) => json!([e]),
            None => json!([]),
        }
    }

    #[test]
    fn every_judged_case_of_the_shared_contract_passes() {
        let fixture: Value = serde_json::from_str(FIXTURE.trim_start_matches('\u{feff}')).unwrap();
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 112);
        let mut failed = vec![];
        for case in cases {
            let deny: Vec<Value> = case.get("deny").and_then(Value::as_array).cloned().unwrap_or_default();
            let builtin = case.get("builtin_read").and_then(Value::as_bool).unwrap_or(true);
            let env: Vec<String> =
                case.get("env_names").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
            let eff = effective_from(&texts(case), &json!([]), &deny, builtin, &env, vec![], "");
            let req = &case["request"];
            let (allowed, reason) = allows(&eff, req["method"].as_str().unwrap(), req["url"].as_str().unwrap(), req["cred"].as_bool().unwrap_or(false), None);
            if allowed != case["allowed"].as_bool().unwrap() {
                failed.push(format!("{}: got {allowed} ({reason})", case["note"]));
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    #[test]
    fn every_invalid_entry_of_the_shared_contract_is_refused() {
        let fixture: Value = serde_json::from_str(FIXTURE.trim_start_matches('\u{feff}')).unwrap();
        let invalid = fixture["invalid"].as_array().unwrap();
        assert_eq!(invalid.len(), 35);
        let mut failed = vec![];
        for case in invalid {
            let want = case["reason_contains"].as_str().unwrap().to_lowercase();
            match parse_entry(&case["invalid"], ORIGIN_PROJECT) {
                Ok(_) => failed.push(format!("accepted: {}", case["invalid"])),
                Err(reason) if !reason.to_lowercase().contains(&want) => failed.push(format!("{}: '{reason}' lacks '{want}'", case["invalid"])),
                Err(_) => {}
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    #[test]
    fn broken_list_fails_closed_but_local_stays_open() {
        let eff = effective_from(&json!([]), &json!([]), &[], true, &[], vec![], "NETWORK.json");
        assert!(!allows(&eff, "GET", "https://github.com/x", false, None).0);
        assert!(allows(&eff, "GET", "http://localhost:3000/", false, None).0);
    }

    #[test]
    fn loads_the_effective_list_from_disk() {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".specs/project")).unwrap();
        std::fs::write(project.path().join(".specs/project/NETWORK.json"), r#"{"allow": ["api.example.com POST cred"]}"#).unwrap();
        let eff = load_effective(project.path(), data.path(), &[]);
        assert!(allows(&eff, "POST", "https://api.example.com/login", true, None).0);
        std::fs::write(project.path().join(".specs/project/NETWORK.json"), "{oops").unwrap();
        assert_eq!(load_effective(project.path(), data.path(), &[]).broken, NETWORK_LABEL);
    }
}
