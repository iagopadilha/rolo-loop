// SPDX-License-Identifier: MIT
//! Reopen planning: after the roadmap was approved (or a loop ran) the user goes back to planning to change the roadmap or the repositories. The loop
//! screen only runs; repositories and roadmap are defined in planning.
//!
//! `check_reopen` (read only) lists the loop worktrees the app created and what would be lost. `reopen` (1) refuses while a loop or a claude job
//! runs, (2) refuses with `needs_confirm` when a worktree is dirty or has unpushed commits and the user did not confirm, (3) saves the plan that lives
//! in the loop worktree back into the primary repo (`.specs/` features, phases, project docs, the planning chat, a few kept loop files; never loop
//! state such as LOOP.json/turns, so the next loop starts clean on the ROADMAP statuses), (4) removes ONLY the worktrees the app created (registered
//! in `git worktree list`, `loop/*` branch, the path REPOS.json/the default sibling folder says, never the user's folder), (5) removes the roadmap
//! approval and writes the reopened marker (see `planning_requirement`). Branches and commits are never deleted; nothing is pushed. Reopening is
//! idempotent.

use super::loop_worktree::{is_same_path, LOOP_BRANCH_PREFIX};
use super::specs_reset::KEPT_LOOP_FILE_NAMES;
use super::App;
use crate::domain::errors::{AppError, Kind, LOOP_RUNNING_CODE, PLANNING_BUSY_CODE};
use crate::domain::project::ProjectContext;
use crate::repositories::loop_read_formatting::now_iso;
use crate::repositories::specs_file::is_link_or_junction;
use engine_state::loop_state;
use engine_state::project_repos::{self, RepoRef};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const NEEDS_CONFIRM_CODE: &str = "needs_confirm";
pub const NOT_REMOVABLE_CODE: &str = "worktree_not_removable";
pub const REMOVE_FAILED_CODE: &str = "worktree_remove_failed";
/// Copied whole from the worktree.
const PLAN_TREES: [&str; 2] = [".specs/features", ".specs/project/phases"];
const PLAN_FILES: [&str; 7] = [
    ".specs/project/ROADMAP.md",
    ".specs/project/FLOWS.md",
    ".specs/project/STATE.md",
    ".specs/project/PROJECT.md",
    ".specs/project/RULES.md",
    ".specs/project/TEST-ENVIRONMENT.md",
    ".specs/project/ACCESS.json",
];
/// `.specs/project/loop/internal/planning` (the chat history and its claude session).
const PLANNING_THREAD_TREE: [&str; 2] = ["internal", "planning"];

fn reason_text(code: &str) -> &'static str {
    match code {
        "loop_running" => "The loop is running; stop it first.",
        "tree_dirty" => "The worktree has uncommitted changes (outside .specs/) that would be lost.",
        "unpushed_commits" => "The loop branch has commits that were not pushed. The branch is kept; only the worktree folder goes.",
        _ => "This folder is not a loop worktree created by the app (not registered in git, not on a loop/* branch); it is never removed.",
    }
}

fn to_posix(path: &str) -> String {
    path.replace('\\', "/")
}

fn conflict(message: impl Into<String>, code: &str) -> AppError {
    AppError::new(Kind::RepoConflict, message).with_code(code)
}

/// `shutil.copytree(source, destination, dirs_exist_ok=True)`.
fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn copy_file(source: &Path, target: &Path) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, target).map(|_| ())
}

fn under(base: &str, relative: &str) -> PathBuf {
    relative.split('/').fold(PathBuf::from(base), |p, part| p.join(part))
}

pub struct PlanningReopenService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn planning_reopen(&self) -> PlanningReopenService<'_> {
        PlanningReopenService { app: self }
    }
}

