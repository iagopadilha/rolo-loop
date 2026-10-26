// SPDX-License-Identifier: MIT
//! Email providers behind the `email` notification channel. `LOOP_EMAIL_PROVIDER` chooses:
//! `smtp` (default): LOOP_SMTP_HOST / PORT / USER / PASSWORD / FROM / SECURITY (starttls|ssl|none);
//! `http`: a mail service reached by POST (LOOP_EMAIL_HTTP_URL, optional LOOP_EMAIL_HTTP_TOKEN as Bearer, LOOP_EMAIL_HTTP_FROM),
//! body JSON `{to, subject, html, from?}`. Recipients are always LOOP_NOTIFY_TO.

use cfg::Env;
use lettre::message::{header::ContentType, Mailbox, Message};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{SmtpTransport, Transport};
use serde_json::json;
use std::time::Duration;

pub const DEFAULT_PROVIDER: &str = "smtp";
const DEFAULT_SMTP_PORT: u16 = 587;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SEND_ATTEMPTS: usize = 3;
const RETRY_WAIT: Duration = Duration::from_secs(3);
const ERROR_DETAIL_LIMIT: usize = 150;
const SERVER_ANSWER_LIMIT: usize = 200;

pub fn chosen_provider(env: &Env) -> String {
    let v = env.text("LOOP_EMAIL_PROVIDER");
    let v = v.trim();
    if v.is_empty() { DEFAULT_PROVIDER.to_string() } else { v.to_lowercase() }
}

/// Variables the chosen provider needs (an unknown provider needs LOOP_EMAIL_PROVIDER to be fixed).
pub fn required_variables(env: &Env) -> Vec<String> {
    match chosen_provider(env).as_str() {
        "smtp" => ["LOOP_NOTIFY_TO", "LOOP_SMTP_HOST", "LOOP_SMTP_FROM"].map(String::from).to_vec(),
        "http" => ["LOOP_NOTIFY_TO", "LOOP_EMAIL_HTTP_URL"].map(String::from).to_vec(),
        _ => vec!["LOOP_NOTIFY_TO".into(), "LOOP_EMAIL_PROVIDER (smtp|http)".into()],
    }
}

