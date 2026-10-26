// SPDX-License-Identifier: MIT
//! Preflight checks of the test credentials convention (the same one as the dev toolkit's `craft testenv`): TEST-ENVIRONMENT.md holds
//! NAMES only; the values live in the project's `.env.test`, which git ignores. Pure functions plus one file read.
//!
//! At run time there is ONE source, like `craft testenv`: `<project>/.specs/project/.env.test`. The real environment variable,
//! `~/.loop-harness/.env` and the user-level shared `.env` are NOT read to resolve a name. Those files are only PLANNING sources
//! ([`planning_sources`]: `env_sources` of config.json, default the app data folder's `.env`): the app copies chosen names from them into
//! the project's `.env.test`, on the user's request. [`real_environment`] and the shared-folder helpers stay for the access report, which
//! labels them "not used at run time".
//!
//! Test users are `;`-separated lists (TEST_USER_EMAILS / TEST_USER_PASSWORDS / TEST_USER_ROLES) that also become TEST_USER_<n>_EMAIL /
//! _PASSWORD / _ROLE. Nothing here returns or logs a value: callers get names, sources and labels.

use cfg::env::{parse_env, read_text_sig};
use cfg::locator::read_user_config;
use cfg::Env;
use pycompat::{nt_abspath, Re};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const SHARED_DIR_VARIABLE: &str = "LOOP_USER_SHARED_DIR";
pub const ENVIRONMENT_LABEL: &str = "environment variable";
pub const PROJECT_LABEL: &str = "project .env.test";
pub const MACHINE_LABEL: &str = "~/.loop-harness/.env";
pub const NOT_USED_LABEL: &str = "not used at run time";
pub const ENV_SOURCES_KEY: &str = "env_sources";
pub const USER_SHARED_LABEL: &str = "user shared .env";
const LIST_SEPARATOR: char = ';';
const ESCAPED_SEPARATOR: &str = "\\;";
const USER_COLUMNS: [(&str, &str); 3] = [("EMAILS", "EMAIL"), ("PASSWORDS", "PASSWORD"), ("ROLES", "ROLE")];
const MAX_REPORTED_NAMES: usize = 8;
/// Literal words of the template prose (case-sensitive), never credential names.
const PLACEHOLDER_WORDS: [&str; 2] = ["NAME", "NAMES"];
/// Test DATA of one feature: a missing one only affects that feature's checks, so it is a warning.
const OPTIONAL_NAME_PREFIX: &str = "SAMPLE_";

static NAME_REFERENCE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\$\{?([A-Z][A-Z0-9_]{2,})\}?"));
// `password: abc123`, `token = abc123`, `Bearer eyJ...`: a value written after a secret-looking word, not a $NAME or ***
static PLAINTEXT_SECRET: LazyLock<Re> = LazyLock::new(|| {
    Re::new(r"(?i)\b(?:password|passwd|senha|secret|token|api[_-]?key|apikey|bearer)\b[`*\s]*([:=|])[`*\s]*([^\s|`*]{4,})|\bbearer\s+([A-Za-z0-9._~+/=-]{12,})")
});
static HARMLESS_VALUE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^(\$|\*{3}|FILL_IN|<|\(|none$|name$|value$)"));
// same rule as testenv: shown as ***
static SECRET_NAME: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?i)(token|secret|passw|credential|private_key|api_?key|auth|pwd)"));

pub fn split_values(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![];
    }
    text.replace(ESCAPED_SEPARATOR, "\0").split(LIST_SEPARATOR).map(|p| p.replace('\0', &LIST_SEPARATOR.to_string()).trim().to_string()).collect()
}

/// `{name: value}` of one `.env` style file (empty when it does not exist). Values never leave the caller.
pub fn parse_env_file(path: &Path) -> BTreeMap<String, String> {
    if path.as_os_str().is_empty() {
        return BTreeMap::new();
    }
    match read_text_sig(path) {
        Some(text) => parse_env(&text).into_iter().collect(),
        None => BTreeMap::new(),
    }
}

fn project_env_path(root: &str) -> PathBuf {
    Path::new(root).join(".specs").join("project").join(".env.test")
}

