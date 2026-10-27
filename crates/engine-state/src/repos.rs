// SPDX-License-Identifier: MIT
//! Repo list of a project (`<primary repo>/.specs/project/REPOS.json`), read by the engine.
//!
//! No file = exactly ONE primary repo with the same worktree path as a single-repo project, so single-repo projects need no migration.

use crate::loop_state::Features;
use pycompat::json::py_json_dumps_indent;
use pycompat::{nt_normpath, Re};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::LazyLock;

pub const ROLE_EDIT: &str = "edit";
pub const ROLE_REFERENCE: &str = "reference";
pub const SCOPE_SOURCE_PLANNING: &str = "planning";
static REPO_PATH_PREFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"^([a-z0-9._-]+):(?![/\\])(.+)\z"));

#[derive(Clone, Debug, PartialEq)]
pub struct RepoRef {
    pub name: String,
    pub path: String,
    pub primary: bool,
    pub worktree_path: String,
    pub copy: Vec<String>,
    pub setup: String,
    pub copy_configured: bool,
    pub role: String,
    pub reason: String,
}

impl RepoRef {
    pub fn new(name: &str, path: &str, primary: bool) -> RepoRef {
        RepoRef {
            name: name.to_string(),
            path: path.to_string(),
            primary,
            worktree_path: String::new(),
            copy: vec![],
            setup: String::new(),
            copy_configured: false,
            role: ROLE_EDIT.to_string(),
            reason: String::new(),
        }
    }
}

pub fn repos_file_path(primary_repo: &str) -> std::path::PathBuf {
    Path::new(primary_repo).join(".specs").join("project").join("REPOS.json")
}

/// Short, lowercase name from the folder name (free of `:` and `,`).
pub fn short_name_of(path: &str) -> String {
    static BAD: LazyLock<Re> = LazyLock::new(|| Re::new(r"[^a-z0-9._-]+"));
    let normalized = nt_normpath(path);
    let base = normalized.rsplit('\\').next().unwrap_or("").to_lowercase();
    let cleaned = BAD.replace_lit(&base, "-");
    let cleaned = cleaned.trim_matches('-');
    if cleaned.is_empty() {
        "repo".to_string()
    } else {
        cleaned.to_string()
    }
}

/// `name`, or `name-2`, `name-3`... when already taken.
pub fn unique_name(name: &str, taken: &[String]) -> String {
    let mut candidate = name.to_string();
    let mut number = 2;
    while taken.contains(&candidate) {
        candidate = format!("{name}-{number}");
        number += 1;
    }
    candidate
}

pub fn single_repo(primary_repo: &str) -> Vec<RepoRef> {
    vec![RepoRef::new(&short_name_of(primary_repo), primary_repo, true)]
}

/// The raw file: `(repos, scope_defined, scope_source)`; absent or broken = nothing defined.
pub fn read_repos_file(primary_repo: &str) -> (Vec<Map<String, Value>>, bool, String) {
    let data = std::fs::read(repos_file_path(primary_repo)).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let Some(Value::Object(map)) = data else { return (vec![], false, String::new()) };
    let Some(Value::Array(items)) = map.get("repos") else { return (vec![], false, String::new()) };
    let repos: Vec<Map<String, Value>> = items.iter().filter_map(|i| i.as_object()).filter(|o| o.get("path").is_some_and(truthy)).cloned().collect();
    let scope_defined = map.get("scope_defined").is_some_and(truthy);
    let scope_source = map.get("scope_source").filter(|v| truthy(v)).map(text_of).unwrap_or_default();
    (repos, scope_defined, scope_source)
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => pycompat::json::py_json_dumps(other, false),
    }
}

pub fn normalize_role(value: Option<&Value>) -> String {
    match value.and_then(Value::as_str) {
        Some(r) if r == ROLE_EDIT || r == ROLE_REFERENCE => r.to_string(),
        _ => ROLE_EDIT.to_string(),
    }
}

