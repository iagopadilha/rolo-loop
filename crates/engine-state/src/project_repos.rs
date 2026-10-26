// SPDX-License-Identifier: MIT
//! Repo list of a project (`<primary repo>/.specs/project/REPOS.json`), read by the engine.
//!
//! No file = exactly ONE primary repo with the same worktree path as a single-repo project (`../<repo>-worktree-loop`), so single-repo
//! projects need no migration.

use crate::loop_state::Features;
use crate::setup_approval;
use pycompat::json::py_json_dumps_indent;
use pycompat::{last_segment, nt_dirname, nt_join, nt_normcase, nt_normpath, Re};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const ROLE_EDIT: &str = "edit";
pub const ROLE_REFERENCE: &str = "reference";
pub const LOOP_WORKTREE_SUFFIX: &str = "-worktree-loop";

static REPO_PATH_PREFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"^([a-z0-9._-]+):(?![/\\])(.+)\z"));
static NAME_INVALID: LazyLock<Re> = LazyLock::new(|| Re::new(r"[^a-z0-9._-]+"));

/// One git repository of a project. The PRIMARY repo holds `.specs/` and is where the loop runs.
#[derive(Clone, Debug, PartialEq)]
pub struct RepoRef {
    pub name: String,
    pub path: String,
    pub primary: bool,
    /// Empty = the default sibling folder (`../<folder>-worktree-loop`).
    pub worktree_path: String,
    /// Git-ignored files (relative to the repo) copied into the worktree when it is created.
    pub copy: Vec<String>,
    /// One-time command run in this repo's worktree after it is created (empty = auto-detected by the engine).
    pub setup: String,
    /// False = REPOS.json had no `copy` key: the engine copies the suggested local files; true with `[]` = none.
    pub copy_configured: bool,
    /// `edit` (a feature may change it: gets a worktree) or `reference` (read only, `--add-dir`); the primary is always edit.
    pub role: String,
    /// Why the planning chat listed it (shown in the panel).
    pub reason: String,
}

impl RepoRef {
    pub fn new(name: &str, path: &str, primary: bool) -> Self {
        RepoRef { name: name.into(), path: path.into(), primary, worktree_path: String::new(), copy: vec![], setup: String::new(), copy_configured: false, role: ROLE_EDIT.into(), reason: String::new() }
    }

    pub fn loop_worktree_path(&self) -> String {
        if !self.worktree_path.is_empty() {
            return self.worktree_path.clone();
        }
        nt_join(&nt_dirname(&self.path), &format!("{}{LOOP_WORKTREE_SUFFIX}", last_segment(&self.path)))
    }
}

pub fn repos_file_path(primary_repo: &str) -> PathBuf {
    Path::new(primary_repo).join(".specs").join("project").join("REPOS.json")
}

/// Short, lowercase name from the folder name; used in `paths=<repo>:<path>` so it stays free of `:` and `,`.
pub fn short_name_of(path: &str) -> String {
    let base = last_segment(&nt_normpath(path)).to_lowercase();
    let cleaned = NAME_INVALID.replace_lit(&base, "-");
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() { "repo".into() } else { trimmed.into() }
}

/// `name`, or `name-2`, `name-3`... when already taken.
pub fn unique_name(name: &str, taken: &BTreeSet<String>) -> String {
    let (mut candidate, mut number) = (name.to_string(), 2);
    while taken.contains(&candidate) {
        candidate = format!("{name}-{number}");
        number += 1;
    }
    candidate
}

pub fn single_repo(primary_repo: &str) -> Vec<RepoRef> {
    vec![RepoRef::new(&short_name_of(primary_repo), primary_repo, true)]
}

/// The raw file: `multi` is derived (more than one entry); absent or broken = nothing defined.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReposFile {
    pub multi: bool,
    pub repos: Vec<Map<String, Value>>,
    pub scope_defined: bool,
    pub scope_source: String,
}

fn truthy(v: Option<&Value>) -> bool {
    crate::notify_truthy(v)
}

fn text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => pycompat::json::py_json_dumps(other, true),
    }
}

