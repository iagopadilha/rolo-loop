// SPDX-License-Identifier: MIT
//! The loop worktrees a Reset specs deletes with the plan: find them (preview) and remove them (worktree folder + local loop branch).
//!
//! A repo's worktree is removable only when ALL hold: it sits at the repo's loop worktree path, `git worktree list` of that repo registers it, it
//! is not the main checkout, and its branch is a loop branch (`loop/*`). Anything else is reported as skipped and never touched (the primary
//! checkout, other worktrees and other branches are never removed). Removal is `git worktree remove --force` + prune + `git branch -D` through
//! the git client: no merge, no commit, no push, no backup: what was not delivered or pushed is lost.

use super::loop_worktree::{is_same_path, LOOP_BRANCH_PREFIX};
use super::planning_gates::get_integration_branch;
use super::App;
use crate::domain::project::ProjectContext;
use crate::infrastructure::git_client::{remove_emptied_folder, OTHER_COMPARISON_BASES};
use crate::repositories::specs_file::is_link_or_junction;
use engine_state::loop_state;
use engine_state::project_repos::{self, RepoRef};
use pycompat::nt_normpath;
use serde_json::{json, Map, Value};
use std::path::Path;

const META_FILE_NAME: &str = "meta.json";

/// A real folder (not a link) with nothing inside: the only leftover a reset deletes without git registering it as a worktree.
fn is_empty_folder(path: &str) -> bool {
    let folder = Path::new(path);
    folder.is_dir() && !is_link_or_junction(folder) && std::fs::read_dir(folder).map(|mut entries| entries.next().is_none()).unwrap_or(false)
}

/// The worktree a repo may lose, with the folder git commands run from.
struct Found {
    repo: String,
    primary: bool,
    path: String,
    branch: String,
    main_repo: String,
}

pub struct LoopWorktreeRemovalService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn loop_worktree_removal(&self) -> LoopWorktreeRemovalService<'_> {
        LoopWorktreeRemovalService { app: self }
    }
}