impl PlanningReopenService<'_> {
    // ------------------------------------------------ read
    pub fn check_reopen(&self, context: &ProjectContext) -> Value {
        let running = self.app.loop_status().is_loop_running(context);
        let worktrees = self.describe_worktrees(context, running);
        let can_reopen = !running && worktrees.iter().all(|item| item["can_remove"] == true);
        let needs_confirm = worktrees.iter().any(|item| item["dirty"] == true || item["unpushed"].as_u64().unwrap_or(0) > 0);
        json!({"ok": true, "can_reopen": can_reopen, "blocked_by": if running { json!(LOOP_RUNNING_CODE) } else { Value::Null }, "needs_confirm": needs_confirm, "worktrees": worktrees})
    }

    /// One item per repo whose loop worktree folder exists: `{repo, path, branch, dirty, unpushed, can_remove, reasons}`.
    fn describe_worktrees(&self, context: &ProjectContext, running: bool) -> Vec<Value> {
        let mut items = Vec::new();
        for repo in project_repos::load_repos(context.repo_path()) {
            let path = repo.loop_worktree_path();
            if !Path::new(&path).is_dir() {
                continue;
            }
            let mut item =
                json!({"repo": repo.name, "path": pycompat::nt_normpath(&path), "branch": "", "dirty": false, "unpushed": 0, "can_remove": false, "reasons": []});
            let Some(branch) = self.find_registered_loop_branch(&repo, &path) else {
                item["branch"] = json!(self.app.git.get_current_branch(&path));
                item["reasons"] = json!([{"code": "not_app_worktree", "text": reason_text("not_app_worktree")}]);
                items.push(item);
                continue;
            };
            item["branch"] = json!(branch);
            let dirty = self.has_changes_outside_specs(&path);
            let unpushed = self.app.git.count_unpushed_commits(&path, &self.comparison_bases(&path)).unwrap_or(0);
            item["dirty"] = json!(dirty);
            item["unpushed"] = json!(unpushed);
            let mut reasons: Vec<Value> = Vec::new();
            if running {
                reasons.push(json!({"code": "loop_running", "text": reason_text("loop_running")}));
            } else {
                item["can_remove"] = json!(true);
            }
            if dirty {
                reasons.push(json!({"code": "tree_dirty", "text": reason_text("tree_dirty")}));
            }
            if unpushed > 0 {
                reasons.push(json!({"code": "unpushed_commits", "text": reason_text("unpushed_commits")}));
            }
            item["reasons"] = json!(reasons);
            items.push(item);
        }
        items
    }

    fn comparison_bases(&self, path: &str) -> Vec<String> {
        let meta = self.app.loop_commands.read_meta(path);
        let mut bases: Vec<String> = meta.get("base").and_then(Value::as_str).filter(|b| !b.is_empty()).map(String::from).into_iter().collect();
        bases.push(super::planning_gates::get_integration_branch(self.app, ""));
        bases.extend(crate::infrastructure::git_client::OTHER_COMPARISON_BASES.iter().map(|b| b.to_string()));
        let mut unique: Vec<String> = Vec::new();
        for base in bases {
            if !unique.contains(&base) {
                unique.push(base);
            }
        }
        unique
    }

    /// Branch of `path` when git lists it as a secondary worktree of the repo on a loop/* branch; `None` otherwise.
    fn find_registered_loop_branch(&self, repo: &RepoRef, path: &str) -> Option<String> {
        if is_same_path(path, &repo.path) {
            return None;
        }
        let listed = self.app.git.list_worktrees(&repo.path);
        let worktree = listed.iter().skip(1).find(|w| is_same_path(&w.path, path))?;
        worktree.branch.starts_with(LOOP_BRANCH_PREFIX).then(|| worktree.branch.clone())
    }

    fn has_changes_outside_specs(&self, worktree: &str) -> bool {
        let (code, output, _) = self.app.git.run_git(worktree, &["status", "--porcelain"]);
        if code != 0 {
            return true; // unknown state: ask for confirmation rather than assume clean
        }
        pycompat::splitlines(&output).into_iter().any(|line| !to_posix(line.chars().skip(3).collect::<String>().trim().trim_matches('"')).starts_with(".specs/"))
    }

    // ------------------------------------------------ act
    pub fn reopen(&self, context: &ProjectContext, body: &Value) -> Result<Value, AppError> {
        let confirm = match body.get("confirm_remove_worktrees") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) if !body.is_object() => false,
            Some(_) => return Err(AppError::invalid_request("confirm_remove_worktrees must be true or false")),
        };
        if self.app.loop_status().is_loop_running(context) {
            return Err(AppError::loop_running("the loop is running: stop it before reopening the planning").with_code(LOOP_RUNNING_CODE));
        }
        if self.app.jobs().get_running_job(context.repo_path()).is_some() {
            return Err(
                AppError::new(Kind::JobInProgress, "a claude job (planning reply, roadmap or map) is running: wait for it to finish").with_code(PLANNING_BUSY_CODE)
            );
        }
        let worktrees = self.describe_worktrees(context, false);
        if let Some(blocked) = worktrees.iter().find(|item| item["can_remove"] != true) {
            return Err(conflict(
                format!("a worktree is not a loop worktree created by the app: remove it by hand ({})", blocked["path"].as_str().unwrap_or("")),
                NOT_REMOVABLE_CODE,
            )
            .with_extra("worktrees", json!(worktrees)));
        }
        if worktrees.iter().any(|item| item["dirty"] == true || item["unpushed"].as_u64().unwrap_or(0) > 0) && !confirm {
            return Err(conflict("worktrees with uncommitted or unpushed work: confirm to remove them (the branches are kept)", NEEDS_CONFIRM_CODE)
                .with_extra("worktrees", json!(worktrees)));
        }
        let previous_work_dir = context.work_dir();
        if !is_same_path(&previous_work_dir, context.repo_path()) {
            self.save_plan_into_repo(&previous_work_dir, context.repo_path())?; // a failure aborts here, nothing was removed
        }
        let (removed, failures) = self.remove_worktrees(context, &worktrees);
        let removed_names: Vec<&str> = removed.iter().filter_map(|e| e["repo"].as_str()).collect();
        let mut folders = vec![previous_work_dir.clone()];
        folders.extend(
            worktrees.iter().filter(|item| !removed_names.contains(&item["repo"].as_str().unwrap_or(""))).filter_map(|item| item["path"].as_str().map(String::from)),
        );
        self.invalidate_approval(context, &folders);
        self.app.planning_requirement().mark_reopened(context, &now_iso(), &folders);
        self.app.log.info("planning_reopened", &[("project", json!(context.project.id)), ("removed", json!(removed.len())), ("failed", json!(failures.len()))]);
        if !failures.is_empty() {
            return Err(conflict(format!("some worktrees could not be removed: {}", failures.join("; ")), REMOVE_FAILED_CODE).with_extra("removed", json!(removed)));
        }
        Ok(json!({"ok": true, "reopened": true, "removed": removed}))
    }

    /// The worktree is where planning/loop wrote while it ran: bring the plan back before it is deleted.
    fn save_plan_into_repo(&self, worktree: &str, repo_path: &str) -> Result<(), AppError> {
        let (source_specs, target_specs) = (Path::new(worktree).join(".specs"), Path::new(repo_path).join(".specs"));
        if is_link_or_junction(&source_specs) || is_link_or_junction(&target_specs) {
            return Err(conflict(".specs/ is a symbolic link (or junction): refusing to reopen", NOT_REMOVABLE_CODE));
        }
        let result: std::io::Result<()> = (|| {
            for relative in PLAN_TREES {
                let source = under(worktree, relative);
                if source.is_dir() {
                    copy_tree(&source, &under(repo_path, relative))?;
                }
            }
            for relative in PLAN_FILES {
                let source = under(worktree, relative);
                if source.is_file() {
                    copy_file(&source, &under(repo_path, relative))?;
                }
            }
            let thread_source = PLANNING_THREAD_TREE.iter().fold(loop_state::loop_dir(worktree), |p, part| p.join(part));
            if thread_source.is_dir() {
                copy_tree(&thread_source, &PLANNING_THREAD_TREE.iter().fold(loop_state::loop_dir(repo_path), |p, part| p.join(part)))?;
            }
            for name in KEPT_LOOP_FILE_NAMES {
                let source = loop_state::loop_path(worktree, name);
                if source.is_file() {
                    copy_file(&source, &loop_state::loop_path(repo_path, name))?;
                }
            }
            Ok(())
        })();
        result.map_err(|failure| {
            conflict(
                format!(
                    "the plan could not be saved into the repo before removing the worktree ({}); nothing was removed",
                    failure.to_string().chars().take(200).collect::<String>()
                ),
                REMOVE_FAILED_CODE,
            )
        })
    }

    /// `git worktree remove --force` (the plan was saved; dirt was confirmed) + prune, primary last. Branches stay.
    fn remove_worktrees(&self, context: &ProjectContext, worktrees: &[Value]) -> (Vec<Value>, Vec<String>) {
        let (mut removed, mut failures) = (Vec::new(), Vec::new());
        let repos = project_repos::load_repos(context.repo_path());
        let mut ordered: Vec<&Value> = worktrees.iter().collect();
        ordered.sort_by_key(|item| repos.iter().find(|r| r.name == item["repo"].as_str().unwrap_or("")).is_some_and(|r| r.primary));
        for item in ordered {
            let name = item["repo"].as_str().unwrap_or("");
            let Some(repo) = repos.iter().find(|r| r.name == name) else { continue };
            let path = item["path"].as_str().unwrap_or("");
            if is_same_path(path, &repo.path) {
                failures.push(format!("{name}: refused, it is the repo folder")); // belt and braces: never the user's own folder
                continue;
            }
            self.app.git.run_git(&repo.path, &["worktree", "remove", "--force", path]);
            self.app.git.run_git(&repo.path, &["worktree", "prune"]);
            if Path::new(path).is_dir() {
                failures.push(format!("{name}: git worktree remove failed, the folder is still there"));
                continue;
            }
            removed.push(json!({"repo": name, "path": path, "branch_kept": item["branch"]}));
        }
        (removed, failures)
    }

    fn invalidate_approval(&self, context: &ProjectContext, folders: &[String]) {
        let mut seen: Vec<String> = Vec::new();
        for folder in std::iter::once(context.repo_path().to_string()).chain(folders.iter().cloned()) {
            if seen.contains(&folder) || !Path::new(&folder).is_dir() {
                continue;
            }
            let _ = std::fs::remove_file(loop_state::loop_path(&folder, loop_state::APPROVAL_NAME));
            seen.push(folder);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_without_worktrees_reopens_and_unknown_folders_are_never_removed() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(repo.join(".specs/project/loop/json")).unwrap();
        std::fs::write(repo.join(".specs/project/loop/json/ROADMAP-APPROVED.json"), "{}").unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let service = app.planning_reopen();
        assert_eq!(service.check_reopen(&context), json!({"ok": true, "can_reopen": true, "blocked_by": null, "needs_confirm": false, "worktrees": []}));
        assert_eq!(service.reopen(&context, &json!({"confirm_remove_worktrees": "yes"})).unwrap_err().message, "confirm_remove_worktrees must be true or false");
        assert_eq!(service.reopen(&context, &json!({})).unwrap(), json!({"ok": true, "reopened": true, "removed": []}));
        assert!(!repo.join(".specs/project/loop/json/ROADMAP-APPROVED.json").exists());
        // a plain folder at the worktree path is not an app worktree: reopening refuses and removes nothing
        std::fs::create_dir_all(dir.path().join("proj-worktree-loop")).unwrap();
        let blocked = service.reopen(&context, &json!({})).unwrap_err();
        assert_eq!((blocked.kind, blocked.code.as_deref()), (Kind::RepoConflict, Some("worktree_not_removable")));
        assert!(dir.path().join("proj-worktree-loop").is_dir());
        assert_eq!(service.check_reopen(&context)["worktrees"][0]["reasons"][0]["code"], "not_app_worktree");
    }
}
