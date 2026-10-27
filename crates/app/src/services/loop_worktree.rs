// SPDX-License-Identifier: MIT
//! Loop worktrees of ONE project: list (preview), push of the feature branch only, and cleanup WITH a verified backup.
//!
//! A valid target: branch `loop/*`, a sibling folder of the MAIN repo with `-worktree-loop` in its name; never the main repo, never another
//! worktree. Clean: (1) mandatory backup in the output folder (`summary --out` + copy of `.specs/` without node_modules/bin/obj/.git, checked by
//! file count); a failure aborts without deleting; (2) `git worktree remove` without `--force` (only with `force` on a dirty worktree); (3) `git
//! worktree prune`. The branch and its commits are never deleted.

use super::planning_gates::get_integration_branch;
use super::App;
use crate::domain::errors::{AppError, Kind, LOOP_RUNNING_CODE};
use crate::domain::project::ProjectContext;
use crate::infrastructure::claude_client::redact_secrets;
use crate::infrastructure::git_client::{WorktreeInfo, OTHER_COMPARISON_BASES};
use engine_state::loop_io;
use pycompat::{nt_abspath, nt_dirname, nt_normcase, nt_normpath, nt_realpath, Re};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::LazyLock;

pub const LOOP_BRANCH_PREFIX: &str = "loop/";
pub const LOOP_WORKTREE_FOLDER_MARK: &str = "-worktree-loop";
const FOLDERS_OUTSIDE_BACKUP: [&str; 4] = ["node_modules", "bin", "obj", ".git"];
pub const SCRIPT_TIMEOUT_SECONDS: u64 = 180;
const OUTPUT_LIMIT: usize = 4000;
const PUSH_LIMIT: usize = 500;
const BACKUP_ERROR_LIMIT: usize = 200;
pub const MAX_TARGETS: usize = 20;
const PR_MODE_VARIABLE: &str = "LOOP_PR_MODE";
const PR_MODE_OFF: &str = "off";
const BACKUP_FALLBACK_PROJECT_NAME: &str = "project";
const TIMED_OUT_TEXT: &str = "timed out";
const CLOSE_LOOP_SCRIPT: &str = "close_loop";
const CLOSE_LOOP_TIMEOUT_SECONDS: u64 = 600; // a close backs up and removes worktrees: longer than a push
pub const CLOSE_FAILED_CODE: &str = "close_failed";
const LOOP_FINISHED_STATUS: &str = "done";
const LOOP_RUNNING_REASON: &str = "The loop is still running; stop it before closing.";

static WORKTREE_MARK_TAIL: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("{}.*\\z", pycompat::escape(LOOP_WORKTREE_FOLDER_MARK))));

pub fn is_same_path(first: &str, second: &str) -> bool {
    nt_normcase(&nt_realpath(first)) == nt_normcase(&nt_realpath(second))
}

/// Last JSON object printed in a script output (delivery prints it indented); `{}` when there is none.
pub fn extract_last_json(text: &str) -> Map<String, Value> {
    let mut start = text.find('{');
    while let Some(at) = start {
        if let Ok(value) = serde_json::from_str::<Value>(text[at..].trim()) {
            return value.as_object().cloned().unwrap_or_default();
        }
        start = text[at + 1..].find('{').map(|i| at + 1 + i);
    }
    Map::new()
}

fn count_backup_files(folder: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(folder) else { return 0 };
    let mut total = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() {
            if !FOLDERS_OUTSIDE_BACKUP.contains(&name.as_str()) {
                total += count_backup_files(&entry.path());
            }
        } else {
            total += 1;
        }
    }
    total
}

/// `shutil.copytree(source, destination, ignore=ignore_patterns(...))`: any entry named like one of the ignored names is left out.
fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if FOLDERS_OUTSIDE_BACKUP.contains(&name.as_str()) {
            continue;
        }
        let target = destination.join(&name);
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