fn normcase(path: &str) -> String {
    nt_normpath(path).to_lowercase()
}

/// The project's repos, primary FIRST: every entry of the file, the primary always `edit`. One entry when the file is absent, broken or empty.
pub fn load_repos(primary_repo: &str) -> Vec<RepoRef> {
    let (items, _, _) = read_repos_file(primary_repo);
    if items.is_empty() {
        return single_repo(primary_repo);
    }
    let primary_key = normcase(primary_repo);
    let mut repos: Vec<RepoRef> = Vec::new();
    let mut taken: Vec<String> = Vec::new();
    for item in &items {
        let path = item.get("path").map(text_of).unwrap_or_default();
        let is_primary = normcase(&path) == primary_key;
        let wanted = item.get("name").filter(|v| truthy(v)).map(text_of).unwrap_or_else(|| short_name_of(&path));
        let name = unique_name(&wanted, &taken);
        taken.push(name.clone());
        let copy_value = item.get("copy");
        let copy: Vec<String> = match copy_value {
            Some(Value::Array(a)) => a.iter().filter_map(|e| e.as_str().map(String::from)).collect(),
            _ => vec![],
        };
        repos.push(RepoRef {
            name,
            path,
            primary: is_primary,
            worktree_path: item.get("worktree_path").filter(|v| truthy(v)).map(text_of).unwrap_or_default(),
            copy,
            setup: item.get("setup").filter(|v| truthy(v)).map(text_of).unwrap_or_default().trim().to_string(),
            copy_configured: matches!(copy_value, Some(Value::Array(_))),
            role: if is_primary { ROLE_EDIT.to_string() } else { normalize_role(item.get("role")) },
            reason: item.get("reason").filter(|v| truthy(v)).map(text_of).unwrap_or_default().trim().to_string(),
        });
    }
    if !repos.iter().any(|r| r.primary) {
        repos.insert(0, single_repo(primary_repo).remove(0));
    }
    let (mut first, mut rest): (Vec<RepoRef>, Vec<RepoRef>) = repos.into_iter().partition(|r| r.primary);
    first.append(&mut rest);
    first
}

pub fn primary_of(repos: &[RepoRef]) -> &RepoRef {
    repos.iter().find(|r| r.primary).unwrap_or(&repos[0])
}

pub fn find_repo<'a>(repos: &'a [RepoRef], name: &str) -> Option<&'a RepoRef> {
    repos.iter().find(|r| r.name == name)
}

/// Repos a feature may change (role `edit`; the primary always counts).
pub fn edit_repos(repos: &[RepoRef]) -> Vec<&RepoRef> {
    repos.iter().filter(|r| r.primary || r.role != ROLE_REFERENCE).collect()
}

/// Read-only repos (role `reference`, never the primary).
pub fn reference_repos(repos: &[RepoRef]) -> Vec<&RepoRef> {
    repos.iter().filter(|r| !r.primary && r.role == ROLE_REFERENCE).collect()
}

pub fn is_scope_defined(primary_repo: &str) -> bool {
    !edit_repos(&load_repos(primary_repo)).is_empty()
}

/// Names of the repos named by `paths=` of the ROADMAP features (unprefixed = the primary).
pub fn repos_changed_by_roadmap(primary_name: &str, features: &Features) -> Vec<String> {
    let mut changed: Vec<String> = Vec::new();
    for (_, f) in features.iter() {
        for entry in f.text("paths").split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let name = REPO_PATH_PREFIX.match_start(entry).map(|m| m.group(1).unwrap_or("").to_string()).unwrap_or_else(|| primary_name.to_string());
            if !changed.contains(&name) {
                changed.push(name);
            }
        }
    }
    changed
}