pub fn read_project_values(root: &str) -> BTreeMap<String, String> {
    parse_env_file(&project_env_path(root))
}

/// Folder of the user-level shared `.env`: LOOP_USER_SHARED_DIR (tests), then `user_shared_dir` of `<data dir>/config.json`; empty when
/// neither is set (no default folder: the layer is then skipped).
pub fn user_shared_dir(env: &Env) -> String {
    let explicit = env.text(SHARED_DIR_VARIABLE);
    if !explicit.is_empty() {
        return explicit;
    }
    match read_user_config(&env.user_data_dir()).get("user_shared_dir") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
        _ => String::new(),
    }
}

/// The user-level shared `.env` path, or empty when no folder is configured.
pub fn user_shared_env_path(env: &Env) -> String {
    let folder = user_shared_dir(env);
    if folder.is_empty() { folder } else { Path::new(&folder).join(".env").display().to_string() }
}

/// Label of the user-level shared layer: `user shared .env (<folder>)`, or the bare base when no folder is configured.
pub fn user_shared_label(env: &Env) -> String {
    let folder = user_shared_dir(env);
    if folder.is_empty() { USER_SHARED_LABEL.to_string() } else { format!("{USER_SHARED_LABEL} ({folder})") }
}

/// One `.env` file of the stack. `shared` layers are the user's generic account; they always count (lowest priority).
#[derive(Clone, Debug)]
pub struct Layer {
    pub source: String,
    pub label: String,
    pub path: PathBuf,
    pub shared: bool,
    pub values: BTreeMap<String, String>,
}

impl Layer {
    pub fn new(source: &str, label: &str, path: PathBuf, shared: bool) -> Self {
        let values = parse_env_file(&path);
        Layer { source: source.into(), label: label.into(), path, shared, values }
    }

    pub fn found(&self) -> bool {
        !self.path.as_os_str().is_empty() && self.path.is_file()
    }
}

/// The layers read at run time: ONLY the project's `.env.test` (single source).
pub fn file_layers(root: &str) -> Vec<Layer> {
    vec![Layer::new("project", PROJECT_LABEL, project_env_path(root), false)]
}

/// Files that used to be credential layers and are NOT read at run time any more: the app data folder's `.env` and the user-level shared
/// `.env` (when configured). For the access report only.
pub fn unused_file_layers(env: &Env) -> Vec<Value> {
    let candidates = env.env_file_candidates();
    let items = [(MACHINE_LABEL.to_string(), candidates.first().map(|p| p.display().to_string()).unwrap_or_default()), (user_shared_label(env), user_shared_env_path(env))];
    items.into_iter().filter(|(_, path)| !path.is_empty()).map(|(label, path)| json!({"label": label, "exists": Path::new(&path).is_file(), "path": path, "note": NOT_USED_LABEL})).collect()
}

/// `~/.loop-harness/.env` (the app data folder's `.env`: LOOP_DATA_DIR when set).
pub fn default_planning_source(env: &Env) -> String {
    env.user_data_dir().join(".env").display().to_string()
}

fn expand_user(env: &Env, item: &str) -> String {
    if item == "~" {
        return env.home_dir().display().to_string();
    }
    match item.strip_prefix("~\\").or_else(|| item.strip_prefix("~/")) {
        Some(rest) => env.home_dir().join(rest).display().to_string(),
        None => item.to_string(),
    }
}