pub fn read_repos_file(primary_repo: &str) -> ReposFile {
    let parsed = std::fs::read(repos_file_path(primary_repo)).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let Some(Value::Object(data)) = parsed else { return ReposFile::default() };
    let Some(Value::Array(list)) = data.get("repos") else { return ReposFile::default() };
    let repos: Vec<Map<String, Value>> = list.iter().filter_map(Value::as_object).filter(|item| truthy(item.get("path"))).cloned().collect();
    ReposFile { multi: repos.len() > 1, repos, scope_defined: truthy(data.get("scope_defined")), scope_source: text(data.get("scope_source")) }
}

pub fn normalize_role(value: Option<&Value>) -> &'static str {
    match value.and_then(Value::as_str) {
        Some("reference") => ROLE_REFERENCE,
        _ => ROLE_EDIT,
    }
}

/// The project's repos, primary FIRST: every entry of the file, the primary always `edit`. One entry when the file is absent, broken or empty.
pub fn load_repos(primary_repo: &str) -> Vec<RepoRef> {
    let data = read_repos_file(primary_repo);
    if data.repos.is_empty() {
        return single_repo(primary_repo);
    }
    let primary_key = nt_normcase(&nt_normpath(primary_repo));
    let (mut repos, mut taken) = (Vec::<RepoRef>::new(), BTreeSet::new());
    for item in &data.repos {
        let path = text(item.get("path"));
        let is_primary = nt_normcase(&nt_normpath(&path)) == primary_key;
        let given = text(item.get("name"));
        let name = unique_name(&if given.is_empty() { short_name_of(&path) } else { given }, &taken);
        taken.insert(name.clone());
        let copy_list = item.get("copy").and_then(Value::as_array);
        let copy: Vec<String> = copy_list.map(|l| l.iter().filter_map(|e| e.as_str().map(String::from)).collect()).unwrap_or_default();
        repos.push(RepoRef {
            name,
            path,
            primary: is_primary,
            worktree_path: text(item.get("worktree_path")),
            copy,
            setup: text(item.get("setup")).trim().to_string(),
            copy_configured: copy_list.is_some(),
            role: if is_primary { ROLE_EDIT.into() } else { normalize_role(item.get("role")).into() },
            reason: text(item.get("reason")).trim().to_string(),
        });
    }
    if !repos.iter().any(|r| r.primary) {
        repos.insert(0, single_repo(primary_repo).remove(0)); // the primary always exists, even if the file forgot it
    }
    repos.sort_by_key(|r| !r.primary);
    repos
}

/// Always true now that the confirm step is gone: the primary is always `edit`.
pub fn is_scope_defined(primary_repo: &str) -> bool {
    !edit_repos(&load_repos(primary_repo)).is_empty()
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

/// Read-only repos (role `reference`, never the primary): passed to the loop's claude with `--add-dir`, no worktree.
pub fn reference_repos(repos: &[RepoRef]) -> Vec<&RepoRef> {
    repos.iter().filter(|r| !r.primary && r.role == ROLE_REFERENCE).collect()
}

/// Repo names named by `paths=` of the ROADMAP features. An unprefixed path belongs to the primary, so `primary_name` is in the set
/// whenever a feature has any path. Names are not checked against REPOS.json here.
pub fn repos_changed_by_roadmap(primary_name: &str, features: &Features) -> BTreeSet<String> {
    let mut changed = BTreeSet::new();
    for (_, feature) in features.iter() {
        for entry in feature.text("paths").split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            changed.insert(REPO_PATH_PREFIX.match_start(entry).and_then(|m| m.group(1).map(String::from)).unwrap_or_else(|| primary_name.to_string()));
        }
    }
    changed
}

