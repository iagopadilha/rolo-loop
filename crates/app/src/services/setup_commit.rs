// SPDX-License-Identifier: MIT
//! Commit setup files and push (BR-03): commits ONLY the setup files of one repo (maps, app CLAUDE.md blocks, `.specs` plan files: the engine's
//! `setup_files`) on the folder's current branch and pushes that branch to its upstream.
//!
//! Refused (409, `reason` + `code`): detached HEAD, `main`/`master`, no upstream, a loop or a job running, nothing to commit. 400 when
//! `confirm_branch` is not the current branch (the user must confirm the branch the dialog named). Nothing else in the folder is staged or
//! committed; the push never forces and never goes to another branch.

use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use crate::infrastructure::git_client::{DEFAULT_GIT_USER_EMAIL, DEFAULT_GIT_USER_NAME};
use engine_state::{setup_files, subprojects};
use serde_json::{json, Value};
use std::path::Path;

const PROTECTED_BRANCHES: [&str; 2] = ["main", "master"];
const DETACHED_CODE: &str = "detached_head";
const PROTECTED_CODE: &str = "protected_branch";
const NO_UPSTREAM_CODE: &str = "no_upstream";
const BUSY_CODE: &str = "busy";
const NOTHING_CODE: &str = "nothing_to_commit";
const PUSH_FAILED_CODE: &str = "push_failed";
const COMMIT_FAILED_CODE: &str = "commit_failed";

pub struct SetupCommitService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn setup_commit(&self) -> SetupCommitService<'_> {
        SetupCommitService { app: self }
    }
}

fn refuse(reason: impl Into<String>, code: &str, extras: &[(&str, Value)]) -> AppError {
    let reason = reason.into();
    let mut error = AppError::new(Kind::RepoConflict, reason.clone()).with_code(code);
    for (key, value) in extras {
        error = error.with_extra(key, value.clone());
    }
    error.with_extra("reason", json!(reason))
}