/// Paths of the `.env` files the user chose as sources to import test values FROM (never read at run time): `env_sources` of
/// `<data dir>/config.json` (`~` expanded), default `[~/.loop-harness/.env]`. A malformed value falls back to the default.
pub fn planning_sources(env: &Env) -> Vec<String> {
    let config = read_user_config(&env.user_data_dir());
    let items: Vec<String> = match config.get(ENV_SOURCES_KEY) {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
        _ => vec![],
    };
    let items = if items.is_empty() { vec![default_planning_source(env)] } else { items };
    let mut paths: Vec<String> = Vec::new();
    for item in items {
        let path = nt_abspath(&expand_user(env, &item));
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

type Origin = (String, String);

/// TEST_USER_<n>_EMAIL/PASSWORD/ROLE and TEST_USER_COUNT derived from the `;` lists (origin = the list's origin).
pub fn add_test_users(values: &mut BTreeMap<String, String>, origins: &mut BTreeMap<String, Option<Origin>>) {
    let columns: Vec<(&str, Vec<String>)> = USER_COLUMNS.iter().map(|(plural, single)| (*single, split_values(values.get(&format!("TEST_USER_{plural}")).map(String::as_str).unwrap_or("")))).collect();
    let count = columns.iter().map(|(_, items)| items.len()).max().unwrap_or(0);
    for number in 1..=count {
        for (plural, single) in USER_COLUMNS {
            let items = &columns.iter().find(|(s, _)| *s == single).expect("column").1;
            if number <= items.len() && !items[number - 1].is_empty() {
                let name = format!("TEST_USER_{number}_{single}");
                values.insert(name.clone(), items[number - 1].clone());
                let origin = origins.get(&format!("TEST_USER_{plural}")).cloned().flatten();
                origins.insert(name, origin);
            }
        }
    }
    values.insert("TEST_USER_COUNT".into(), count.to_string());
    let origin = origins.get("TEST_USER_EMAILS").cloned().flatten();
    origins.insert("TEST_USER_COUNT".into(), origin);
}

/// Real environment variables with a value, for the access report only ("not used at run time"). A value the app copied from its `.env`
/// is not a variable the user set, and neither is one identical to a FILE layer's: both are left out.
pub fn real_environment(env: &Env, layers: &[Layer]) -> BTreeMap<String, String> {
    let file_values: Vec<&BTreeMap<String, String>> = layers.iter().filter(|l| l.source != "project").map(|l| &l.values).collect();
    env.vars().into_iter().filter(|(name, value)| !value.is_empty() && !env.is_loaded_value(name, value) && !file_values.iter().any(|v| v.get(name) == Some(value))).collect()
}

/// `(values, origins)` of a stack of layers (highest priority first) under the real environment. Origin = `(source, label)`.
pub fn stack_values(layers: &[Layer], environment: &BTreeMap<String, String>) -> (BTreeMap<String, String>, BTreeMap<String, Option<Origin>>) {
    let (mut values, mut origins) = (BTreeMap::new(), BTreeMap::new());
    for layer in layers.iter().rev() {
        // lowest first, so higher layers overwrite; a blank value never hides a lower one
        for (name, value) in &layer.values {
            if !value.is_empty() {
                values.insert(name.clone(), value.clone());
                origins.insert(name.clone(), Some((layer.source.clone(), layer.label.clone())));
            }
        }
    }
    for (name, value) in environment {
        values.insert(name.clone(), value.clone());
        origins.insert(name.clone(), Some(("env".to_string(), ENVIRONMENT_LABEL.to_string())));
    }
    add_test_users(&mut values, &mut origins);
    (values, origins)
}

/// Where every name comes from for one project. [`Resolution::lookup`] never exposes a value. Only the project's `.env.test` counts
/// (source `project`, else `missing`).
pub struct Resolution {
    pub layers: Vec<Layer>,
    pub values: BTreeMap<String, String>,
    origins: BTreeMap<String, Option<Origin>>,
}

impl Resolution {
    pub fn new(root: &str) -> Self {
        let layers = file_layers(root);
        let (values, origins) = stack_values(&layers, &BTreeMap::new());
        Resolution { layers, values, origins }
    }

    pub fn lookup(&self, name: &str) -> Origin {
        match (self.values.get(name).filter(|v| !v.is_empty()), self.origins.get(name).cloned().flatten()) {
            (Some(_), Some(origin)) => origin,
            _ => ("missing".to_string(), String::new()),
        }
    }

    pub fn names_defined(&self) -> BTreeSet<String> {
        self.values.iter().filter(|(_, v)| !v.is_empty()).map(|(k, _)| k.clone()).collect()
    }

    pub fn shared_files_found(&self) -> Vec<String> {
        self.layers.iter().filter(|l| l.shared && l.found()).map(|l| l.label.clone()).collect()
    }

    /// `[{name, label, path, exists, names}]` per file layer, highest priority first: which of `names` each layer defines (never a value).
    pub fn layer_report(&self, names: &[String]) -> Vec<Value> {
        self.layers
            .iter()
            .map(|l| {
                let defined: BTreeSet<&String> = names.iter().filter(|n| l.values.get(*n).is_some_and(|v| !v.is_empty())).collect();
                json!({"name": l.source, "label": l.label, "path": l.path.display().to_string(), "exists": l.found(), "names": defined})
            })
            .collect()
    }
}

/// Names that have a non-empty value in some counted layer, including the derived TEST_USER_<n>_* names.
pub fn defined_names(root: &str) -> BTreeSet<String> {
    Resolution::new(root).names_defined()
}

pub fn is_secret_name(name: &str) -> bool {
    SECRET_NAME.search(name).is_some()
}

/// Every `$NAME` the document mentions, except the documentation placeholders `$NAME` / `$NAMES` of the template header.
pub fn mentioned_names(document_text: &str) -> Vec<String> {
    let found: BTreeSet<String> = NAME_REFERENCE.find_all(document_text).iter().filter_map(|m| m.group(1).map(String::from)).collect();
    found.into_iter().filter(|n| !PLACEHOLDER_WORDS.contains(&n.as_str())).collect()
}

/// `{"missing": [...], "shared-blocked": []}` of the names the document mentions (the ones that stop a run). `shared-blocked` is always
/// empty now: there are no shared layers at run time.
pub fn names_by_source(root: &str, document_text: &str) -> (Vec<String>, Vec<String>) {
    let resolution = Resolution::new(root);
    let (mut missing, blocked) = (Vec::new(), Vec::new());
    for name in mentioned_names(document_text) {
        if resolution.lookup(&name).0 == "missing" {
            missing.push(name);
        }
    }
    (missing, blocked)
}

/// Mentioned names with no value anywhere.
pub fn undefined_names(root: &str, document_text: &str) -> Vec<String> {
    names_by_source(root, document_text).0
}

pub fn blocked_names(root: &str, document_text: &str) -> Vec<String> {
    names_by_source(root, document_text).1
}

/// 1-based line numbers where a secret-looking word is followed by a literal value (never the value itself).
pub fn plaintext_secret_lines(document_text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    for (index, line) in pycompat::splitlines(document_text).into_iter().enumerate() {
        for m in PLAINTEXT_SECRET.find_all(line) {
            let value = m.group(2).or(m.group(3)).unwrap_or("");
            if m.group(1) == Some("|") && !value.chars().any(|c| c.is_ascii_digit() || (c.is_numeric() && c.to_digit(10).is_some())) {
                continue; // a table cell next to a secret-looking word ("| Access token | obtained from login |") is prose, not a value
            }
            if !value.is_empty() && HARMLESS_VALUE.match_start(value).is_none() {
                found.push(index + 1);
                break;
            }
        }
    }
    found
}

fn names_text(names: &[String]) -> String {
    names.iter().take(MAX_REPORTED_NAMES).map(|n| format!("${n}")).collect::<Vec<_>>().join(", ")
}

/// Problems for the preflight (empty = fine). Reports names and line numbers, never a value.
pub fn credential_problems(root: &str, document_text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let lines = plaintext_secret_lines(document_text);
    if !lines.is_empty() {
        problems.push(format!(
            ".specs/project/TEST-ENVIRONMENT.md has a plaintext secret on line(s) {}: write only the $NAME and put the value in .specs/project/.env.test, which git ignores",
            lines.iter().take(MAX_REPORTED_NAMES).map(usize::to_string).collect::<Vec<_>>().join(", ")
        ));
    }
    let required: Vec<String> = undefined_names(root, document_text).into_iter().filter(|n| !n.starts_with(OPTIONAL_NAME_PREFIX)).collect();
    if !required.is_empty() {
        problems.push(format!(
            "TEST-ENVIRONMENT.md mentions names no .env defines: {}. Action: add NAME=value for each one to .specs/project/.env.test (the project's only credential source; \"Import from machine sources\" copies names from a machine file), then run the check again",
            names_text(&required)
        ));
    }
    problems
}

/// Warnings for the preflight: sample-data names (`$SAMPLE_*`) with no value. The loop runs; the checks that need them are skipped.
pub fn credential_warnings(root: &str, document_text: &str) -> Vec<String> {
    let optional: Vec<String> = undefined_names(root, document_text).into_iter().filter(|n| n.starts_with(OPTIONAL_NAME_PREFIX)).collect();
    if optional.is_empty() {
        return vec![];
    }
    vec![format!("TEST-ENVIRONMENT.md mentions sample data no .env defines: {} (the checks that use them cannot run; add them to .specs/project/.env.test)", names_text(&optional))]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(env_test: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".specs/project")).unwrap();
        std::fs::write(dir.path().join(".specs/project/.env.test"), env_test).unwrap();
        let root = dir.path().display().to_string();
        (dir, root)
    }

    #[test]
    fn lists_split_and_derive_test_users() {
        assert_eq!(split_values("a@x.com; b@x.com ;c\\;d"), vec!["a@x.com", "b@x.com", "c;d"]);
        assert!(split_values("").is_empty());
        let (_d, root) = project("TEST_USER_EMAILS=a@example.com;b@example.com\nTEST_USER_PASSWORDS=p1;p2\nOTHER=\nAPI_KEY=zzz\n");
        let names = defined_names(&root);
        for expected in ["TEST_USER_1_EMAIL", "TEST_USER_2_PASSWORD", "TEST_USER_COUNT", "API_KEY"] {
            assert!(names.contains(expected), "{expected}");
        }
        assert!(!names.contains("OTHER") && !names.contains("TEST_USER_1_ROLE"));
        let resolution = Resolution::new(&root);
        assert_eq!(resolution.lookup("API_KEY"), ("project".to_string(), PROJECT_LABEL.to_string()));
        assert_eq!(resolution.lookup("NOPE"), ("missing".to_string(), String::new()));
        assert!(is_secret_name("API_KEY") && is_secret_name("db_passw") && !is_secret_name("BASE_URL"));
    }

    #[test]
    fn problems_name_the_missing_and_the_plaintext() {
        let (_d, root) = project("DEFINED=1\n");
        let doc = "Use $DEFINED and $MISSING_ONE, sample $SAMPLE_ROW and $NAME.\npassword: hunter22\n| Access token | obtained from login |\ntoken = $TOKEN_VAR\n";
        assert_eq!(mentioned_names(doc), vec!["DEFINED", "MISSING_ONE", "SAMPLE_ROW", "TOKEN_VAR"]);
        assert_eq!(plaintext_secret_lines(doc), vec![2]);
        let problems = credential_problems(&root, doc);
        assert_eq!(problems.len(), 2);
        assert!(problems[0].contains("plaintext secret on line(s) 2:"));
        assert!(problems[1].starts_with("TEST-ENVIRONMENT.md mentions names no .env defines: $MISSING_ONE, $TOKEN_VAR. Action: add NAME=value"));
        assert_eq!(credential_warnings(&root, doc), vec!["TEST-ENVIRONMENT.md mentions sample data no .env defines: $SAMPLE_ROW (the checks that use them cannot run; add them to .specs/project/.env.test)"]);
        assert!(credential_problems(&root, "").is_empty() && plaintext_secret_lines("Bearer abcdefghijklmnop1").contains(&1));
    }

    #[test]
    fn planning_sources_default_and_configured() {
        let data = tempfile::tempdir().unwrap();
        let env = Env::from_pairs([("LOOP_DATA_DIR", data.path().display().to_string())]);
        assert_eq!(planning_sources(&env), vec![nt_abspath(&data.path().join(".env").display().to_string())]);
        std::fs::write(data.path().join("config.json"), r#"{"env_sources": ["C:\\a\\.env", " ", "C:\\a\\.env", 3]}"#).unwrap();
        assert_eq!(planning_sources(&env), vec!["C:\\a\\.env"]);
        assert_eq!(user_shared_label(&env), "user shared .env");
        let layers = unused_file_layers(&env);
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0]["label"], MACHINE_LABEL);
    }
}
