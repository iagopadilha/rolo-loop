// SPDX-License-Identifier: MIT
//! One credential redaction for text that leaves the machine or lands in a log, a transcript or a copied spec.
//!
//! [`redact`] returns the text with the VALUE of anything that looks like a credential replaced by the mark (the name, the separator and
//! the quotes stay, so a log line is still readable and a JSON line is still JSON). It masks:
//!   * `NAME=value`, `NAME: value`, `"NAME": "value"` when NAME (letters, digits, `_`, `-`, `.`) contains, anywhere and in any case,
//!     `token`, `secret`, `passw`, `pwd`, `apikey`, `api_key`, `api-key`, `private_key`, `credential`, `auth`, `session` or `cookie`
//!     (`DB_PASSWORD`, `X-Admin-Token`);
//!   * `Authorization: <scheme> <value>` and a bare `Bearer <value>` / `Basic <value>` (the value, not only the word);
//!   * the password of a URL (`scheme://user:password@host` becomes `scheme://user:[redacted]@host`);
//!   * JWT-shaped strings (`eyJ...` three base64url parts) and any run of 40 or more letters, digits, `_`, `=`, `-`.
//!
//! It is a net, not a proof: a secret with no name, no scheme and under 40 characters cannot be told from text. Never fails.

use pycompat::Re;
use std::sync::LazyLock;

pub const REDACTION_MARK: &str = "[redacted]";
const LONG_RUN_LENGTH: usize = 40;
const KEYWORDS: &str = r"token|secret|passw|pwd|api[_-]?key|private[_-]?key|credential|auth|session|cookie";
const AUTH_SCHEMES: [&str; 10] = ["bearer", "basic", "digest", "negotiate", "ntlm", "token", "hawk", "mac", "oauth", "aws4-hmac-sha256"];

// `NAME<sep>value`: the lookbehind starts a name only at the beginning of a run of name characters (no quadratic scan of a long run), the
// lookahead asks for a keyword inside that run, so a pair that is not a credential is never consumed (`url=http://x?token=abc` still
// reaches `token=abc`). Groups: 1 quote, 2 name, 3 separator, 4 scheme, 5 value.
static NAMED_VALUE: LazyLock<Re> = LazyLock::new(|| {
    let schemes = AUTH_SCHEMES.iter().map(|s| pycompat::escape(s)).collect::<Vec<_>>().join("|");
    Re::ci(&format!(
        r#"(?<![\w.-])(?P<q>["']?)(?=[\w.-]*(?:{KEYWORDS}))(?P<name>[\w.-]+)\k<q>(?P<sep>[ \t]*[:=][ \t]*)(?P<scheme>(?:{schemes})[ \t]+(?=[^\s"']))?(?P<value>"(?:[^"\\]|\\.)*"|'[^']*'|["']?[^\s"']*[^\s"',;])"#
    ))
});
// groups: 1 scheme, 2 gap, 3 value
static BARE_SCHEME: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?<![\w-])(bearer|basic)([ \t]+)([A-Za-z0-9._~+/=-]{6,})"));
// groups: 1 head, 2 password
static URL_PASSWORD: LazyLock<Re> = LazyLock::new(|| Re::new(r#"((?<![A-Za-z0-9+.-])[A-Za-z][A-Za-z0-9+.-]*://[^\s/@:"']*:)([^\s/"']*)(?=@[^\s@/"'])"#));
static JWT: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?<![\w-])eyJ[\w-]*\.[\w-]+\.[\w-]*"));
static LONG_RUN: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[A-Za-z0-9_=-]{{{LONG_RUN_LENGTH},}}")));
static BASIC_DIGIT: LazyLock<Re> = LazyLock::new(|| Re::new(r"[0-9=+/]"));
static BASIC_CASE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[a-z][A-Z]"));

/// The pattern the NAME=value redaction uses (shared with `cli_output`'s suspicious-text check).
pub fn named_value_pattern() -> &'static Re {
    &NAMED_VALUE
}

/// The pattern for a long opaque run.
pub fn long_run_pattern() -> &'static Re {
    &LONG_RUN
}

fn is_mark(value: &str, mark: &str) -> bool {
    let inner = value.trim_matches(['"', '\'']);
    inner == mark || inner == REDACTION_MARK || inner == "***"
}