impl SetupCommitService<'_> {
    /// `{"ok", "repo", "branch", "commit", "pushed_to", "files"}`; see the module text for the refusals.
    pub fn commit_setup(&self, context: &ProjectContext, repo_name: &str, confirm_branch: &Value) -> Result<Value, AppError> {
        let repo = self.app.repo_registry().resolve(context, repo_name)?;
        if !Path::new(&repo.path).is_dir() {
            return Err(AppError::new(Kind::InvalidRepo, format!("the repo folder does not exist: {}", repo.path)));
        }
        let git = &self.app.git;
        let branch = git.get_current_branch(&repo.path);
        if branch.is_empty() {
            return Err(refuse("the folder is on a detached HEAD: check out a branch first", DETACHED_CODE, &[]));
        }
        if PROTECTED_BRANCHES.contains(&branch.to_lowercase().as_str()) {
            return Err(refuse(
                format!("`{branch}` is a protected branch: the app never commits or pushes there. Switch to your working branch (for example developer)"),
                PROTECTED_CODE,
                &[("branch", json!(branch))],
            ));
        }
        if confirm_branch.as_str().map(str::trim) != Some(branch.as_str()) {
            return Err(AppError::new(Kind::InvalidRepo, format!("confirm_branch must be the current branch `{branch}`")).with_extra("branch", json!(branch)));
        }
        let (remote, upstream_branch, upstream) = self.read_upstream(&repo.path, &branch);
        if upstream.is_empty() {
            return Err(refuse(
                format!("`{branch}` has no upstream branch: set one first (git push -u origin {branch})"),
                NO_UPSTREAM_CODE,
                &[("branch", json!(branch))],
            ));
        }
        if self.app.loop_status().is_loop_running(context) || self.app.jobs().get_running_job(context.repo_path()).is_some() {
            return Err(refuse("a loop or a job is running in this project: wait for it to finish", BUSY_CODE, &[]));
        }
        let (entry, _) = subprojects::load(context.repo_path(), Some(&repo.name));
        let paths: Vec<String> = setup_files::list_setup_files(&repo.path, entry.as_ref()).iter().filter_map(|i| i["path"].as_str().map(String::from)).collect();
        if paths.is_empty() {
            return Err(refuse(format!("there are no uncommitted setup files in {}", repo.name), NOTHING_CODE, &[]));
        }
        self.commit_paths(&repo.path, &paths)?;
        let commit = git.get_short_head(&repo.path);
        let (code, _, error) = git.run_git(&repo.path, &["push", &remote, &format!("HEAD:refs/heads/{upstream_branch}")]);
        if code != 0 {
            let last = error.lines().last().map(String::from).unwrap_or_else(|| code.to_string());
            return Err(refuse(
                format!("the commit {commit} was made but the push failed: {last}"),
                PUSH_FAILED_CODE,
                &[("committed", json!(commit)), ("branch", json!(branch))],
            ));
        }
        self.app
            .log
            .info("setup_commit_pushed", &[("project", json!(context.project.id)), ("repo", json!(repo.name)), ("branch", json!(branch)), ("files", json!(paths.len()))]);
        Ok(json!({"ok": true, "repo": repo.name, "branch": branch, "commit": commit, "pushed_to": upstream, "files": paths}))
    }

    /// `(remote, branch name at the remote, "remote/branch" text)`; empty strings when the branch tracks nothing.
    fn read_upstream(&self, folder: &str, branch: &str) -> (String, String, String) {
        let remote = self.app.git.get_config(folder, &format!("branch.{branch}.remote"));
        let merge = self.app.git.get_config(folder, &format!("branch.{branch}.merge"));
        match merge.strip_prefix("refs/heads/") {
            Some(name) if !remote.is_empty() => (remote.clone(), name.to_string(), format!("{remote}/{name}")),
            _ => (String::new(), String::new(), String::new()),
        }
    }

    /// Stages and commits exactly `paths` (`commit -- paths` ignores anything else already staged).
    fn commit_paths(&self, folder: &str, paths: &[String]) -> Result<(), AppError> {
        let git = &self.app.git;
        let mut add: Vec<&str> = vec!["add", "--"];
        add.extend(paths.iter().map(String::as_str));
        let (code, _, error) = git.run_git(folder, &add);
        if code != 0 {
            return Err(refuse(format!("git add failed: {}", if error.is_empty() { code.to_string() } else { error }), COMMIT_FAILED_CODE, &[]));
        }
        let name = Some(git.get_config(folder, "user.name")).filter(|n| !n.is_empty()).unwrap_or_else(|| DEFAULT_GIT_USER_NAME.to_string());
        let email = Some(git.get_config(folder, "user.email")).filter(|n| !n.is_empty()).unwrap_or_else(|| DEFAULT_GIT_USER_EMAIL.to_string());
        let (user, mail) = (format!("user.name={name}"), format!("user.email={email}"));
        let mut commit: Vec<&str> = vec!["-c", &user, "-c", &mail, "commit", "-m", setup_files::COMMIT_MESSAGE, "--"];
        commit.extend(paths.iter().map(String::as_str));
        let (code, _, error) = git.run_git(folder, &commit);
        if code != 0 {
            return Err(refuse(format!("git commit failed: {}", if error.is_empty() { code.to_string() } else { error }), COMMIT_FAILED_CODE, &[]));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_come_before_anything_is_committed() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        // not a git repository: no branch
        let error = app.setup_commit().commit_setup(&context, "proj", &json!("main")).unwrap_err();
        assert_eq!((error.kind, error.code.as_deref()), (Kind::RepoConflict, Some("detached_head")));
        assert_eq!(error.extras["reason"], "the folder is on a detached HEAD: check out a branch first");
        assert_eq!(app.setup_commit().commit_setup(&context, "nope", &json!("main")).unwrap_err().kind, Kind::RepoNotFound);
    }
}