impl LoopWorktreeRemovalService<'_> {
    // ------------------------------------------------ read
    /// `{"worktrees": [{"repo","primary","path","branch","unpushed_commits"}], "skipped": [{"repo","path","reason"}]}`; read only. `unpushed_commits`
    /// is `null` when git has no reference to compare with.
    pub fn preview_removable_worktrees(&self, context: &ProjectContext) -> Value {
        let (mut removable, mut skipped) = (Vec::new(), Vec::new());
        for repo in project_repos::load_repos(context.repo_path()) {
            match self.inspect_repo_worktree(&repo) {
                (Some(found), _) => {
                    let unpushed = self.count_unpushed_commits(&repo, &found);
                    removable.push(json!({"repo": found.repo, "primary": found.primary, "path": found.path, "branch": found.branch, "unpushed_commits": unpushed}));
                }
                (None, Some(reason)) => skipped.push(json!({"repo": repo.name, "path": nt_normpath(&repo.loop_worktree_path()), "reason": reason})),
                (None, None) => {}
            }
        }
        json!({"worktrees": removable, "skipped": skipped})
    }

    /// `(worktree, None)` when removable; `(None, reason)` when a folder is there but must not be touched; `(None, None)` when there is no worktree.
    fn inspect_repo_worktree(&self, repo: &RepoRef) -> (Option<Found>, Option<String>) {
        let path = nt_normpath(&repo.loop_worktree_path());
        if std::fs::symlink_metadata(&path).is_err() {
            return (None, None);
        }
        if !Path::new(&path).is_dir() {
            return (None, Some("the worktree path is not a folder".into()));
        }
        if is_same_path(&path, &repo.path) {
            return (None, Some("it is the repo folder itself".into()));
        }
        let registered = self.app.git.list_worktrees(&repo.path);
        let Some(main) = registered.first() else { return (None, Some("the repo is not a git repository".into())) };
        let Some(entry) = registered.iter().find(|item| is_same_path(&item.path, &path)) else {
            return (None, Some("the folder is not a git worktree of this repo".into()));
        };
        if is_same_path(&entry.path, &main.path) {
            return (None, Some("it is the main checkout".into()));
        }
        if !entry.branch.starts_with(LOOP_BRANCH_PREFIX) {
            let shown = if entry.branch.is_empty() { "detached HEAD" } else { entry.branch.as_str() };
            return (None, Some(format!("its branch ({shown}) is not a loop branch")));
        }
        (Some(Found { repo: repo.name.clone(), primary: repo.primary, path, branch: entry.branch.clone(), main_repo: main.path.clone() }), None)
    }

    fn count_unpushed_commits(&self, repo: &RepoRef, worktree: &Found) -> Option<u64> {
        self.app.git.count_unpushed_commits(&worktree.path, &self.get_comparison_bases(repo, &worktree.path))
    }

    /// The branch the loop forked from (meta.json of the worktree), the integration branch, then main/master.
    fn get_comparison_bases(&self, repo: &RepoRef, worktree_path: &str) -> Vec<String> {
        let meta = self.app.specs.read_json(&loop_state::loop_path(worktree_path, META_FILE_NAME));
        let own = meta.get("repos").and_then(Value::as_object).and_then(|repos| repos.get(&repo.name)).and_then(Value::as_object).cloned();
        let recorded: Map<String, Value> = match (meta.get("repos").is_some_and(Value::is_object), own) {
            (true, Some(own)) => own,
            _ if repo.primary && !meta.get("repos").is_some_and(Value::is_object) => meta.clone(),
            _ => Map::new(),
        };
        let mut bases: Vec<String> = Vec::new();
        if let Some(base) = recorded.get("base").and_then(Value::as_str).filter(|b| !b.is_empty()) {
            bases.push(base.to_string());
        }
        bases.push(get_integration_branch(self.app, ""));
        bases.extend(OTHER_COMPARISON_BASES.iter().map(|b| b.to_string()));
        let mut unique: Vec<String> = Vec::new();
        for base in bases {
            if !unique.contains(&base) {
                unique.push(base);
            }
        }
        unique
    }

    // ------------------------------------------------ act
    /// Removes every removable loop worktree of the project (each repo on its own: one failure never stops the others). Returns
    /// `{"worktrees_removed": [{"repo","path","branch","branch_deleted"}], "worktree_failures": [{"repo","path","branch","error"}]}`.
    pub fn remove_loop_worktrees(&self, context: &ProjectContext) -> Value {
        let (mut removed, mut failures): (Vec<Value>, Vec<Value>) = (vec![], vec![]);
        for repo in project_repos::load_repos(context.repo_path()) {
            let (found, reason) = self.inspect_repo_worktree(&repo);
            let leftover = nt_normpath(&repo.loop_worktree_path());
            if found.is_none() && reason.is_some() && is_empty_folder(&leftover) && remove_emptied_folder(&leftover) {
                // an EMPTY folder left at the loop worktree path (an earlier removal on Windows emptied it but could not delete it)
                removed.push(json!({"repo": repo.name, "path": leftover, "branch": null, "branch_deleted": false, "folder_left": false}));
                continue;
            }
            let Some(found) = found else {
                if let Some(reason) = reason {
                    failures.push(json!({"repo": repo.name, "path": nt_normpath(&repo.loop_worktree_path()), "branch": null, "error": format!("not removed: {reason}")}));
                }
                continue;
            };
            let outcome = self.app.git.remove_worktree_and_branch(&found.main_repo, &found.path, &found.branch);
            let item = |extra: &[(&str, Value)]| {
                let mut map = Map::new();
                map.insert("repo".into(), json!(found.repo));
                map.insert("path".into(), json!(found.path));
                map.insert("branch".into(), json!(found.branch));
                for (key, value) in extra {
                    map.insert((*key).to_string(), value.clone());
                }
                Value::Object(map)
            };
            if outcome["worktree_removed"] == true {
                removed.push(item(&[("branch_deleted", outcome["branch_deleted"].clone()), ("folder_left", json!(outcome["folder_left"] == true))]));
                if engine_state::notify_truthy(outcome.get("error")) {
                    failures.push(item(&[("error", outcome["error"].clone())]));
                    // the worktree is gone but the branch could not be deleted, or its emptied folder is still held open
                }
            } else {
                failures.push(item(&[("error", outcome["error"].clone())]));
            }
            self.app.log.info(
                if outcome["worktree_removed"] == true { "reset_worktree_removed" } else { "reset_worktree_not_removed" },
                &[
                    ("project", json!(context.project.id)),
                    ("repo", json!(found.repo)),
                    ("branch", json!(found.branch)),
                    ("branch_deleted", outcome["branch_deleted"].clone()),
                ],
            );
        }
        json!({"worktrees_removed": removed, "worktree_failures": failures})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_without_git_are_skipped_and_empty_leftovers_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let service = app.loop_worktree_removal();
        assert_eq!(service.preview_removable_worktrees(&context), json!({"worktrees": [], "skipped": []}));
        let leftover = dir.path().join("proj-worktree-loop");
        std::fs::create_dir_all(&leftover).unwrap();
        let preview = service.preview_removable_worktrees(&context);
        assert!(
            preview["skipped"][0]["reason"].as_str().unwrap().starts_with("the repo is not a git repository")
                || preview["skipped"][0]["reason"].as_str().unwrap().starts_with("the folder")
        );
        let outcome = service.remove_loop_worktrees(&context);
        assert_eq!((outcome["worktrees_removed"].as_array().unwrap().len(), outcome["worktree_failures"].as_array().unwrap().len()), (1, 0));
        assert!(!leftover.exists());
    }
}