/// The mark in the place of `value`, inside the same quotes when it was quoted.
fn masked_value(value: &str, mark: &str) -> String {
    let quote = match value.chars().next() {
        Some(c @ ('"' | '\'')) => c.to_string(),
        _ => String::new(),
    };
    let closing = if value.chars().count() > 1 && value.ends_with(&quote) { quote.clone() } else { String::new() };
    format!("{quote}{mark}{closing}")
}

/// `Basic <value>` is also plain English ("Basic configuration"): only a base64-looking word counts.
fn looks_like_basic_credential(value: &str) -> bool {
    BASIC_DIGIT.search(value).is_some() || BASIC_CASE.search(value).is_some()
}

/// `text` without credentials (see the module text), clipped to `limit` characters when it is given.
pub fn redact(text: &str, limit: Option<usize>, mark: &str) -> String {
    let named = NAMED_VALUE.replace_all(text, |m| {
        let value = m.group(5).unwrap_or("");
        if is_mark(value, mark) {
            return m.group(0).unwrap_or("").to_string();
        }
        let quote = m.group(1).unwrap_or("");
        format!("{quote}{}{quote}{}{}{}", m.group(2).unwrap_or(""), m.group(3).unwrap_or(""), m.group(4).unwrap_or(""), masked_value(value, mark))
    });
    let bare = BARE_SCHEME.replace_all(&named, |m| {
        let (scheme, value) = (m.group(1).unwrap_or(""), m.group(3).unwrap_or(""));
        if scheme.eq_ignore_ascii_case("basic") && !looks_like_basic_credential(value) {
            return m.group(0).unwrap_or("").to_string();
        }
        format!("{scheme}{}{mark}", m.group(2).unwrap_or(""))
    });
    let urls = URL_PASSWORD.replace_all(&bare, |m| format!("{}{mark}", m.group(1).unwrap_or("")));
    let jwt = JWT.replace_lit(&urls, mark);
    let long = LONG_RUN.replace_lit(&jwt, mark);
    match limit {
        Some(n) if n > 0 => long.chars().take(n).collect(),
        _ => long,
    }
}

/// [`redact`] with the standard mark and no limit.
pub fn redact_default(text: &str) -> String {
    redact(text, None, REDACTION_MARK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_named_values_keeping_the_shape() {
        assert_eq!(redact_default("DB_PASSWORD=hunter22 ok"), "DB_PASSWORD=[redacted] ok");
        assert_eq!(redact_default(r#"{"api_key": "abc123xyz"}"#), r#"{"api_key": "[redacted]"}"#);
        assert_eq!(redact_default("url=http://x?token=abc&b=1"), "url=http://x?token=[redacted]");
        assert_eq!(redact_default("cookie='a b'; session=xyz"), "cookie='[redacted]'; session=[redacted]");
        assert_eq!(redact_default("curl -H 'X-Api-Key: abcdef' https://h"), "curl -H 'X-Api-Key: [redacted]' https://h");
        assert_eq!(redact_default("auth: \"q\" next"), "auth: \"[redacted]\" next");
        assert_eq!(redact_default("Authorization: Bearer abc.def.ghi"), "Authorization: Bearer [redacted]");
        assert_eq!(redact_default("X-Admin-Token: s3cr3t"), "X-Admin-Token: [redacted]");
        assert_eq!(redact_default("password=***"), "password=***");
        assert_eq!(redact_default("nothing here"), "nothing here");
    }

    #[test]
    fn masks_bare_schemes_urls_jwts_and_long_runs() {
        assert_eq!(redact_default("send Bearer abcdef123456 now"), "send Bearer [redacted] now");
        assert_eq!(redact_default("Basic configuration is fine"), "Basic configuration is fine");
        assert_eq!(redact_default("Basic dXNlcjpwYXNz=="), "Basic [redacted]");
        assert_eq!(redact_default("postgres://user:pw0rd@db.example.com/x"), "postgres://user:[redacted]@db.example.com/x");
        assert_eq!(redact_default("eyJhbGciOi.eyJzdWIiOiIx.sig-1"), "[redacted]");
        assert_eq!(redact_default(&format!("id {}", "a".repeat(40))), "id [redacted]");
        assert_eq!(redact("short", Some(3), REDACTION_MARK), "sho");
    }
}
