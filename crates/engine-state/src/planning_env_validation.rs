// SPDX-License-Identifier: MIT
//! Static checks of the planning gate "Test environment" (ENV-01): TEST-ENVIRONMENT.md against the project's `.env.test` values and the effective
//! network list. Pure functions: (doc text, env values, effective network, now, loop_max_hours) -> `{problems, warnings}`; the only I/O is
//! [`validate_project`], a small separate helper that reads the two files from a project root.
//!
//! Every item is `{code, name, message}`. `name` is the variable NAME (or the claim name) and `message` is readable text: neither ever holds a VALUE
//! of `.env.test` (not a password, not a token, not an e-mail, not a claim of a token). The one value-derived piece that may appear is the HOST of a
//! `*_URL` value, because the fix is to list that host (URL values are non-secret by convention, the same rule the guard uses).
//!
//! Checks:
//! - every `$NAME` of TEST-ENVIRONMENT.md has a non-empty value (`SAMPLE_*` test data: warning; `TEST_USER_<n>_*` come from the `;` lists);
//! - `*_EMAIL` / `*_EMAILS` hold e-mail addresses and `*_URL` hold http(s)/ws(s) URLs;
//! - `TEST_USER_EMAILS`, `TEST_USER_PASSWORDS` and `TEST_USER_ROLES` have the same number of `;` items (`\;` = a literal `;`);
//! - the host of every `*_URL` value matches an effective network entry (method `*`: the URL is only a literal); a host in `egress_never` = problem;
//! - a JWT value (three base64url parts): decodable header and payload, `exp` later than now + `loop_max_hours`, and each claim of the doc's
//!   `Expected token claims: aud=...; domain=...` line present (a list claim must contain the expected value); more values than expected (more apps
//!   or companies) = warning.

use crate::network_policy;
use egress_policy::{allows, host_matches, Effective, UNKNOWN_METHOD};
use pycompat::{format_g, Re};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

pub const DOC_RELATIVE: [&str; 3] = [".specs", "project", "TEST-ENVIRONMENT.md"];
pub const ENV_RELATIVE: [&str; 3] = [".specs", "project", ".env.test"];
pub const DEFAULT_LOOP_MAX_HOURS: f64 = 24.0;
const LIST_SEPARATOR: &str = ";";
const ESCAPED_SEPARATOR: &str = "\\;";
const USER_COLUMNS: [(&str, &str); 3] = [("EMAILS", "EMAIL"), ("PASSWORDS", "PASSWORD"), ("ROLES", "ROLE")];
static NAME_REFERENCE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\$\{?([A-Z][A-Z0-9_]{2,})\}?"));
/// Words of the template prose, never credential names.
const PLACEHOLDER_WORDS: [&str; 2] = ["NAME", "NAMES"];
/// Test data of one feature: missing = warning.
const OPTIONAL_NAME_PREFIX: &str = "SAMPLE_";
static EMAIL: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[^@\s;,<>]+@[^@\s;,<>]+\.[^@\s;,<>]+\z"));
const URL_SCHEMES: [&str; 4] = ["http", "https", "ws", "wss"];
static JWT_SHAPE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z0-9_-]{2,}\.[A-Za-z0-9_-]{2,}\.[A-Za-z0-9_-]*\z"));
static EXPECTED_CLAIMS_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?im)^[^\w\n]*Expected token claims\s*:\s*(.+)$"));
static TOKEN_NAME: LazyLock<Re> = LazyLock::new(|| Re::new(r"_TOKEN\z"));

type Values = BTreeMap<String, String>;

fn item(code: &str, name: &str, message: String) -> Value {
    json!({"code": code, "name": name, "message": message})
}

/// The items of a `;` list (`\;` is a literal `;`); '' = no items.
pub fn split_values(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![];
    }
    text.replace(ESCAPED_SEPARATOR, "\0").split(LIST_SEPARATOR).map(|part| part.replace('\0', LIST_SEPARATOR).trim().to_string()).collect()
}