/// Writes REPOS.json. `scope_defined` / `scope_source` of `None` keep what the file says. `multi` is written derived.
pub fn save_repos(primary_repo: &str, repos: &[RepoRef], scope_defined: Option<bool>, scope_source: Option<&str>) -> std::io::Result<()> {
    let (_, current_defined, current_source) = read_repos_file(primary_repo);
    let scope_defined = scope_defined.unwrap_or(current_defined);
    let scope_source = scope_source.map(String::from).unwrap_or(current_source);
    let mut items: Vec<Value> = Vec::new();
    for repo in repos {
        let mut item = Map::new();
        item.insert("name".into(), json!(repo.name));
        item.insert("path".into(), json!(repo.path));
        item.insert("primary".into(), json!(repo.primary));
        item.insert("role".into(), json!(if repo.primary { ROLE_EDIT.to_string() } else { normalize_role(Some(&json!(repo.role))) }));
        if !repo.reason.is_empty() {
            item.insert("reason".into(), json!(repo.reason));
        }
        if !repo.worktree_path.is_empty() {
            item.insert("worktree_path".into(), json!(repo.worktree_path));
        }
        if !repo.copy.is_empty() || repo.copy_configured {
            item.insert("copy".into(), json!(repo.copy));
        }
        if !repo.setup.is_empty() {
            item.insert("setup".into(), json!(repo.setup));
        }
        items.push(Value::Object(item));
    }
    let target = repos_file_path(primary_repo);
    std::fs::create_dir_all(target.parent().expect("has parent"))?;
    let mut payload = Map::new();
    payload.insert("multi".into(), json!(items.len() > 1));
    payload.insert("scope_defined".into(), json!(scope_defined));
    payload.insert("repos".into(), Value::Array(items));
    if !scope_source.is_empty() {
        payload.insert("scope_source".into(), json!(scope_source));
    }
    let mut body = py_json_dumps_indent(&Value::Object(payload), false, 2);
    body.push('\n');
    std::fs::write(target, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_repo_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let repos = load_repos(&dir.path().display().to_string());
        assert_eq!(repos.len(), 1);
        assert!(repos[0].primary);
        assert_eq!(repos[0].name, short_name_of(&dir.path().display().to_string()));
    }

    #[test]
    fn file_defines_roles_and_unique_names() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("api").display().to_string();
        std::fs::create_dir_all(dir.path().join("api/.specs/project")).unwrap();
        let body = json!({"repos": [
            {"name": "web", "path": dir.path().join("web").display().to_string(), "role": "reference"},
            {"path": primary, "role": "reference"},
            {"name": "web", "path": dir.path().join("web2").display().to_string(), "copy": [".env"]},
            {"nopath": 1}]});
        std::fs::write(dir.path().join("api/.specs/project/REPOS.json"), body.to_string()).unwrap();
        let repos = load_repos(&primary);
        assert_eq!(repos.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["api", "web", "web-2"]);
        assert!(repos[0].primary && repos[0].role == ROLE_EDIT);
        assert_eq!(reference_repos(&repos).len(), 1);
        assert!(repos[2].copy_configured && repos[2].copy == vec![".env".to_string()]);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().display().to_string();
        let mut other = RepoRef::new("lib", &dir.path().join("lib").display().to_string(), false);
        other.role = ROLE_REFERENCE.into();
        other.reason = "contracts".into();
        save_repos(&primary, &[RepoRef::new("main", &primary, true), other], Some(true), Some(SCOPE_SOURCE_PLANNING)).unwrap();
        let text = std::fs::read_to_string(repos_file_path(&primary)).unwrap();
        assert!(text.starts_with("{\n  \"multi\": true,\n  \"scope_defined\": true,\n  \"repos\": ["));
        assert!(text.ends_with("\"scope_source\": \"planning\"\n}\n"));
        let repos = load_repos(&primary);
        assert_eq!(repos[1].reason, "contracts");
    }

    #[test]
    fn names_from_paths() {
        assert_eq!(short_name_of("C:\\work\\My Repo!"), "my-repo");
        assert_eq!(short_name_of("C:\\"), "repo");
        assert_eq!(unique_name("a", &["a".into(), "a-2".into()]), "a-3");
    }
}