pub fn recipients(env: &Env) -> Vec<String> {
    env.text("LOOP_NOTIFY_TO").split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

fn clip(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

fn mask(text: &str, secret: &str) -> String {
    if secret.is_empty() { text.to_string() } else { text.replace(secret, "***") }
}

/// `(ok, detail)` of sending `html_body` through the chosen provider; `None` when the provider is unknown.
pub fn send(env: &Env, recipients: &[String], subject: &str, html_body: &str) -> Option<(bool, String)> {
    match chosen_provider(env).as_str() {
        "smtp" => Some(send_smtp(env, recipients, subject, html_body)),
        "http" => Some(send_http(env, recipients, subject, html_body)),
        _ => None,
    }
}

fn send_smtp(env: &Env, recipients: &[String], subject: &str, html_body: &str) -> (bool, String) {
    let host = env.text("LOOP_SMTP_HOST");
    let port = env.text("LOOP_SMTP_PORT").trim().parse::<u16>().unwrap_or(DEFAULT_SMTP_PORT);
    let (user, password) = (env.text("LOOP_SMTP_USER"), env.text("LOOP_SMTP_PASSWORD"));
    let sender = env.text("LOOP_SMTP_FROM");
    let security = { let s = env.text("LOOP_SMTP_SECURITY"); if s.trim().is_empty() { "starttls".to_string() } else { s.trim().to_lowercase() } };
    let from: Mailbox = match sender.parse() {
        Ok(m) => m,
        Err(e) => return (false, format!("no response from the SMTP server: {}", clip(&e.to_string(), ERROR_DETAIL_LIMIT))),
    };
    let mut builder = Message::builder().from(from).subject(subject);
    for r in recipients {
        match r.parse::<Mailbox>() {
            Ok(m) => builder = builder.to(m),
            Err(e) => return (false, format!("no response from the SMTP server: {}", clip(&e.to_string(), ERROR_DETAIL_LIMIT))),
        }
    }
    let message = match builder.header(ContentType::TEXT_HTML).body(html_body.to_string()) {
        Ok(m) => m,
        Err(e) => return (false, format!("no response from the SMTP server: {}", clip(&e.to_string(), ERROR_DETAIL_LIMIT))),
    };
    let mut last = String::new();
    for attempt in 0..SEND_ATTEMPTS {
        let tls = TlsParameters::new(host.clone());
        let relay = match (security.as_str(), tls) {
            ("ssl", Ok(t)) => SmtpTransport::relay(&host).map(|b| b.tls(Tls::Wrapper(t))),
            ("none", _) => Ok(SmtpTransport::builder_dangerous(&host).tls(Tls::None)),
            (_, Ok(t)) => SmtpTransport::starttls_relay(&host).map(|b| b.tls(Tls::Required(t))),
            (_, Err(e)) => {
                last = clip(&e.to_string(), ERROR_DETAIL_LIMIT);
                std::thread::sleep(RETRY_WAIT);
                continue;
            }
        };
        let mut transport = match relay {
            Ok(b) => b.port(port).timeout(Some(REQUEST_TIMEOUT)),
            Err(e) => {
                last = clip(&e.to_string(), ERROR_DETAIL_LIMIT);
                std::thread::sleep(RETRY_WAIT);
                continue;
            }
        };
        if !user.is_empty() {
            transport = transport.credentials(Credentials::new(user.clone(), password.clone()));
        }
        match transport.build().send(&message) {
            Ok(_) => return (true, format!("sent to {}", recipients.join(", "))),
            Err(e) => {
                let text = e.to_string();
                if e.is_permanent() && text.to_lowercase().contains("auth") {
                    let code = e.status().map(|c| c.to_string()).unwrap_or_default();
                    return (false, format!("SMTP authentication failed ({code})"));
                }
                last = mask(&clip(&text, ERROR_DETAIL_LIMIT), &password);
                if attempt + 1 < SEND_ATTEMPTS {
                    std::thread::sleep(RETRY_WAIT);
                }
            }
        }
    }
    (false, format!("no response from the SMTP server: {last}"))
}

fn send_http(env: &Env, recipients: &[String], subject: &str, html_body: &str) -> (bool, String) {
    let url = env.text("LOOP_EMAIL_HTTP_URL");
    let token = env.text("LOOP_EMAIL_HTTP_TOKEN");
    let sender = env.text("LOOP_EMAIL_HTTP_FROM");
    let mut payload = json!({"to": recipients, "subject": subject, "html": html_body});
    if !sender.is_empty() {
        payload["from"] = json!(sender);
    }
    let body = pycompat::json::py_json_dumps(&payload, true);
    let mut last = String::new();
    for attempt in 0..SEND_ATTEMPTS {
        let mut request = ureq::post(&url).timeout(REQUEST_TIMEOUT).set("Content-Type", "application/json");
        if !token.is_empty() {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        match request.send_string(&body) {
            Ok(response) => {
                let status = response.status();
                return ((200..300).contains(&status), format!("HTTP {status}, sent to {}", recipients.join(", ")));
            }
            Err(ureq::Error::Status(code, response)) => {
                let answer = clip(&response.into_string().unwrap_or_default(), SERVER_ANSWER_LIMIT);
                return (false, format!("HTTP {code} {}", mask(&answer, &token)));
            }
            Err(e) => {
                last = clip(&e.to_string(), ERROR_DETAIL_LIMIT);
                if attempt + 1 < SEND_ATTEMPTS {
                    std::thread::sleep(RETRY_WAIT);
                }
            }
        }
    }
    (false, format!("no response from the email service: {last}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_and_requirements() {
        let env = Env::from_pairs([("LOOP_NOTIFY_TO", "a@example.com, b@example.com")]);
        assert_eq!(chosen_provider(&env), "smtp");
        assert_eq!(required_variables(&env), vec!["LOOP_NOTIFY_TO", "LOOP_SMTP_HOST", "LOOP_SMTP_FROM"]);
        assert_eq!(recipients(&env), vec!["a@example.com", "b@example.com"]);
        let weird = Env::from_pairs([("LOOP_EMAIL_PROVIDER", "pigeon")]);
        assert_eq!(required_variables(&weird)[1], "LOOP_EMAIL_PROVIDER (smtp|http)");
        assert!(send(&weird, &[], "s", "b").is_none());
    }
}