/// The `$NAME`s of the document, in order of appearance, without repeats.
pub fn referenced_names(doc_text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for found in NAME_REFERENCE.find_all(doc_text) {
        let name = found[1].to_string();
        if !PLACEHOLDER_WORDS.contains(&name.as_str()) && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// `values` plus TEST_USER_<n>_EMAIL/PASSWORD/ROLE and TEST_USER_COUNT taken from the `;` lists (same rule as credential_check).
pub fn with_derived_users(values: &Values) -> Values {
    let mut result = values.clone();
    let columns: Vec<(&str, Vec<String>)> = USER_COLUMNS.iter().map(|(plural, single)| (*single, split_values(values.get(&format!("TEST_USER_{plural}")).map(String::as_str).unwrap_or("")))).collect();
    let count = columns.iter().map(|(_, items)| items.len()).max().unwrap_or(0);
    for number in 1..=count {
        for (single, items) in &columns {
            if number <= items.len() && !items[number - 1].is_empty() {
                result.insert(format!("TEST_USER_{number}_{single}"), items[number - 1].clone());
            }
        }
    }
    result.insert("TEST_USER_COUNT".into(), count.to_string());
    result
}

fn value_of<'a>(values: &'a Values, name: &str) -> &'a str {
    values.get(name).map(String::as_str).unwrap_or("")
}

// ---- checks (each returns problems, warnings) ----------------------------------------------------------------------------------

fn check_names_defined(doc_text: &str, values: &Values) -> (Vec<Value>, Vec<Value>) {
    let (mut problems, mut warnings) = (vec![], vec![]);
    for name in referenced_names(doc_text) {
        if !value_of(values, &name).trim().is_empty() {
            continue;
        }
        if name.starts_with(OPTIONAL_NAME_PREFIX) {
            warnings.push(item("value_missing_sample", &name, format!("{name} (test data) has no value in .env.test: the checks that use it cannot run")));
        } else {
            problems.push(item("value_missing", &name, format!("{name} is written in TEST-ENVIRONMENT.md but has no value in .env.test")));
        }
    }
    (problems, warnings)
}

fn check_formats(values: &Values) -> Vec<Value> {
    let mut problems = Vec::new();
    for (name, raw) in values {
        let value = raw.trim();
        if value.is_empty() {
            continue;
        }
        if name.ends_with("_EMAILS") || name.ends_with("_EMAIL") {
            let items = if name.ends_with("_EMAILS") { split_values(value) } else { vec![value.to_string()] };
            let bad: Vec<String> = items.iter().enumerate().filter(|(_, i)| !EMAIL.is_match(i)).map(|(index, _)| (index + 1).to_string()).collect();
            if !bad.is_empty() {
                problems.push(item("email_invalid", name, format!("{name}: item(s) {} are not an e-mail address", bad.join(", "))));
            }
        } else if name.ends_with("_URL") && url_host(value).is_empty() {
            problems.push(item("url_invalid", name, format!("{name} is not a valid http(s) or ws(s) URL")));
        }
    }
    problems
}

fn check_user_lists(values: &Values) -> Vec<Value> {
    let mut problems = Vec::new();
    let mut lengths: BTreeMap<String, usize> = BTreeMap::new();
    for (plural, _) in USER_COLUMNS {
        let name = format!("TEST_USER_{plural}");
        let text = value_of(values, &name).trim().to_string();
        if text.is_empty() {
            continue;
        }
        let items = split_values(&text);
        lengths.insert(name.clone(), items.len());
        if items.iter().any(|i| i.is_empty()) && plural != "ROLES" {
            // a role may be left empty for one user
            problems.push(item("list_empty_item", &name, format!("{name} has an empty position (two `;` in a row or a trailing `;`)")));
        }
    }
    let distinct: std::collections::BTreeSet<&usize> = lengths.values().collect();
    if distinct.len() > 1 {
        let detail = lengths.iter().map(|(name, count)| format!("{name}={count}")).collect::<Vec<_>>().join(", ");
        for name in lengths.keys() {
            problems.push(item("list_misaligned", name, format!("the test user lists must have the same number of items, by position ({detail})")));
        }
    }
    problems
}