pub struct LoopWorktreeService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn loop_worktrees(&self) -> LoopWorktreeService<'_> {
        LoopWorktreeService { app: self }
    }
}

fn dirname_of(path: &str) -> String {
    nt_dirname(path)
}

impl LoopWorktreeService<'_> {
    /// 1st item of `git worktree list` from the project repo (which may be the loop worktree itself).
    pub fn get_main_repo(&self, context: &ProjectContext) -> String {
        let worktrees = self.app.git.list_worktrees(context.repo_path());
        match worktrees.first() {
            Some(first) if !first.path.is_empty() && Path::new(&first.path).is_dir() => nt_normpath(&first.path),
            _ => context.repo_path().to_string(),
        }
    }

    fn is_loop_worktree(worktree: &WorktreeInfo, main_repo: &str) -> bool {
        let path = &worktree.path;
        worktree.branch.starts_with(LOOP_BRANCH_PREFIX)
            && Path::new(path).is_dir()
            && !is_same_path(path, main_repo)
            && is_same_path(&dirname_of(path), &dirname_of(main_repo))
            && pycompat::last_segment(path).contains(LOOP_WORKTREE_FOLDER_MARK)
    }

    /// meta.json `base` (the branch the loop forked from), the integration branch, then main/master.
    fn get_comparison_bases(&self, worktree_path: &str) -> Vec<String> {
        let meta = self.app.loop_commands.read_meta(worktree_path);
        let mut bases: Vec<String> = Vec::new();
        if let Some(base) = meta.get("base").and_then(Value::as_str).filter(|b| !b.is_empty()) {
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

    /// `[{"path","branch","dirty","unpushed_commits","has_specs"}]` of the worktrees the Act mode may touch.
    pub fn list_loop_worktrees(&self, context: &ProjectContext) -> Vec<Value> {
        let main_repo = self.get_main_repo(context);
        self.app
            .git
            .list_worktrees(&main_repo)
            .into_iter()
            .filter(|w| Self::is_loop_worktree(w, &main_repo))
            .map(|worktree| {
                let path = worktree.path.clone();
                json!({
                    "path": nt_normpath(&path), "branch": worktree.branch, "dirty": self.app.git.has_uncommitted_changes(&path, true),
                    "unpushed_commits": self.app.git.count_unpushed_commits(&path, &self.get_comparison_bases(&path)),
                    "has_specs": Path::new(&path).join(".specs").is_dir(),
                })
            })
            .collect()
    }

    /// Requested targets -> preview worktrees (by path). One bad target aborts everything.
    pub fn select_targets_from_preview(&self, requested: Option<&Value>, worktrees: &[Value]) -> Result<Vec<Value>, String> {
        let list_message = format!("targets must be a list of paths (up to {MAX_TARGETS})");
        let Some(Value::Array(items)) = requested else { return Err(list_message) };
        if items.len() > MAX_TARGETS || !items.iter().all(|i| i.as_str().is_some_and(|s| !s.trim().is_empty())) {
            return Err(list_message);
        }
        let mut chosen: Vec<Value> = Vec::new();
        for target in items.iter().filter_map(Value::as_str) {
            let Some(found) = worktrees.iter().find(|w| is_same_path(w["path"].as_str().unwrap_or(""), target)) else {
                return Err(format!("target outside the list of loop worktrees: {}", redact_secrets(target, Some(BACKUP_ERROR_LIMIT))));
            };
            if !chosen.contains(found) {
                chosen.push(found.clone());
            }
        }
        Ok(chosen)
    }

    /// Preview paths cited by the user (path, branch name or folder name); the rest is dropped.
    pub fn match_cited_targets(&self, cited: &[String], worktrees: &[Value]) -> Vec<String> {
        worktrees
            .iter()
            .filter(|w| {
                let (path, branch) = (w["path"].as_str().unwrap_or(""), w["branch"].as_str().unwrap_or("").to_lowercase());
                cited.iter().any(|item| !item.trim().is_empty() && (is_same_path(path, item) || [branch.clone(), pycompat::last_segment(path).to_lowercase()].contains(&item.trim().to_lowercase())))
            })
            .filter_map(|w| w["path"].as_str().map(String::from))
            .collect()
    }

    /// `delivery <target>` with LOOP_PR_MODE=off: push only, no PR; delivery refusals still apply.
    pub fn push_worktree_branch(&self, worktree: &Value) -> Value {
        let path = worktree["path"].as_str().unwrap_or("");
        let result = self.app.engine_scripts().run_engine_script_and_wait("delivery", path, &[], SCRIPT_TIMEOUT_SECONDS, &[(PR_MODE_VARIABLE.into(), PR_MODE_OFF.into())]);
        let returned = extract_last_json(&result.output);
        let push = returned.get("push").and_then(Value::as_str).map(|p| redact_secrets(p, Some(PUSH_LIMIT)));
        let output = if result.timed_out { TIMED_OUT_TEXT.to_string() } else { combine_output(&result.output, &result.error) };
        json!({
            "path": path, "branch": worktree["branch"], "ok": !result.timed_out && result.code == Some(0), "push": push,
            "output": redact_secrets(&output, Some(OUTPUT_LIMIT)),
        })
    }

    pub fn get_output_dir(&self) -> String {
        nt_abspath(&self.app.config.output_dir)
    }

    /// Backup in `<output_dir>/loop-<project>-<YYYY-MM-DD-HHMM>/` (summary + specs/), checked. `(folder, None)` or `(folder, Some(error))`.
    fn back_up_worktree(&self, worktree: &Value) -> (String, Option<String>) {
        let path = worktree["path"].as_str().unwrap_or("");
        let project = WORKTREE_MARK_TAIL.replace_lit(pycompat::last_segment(path), "");
        let project = if project.is_empty() { BACKUP_FALLBACK_PROJECT_NAME.to_string() } else { project };
        let stamp = loop_io::local_from_epoch(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0))
            .map(loop_io::iso_seconds)
            .map(|iso| format!("{}-{}{}", &iso[..10], &iso[11..13], &iso[14..16]))
            .unwrap_or_default();
        let base = pycompat::nt_join(&self.get_output_dir(), &format!("loop-{project}-{stamp}"));
        let (mut folder, mut suffix) = (base.clone(), 1);
        while Path::new(&folder).exists() {
            suffix += 1;
            folder = format!("{base}-{suffix}");
        }
        if let Err(failure) = std::fs::create_dir_all(&folder) {
            return (folder, Some(format!("backup failed: {}", redact_secrets(&failure.to_string(), Some(BACKUP_ERROR_LIMIT)))));
        }
        let result = self.app.engine_scripts().run_engine_script_and_wait("loop_summary", path, &["--out".into(), folder.clone()], SCRIPT_TIMEOUT_SECONDS, &[]);
        if result.timed_out || result.code != Some(0) {
            let text = if result.error.is_empty() { &result.output } else { &result.error };
            let trimmed = text.trim();
            let tail: String = trimmed.chars().rev().take(BACKUP_ERROR_LIMIT).collect::<Vec<_>>().into_iter().rev().collect();
            let detail = if tail.is_empty() { "no output".to_string() } else { tail };
            return (folder.clone(), Some(format!("backup failed: loop_summary.py did not finish cleanly ({})", redact_secrets(&detail, Some(BACKUP_ERROR_LIMIT)))));
        }
        let has_markdown = std::fs::read_dir(&folder).map(|entries| entries.flatten().any(|e| e.file_name().to_string_lossy().to_lowercase().ends_with(".md"))).unwrap_or(false);
        if !has_markdown {
            return (folder.clone(), Some(format!("backup failed: the summary was not generated in {folder}")));
        }
        let source = Path::new(path).join(".specs");
        if source.is_dir() {
            let destination = Path::new(&folder).join("specs");
            if let Err(failure) = copy_tree(&source, &destination) {
                return (folder, Some(format!("backup failed: {}", redact_secrets(&failure.to_string(), Some(BACKUP_ERROR_LIMIT)))));
            }
            let (source_files, destination_files) = (count_backup_files(&source), count_backup_files(&destination));
            if source_files != destination_files {
                return (folder, Some(format!("backup failed: the .specs copy does not match ({source_files} x {destination_files} files)")));
            }
        }
        (folder, None)
    }

    pub fn clean_worktree_with_backup(&self, context: &ProjectContext, worktree: &Value, force: bool) -> Value {
        let path = worktree["path"].as_str().unwrap_or("").to_string();
        let mut item = json!({"path": path, "branch": worktree["branch"], "backup": null, "deleted": false, "error": null, "ok": false});
        let main_repo = self.get_main_repo(context);
        if is_same_path(&path, &main_repo) {
            item["error"] = json!("refused: it is the main repo"); // belt and braces: never the main repo
            return item;
        }
        let dirty = worktree["dirty"] == true;
        if dirty && !force {
            item["error"] = json!("worktree with uncommitted changes: confirm again with force:true to delete it anyway");
            return item;
        }
        let (backup_dir, error) = self.back_up_worktree(worktree);
        item["backup"] = json!(backup_dir);
        if let Some(error) = error {
            item["error"] = json!(format!("{error} (nothing was deleted)"));
            return item;
        }
        let mut args = vec!["worktree", "remove"];
        if dirty && force {
            args.push("--force");
        }
        args.push(&path);
        let (code, _, _) = self.app.git.run_git(&main_repo, &args);
        if code != 0 && Path::new(&path).is_dir() {
            item["error"] = json!(format!("git worktree remove failed (the folder is still there; the backup is in {backup_dir})"));
            return item;
        }
        self.app.git.run_git(&main_repo, &["worktree", "prune"]);
        let deleted = !Path::new(&path).is_dir();
        item["deleted"] = json!(deleted);
        item["ok"] = json!(deleted);
        if !deleted {
            item["error"] = json!("the folder is still there after git worktree remove");
        }
        self.app.log.info(if deleted { "loop_worktree_deleted" } else { "loop_worktree_not_deleted" }, &[("project", json!(context.project.id)), ("branch", worktree["branch"].clone()), ("backup", json!(backup_dir))]);
        item
    }

    // ------------------------------------------------ Close loop (the routine itself is the engine's `close-loop`)
    /// `GET .../close-loop/check`: read only. Per-repo readiness comes from the engine routine; this adds the loop state.
    pub fn check_close_loop(&self, context: &ProjectContext) -> Result<Value, AppError> {
        let returned = self.run_close_loop_routine(context, &["--check".to_string()])?;
        let running = self.app.loop_status().is_loop_running(context);
        let repos: Vec<Value> = returned.get("repos").and_then(Value::as_array).into_iter().flatten().filter(|i| i.is_object()).map(|i| Self::complete_repo_readiness(i, running)).collect();
        let can_close = !repos.is_empty() && repos.iter().all(|r| r["can_close"] == true);
        Ok(json!({"ok": true, "can_close": can_close, "loop_finished": self.is_loop_finished(context), "repos": repos}))
    }

    /// `POST .../close-loop`: one failing repo never stops the others; the per-repo outcome is in the body.
    pub fn close_loop(&self, context: &ProjectContext, body: &Value) -> Result<Value, AppError> {
        let flag = |key: &str, message: &str| -> Result<bool, AppError> {
            match body.get(key) {
                None => Ok(false),
                Some(Value::Bool(b)) => Ok(*b),
                Some(_) => Err(AppError::invalid_request(message)),
            }
        };
        // finished: set by wrap-up Finish only (the plain route strips it): the next start begins on a new branch
        let finished = flag("finished", "finished must be true or false")?;
        let allow_missing_pr = flag("allow_missing_pr", "allow_missing_pr must be true or false")?;
        let confirm = flag("confirm_compare_link", "confirm_compare_link must be true or false")?;
        let force_remove = flag("force_remove", "force_remove must be true or false")?;
        let names: Option<Vec<String>> = match body.get("repos") {
            None | Some(Value::Null) => None,
            Some(Value::Array(list)) if list.iter().all(|n| n.as_str().is_some_and(|s| !s.trim().is_empty())) => Some(list.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()).collect()),
            Some(_) => return Err(AppError::invalid_request("repos must be a list of repo names")),
        };
        if self.app.loop_status().is_loop_running(context) {
            return Err(AppError::loop_running("the loop is running: stop it before closing").with_code(LOOP_RUNNING_CODE));
        }
        let mut arguments: Vec<String> = vec!["--close".into(), "--out".into(), self.get_output_dir()];
        for (enabled, flag_text) in [(confirm, "--confirm-compare-link"), (allow_missing_pr, "--allow-missing-pr"), (finished, "--finished"), (force_remove, "--force-remove")] {
            if enabled {
                arguments.push(flag_text.into()); // the engine still backs up first (typed confirmation is the caller's job)
            }
        }
        if let Some(names) = names.filter(|n| !n.is_empty()) {
            arguments.extend(["--repos".into(), names.join(",")]);
        }
        let returned = self.run_close_loop_routine(context, &arguments)?;
        let repos = match returned.get("repos") {
            Some(Value::Object(map)) => map.clone(),
            _ => Map::new(),
        };
        let mut result = returned.clone();
        result.insert("ok".into(), json!(true));
        result.insert("repos".into(), Value::Object(repos.clone()));
        let named = |closed: bool| -> Vec<&String> { repos.iter().filter(|(_, item)| item.is_object() && (item["status"] == "closed") == closed).map(|(name, _)| name).collect() };
        if !result.contains_key("closed") {
            result.insert("closed".into(), json!(named(true)));
        }
        if !result.contains_key("kept") {
            result.insert("kept".into(), json!(named(false)));
        }
        self.app.log.info("close_loop_done", &[("project", json!(context.project.id)), ("closed", json!(result["closed"].as_array().map_or(0, Vec::len))), ("kept", json!(result["kept"].as_array().map_or(0, Vec::len)))]);
        Ok(Value::Object(result))
    }

    /// `harness close-loop <primary repo> <arguments>` prints ONE JSON object last in stdout. Contract: `--check` -> `{"repos": [...]}` with the
    /// per-repo items of the close-loop/check payload; `--close` -> the body of `POST close-loop` without `ok`.
    fn run_close_loop_routine(&self, context: &ProjectContext, arguments: &[String]) -> Result<Map<String, Value>, AppError> {
        let result = self.app.engine_scripts().run_engine_script_and_wait(CLOSE_LOOP_SCRIPT, context.repo_path(), arguments, CLOSE_LOOP_TIMEOUT_SECONDS, &[]);
        let returned = extract_last_json(&result.output);
        if result.timed_out || returned.is_empty() {
            let detail = if result.timed_out {
                TIMED_OUT_TEXT.to_string()
            } else {
                let text = if result.error.is_empty() { result.output.trim() } else { result.error.trim() };
                let tail: String = text.chars().rev().take(BACKUP_ERROR_LIMIT).collect::<Vec<_>>().into_iter().rev().collect();
                if tail.is_empty() { "no output".to_string() } else { tail }
            };
            return Err(AppError::new(Kind::RepoConflict, format!("close routine failed: {}", redact_secrets(&detail, Some(BACKUP_ERROR_LIMIT)))).with_code(CLOSE_FAILED_CODE));
        }
        Ok(returned)
    }

    fn is_loop_finished(&self, context: &ProjectContext) -> bool {
        self.app.loop_status().get_loop_summary(context)["status"] == LOOP_FINISHED_STATUS
    }

    /// Normalizes one engine item to the contract and adds the `loop_running` reason while the loop is alive.
    fn complete_repo_readiness(item: &Value, running: bool) -> Value {
        let mut reasons: Vec<Value> = item.get("reasons").and_then(Value::as_array).into_iter().flatten().filter(|r| r.is_object()).cloned().collect();
        if running && !reasons.iter().any(|r| r["code"] == LOOP_RUNNING_CODE) {
            reasons.insert(0, json!({"code": LOOP_RUNNING_CODE, "text": LOOP_RUNNING_REASON}));
        }
        let only_pr_missing = !reasons.is_empty() && reasons.iter().all(|r| r["code"] == "pr_missing");
        let mut out = item.as_object().cloned().unwrap_or_default();
        let truthy = |key: &str| engine_state::notify_truthy(item.get(key));
        out.insert("can_close".into(), json!(truthy("can_close") && reasons.is_empty()));
        out.insert("reasons".into(), json!(reasons));
        out.insert("needs_confirm".into(), json!(truthy("needs_confirm") && only_pr_missing));
        Value::Object(out)
    }
}

