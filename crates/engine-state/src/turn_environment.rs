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

/// The standard opt-out variables of the common toolchains (X1). The engine sets them for every process it starts for a loop, so a build or a
/// tool never calls a vendor's telemetry host: nothing is blocked and logged there, and no host is dropped from the guard's log. A variable the
/// user already set (any value) is left as it is.
pub const TELEMETRY_OPT_OUT: [(&str, &str); 22] = [
    ("DOTNET_CLI_TELEMETRY_OPTOUT", "1"),
    ("DOTNET_NOLOGO", "1"),
    ("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1"),
    ("POWERSHELL_TELEMETRY_OPTOUT", "1"),
    ("DO_NOT_TRACK", "1"),
    ("NEXT_TELEMETRY_DISABLED", "1"),
    ("NUXT_TELEMETRY_DISABLED", "1"),
    ("GATSBY_TELEMETRY_DISABLED", "1"),
    ("ASTRO_TELEMETRY_DISABLED", "1"),
    ("TURBO_TELEMETRY_DISABLED", "1"),
    ("STORYBOOK_DISABLE_TELEMETRY", "1"),
    ("NPM_CONFIG_UPDATE_NOTIFIER", "false"),
    ("NO_UPDATE_NOTIFIER", "1"),
    ("DISABLE_OPENCOLLECTIVE", "1"),
    ("ADBLOCK", "1"),
    ("PRISMA_HIDE_UPDATE_MESSAGE", "1"),
    ("CHECKPOINT_DISABLE", "1"),
    ("PIP_DISABLE_PIP_VERSION_CHECK", "1"),
    ("HOMEBREW_NO_ANALYTICS", "1"),
    ("AZURE_CORE_COLLECT_TELEMETRY", "0"),
    ("SAM_CLI_TELEMETRY", "0"),
    ("SCARF_ANALYTICS", "false"),
];

/// The opt-out variables that `environ` does not set yet (names compared without case, as on Windows).
pub fn missing_opt_outs(environ: &[(String, String)]) -> Vec<(String, String)> {
    TELEMETRY_OPT_OUT
        .iter()
        .filter(|(name, _)| !environ.iter().any(|(set, _)| set.eq_ignore_ascii_case(name)))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// The environment of a turn: a copy of `environ` without the app-.env secrets.
pub struct Built {
    pub env: Vec<(String, String)>,
    pub removed: Vec<String>,
}

pub fn build(environ: &[(String, String)], env_paths: &[PathBuf]) -> Built {
    build_from(environ, &values_from_env_files(env_paths), &[])
}

/// The turn environment of an engine whose `Env` also carries the Settings values and the secrets store (`cfg::env::load_all`): those
/// count as app values like the `.env` ones, and EVERY name of the secrets store is left out whatever it is called.
pub fn build_for(env: &cfg::Env) -> Built {
    let mut app_values = values_from_env_files(&env.env_file_candidates());
    app_values.extend(cfg::env::settings_values(env));
    build_from(&env.vars(), &app_values, &cfg::secrets::names(&env.user_data_dir()))
}

/// [`build`] over explicit app values (`(name, value)` the app set) and names that are secrets by definition.
pub fn build_from(environ: &[(String, String)], app_values: &[(String, String)], secret_names: &[String]) -> Built {
    let mut removed: Vec<String> = environ
        .iter()
        .filter(|(name, value)| {
            app_values.iter().any(|(n, v)| n == name && v == value)
                && (looks_secret(name) || secret_names.contains(name))
                && !KEPT_PREFIXES.iter().any(|p| name.starts_with(p))
        })
        .map(|(n, _)| n.clone())
        .collect();
    removed.sort();
    let mut env: Vec<(String, String)> = environ.iter().filter(|(name, _)| !removed.contains(name)).cloned().collect();
    let extra = missing_opt_outs(&env);
    env.extend(extra);
    Built { env, removed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_secrets_never_reach_the_turn() {
        let environ: Vec<(String, String)> = [("MY_STORED_THING", "s1"), ("LOOP_SMTP_HOST", "smtp.x"), ("LOOP_SMTP_PASSWORD", "pw"), ("REAL_TOKEN", "mine")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let app: Vec<(String, String)> =
            [("MY_STORED_THING", "s1"), ("LOOP_SMTP_HOST", "smtp.x"), ("LOOP_SMTP_PASSWORD", "pw")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let built = build_from(&environ, &app, &["MY_STORED_THING".to_string()]);
        assert_eq!(built.removed, vec!["LOOP_SMTP_PASSWORD", "MY_STORED_THING"]);
        assert!(built.env.iter().any(|(k, _)| k == "LOOP_SMTP_HOST") && built.env.iter().any(|(k, _)| k == "REAL_TOKEN"));
    }

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
        assert_eq!(built.env.len(), 4 + TELEMETRY_OPT_OUT.len());
        assert!(built.env.iter().any(|(n, v)| n == "REAL_TOKEN" && v == "mine"));
        assert!(looks_secret("my_api_key") && !looks_secret("USER"));
        assert!(build(&environ, &[PathBuf::from("C:\\nope\\.env")]).removed.is_empty());
    }

    #[test]
    fn telemetry_is_opted_out_for_every_child_and_the_users_choice_stays() {
        let environ: Vec<(String, String)> =
            [("PATH", "p"), ("dotnet_cli_telemetry_optout", "0"), ("DO_NOT_TRACK", "")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let built = build(&environ, &[]);
        let value = |name: &str| built.env.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
        assert_eq!(value("NEXT_TELEMETRY_DISABLED"), Some("1"));
        assert_eq!(value("DOTNET_NOLOGO"), Some("1"));
        assert_eq!(value("NPM_CONFIG_UPDATE_NOTIFIER"), Some("false"));
        assert_eq!(value("dotnet_cli_telemetry_optout"), Some("0"), "a value the user set is not overridden");
        assert!(!built.env.iter().any(|(n, _)| n == "DOTNET_CLI_TELEMETRY_OPTOUT"), "no second spelling of a variable the user set");
        assert_eq!(value("DO_NOT_TRACK"), Some(""));
        // none of them is a secret by name, so they are never dropped as one
        assert!(TELEMETRY_OPT_OUT.iter().all(|(name, _)| !looks_secret(name)));
        assert!(built.removed.is_empty());
        // no host is named anywhere: the opt-out is at the source, not a list of hosts the guard ignores
        assert!(TELEMETRY_OPT_OUT.iter().all(|(name, value)| !name.contains("HOST") && !value.contains('.')));
        assert!(missing_opt_outs(&built.env).is_empty());
    }
}