/// Host of an http(s)/ws(s) URL value, or '' when it is not one.
fn url_host(value: &str) -> String {
    let Ok(parts) = pycompat::urlsplit(value.trim()) else { return String::new() };
    match parts.hostname {
        Some(host) if !host.is_empty() && URL_SCHEMES.contains(&parts.scheme.to_lowercase().as_str()) => host.to_lowercase(),
        _ => String::new(),
    }
}

fn check_url_hosts(values: &Values, effective: &Effective) -> Vec<Value> {
    let mut problems = Vec::new();
    for (name, raw) in values {
        let value = raw.trim();
        if !name.ends_with("_URL") || value.is_empty() || url_host(value).is_empty() {
            continue;
        }
        let host = url_host(value);
        if let Some(denied) = effective.deny.iter().find(|pattern| host_matches(pattern, &host)) {
            problems.push(item("url_host_denied", name, format!("{name} points at host {host}, which is denied by egress_never ({denied}): use a DEV or local address")));
            continue;
        }
        let (allowed, _) = allows(effective, UNKNOWN_METHOD, value, false, Some(&effective.env_names));
        if !allowed {
            problems.push(item("url_host_not_listed", name, format!("{name} points at host {host}, which is not in the network list: add it in the Network section of the Test environment tab")));
        }
    }
    problems
}

/// URL-safe base64 as Python's `urlsafe_b64decode(part + "=" * (-len(part) % 4))`; `None` where it raises.
fn b64_decode(part: &str) -> Option<Vec<u8>> {
    if part.len() % 4 == 1 {
        return None;
    }
    let mut bits = 0u32;
    let mut count = 0u32;
    let mut out = Vec::new();
    for c in part.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push(((bits >> count) & 0xff) as u8);
        }
    }
    Some(out)
}

