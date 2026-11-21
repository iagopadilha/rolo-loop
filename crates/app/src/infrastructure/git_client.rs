// SPDX-License-Identifier: MIT
//! Git client (reads the repo state and creates a new repo). No shell; everything through the process runner; never fails.
//!
//! Never pushes on its own nor touches a protected branch: the loop's push and PR belong to `engine_state::delivery`. The only push here is
//! `commit_all_and_push`, run on the user's click for the user's own folder and current branch.

use crate::infrastructure::process_runner::ProcessRunner;
use pycompat::{nt_abspath, nt_normcase, nt_normpath};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const GIT_TIMEOUT_SECONDS: u64 = 30;
/// A worktree with node_modules/bin/obj takes far longer than a status call to delete.
const WORKTREE_REMOVE_TIMEOUT_SECONDS: u64 = 300;
pub const INTEGRATION_BRANCH_VARIABLE: &str = "LOOP_INTEGRATION_BRANCH";
pub const DEFAULT_INTEGRATION_BRANCH: &str = "developer";
pub const OTHER_COMPARISON_BASES: [&str; 2] = ["main", "master"];
pub const DEFAULT_GIT_USER_NAME: &str = "rolo-loop";
pub const DEFAULT_GIT_USER_EMAIL: &str = "rolo-loop@localhost";
pub const NO_REPLY_EMAIL_DOMAIN: &str = "users.noreply.github.com";
const LEFTOVER_FOLDER_RETRIES: u32 = 5;
const LEFTOVER_FOLDER_RETRY_SECONDS: f64 = 0.4;

fn same_path(first: &str, second: &str) -> bool {
    nt_normcase(&nt_abspath(first)) == nt_normcase(&nt_abspath(second))
}

/// Deletes what `git worktree remove` left of an unregistered worktree (the folder, plus any untracked rest), retrying a few times because
/// Windows releases file handles late. Never follows a link. True when the folder is gone.
pub fn remove_emptied_folder(folder: &str) -> bool {
    let path = Path::new(folder);
    for attempt in 0..LEFTOVER_FOLDER_RETRIES {
        if std::fs::symlink_metadata(path).is_err() {
            return true;
        }
        let is_link = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
        let removed = if is_link { std::fs::remove_dir(path).or_else(|_| std::fs::remove_file(path)) } else { std::fs::remove_dir_all(path) };
        if removed.is_err() {
            std::thread::sleep(Duration::from_secs_f64(LEFTOVER_FOLDER_RETRY_SECONDS * f64::from(attempt + 1)));
        }
    }
    std::fs::symlink_metadata(path).is_err()
}

/// Integration branch (`LOOP_INTEGRATION_BRANCH`, default `developer`) and then main/master.
pub fn default_comparison_bases(integration_env: &str) -> Vec<String> {
    let integration = if integration_env.trim().is_empty() { DEFAULT_INTEGRATION_BRANCH } else { integration_env.trim() };
    std::iter::once(integration).chain(OTHER_COMPARISON_BASES).map(String::from).collect()
}

/// One entry of `git worktree list --porcelain`.
#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
    pub head: String,
    pub is_main: bool,
}

/// One entry of `git status --porcelain -z -uall`.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusEntry {
    pub index: char,
    pub worktree: char,
    pub path: String,
}

#[derive(Clone)]
pub struct GitClient {
    runner: Arc<dyn ProcessRunner>,
    git_binary: String,
}

