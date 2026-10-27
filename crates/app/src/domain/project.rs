// SPDX-License-Identifier: MIT
//! App domain entities: Project (a folder opened in the panel) and ProjectContext (where the loop data lives).

use std::path::{Path, PathBuf};

pub use engine_state::project_repos::RepoRef;

pub const LOOP_WORKTREE_SUFFIX: &str = "-worktree-loop";
const LOOP_STATE_FOLDER_PARTS: [&str; 3] = [".specs", "project", "loop"];

/// True once a loop REALLY ran in this worktree: LOOP.json, a lock file, a turn record or `turns` > 0 in meta.json. A bare worktree (created by
/// Create workspace, or by the loop before its first turn) has only meta.json with turns 0.
pub fn has_loop_started(worktree: &str) -> bool {
    let folder = LOOP_STATE_FOLDER_PARTS.iter().fold(PathBuf::from(worktree), |a, p| a.join(p));
    if folder.join("json").join("LOOP.json").is_file() || folder.join("internal").join("LOOP.lock").is_file() {
        return true;
    }
    if std::fs::read_dir(folder.join("internal").join("turns")).map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().ends_with(".json"))).unwrap_or(false) {
        return true;
    }
    let meta = std::fs::read(folder.join("json").join("meta.json")).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    matches!(meta.as_ref().and_then(|m| m.get("turns")), Some(serde_json::Value::Number(n)) if n.as_i64().is_some_and(|t| t > 0))
}

/// A project folder known to the app. `id` is stable (short hash of the normalized path).
#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub repo_path: String,
    pub opened_at: String,
    pub last_opened_at: String,
    pub favorite: bool,
}

impl Project {
    /// Sibling worktree created by the engine (`../<name>-worktree-loop`).
    pub fn loop_worktree_path(&self) -> String {
        let parent = pycompat::nt_dirname(&self.repo_path);
        pycompat::nt_join(&parent, &format!("{}{LOOP_WORKTREE_SUFFIX}", pycompat::last_segment(&self.repo_path)))
    }
}

/// What a service needs to read/write the files of ONE project.
///
/// `work_dir` is re-evaluated on every call: the loop worktree only becomes the work dir once a loop really ran in it (a bare worktree, e.g.
/// from Create workspace, changes nothing: planning keeps reading and writing the primary repo's `.specs/`). Without a worktree, the data lives
/// in the repo itself (status `no_loop`).
#[derive(Clone, Debug)]
pub struct ProjectContext {
    pub project: Project,
    pub fixed_work_dir: String,
}

impl ProjectContext {
    pub fn new(project: Project) -> Self {
        ProjectContext { project, fixed_work_dir: String::new() }
    }

    pub fn repo_path(&self) -> &str {
        &self.project.repo_path
    }

    /// Where the panel WRITES `.specs/project/.env.test` and `NETWORK.json`: always the repo, never the loop worktree. The engine copies them into
    /// the worktree at loop start, resume and every turn start.
    pub fn credentials_dir(&self) -> &str {
        &self.project.repo_path
    }

    pub fn work_dir(&self) -> String {
        if !self.fixed_work_dir.is_empty() {
            return self.fixed_work_dir.clone();
        }
        let worktree = self.project.loop_worktree_path();
        if Path::new(&worktree).is_dir() && has_loop_started(&worktree) {
            worktree
        } else {
            self.project.repo_path.clone()
        }
    }

    pub fn specs_dir(&self) -> String {
        pycompat::nt_join(&self.work_dir(), ".specs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_dir_follows_the_loop_only_once_it_ran() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        let project = Project {
            id: "x".into(),
            name: "app".into(),
            repo_path: repo.display().to_string(),
            opened_at: String::new(),
            last_opened_at: String::new(),
            favorite: false,
        };
        let context = ProjectContext::new(project.clone());
        assert_eq!(context.work_dir(), project.repo_path);
        let worktree = dir.path().join("app-worktree-loop");
        assert_eq!(project.loop_worktree_path(), worktree.display().to_string());
        let loop_json = worktree.join(".specs/project/loop/json");
        std::fs::create_dir_all(&loop_json).unwrap();
        std::fs::write(loop_json.join("meta.json"), r#"{"turns": 0}"#).unwrap();
        assert!(!has_loop_started(&worktree.display().to_string()));
        assert_eq!(context.work_dir(), project.repo_path);
        std::fs::write(loop_json.join("meta.json"), r#"{"turns": 2}"#).unwrap();
        assert_eq!(context.work_dir(), worktree.display().to_string());
        assert_eq!(context.specs_dir(), worktree.join(".specs").display().to_string());
        assert_eq!(context.credentials_dir(), project.repo_path);
        let fixed = ProjectContext { fixed_work_dir: "C:\\fixed".into(), ..context };
        assert_eq!(fixed.work_dir(), "C:\\fixed");
    }
}
