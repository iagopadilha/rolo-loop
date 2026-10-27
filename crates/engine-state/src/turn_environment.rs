// SPDX-License-Identifier: MIT
//! The environment a loop turn (`claude -p`) starts with, minus the secrets that only came from the app `.env`.
//!
//! The engine loads `<LOOP_DATA_DIR or ~/.loop-harness>/.env` into its own process (notice channels, git identity...). Every child process
//! inherits it, so a turn could read the mail password or a webhook address with a plain `echo`. The turn does not need them: notices are
//! sent by the engine, outside the turn.
//!
//! A variable is left out only when ALL of these hold:
//!   - its value is the one an app `.env` file gives it (a real variable the user had with a different value stays: it is not ours to hide);
//!   - its name looks like a secret (PASSWORD, PASSWD, SECRET, TOKEN, API_KEY, APIKEY, PRIVATE_KEY, WEBHOOK, CREDENTIAL);
//!   - it does not start with GUARD_ (the guard's settings reach the turn as is).
//!
//! There is no exception for the names TEST-ENVIRONMENT.md mentions or for the generic test user (TEST_USER_*): the project's `.env.test` is
//! the only credential source and its values reach the agents only through `craft testenv exec`, never through the turn's environment.
//!
//! [`build`] returns the environment for the turn and the sorted NAMES left out. Values are never returned in the removed list, and never
//! printed.

use cfg::env::{parse_env, read_text_sig};
use std::path::PathBuf;

const SECRET_NAME_PARTS: [&str; 9] = ["PASSWORD", "PASSWD", "SECRET", "TOKEN", "API_KEY", "APIKEY", "PRIVATE_KEY", "WEBHOOK", "CREDENTIAL"];
/// The guard's own settings (GUARD_SECRET_PATHS holds paths, not a secret).
const KEPT_PREFIXES: [&str; 1] = ["GUARD_"];

pub fn looks_secret(name: &str) -> bool {
    let upper = name.to_uppercase();
    SECRET_NAME_PARTS.iter().any(|p| upper.contains(p))
}

/// `{name: value}` the app `.env` files give. The first file that defines a name wins.
pub fn values_from_env_files(env_paths: &[PathBuf]) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    for path in env_paths {
        if path.as_os_str().is_empty() || !path.is_file() {
            continue;
        }
        let Some(text) = read_text_sig(path) else { continue };
        let mut local: Vec<(String, String)> = Vec::new();
        for (name, value) in parse_env(&text) {
            match local.iter_mut().find(|(n, _)| *n == name) {
                Some(slot) => slot.1 = value,
                None => local.push((name, value)),
            }
        }
        for (name, value) in local {
            if !found.iter().any(|(n, _)| *n == name) {
                found.push((name, value));
            }
        }
    }
    found
}

/// The environment of a turn: a copy of `environ` without the app-.env secrets.
pub struct Built {
    pub env: Vec<(String, String)>,
    pub removed: Vec<String>,
}

pub fn build(environ: &[(String, String)], env_paths: &[PathBuf]) -> Built {
    let from_files = values_from_env_files(env_paths);
    let mut removed: Vec<String> = environ
        .iter()
        .filter(|(name, value)| from_files.iter().any(|(n, v)| n == name && v == value) && looks_secret(name) && !KEPT_PREFIXES.iter().any(|p| name.starts_with(p)))
        .map(|(n, _)| n.clone())
        .collect();
    removed.sort();
    let env = environ.iter().filter(|(name, _)| !removed.contains(name)).cloned().collect();
    Built { env, removed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_out_only_app_env_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".env");
        std::fs::write(&file, "SMTP_PASSWORD=secret1\nLOOP_WEBHOOK_URL=http://x\nGUARD_SECRET_PATHS=a;b\nPLAIN=1\nREAL_TOKEN=fromfile\n").unwrap();
        let environ: Vec<(String, String)> =
            [("SMTP_PASSWORD", "secret1"), ("LOOP_WEBHOOK_URL", "http://x"), ("GUARD_SECRET_PATHS", "a;b"), ("PLAIN", "1"), ("REAL_TOKEN", "mine"), ("PATH", "p")]
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect();
        let built = build(&environ, &[file]);
        assert_eq!(built.removed, vec!["LOOP_WEBHOOK_URL", "SMTP_PASSWORD"]);
        assert_eq!(built.env.len(), 4);
        assert!(built.env.iter().any(|(n, v)| n == "REAL_TOKEN" && v == "mine"));
        assert!(looks_secret("my_api_key") && !looks_secret("USER"));
        assert!(build(&environ, &[PathBuf::from("C:\\nope\\.env")]).removed.is_empty());
    }
}