/// Writes REPOS.json (the registry service and the engine share this one writer). `scope_defined`/`scope_source` of `None` keep what the
/// file says. `approve_setup_for` = names of repos whose `setup` the USER just typed in the app: only those texts are recorded as approved.
pub fn save_repos(primary_repo: &str, repos: &[RepoRef], scope_defined: Option<bool>, scope_source: Option<&str>, approve_setup_for: &[&str], data_dir: &Path) -> std::io::Result<()> {
    let current = read_repos_file(primary_repo);
    let scope_defined = scope_defined.unwrap_or(current.scope_defined);
    let scope_source = scope_source.map(String::from).unwrap_or(current.scope_source);
    let items: Vec<Value> = repos
        .iter()
        .map(|repo| {
            let mut item = Map::new();
            item.insert("name".into(), json!(repo.name));
            item.insert("path".into(), json!(repo.path));
            item.insert("primary".into(), json!(repo.primary));
            item.insert("role".into(), json!(if repo.primary { ROLE_EDIT } else { normalize_role(Some(&json!(repo.role))) }));
            if !repo.reason.is_empty() {
                item.insert("reason".into(), json!(repo.reason));
            }
            if !repo.worktree_path.is_empty() {
                item.insert("worktree_path".into(), json!(repo.worktree_path));
            }
            if !repo.copy.is_empty() || repo.copy_configured {
                item.insert("copy".into(), json!(repo.copy)); // absent key = never configured (auto copy); `[]` = the user chose none
            }
            if !repo.setup.is_empty() {
                item.insert("setup".into(), json!(repo.setup));
            }
            Value::Object(item)
        })
        .collect();
    let target = repos_file_path(primary_repo);
    std::fs::create_dir_all(target.parent().expect("has parent"))?;
    let mut payload = Map::new();
    payload.insert("multi".into(), json!(items.len() > 1));
    payload.insert("scope_defined".into(), json!(scope_defined));
    payload.insert("repos".into(), Value::Array(items));
    if !scope_source.is_empty() {
        payload.insert("scope_source".into(), json!(scope_source));
    }
    std::fs::write(&target, format!("{}\n", py_json_dumps_indent(&Value::Object(payload), false, 2)))?;
    for repo in repos {
        if approve_setup_for.contains(&repo.name.as_str()) && !repo.setup.is_empty() {
            setup_approval::approve_setup(data_dir, primary_repo, &repo.path, &repo.setup);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_state::parse_roadmap;

    #[test]
    fn names() {
        assert_eq!(short_name_of("C:\\Work\\My Repo!"), "my-repo");
        assert_eq!(short_name_of("C:\\"), "repo");
        let taken: BTreeSet<String> = ["api".to_string(), "api-2".to_string()].into();
        assert_eq!(unique_name("api", &taken), "api-3");
        assert_eq!(RepoRef::new("a", "C:\\w\\api", true).loop_worktree_path(), "C:\\w\\api-worktree-loop");
    }

    #[test]
    fn no_file_means_one_primary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        let repos = load_repos(&root);
        assert_eq!((repos.len(), repos[0].primary, repos[0].role.as_str()), (1, true, "edit"));
        assert!(is_scope_defined(&root));
    }

    #[test]
    fn file_roundtrip_roles_and_unique_names() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let root = dir.path().join("main").display().to_string();
        let other = dir.path().join("other").display().to_string();
        let ref_path = dir.path().join("docs").display().to_string();
        let mut other_repo = RepoRef::new("other", &other, false);
        other_repo.setup = "npm ci".into();
        let mut reference = RepoRef::new("other", &ref_path, false);
        reference.role = ROLE_REFERENCE.into();
        reference.copy_configured = true;
        let repos = vec![RepoRef::new("main", &root, true), other_repo, reference];
        save_repos(&root, &repos, Some(true), Some("planning"), &["other"], data.path()).unwrap();
        let loaded = load_repos(&root);
        assert_eq!(loaded.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["main", "other", "other-2"]);
        assert_eq!((loaded[2].role.as_str(), loaded[2].copy_configured, loaded[1].copy_configured), ("reference", true, false));
        assert_eq!(edit_repos(&loaded).len(), 2);
        assert_eq!(reference_repos(&loaded)[0].name, "other-2");
        let file = read_repos_file(&root);
        assert!(file.multi && file.scope_defined && file.scope_source == "planning");
        assert!(setup_approval::is_setup_approved(data.path(), &root, &other, "npm ci"));
        assert!(!setup_approval::is_setup_approved(data.path(), &root, &other, "npm install"));
    }

    #[test]
    fn roadmap_names_repos() {
        let features = parse_roadmap("## Stage 1\n- [a] status=pending paths=src/api,web:ui/x\n- [b] status=pending paths=C:/drive\n- [c] status=pending\n").0;
        let changed = repos_changed_by_roadmap("main", &features);
        assert_eq!(changed.into_iter().collect::<Vec<_>>(), vec!["main", "web"]);
    }
}