/// `(output + ("\n" + error if error.strip() else "")).strip()`.
pub fn combine_output(output: &str, error: &str) -> String {
    let mut text = output.to_string();
    if !error.trim().is_empty() {
        text.push('\n');
        text.push_str(error);
    }
    text.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_json_and_targets() {
        assert_eq!(extract_last_json("noise {\"push\": \"done\"}\n"), json!({"push": "done"}).as_object().cloned().unwrap());
        assert_eq!(extract_last_json("{\n  \"a\": {\"b\": 1}\n}"), json!({"a": {"b": 1}}).as_object().cloned().unwrap());
        assert!(extract_last_json("no json at all").is_empty());
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let path = dir.path().join("proj-worktree-loop").display().to_string();
        std::fs::create_dir_all(&path).unwrap();
        let worktrees = vec![json!({"path": path, "branch": "loop/x", "dirty": false})];
        let service = app.loop_worktrees();
        assert_eq!(service.select_targets_from_preview(Some(&json!([path.clone(), path.clone()])), &worktrees).unwrap().len(), 1);
        assert!(service.select_targets_from_preview(Some(&json!(["C:\\other"])), &worktrees).unwrap_err().starts_with("target outside the list of loop worktrees: "));
        assert_eq!(service.select_targets_from_preview(None, &worktrees).unwrap_err(), "targets must be a list of paths (up to 20)");
        assert_eq!(service.match_cited_targets(&["LOOP/X".to_string(), "nothing".to_string()], &worktrees), [path.clone()]);
        assert_eq!(service.match_cited_targets(&["proj-worktree-loop".to_string()], &worktrees), [path]);
    }

    #[test]
    fn complete_readiness_adds_the_running_reason_and_limits_confirmation() {
        let done = LoopWorktreeService::complete_repo_readiness(&json!({"can_close": true, "needs_confirm": true, "reasons": [{"code": "pr_missing"}]}), false);
        assert_eq!((done["can_close"].clone(), done["needs_confirm"].clone()), (json!(false), json!(true)));
        let running = LoopWorktreeService::complete_repo_readiness(&json!({"can_close": true, "needs_confirm": true, "reasons": [{"code": "pr_missing"}]}), true);
        assert_eq!((running["reasons"][0]["code"].clone(), running["needs_confirm"].clone()), (json!("loop_running"), json!(false)));
    }

    #[test]
    fn close_loop_checks_its_flags_before_running_anything() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let service = app.loop_worktrees();
        assert_eq!(service.close_loop(&context, &json!({"finished": "yes"})).unwrap_err().message, "finished must be true or false");
        assert_eq!(service.close_loop(&context, &json!({"repos": [1]})).unwrap_err().message, "repos must be a list of repo names");
    }
}