fn b64_json(part: &str) -> Option<Map<String, Value>> {
    let bytes = b64_decode(part)?;
    match serde_json::from_slice::<Value>(&bytes).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// `(header, payload)` of a JWT-shaped value, or `None` when it is not a decodable JWT. The signature is never checked.
pub fn decode_jwt(value: &str) -> Option<(Map<String, Value>, Map<String, Value>)> {
    let mut text = value.trim();
    if text.to_lowercase().starts_with("bearer ") {
        text = text[7..].trim();
    }
    if !JWT_SHAPE.is_match(text) {
        return None;
    }
    let parts: Vec<&str> = text.split('.').collect();
    Some((b64_json(parts[0])?, b64_json(parts[1])?))
}

/// `{claim: [expected values]}` of the doc's `Expected token claims: aud=a; domain=b,c` line (`{}` without the line).
pub fn expected_claims(doc_text: &str) -> Vec<(String, Vec<String>)> {
    let mut claims: Vec<(String, Vec<String>)> = Vec::new();
    let Some(found) = EXPECTED_CLAIMS_LINE.search(doc_text) else { return claims };
    for part in found[1].replace(['`', '*'], "").split(';') {
        let (key, raw) = part.split_once('=').unwrap_or((part, ""));
        let wanted: Vec<String> = raw.split(',').map(|i| i.trim().to_string()).filter(|i| !i.is_empty()).collect();
        if !key.trim().is_empty() && !wanted.is_empty() {
            claims.retain(|(k, _)| k != key.trim());
            claims.push((key.trim().to_string(), wanted));
        }
    }
    claims
}

fn claim_values(claim: &Value) -> Vec<String> {
    let items: Vec<&Value> = match claim {
        Value::Array(list) => list.iter().collect(),
        other => vec![other],
    };
    items
        .into_iter()
        .filter(|i| !matches!(i, Value::Object(_) | Value::Array(_) | Value::Null))
        .map(|i| match i {
            Value::String(s) => s.clone(),
            Value::Bool(true) => "True".into(),
            Value::Bool(false) => "False".into(),
            other => other.to_string(),
        })
        .collect()
}

fn check_tokens(doc_text: &str, values: &Values, now: f64, loop_max_hours: f64) -> (Vec<Value>, Vec<Value>) {
    let (mut problems, mut warnings) = (vec![], vec![]);
    let wanted = expected_claims(doc_text);
    let hours = if loop_max_hours > 0.0 { loop_max_hours } else { DEFAULT_LOOP_MAX_HOURS };
    let limit = now + hours * 3600.0;
    for (name, raw) in values {
        let value = raw.trim();
        if value.is_empty() {
            continue;
        }
        let shaped = if value.to_lowercase().starts_with("bearer ") { value[7..].trim() } else { value };
        let looks_like_jwt = JWT_SHAPE.is_match(shaped);
        let decoded = if looks_like_jwt { decode_jwt(value) } else { None };
        if looks_like_jwt && decoded.is_none() {
            problems.push(item("jwt_undecodable", name, format!("{name} has the shape of a JWT but its header or payload cannot be decoded")));
            continue;
        }
        let Some((_, payload)) = decoded else {
            if !wanted.is_empty() && TOKEN_NAME.is_match(name) {
                warnings.push(item("token_not_jwt", name, format!("{name} is not a JWT: the expected token claims cannot be checked")));
            }
            continue;
        };
        if let Some(exp) = payload.get("exp").filter(|e| !e.is_null()) {
            match exp.as_f64().filter(|_| exp.is_number()) {
                None => problems.push(item("jwt_exp_invalid", name, format!("{name} has an `exp` claim that is not a number"))),
                Some(exp) if exp <= now => problems.push(item("jwt_expired", name, format!("{name} is expired: get a new token"))),
                Some(exp) if exp < limit => problems.push(item("jwt_expiring", name, format!("{name} expires within the {} hour(s) a loop may run (loop_max_hours): get a longer-lived token", format_g(hours)))),
                Some(_) => {}
            }
        }
        for (claim, expected) in &wanted {
            let Some(actual_value) = payload.get(claim) else {
                problems.push(item("jwt_claim_mismatch", name, format!("{name} has no `{claim}` claim, TEST-ENVIRONMENT.md expects one")));
                continue;
            };
            let actual = claim_values(actual_value);
            if expected.iter().any(|i| !actual.contains(i)) {
                problems.push(item("jwt_claim_mismatch", name, format!("the `{claim}` claim of {name} does not match TEST-ENVIRONMENT.md (Expected token claims)")));
            } else if actual.iter().any(|i| !expected.contains(i)) {
                let extra = actual.iter().filter(|i| !expected.contains(i)).count();
                warnings.push(item("jwt_extra_scope", name, format!("the `{claim}` claim of {name} lists {extra} more value(s) than expected: the account has a wider scope than the test needs")));
            }
        }
    }
    (problems, warnings)
}

// ---- entry points ----------------------------------------------------------------------------------------------------------------

/// ENV-01 checks -> `{"problems": [{code, name, message}], "warnings": [...]}`. `env_values` = `{NAME: value}` of `.env.test` (values are read,
/// never returned); `effective` = the network_policy shape; `now` = epoch seconds.
pub fn validate(doc_text: &str, env_values: &Values, effective: &Effective, now: f64, loop_max_hours: f64) -> Value {
    let values = with_derived_users(env_values);
    let (mut problems, warnings) = check_names_defined(doc_text, &values);
    problems.extend(check_formats(env_values));
    problems.extend(check_user_lists(&values));
    problems.extend(check_url_hosts(&values, effective));
    let (token_problems, token_warnings) = check_tokens(doc_text, &values, now, loop_max_hours);
    problems.extend(token_problems);
    let mut all_warnings = warnings;
    all_warnings.extend(token_warnings);
    json!({"problems": problems, "warnings": all_warnings})
}

/// `{NAME: value}` of an `.env` style file (`{}` when it does not exist). Values never leave the caller.
pub fn read_env_values(path: &Path) -> Values {
    let mut values = Values::new();
    let Some(text) = cfg::env::read_text_sig(path) else { return values };
    for raw in text.lines() {
        let mut line = raw.trim();
        if line.is_empty() || line.starts_with('#') || !line.contains('=') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim_start();
        }
        let (name, value) = line.split_once('=').unwrap_or((line, ""));
        let mut value = value.trim();
        let chars: Vec<char> = value.chars().collect();
        if chars.len() >= 2 && chars[0] == chars[chars.len() - 1] && (chars[0] == '\'' || chars[0] == '"') {
            value = &value[1..value.len() - 1];
        }
        if !name.trim().is_empty() {
            values.insert(name.trim().to_string(), value.to_string());
        }
    }
    values
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Reads `.specs/project/TEST-ENVIRONMENT.md`, `.env.test` and the effective network list of `project_root` and runs [`validate`]. `exists` false
/// (no TEST-ENVIRONMENT.md) = nothing to check, so the gate does not block.
pub fn validate_project(project_root: &str, data_dir: &Path, now: Option<f64>, loop_max_hours: f64) -> Value {
    let doc_path = DOC_RELATIVE.iter().fold(Path::new(project_root).to_path_buf(), |a, p| a.join(p));
    let Some(doc_text) = cfg::env::read_text_sig(&doc_path) else { return json!({"exists": false, "problems": [], "warnings": []}) };
    let values = read_env_values(&ENV_RELATIVE.iter().fold(Path::new(project_root).to_path_buf(), |a, p| a.join(p)));
    let effective = network_policy::load(project_root, data_dir, None);
    let mut result = validate(&doc_text, &values, &effective, now.unwrap_or_else(now_epoch), loop_max_hours);
    result["exists"] = json!(true);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effective(allow_text: &[&str]) -> Effective {
        let entries: Vec<Value> = allow_text.iter().map(|t| json!(t)).collect();
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p");
        std::fs::create_dir_all(project.join(".specs/project")).unwrap();
        std::fs::write(project.join(".specs/project/NETWORK.json"), pycompat::json::py_json_dumps(&json!({"version": 1, "allow": entries}), true)).unwrap();
        network_policy::load(&project.display().to_string(), &dir.path().join("data"), None)
    }

    fn jwt(payload: Value) -> String {
        fn encode(bytes: &[u8]) -> String {
            const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for chunk in bytes.chunks(3) {
                let n = chunk.iter().enumerate().fold(0u32, |a, (i, b)| a | (u32::from(*b) << (16 - 8 * i)));
                for i in 0..=chunk.len() {
                    out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
                }
            }
            out
        }
        format!("{}.{}.sig", encode(br#"{"alg":"none"}"#), encode(payload.to_string().as_bytes()))
    }

    fn map(pairs: &[(&str, &str)]) -> Values {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn codes(result: &Value, key: &str) -> Vec<String> {
        result[key].as_array().unwrap().iter().map(|i| i["code"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn names_formats_lists_and_hosts() {
        let doc = "uses $API_URL and $TEST_USER_1_EMAIL and ${DB_PASSWORD} and $SAMPLE_ID; $NAME is prose";
        let values = map(&[("API_URL", "https://api.dev.example.com/x"), ("TEST_USER_EMAILS", "a@b.co;bad"), ("TEST_USER_PASSWORDS", "p1;p2;p3"), ("OTHER_URL", "ftp://x"), ("DB_PASSWORD", " ")]);
        let result = validate(doc, &values, &effective(&["api.dev.example.com"]), 1_000.0, 24.0);
        let mut problems = codes(&result, "problems");
        problems.sort();
        assert_eq!(problems, ["email_invalid", "list_misaligned", "list_misaligned", "url_invalid", "value_missing"]);
        assert_eq!(codes(&result, "warnings"), ["value_missing_sample"]);
        let unlisted = validate("", &map(&[("API_URL", "https://nope.example.com")]), &effective(&[]), 0.0, 24.0);
        assert_eq!(codes(&unlisted, "problems"), ["url_host_not_listed"]);
        assert!(!unlisted.to_string().contains("secret"));
        assert_eq!(split_values("a\\;b;c"), ["a;b", "c"]);
        assert_eq!(referenced_names("$AAA $BBB $AAA ${CCC} $NAMES $lower"), ["AAA", "BBB", "CCC"]);
    }

    #[test]
    fn tokens_are_decoded_and_judged() {
        let now = 1_000_000.0;
        let doc = "Expected token claims: aud=app1; roles=admin,user";
        let ok = jwt(json!({"exp": now + 100.0 * 3600.0, "aud": "app1", "roles": ["admin", "user"]}));
        let result = validate(doc, &map(&[("A_TOKEN", &ok)]), &effective(&[]), now, 24.0);
        assert!(codes(&result, "problems").is_empty() && codes(&result, "warnings").is_empty());
        let expired = jwt(json!({"exp": now - 1.0, "aud": "app1", "roles": ["admin", "user"]}));
        assert_eq!(codes(&validate(doc, &map(&[("A_TOKEN", &expired)]), &effective(&[]), now, 24.0), "problems"), ["jwt_expired"]);
        let soon = jwt(json!({"exp": now + 3600.0, "aud": "app1", "roles": ["admin", "user"]}));
        let result = validate(doc, &map(&[("A_TOKEN", &soon)]), &effective(&[]), now, 24.0);
        assert_eq!(codes(&result, "problems"), ["jwt_expiring"]);
        assert!(result["problems"][0]["message"].as_str().unwrap().contains("within the 24 hour(s)"));
        let wrong = jwt(json!({"aud": "other", "roles": ["admin", "user", "root"]}));
        assert_eq!(codes(&validate(doc, &map(&[("A_TOKEN", &wrong)]), &effective(&[]), now, 24.0), "problems"), ["jwt_claim_mismatch"]);
        let wide = jwt(json!({"aud": "app1", "roles": ["admin", "user", "root"]}));
        assert_eq!(codes(&validate(doc, &map(&[("A_TOKEN", &wide)]), &effective(&[]), now, 24.0), "warnings"), ["jwt_extra_scope"]);
        assert_eq!(codes(&validate(doc, &map(&[("A_TOKEN", "abc.def.ghi")]), &effective(&[]), now, 24.0), "problems"), ["jwt_undecodable"]);
        assert_eq!(codes(&validate(doc, &map(&[("B_TOKEN", "plain")]), &effective(&[]), now, 24.0), "warnings"), ["token_not_jwt"]);
        assert_eq!(codes(&validate(doc, &map(&[("A_TOKEN", &format!("Bearer {}", jwt(json!({"exp": "x", "aud": "app1", "roles": ["admin", "user"]}))))]), &effective(&[]), now, 24.0), "problems"), ["jwt_exp_invalid"]);
    }

    #[test]
    fn a_project_without_the_document_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        assert_eq!(validate_project(&root, &dir.path().join("data"), None, 24.0)["exists"], false);
        std::fs::create_dir_all(dir.path().join(".specs/project")).unwrap();
        std::fs::write(dir.path().join(".specs/project/TEST-ENVIRONMENT.md"), "needs $API_KEY_X").unwrap();
        std::fs::write(dir.path().join(".specs/project/.env.test"), "API_KEY_X='v'\n").unwrap();
        let result = validate_project(&root, &dir.path().join("data"), Some(0.0), 24.0);
        assert_eq!((result["exists"].clone(), result["problems"].clone()), (json!(true), json!([])));
    }
}