impl GitClient {
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        GitClient { runner, git_binary: "git".into() }
    }

    fn argv(&self, folder: &str, args: &[&str]) -> Vec<String> {
        let mut argv = vec![self.git_binary.clone(), "-C".to_string(), folder.to_string()];
        argv.extend(args.iter().map(|a| a.to_string()));
        argv
    }

    /// `(code, output stripped, error)` of `git -C folder ...`.
    pub fn run_git(&self, folder: &str, args: &[&str]) -> (i32, String, String) {
        let (code, output, error) = self.runner.run_and_wait(&self.argv(folder, args), folder, GIT_TIMEOUT_SECONDS, None, &[], None);
        (code, output.trim().to_string(), error.trim().to_string())
    }

    /// True if `folder` is inside a git work tree (repo or worktree).
    pub fn is_git_repo(&self, folder: &str) -> bool {
        if folder.is_empty() || !Path::new(folder).is_dir() {
            return false;
        }
        let (code, output, _) = self.run_git(folder, &["rev-parse", "--is-inside-work-tree"]);
        code == 0 && output == "true"
    }

    /// Short sha of HEAD; empty when it cannot be read (no commit, not a repo).
    pub fn get_short_head(&self, folder: &str) -> String {
        let (code, output, _) = self.run_git(folder, &["rev-parse", "--short", "HEAD"]);
        if code == 0 {
            output
        } else {
            String::new()
        }
    }

    /// Current branch name; empty with a detached HEAD or outside a repo.
    pub fn get_current_branch(&self, folder: &str) -> String {
        let (code, output, _) = self.run_git(folder, &["branch", "--show-current"]);
        if code == 0 {
            output
        } else {
            String::new()
        }
    }

    /// `git worktree list --porcelain`. The 1st item is the main repo. `branch` comes without `refs/heads/` (empty with a detached HEAD).
    /// Outside a repo = empty.
    pub fn list_worktrees(&self, folder: &str) -> Vec<WorktreeInfo> {
        let (code, output, _) = self.run_git(folder, &["worktree", "list", "--porcelain"]);
        if code != 0 {
            return vec![];
        }
        let mut worktrees: Vec<WorktreeInfo> = Vec::new();
        let mut current: Option<WorktreeInfo> = None;
        for line in pycompat::splitlines(&output).into_iter().chain(std::iter::once("")) {
            if let Some(rest) = line.strip_prefix("worktree ") {
                current = Some(WorktreeInfo { path: nt_normpath(rest.trim()), branch: String::new(), head: String::new(), is_main: false });
            } else if let (Some(rest), Some(item)) = (line.strip_prefix("branch "), current.as_mut()) {
                let reference = rest.trim();
                item.branch = reference.strip_prefix("refs/heads/").unwrap_or(reference).to_string();
            } else if let (Some(rest), Some(item)) = (line.strip_prefix("HEAD "), current.as_mut()) {
                item.head = rest.trim().to_string();
            } else if line.trim().is_empty() {
                if let Some(mut item) = current.take() {
                    item.is_main = worktrees.is_empty();
                    worktrees.push(item);
                }
            }
        }
        worktrees
    }

    /// HEAD commits not pushed yet: against the upstream; without an upstream, against `origin/<base>` or `<base>` (in the order of `bases`).
    /// An int, or `None` when there is no reference.
    pub fn count_unpushed_commits(&self, folder: &str, bases: &[String]) -> Option<u64> {
        let (code, upstream, _) = self.run_git(folder, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]);
        let mut references: Vec<String> = if code == 0 && !upstream.is_empty() { vec![upstream] } else { vec![] };
        if references.is_empty() {
            let mut seen: Vec<&String> = Vec::new();
            for base in bases.iter().filter(|b| !b.is_empty()) {
                if !seen.contains(&base) {
                    seen.push(base);
                    references.push(format!("origin/{base}"));
                    references.push(base.clone());
                }
            }
        }
        for reference in references {
            let (code, count, _) = self.run_git(folder, &["rev-list", "--count", &format!("{reference}..HEAD")]);
            if code == 0 && !count.is_empty() && count.chars().all(|c| c.is_ascii_digit()) {
                return count.parse().ok();
            }
        }
        None
    }

    /// True if `git status --porcelain` has any line (with `include_untracked=false`, only tracked files).
    pub fn has_uncommitted_changes(&self, folder: &str, include_untracked: bool) -> bool {
        let mut args = vec!["status", "--porcelain"];
        if !include_untracked {
            args.push("--untracked-files=no");
        }
        let (code, output, _) = self.run_git(folder, &args);
        code == 0 && !output.is_empty()
    }

    /// `git worktree remove --force <path>`, `git worktree prune`, then `git branch -D <branch>` (when `branch` is not empty), all run from the
    /// MAIN repo. No merge, no commit, no push: what was in the worktree and not pushed is lost. The caller decides WHAT may be removed.
    /// Returns `{"worktree_removed", "branch_deleted", "folder_left", "error"}`.
    pub fn remove_worktree_and_branch(&self, main_repo: &str, worktree_path: &str, branch: &str) -> Value {
        let mut result = json!({"worktree_removed": false, "branch_deleted": false, "folder_left": false, "error": ""});
        let (code, _, error) = self.runner.run_and_wait(
            &self.argv(main_repo, &["worktree", "remove", "--force", worktree_path]),
            main_repo,
            WORKTREE_REMOVE_TIMEOUT_SECONDS,
            None,
            &[],
            None,
        );
        self.run_git(main_repo, &["worktree", "prune"]);
        let still_registered = self.list_worktrees(main_repo).iter().any(|w| same_path(&w.path, worktree_path));
        if still_registered {
            let error = error.trim();
            let last = error.lines().last().map(String::from).unwrap_or_else(|| code.to_string());
            result["error"] = json!(format!("git worktree remove failed: {last}"));
            return result;
        }
        result["worktree_removed"] = json!(true);
        result["folder_left"] = json!(!remove_emptied_folder(worktree_path));
        if !branch.is_empty() {
            let (code, _, error) = self.run_git(main_repo, &["branch", "-D", branch]);
            result["branch_deleted"] = json!(code == 0);
            if code != 0 {
                result["error"] = json!(format!("git branch -D failed: {}", error.lines().last().map(String::from).unwrap_or_else(|| code.to_string())));
            }
        }
        if result["folder_left"] == true && result["error"] == "" {
            result["error"] = json!(format!("the worktree is gone but its folder is still there (a program keeps it open): close it and delete {worktree_path} by hand"));
        }
        result
    }

    /// `git status --porcelain -z -uall` (every untracked file listed on its own, paths with `/`). The output is NOT stripped (the first column
    /// of a line is a blank for an unstaged change). `None` when git fails.
    pub fn list_status_entries(&self, folder: &str) -> Option<Vec<StatusEntry>> {
        let argv: Vec<String> = ["git", "-c", "core.quotepath=false", "-C", folder, "status", "--porcelain", "-z", "-uall"].iter().map(|s| s.to_string()).collect();
        let (code, output, _) = self.runner.run_and_wait(&argv, folder, GIT_TIMEOUT_SECONDS, None, &[], None);
        if code != 0 {
            return None;
        }
        let tokens: Vec<&str> = output.split('\0').collect();
        let (mut entries, mut position) = (Vec::new(), 0);
        while position < tokens.len() {
            let token = tokens[position];
            position += 1;
            let chars: Vec<char> = token.chars().collect();
            if chars.len() < 4 {
                continue;
            }
            let (index, worktree) = (chars[0], chars[1]);
            if matches!(index, 'R' | 'C') || matches!(worktree, 'R' | 'C') {
                position += 1; // a rename/copy entry carries the original path as the next token
            }
            entries.push(StatusEntry { index, worktree, path: chars[3..].iter().collect() });
        }
        Some(entries)
    }

    /// Throws away the changes of ONE entry of [`GitClient::list_status_entries`]: an untracked file is deleted, a file added to the index is
    /// removed from index and disk, anything else goes back to HEAD (index and working tree). `(ok, error)`. The path must stay inside `folder`.
    pub fn discard_path_changes(&self, folder: &str, entry: &StatusEntry) -> (bool, String) {
        let root = nt_normcase(&pycompat::nt_realpath(folder));
        let target = pycompat::nt_realpath(&Path::new(folder).join(&entry.path).display().to_string());
        let inside = nt_normcase(&target).starts_with(&format!("{root}\\")) && nt_normcase(&target) != root;
        if !inside {
            return (false, "path outside the work folder".into());
        }
        if entry.index == '?' {
            return match std::fs::remove_file(&target) {
                Ok(()) => (true, String::new()),
                Err(failure) => (false, failure.to_string()),
            };
        }
        let args: Vec<&str> =
            if entry.index == 'A' { vec!["rm", "-f", "--", &entry.path] } else { vec!["restore", "--source=HEAD", "--staged", "--worktree", "--", &entry.path] };
        let (code, _, error) = self.run_git(folder, &args);
        (code == 0, error)
    }

    pub fn get_config(&self, folder: &str, key: &str) -> String {
        let (code, output, _) = self.run_git(folder, &["config", key]);
        if code == 0 {
            output
        } else {
            String::new()
        }
    }

    /// `git add -A` + commit (when there is something staged) + push of the CURRENT branch. Only called from a user click. Pushes to the upstream
    /// when one is set, else `git push -u origin <branch>`. Never forces, never switches branch, never touches another branch. Returns
    /// `{"ok", "branch", "committed", "pushed", "commit", "error"}`; `ok` is false at the first failing step (a commit that succeeded stays).
    pub fn commit_all_and_push(&self, folder: &str, message: &str, push: bool) -> Value {
        self.commit_and_push_in(folder, message, push, false)
    }

    /// [`commit_all_and_push`] for a LOOP WORKTREE (`loop_work`): the loop's own `.specs/` state is not committed (the Finish check ignores it and the
    /// close copies the plan home), and only the branch itself is pushed, by explicit refspec: never a force, never another ref.
    pub fn commit_loop_work_and_push(&self, folder: &str, message: &str, push: bool) -> Value {
        self.commit_and_push_in(folder, message, push, true)
    }

    fn commit_and_push_in(&self, folder: &str, message: &str, push: bool, loop_work: bool) -> Value {
        let mut result = json!({"ok": false, "branch": "", "committed": false, "pushed": false, "commit": "", "error": ""});
        let branch = self.get_current_branch(folder);
        if branch.is_empty() {
            result["error"] = json!("the folder has no current branch (detached HEAD): check out a branch first");
            return result;
        }
        result["branch"] = json!(branch);
        if matches!(branch.to_lowercase().as_str(), "main" | "master") {
            result["error"] = json!(format!("`{branch}` is a protected branch: nothing is committed or pushed there"));
            return result;
        }
        let describe = |error: &str, code: i32| if error.is_empty() { code.to_string() } else { error.to_string() };
        let add: Vec<&str> = if loop_work { vec!["add", "-A", "--", ".", ":(exclude).specs"] } else { vec!["add", "-A"] };
        let (code, _, error) = self.run_git(folder, &add);
        if code != 0 {
            result["error"] = json!(format!("git add failed: {}", describe(&error, code)));
            return result;
        }
        let (code, staged, _) = self.run_git(folder, &["diff", "--cached", "--name-only"]);
        if code == 0 && !staged.is_empty() {
            let name = Some(self.get_config(folder, "user.name")).filter(|n| !n.is_empty()).unwrap_or_else(|| DEFAULT_GIT_USER_NAME.into());
            let email = Some(self.get_config(folder, "user.email")).filter(|n| !n.is_empty()).unwrap_or_else(|| DEFAULT_GIT_USER_EMAIL.into());
            let (code, _, error) = self.run_git(folder, &["-c", &format!("user.name={name}"), "-c", &format!("user.email={email}"), "commit", "-m", message]);
            if code != 0 {
                result["error"] = json!(format!("git commit failed: {}", describe(&error, code)));
                return result;
            }
            result["committed"] = json!(true);
            result["commit"] = json!(self.get_short_head(folder));
        }
        if !push {
            result["ok"] = json!(true);
            return result;
        }
        let has_upstream = self.run_git(folder, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]).0 == 0;
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        let args: Vec<&str> = if loop_work {
            vec!["push", "-u", "origin", &refspec]
        } else if has_upstream {
            vec!["push"]
        } else {
            vec!["push", "-u", "origin", &branch]
        };
        let (code, _, error) = self.run_git(folder, &args);
        if code != 0 {
            let last = error.lines().last().map(String::from).unwrap_or_else(|| code.to_string());
            result["error"] = json!(format!("git push failed: {last}"));
            return result;
        }
        result["pushed"] = json!(true);
        result["ok"] = json!(true);
        result
    }

    /// `git init` + `git add -A` + initial commit (`--allow-empty`: an empty folder also gets a commit). Returns `(ok, error)`. Commit identity:
    /// `user` (when given) becomes user.name only for this commit (email = the configured one or `<user>@users.noreply.github.com`); without
    /// `user` and without a configured identity, it uses `rolo-loop`. Nothing is written to the git config. A folder that already is a repo:
    /// no re-init, just `(true, '')` without a commit.
    pub fn init_repo_with_initial_commit(&self, folder: &str, message: &str, user: &str) -> (bool, String) {
        if folder.is_empty() || !Path::new(folder).is_dir() {
            return (false, format!("folder does not exist: {folder}"));
        }
        if self.is_git_repo(folder) && Path::new(folder).join(".git").exists() {
            return (true, String::new());
        }
        let describe = |error: &str, code: i32| if error.is_empty() { code.to_string() } else { error.to_string() };
        let (code, _, error) = self.run_git(folder, &["init"]);
        if code != 0 {
            return (false, format!("git init failed: {}", describe(&error, code)));
        }
        let (code, _, error) = self.run_git(folder, &["add", "-A"]);
        if code != 0 {
            return (false, format!("git add failed: {}", describe(&error, code)));
        }
        let name = if !user.is_empty() {
            user.to_string()
        } else {
            Some(self.get_config(folder, "user.name")).filter(|n| !n.is_empty()).unwrap_or_else(|| DEFAULT_GIT_USER_NAME.into())
        };
        let configured = self.get_config(folder, "user.email");
        let email = if !configured.is_empty() {
            configured
        } else if !user.is_empty() {
            format!("{user}@{NO_REPLY_EMAIL_DOMAIN}")
        } else {
            DEFAULT_GIT_USER_EMAIL.to_string()
        };
        let (code, _, error) =
            self.run_git(folder, &["-c", &format!("user.name={name}"), "-c", &format!("user.email={email}"), "commit", "--allow-empty", "-m", message]);
        if code != 0 {
            return (false, format!("git commit failed: {}", describe(&error, code)));
        }
        (true, String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::process_runner::SystemProcesses;

    fn client() -> GitClient {
        GitClient::new(Arc::new(SystemProcesses))
    }

    #[test]
    fn init_commit_status_and_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("a.txt"), "1").unwrap();
        let (git, f) = (client(), folder.display().to_string());
        assert!(!git.is_git_repo(&f));
        assert_eq!(git.init_repo_with_initial_commit(&f, "init", "me"), (true, String::new()));
        assert!(git.is_git_repo(&f));
        assert_eq!(git.init_repo_with_initial_commit(&f, "again", ""), (true, String::new()));
        assert!(!git.get_short_head(&f).is_empty());
        assert!(!git.has_uncommitted_changes(&f, true));
        std::fs::write(folder.join("a.txt"), "2").unwrap();
        std::fs::write(folder.join("new.txt"), "n").unwrap();
        assert!(git.has_uncommitted_changes(&f, true) && git.has_uncommitted_changes(&f, false));
        let entries = git.list_status_entries(&f).unwrap();
        assert_eq!(entries, vec![StatusEntry { index: ' ', worktree: 'M', path: "a.txt".into() }, StatusEntry { index: '?', worktree: '?', path: "new.txt".into() }]);
        assert_eq!(git.discard_path_changes(&f, &entries[1]), (true, String::new()));
        assert_eq!(git.discard_path_changes(&f, &entries[0]), (true, String::new()));
        assert!(!git.has_uncommitted_changes(&f, true));
        assert_eq!(git.discard_path_changes(&f, &StatusEntry { index: '?', worktree: '?', path: "..\\x".into() }), (false, "path outside the work folder".into()));
        let branch = git.get_current_branch(&f);
        assert!(!branch.is_empty());
        let wt = dir.path().join("app-wt").display().to_string();
        git.run_git(&f, &["worktree", "add", "-q", "-b", "loop/x", &wt]);
        let listed = git.list_worktrees(&f);
        assert_eq!((listed.len(), listed[0].is_main, listed[1].branch.as_str(), listed[1].is_main), (2, true, "loop/x", false));
        let removed = git.remove_worktree_and_branch(&f, &wt, "loop/x");
        assert_eq!(
            (removed["worktree_removed"].clone(), removed["branch_deleted"].clone(), removed["folder_left"].clone(), removed["error"].clone()),
            (json!(true), json!(true), json!(false), json!(""))
        );
        assert_eq!(git.list_worktrees(&f).len(), 1);
        assert_eq!(git.count_unpushed_commits(&f, &["no-such-base".to_string()]), None);
        assert_eq!(default_comparison_bases(" qa "), vec!["qa", "main", "master"]);
    }

    #[test]
    fn saving_a_loop_worktree_commits_the_work_not_the_loop_state_and_pushes_only_its_branch() {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let folder = dir.path().join("work");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(&remote).unwrap();
        let git = client();
        let (f, r) = (folder.display().to_string(), remote.display().to_string());
        git.run_git(&r, &["init", "-q", "--bare"]);
        std::fs::write(folder.join("a.txt"), "1").unwrap();
        assert!(git.init_repo_with_initial_commit(&f, "init", "").0);
        git.run_git(&f, &["remote", "add", "origin", &r]);
        git.run_git(&f, &["checkout", "-q", "-b", "loop/x"]);
        // an upstream pointing at ANOTHER branch name: a plain `git push` would follow it; the explicit refspec never does
        git.run_git(&f, &["config", "branch.loop/x.remote", "origin"]);
        git.run_git(&f, &["config", "branch.loop/x.merge", "refs/heads/somewhere-else"]);
        std::fs::create_dir_all(folder.join(".specs/project/loop")).unwrap();
        std::fs::write(folder.join(".specs/project/loop/state.txt"), "engine state").unwrap();
        std::fs::write(folder.join("work.txt"), "the work").unwrap();
        let saved = git.commit_loop_work_and_push(&f, "save", true);
        assert_eq!((saved["ok"].clone(), saved["committed"].clone(), saved["pushed"].clone()), (json!(true), json!(true), json!(true)), "{saved}");
        let committed = git.run_git(&f, &["show", "--name-only", "--pretty=format:", "HEAD"]).1;
        assert!(committed.contains("work.txt") && !committed.contains(".specs"), "{committed}");
        let branches = git.run_git(&r, &["branch", "--list"]).1;
        assert!(branches.contains("loop/x") && !branches.contains("somewhere-else"), "{branches}");
        // the check's own question, asked the same way: the remote holds the worktree HEAD
        let head = git.run_git(&f, &["rev-parse", "HEAD"]).1;
        assert_eq!(git.run_git(&r, &["rev-parse", "refs/heads/loop/x"]).1, head);
        assert_eq!(git.run_git(&f, &["status", "--porcelain", "--", ".", ":(exclude).specs"]).1, "", "the tree the Finish check looks at is clean");
    }

    #[test]
    fn commit_and_push_guards() {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let folder = dir.path().join("work");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(&remote).unwrap();
        let git = client();
        let (f, r) = (folder.display().to_string(), remote.display().to_string());
        git.run_git(&r, &["init", "-q", "--bare"]);
        std::fs::write(folder.join("a.txt"), "1").unwrap();
        assert!(git.init_repo_with_initial_commit(&f, "init", "").0);
        git.run_git(&f, &["remote", "add", "origin", &r]);
        let branch = git.get_current_branch(&f);
        git.run_git(&f, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(folder.join("b.txt"), "2").unwrap();
        let pushed = git.commit_all_and_push(&f, "feat: b", true);
        assert_eq!(
            (pushed["ok"].clone(), pushed["committed"].clone(), pushed["pushed"].clone(), pushed["branch"].clone()),
            (json!(true), json!(true), json!(true), json!("feature"))
        );
        assert_eq!(git.count_unpushed_commits(&f, &[]), Some(0));
        let nothing = git.commit_all_and_push(&f, "empty", false);
        assert_eq!((nothing["ok"].clone(), nothing["committed"].clone()), (json!(true), json!(false)));
        git.run_git(&f, &["checkout", "-q", &branch]);
        git.run_git(&f, &["branch", "-m", "master"]);
        assert_eq!(git.commit_all_and_push(&f, "x", true)["error"], "`master` is a protected branch: nothing is committed or pushed there");
        assert_eq!(
            git.init_repo_with_initial_commit(&dir.path().join("nope").display().to_string(), "m", ""),
            (false, format!("folder does not exist: {}", dir.path().join("nope").display()))
        );
    }
}
