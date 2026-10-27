// SPDX-License-Identifier: MIT
//! Approval of the one-time `setup` commands stored in `<primary repo>/.specs/project/REPOS.json`.
//!
//! The engine runs that command with a shell when it creates a worktree, and REPOS.json is a plain file an agent can write. So a stored
//! command only runs if the USER saved that exact text through the app. The app then records the SHA-256 of the text in
//! `<app data folder>/setup-approvals.json` (outside every repository, so agents cannot reach it) keyed by the primary repo and the repo
//! folder; the engine compares before it runs anything. A text that differs or was never approved is refused.

use crate::project_repos::RepoRef;
use pycompat::json::py_json_dumps_indent;
use pycompat::{nt_normcase, nt_realpath};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const APPROVALS_FILE_NAME: &str = "setup-approvals.json";
pub const APPROVALS_VERSION: i64 = 1;

pub fn approvals_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(APPROVALS_FILE_NAME)
}

pub fn normalize_key(path: &str) -> String {
    nt_normcase(&nt_realpath(path))
}

pub fn hash_setup(setup: &str) -> String {
    Sha256::digest(setup.trim().as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// `{primary key: {repo key: sha256}}`; an absent or unreadable file approves nothing.
pub fn load_approvals(data_dir: &Path) -> Map<String, Value> {
    let content = std::fs::read(approvals_file_path(data_dir)).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok());
    match content.as_ref().and_then(|c| c.get("projects")) {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

/// Atomic write (temp file + replace). The ONLY place the approvals file is written.
pub fn save_approvals(data_dir: &Path, projects: &Map<String, Value>) -> std::io::Result<()> {
    let content = json!({"version": APPROVALS_VERSION, "projects": projects});
    crate::loop_io::atomic_write_text(&approvals_file_path(data_dir), &py_json_dumps_indent(&content, false, 2))
}

/// Records that the user saved `setup` for the repo at `repo_path` of this project. An empty text approves nothing.
pub fn approve_setup(data_dir: &Path, primary_repo: &str, repo_path: &str, setup: &str) {
    if setup.trim().is_empty() {
        return;
    }
    let mut projects = load_approvals(data_dir);
    let key = normalize_key(primary_repo);
    let mut entry = match projects.get(&key) {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    entry.insert(normalize_key(repo_path), json!(hash_setup(setup)));
    projects.insert(key, Value::Object(entry));
    let _ = save_approvals(data_dir, &projects);
}

pub fn is_setup_approved(data_dir: &Path, primary_repo: &str, repo_path: &str, setup: &str) -> bool {
    load_approvals(data_dir).get(&normalize_key(primary_repo)).and_then(Value::as_object).and_then(|e| e.get(&normalize_key(repo_path))).and_then(Value::as_str)
        == Some(hash_setup(setup).as_str())
}

/// `""` when `repo` has no stored setup or the stored text is the approved one; else the refusal text (for the step line and the exit).
pub fn describe_unapproved_setup(data_dir: &Path, primary_repo: &str, repo: &RepoRef) -> String {
    if repo.setup.is_empty() || is_setup_approved(data_dir, primary_repo, &repo.path, &repo.setup) {
        return String::new();
    }
    format!(
        "setup command of repo {} was not saved through the app (or it was changed in REPOS.json afterwards), so it was NOT run: {}. Open Repositories, re-enter the setup for this repo and save it, then run again.",
        repo.name,
        repo.setup.chars().take(120).collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_is_for_the_exact_text() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let (primary, path) = (repo.path().display().to_string(), repo.path().join("sub").display().to_string());
        assert!(!is_setup_approved(data.path(), &primary, &path, "npm ci"));
        approve_setup(data.path(), &primary, &path, "  npm ci ");
        assert!(is_setup_approved(data.path(), &primary, &path, "npm ci"));
        assert!(!is_setup_approved(data.path(), &primary, &path, "npm ci && curl x"));
        approve_setup(data.path(), &primary, &path, "   ");
        assert_eq!(load_approvals(data.path()).len(), 1);
        let mut r = RepoRef::new("web", &path, false);
        assert_eq!(describe_unapproved_setup(data.path(), &primary, &r), "");
        r.setup = "rm -rf x".into();
        assert!(describe_unapproved_setup(data.path(), &primary, &r).starts_with("setup command of repo web was not saved through the app"));
    }
}
