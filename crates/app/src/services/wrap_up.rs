// SPDX-License-Identifier: MIT
//! Wrap-up of a finished loop (`GET wrap-up`) and Finish (`POST wrap-up/finish`).
//!
//! Finish never invents a delivery or a close: it orchestrates the two existing pieces. Delivery = the engine's delivery through the engine script
//! service (the way the app runs it today, push of the `loop/*` branch + PR, never a merge). Close = `LoopWorktreeService::close_loop` (the engine's
//! close routine: verified backup, plan back to the repository, `last-run.json`, `git worktree remove`).
//!
//! Modes of `finish`:
//! - `safe`: delivery when pending (and the run asked for it), then close WITHOUT force, only when the run asked to remove the worktree
//!   (`meta.delivery.remove_worktree_on_finish`, on for runs that predate it). Anything that would be lost (uncommitted files, unpushed commits, a
//!   push nobody confirmed) refuses with 409 and a reason per repo; nothing is removed. A repo with only a compare link (no PR could be created) is
//!   closed: choosing Finish is the confirmation.
//! - `keep_worktree`: delivery only, the worktree stays.
//! - `remove_anyway`: needs `confirm_text` equal to the repo folder name; no delivery; the verified backup still runs, then the worktree goes with
//!   `git worktree remove --force`. The local branch and its commits stay.
//!
//! Unreviewed critical decisions block every mode unless `accept_unreviewed` is true. A running loop or a running follow-up job refuses.

use super::loop_panel::{effective_cost_cap, read_command_limits};
use super::loop_worktree::extract_last_json;
use super::planning_gates::get_integration_branch;
use super::App;
use crate::domain::errors::{AppError, Kind, LOOP_RUNNING_CODE};
use crate::domain::project::ProjectContext;
use crate::repositories::loop_read_formatting::text_or_empty;
use engine_state::loop_state;
use serde_json::{json, Map, Value};
use std::path::Path;

pub const MODE_SAFE: &str = "safe";
pub const MODE_KEEP_WORKTREE: &str = "keep_worktree";
pub const MODE_REMOVE_ANYWAY: &str = "remove_anyway";
const FINISH_MODES: [&str; 3] = [MODE_SAFE, MODE_KEEP_WORKTREE, MODE_REMOVE_ANYWAY];
const FOLLOW_UP_JOB_TYPE: &str = "follow_up";
const DELIVERY_SCRIPT: &str = "delivery";
const DELIVERY_TIMEOUT_SECONDS: u64 = 300;
const PR_CONFIRMED: [&str; 2] = ["PR created", "PR already existed"];
const COMPARE_LINK_PREFIX: &str = "compare link";
const PUSH_DONE_PREFIX: &str = "done";
const VERIFIED_STATUS: &str = "verified";
pub const WRAP_UP_BLOCKED_CODE: &str = "wrap_up_blocked";
pub const NO_WORKTREE_CODE: &str = "no_worktree";
pub const FOLLOW_UP_RUNNING_CODE: &str = "follow_up_running";
const FOLLOW_UP_RUNNING_MESSAGE: &str = "a follow-up round is running: wait for it to finish";
const LISTED_FILES_LIMIT: usize = 50;
const LISTED_COMMITS_LIMIT: usize = 20;
const RESULT_TEXT_LIMIT: usize = 300;
const REASON_KIND_UNCOMMITTED: &str = "uncommitted";
const REASON_KIND_UNPUSHED: &str = "unpushed";

fn reason_kind_by_code(code: &str) -> Option<&'static str> {
    match code {
        "tree_dirty" => Some(REASON_KIND_UNCOMMITTED),
        "push_unconfirmed" => Some(REASON_KIND_UNPUSHED),
        _ => None,
    }
}

fn py_str(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

/// True when the delivery record (aggregate with `repos`, or a single repo's) says every push is done.
fn is_push_done(result: &Map<String, Value>) -> bool {
    let starts_done = |item: &Value| text_or_empty(item.get("push")).starts_with(PUSH_DONE_PREFIX);
    match result.get("repos") {
        Some(Value::Object(repos)) if !repos.is_empty() => repos.values().filter(|i| i.is_object()).all(starts_done),
        _ => starts_done(&Value::Object(result.clone())),
    }
}

/// Delivery is done when the record says every push is done, or when the close check confirmed every repo's branch on the remote (the remote holds the
/// worktree HEAD): a remote with no PR and no compare link still counts as delivered.
fn is_delivery_done(record: &Map<String, Value>, checked_repos: &[Value]) -> bool {
    is_push_done(record) || (!checked_repos.is_empty() && checked_repos.iter().all(|item| item["pushed"] == true))
}

fn blocking_reasons(item: &Value) -> Vec<Value> {
    item["reasons"].as_array().into_iter().flatten().filter(|r| r["code"] != "pr_missing").cloned().collect()
}

fn build_blocked(message: impl Into<String>, reasons: Vec<Value>) -> AppError {
    AppError::new(Kind::RepoConflict, message).with_code(WRAP_UP_BLOCKED_CODE).with_extra("reasons", json!(reasons))
}

pub struct WrapUpService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn wrap_up(&self) -> WrapUpService<'_> {
        WrapUpService { app: self }
    }
}

impl WrapUpService<'_> {
    // ------------------------------------------------ read
    pub fn get_wrap_up(&self, context: &ProjectContext) -> Result<Value, AppError> {
        let summary = self.app.loop_status().get_loop_summary(context);
        let meta = summary["meta"].as_object().cloned().unwrap_or_default();
        let settings = self.read_delivery_settings(context);
        let record = self.read_delivery_record(context);
        let check = self.read_close_check(context);
        let repos = check["repos"].as_array().cloned().unwrap_or_default();
        let mut delivery = settings.as_object().cloned().unwrap_or_default();
        delivery.insert("done".into(), json!(is_delivery_done(&record, &repos)));
        delivery.insert("integration_branch".into(), json!(self.integration_branch(context)));
        Ok(json!({
            "status": summary["status"], "summary": self.build_summary(context, &meta, &record), "delivery": delivery, "check": check,
            "unreviewed_critical": self.app.decisions().unreviewed_critical(context).len(),
        }))
    }

    fn integration_branch(&self, context: &ProjectContext) -> String {
        let text = std::fs::read(Path::new(&context.specs_dir()).join("project").join("ROADMAP.md")).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        get_integration_branch(self.app, &text)
    }

    /// The run's delivery choices from the loop control (meta.delivery, the pending file, then on/on); a run that predates them keeps today's
    /// behavior (deliver and remove).
    fn read_delivery_settings(&self, context: &ProjectContext) -> Value {
        let saved = self.app.loop_control().read_delivery(context);
        let flag = |key: &str| saved.get(key).and_then(Value::as_bool).unwrap_or(true);
        json!({"push_pr": flag("push_pr"), "remove_worktree_on_finish": flag("remove_worktree_on_finish")})
    }

    fn read_delivery_record(&self, context: &ProjectContext) -> Map<String, Value> {
        self.app.loop_files.read_json(&loop_state::loop_path(&context.work_dir(), "delivery.json"))
    }

    fn build_summary(&self, context: &ProjectContext, meta: &Map<String, Value>, record: &Map<String, Value>) -> Value {
        let (features, _) = loop_state::parse_roadmap(&self.app.loop_files.read_roadmap(&context.work_dir()));
        let truthy = |key: &str| record.get(key).filter(|v| engine_state::notify_truthy(Some(v))).or_else(|| meta.get(key));
        let link = truthy("pr").filter(|l| engine_state::notify_truthy(Some(l))).cloned();
        let pr_status = text_or_empty(truthy("pr_status"));
        json!({
            "features_done": features.iter().filter(|(_, f)| f.get("status") == Some(VERIFIED_STATUS)).count(), "features_total": features.len(),
            "pr_url": if PR_CONFIRMED.contains(&pr_status.as_str()) { link.clone().unwrap_or(Value::Null) } else { Value::Null },
            "compare_url": if pr_status.starts_with(COMPARE_LINK_PREFIX) { link.unwrap_or(Value::Null) } else { Value::Null },
            "cost_usd": meta.get("cost_usd"), "max_cost_usd": self.read_effective_cap(context, meta), "turns": meta.get("turns"),
        })
    }

    /// meta.max_cost_usd (raised in the panel), else the saved command's --max-cost, else 50: the same rule as the panel's cost meter.
    fn read_effective_cap(&self, context: &ProjectContext, meta: &Map<String, Value>) -> Value {
        let command = self.app.loop_commands.read_loop_command(&context.work_dir());
        effective_cost_cap(&Value::Object(meta.clone()), &read_command_limits(&command))
    }

    /// The close check per repo; a project with no worktree has nothing to check (empty list).
    fn read_close_check(&self, context: &ProjectContext) -> Value {
        if !Path::new(&context.project.loop_worktree_path()).is_dir() {
            return json!({"repos": []});
        }
        match self.app.loop_worktrees().check_close_loop(context) {
            Ok(check) => json!({"repos": check["repos"].as_array().into_iter().flatten().map(|item| self.describe_checked_repo(context, item)).collect::<Vec<_>>()}),
            Err(failure) => {
                // the close routine is not available: the wrap-up still shows the rest
                self.app.log.warn("wrap_up_check_unavailable", &[("project", json!(context.project.id)), ("message", json!(failure.message.chars().take(RESULT_TEXT_LIMIT).collect::<String>()))]);
                json!({"repos": []})
            }
        }
    }

    /// The engine item plus the keys the panel reads: repo, pushed, uncommitted (files), unpushed (commits), pr_url, compare_url.
    fn describe_checked_repo(&self, context: &ProjectContext, item: &Value) -> Value {
        let reasons: Vec<&Value> = item["reasons"].as_array().into_iter().flatten().collect();
        let has = |code: &str| reasons.iter().any(|r| r["code"] == code);
        let name = item["name"].as_str().unwrap_or("");
        let worktree = if Path::new(&context.project.loop_worktree_path()).is_dir() { self.find_worktree(context, name) } else { String::new() };
        let link = item.get("pr").filter(|l| engine_state::notify_truthy(Some(l))).cloned();
        let pr_status = text_or_empty(item.get("pr_status"));
        let notes: Vec<Value> = reasons.iter().filter(|r| r["code"] == "pr_missing").map(|r| json!({"kind": "pr_missing", "blocking": false, "message": format!("{name}: {}", py_str(r.get("text")))})).collect();
        let mut out = item.as_object().cloned().unwrap_or_default();
        out.insert("notes".into(), json!(notes));
        out.insert("repo".into(), item["name"].clone());
        out.insert("pushed".into(), json!(!has("push_unconfirmed")));
        out.insert("uncommitted".into(), json!(if has("tree_dirty") { self.list_uncommitted_files(&worktree) } else { vec![] }));
        out.insert("unpushed".into(), json!(if has("push_unconfirmed") { self.list_unpushed_commits(&worktree, item) } else { vec![] }));
        out.insert("pr_url".into(), if PR_CONFIRMED.contains(&pr_status.as_str()) { link.clone().unwrap_or(Value::Null) } else { Value::Null });
        out.insert("compare_url".into(), if pr_status.starts_with(COMPARE_LINK_PREFIX) { link.unwrap_or(Value::Null) } else { Value::Null });
        Value::Object(out)
    }

    // ------------------------------------------------ finish
    pub fn finish(&self, context: &ProjectContext, body: &Value) -> Result<Value, AppError> {
        let (mode, confirm_text, accept_unreviewed) = Self::validate_body(body)?;
        self.ensure_can_finish(context, &mode, &confirm_text, accept_unreviewed)?;
        let summary = self.app.loop_status().get_loop_summary(context);
        let meta = summary["meta"].as_object().cloned().unwrap_or_default();
        let settings = self.read_delivery_settings(context);
        let before = self.read_close_check(context)["repos"].as_array().cloned().unwrap_or_default(); // the primary comes first: the receipt names its branch
        if mode != MODE_REMOVE_ANYWAY && settings["push_pr"] == true {
            self.deliver_when_pending(context, &before);
        }
        let record = self.read_delivery_record(context);
        let mut receipt = self.build_receipt(context, &meta, &record, &before);
        let closed = if mode == MODE_REMOVE_ANYWAY {
            Some(self.close_worktrees(context, &json!({"force_remove": true, "confirm_compare_link": true, "finished": true}))?)
        } else if mode == MODE_SAFE && settings["remove_worktree_on_finish"] == true {
            Some(self.close_after_check(context)?)
        } else {
            None
        };
        if let Some(closed) = closed {
            let closed_list = closed.get("closed").cloned().unwrap_or(json!([]));
            receipt.insert("backup_dir".into(), closed.get("backup").cloned().unwrap_or(Value::Null));
            receipt.insert("worktree_removed".into(), json!(engine_state::notify_truthy(Some(&closed_list))));
            receipt.insert("closed".into(), closed_list);
            receipt.insert("kept".into(), closed.get("kept").cloned().unwrap_or(json!([])));
        }
        self.app.log.info("wrap_up_finished", &[("project", json!(context.project.id)), ("mode", json!(mode)), ("removed", receipt["worktree_removed"].clone())]);
        Ok(json!({"ok": true, "mode": mode, "receipt": receipt}))
    }

    fn validate_body(body: &Value) -> Result<(String, String, bool), AppError> {
        let empty = json!({});
        let body = if body.is_object() { body } else { &empty };
        let Some(mode) = body.get("mode").and_then(Value::as_str).filter(|m| FINISH_MODES.contains(m)) else {
            return Err(AppError::invalid_request(format!("mode must be one of: {}", FINISH_MODES.join(", "))));
        };
        let confirm_text = match body.get("confirm_text") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.trim().to_string(),
            Some(_) => return Err(AppError::invalid_request("confirm_text must be text")),
        };
        let accept_unreviewed = match body.get("accept_unreviewed") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(AppError::invalid_request("accept_unreviewed must be true or false")),
        };
        Ok((mode.to_string(), confirm_text, accept_unreviewed))
    }

    fn ensure_can_finish(&self, context: &ProjectContext, mode: &str, confirm_text: &str, accept_unreviewed: bool) -> Result<(), AppError> {
        if self.app.loop_status().is_loop_running(context) {
            return Err(AppError::loop_running("the loop is running: wait for it to stop before finishing").with_code(LOOP_RUNNING_CODE));
        }
        if self.app.jobs().get_running_job(context.repo_path()).is_some_and(|job| job["type"] == FOLLOW_UP_JOB_TYPE) {
            return Err(AppError::new(Kind::RepoConflict, FOLLOW_UP_RUNNING_MESSAGE).with_code(FOLLOW_UP_RUNNING_CODE));
        }
        if !Path::new(&context.project.loop_worktree_path()).is_dir() {
            return Err(AppError::new(Kind::RepoConflict, "there is no loop worktree to finish").with_code(NO_WORKTREE_CODE));
        }
        let folder_name = pycompat::last_segment(&pycompat::nt_normpath(context.repo_path())).to_string();
        if mode == MODE_REMOVE_ANYWAY && confirm_text != folder_name {
            return Err(AppError::invalid_request(format!("type the repository folder name ({folder_name}) to confirm removing the worktree anyway")));
        }
        let unreviewed = self.app.decisions().unreviewed_critical(context);
        if !unreviewed.is_empty() && !accept_unreviewed {
            let message = format!("{} critical decision(s) were not reviewed: review them, or accept them as decided", unreviewed.len());
            let ids: Vec<&Value> = unreviewed.iter().map(|i| &i["id"]).collect();
            let reason = json!({"repo": null, "kind": "unreviewed_critical", "files": [], "commits": [], "decisions": ids, "message": message});
            return Err(build_blocked(message, vec![reason]));
        }
        Ok(())
    }

    /// Push + PR through the engine's delivery script, only when the delivery did not complete or a repo holds commits the remote lacks (a later round);
    /// a PR that already exists is reused by the script. It never merges and never raises.
    fn deliver_when_pending(&self, context: &ProjectContext, checked_repos: &[Value]) {
        let unconfirmed = checked_repos.iter().any(|item| item["reasons"].as_array().into_iter().flatten().any(|r| r["code"] == "push_unconfirmed"));
        if is_push_done(&self.read_delivery_record(context)) && !unconfirmed {
            return;
        }
        let result = self.app.engine_scripts().run_engine_script_and_wait(DELIVERY_SCRIPT, &context.work_dir(), &[], DELIVERY_TIMEOUT_SECONDS, &[]);
        let returned = extract_last_json(&result.output);
        if returned.get("push").is_some_and(Value::is_string) {
            let status = returned.get("pr_status").filter(|s| engine_state::notify_truthy(Some(s))).cloned().unwrap_or(json!("not created"));
            let _ = self.app.loop_commands.write_delivery_to_meta(&context.work_dir(), &returned["push"], returned.get("pr").unwrap_or(&Value::Null), &status);
        }
        let push: String = py_str(returned.get("push")).chars().take(RESULT_TEXT_LIMIT).collect();
        self.app.log.info("wrap_up_delivery_ran", &[("project", json!(context.project.id)), ("timed_out", json!(result.timed_out)), ("push", json!(push))]);
    }

    /// The safe close: every repo must be able to close (a compare-link-only repo is confirmed by this very click).
    fn close_after_check(&self, context: &ProjectContext) -> Result<Value, AppError> {
        let checked = self.app.loop_worktrees().check_close_loop(context)?;
        let repos: Vec<Value> = checked["repos"].as_array().cloned().unwrap_or_default();
        let blocked: Vec<&Value> = repos.iter().filter(|item| !blocking_reasons(item).is_empty()).collect();
        if !blocked.is_empty() {
            let reasons: Vec<Value> = blocked.iter().flat_map(|item| self.describe_blocked_repo(context, item)).collect();
            let message = format!("nothing was removed: {}", reasons.iter().map(|r| r["message"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join("; "));
            return Err(build_blocked(message, reasons));
        }
        // a confirmed push with no PR (non-GitHub remote, no gh) is a note, not a blocker: allow_missing_pr lifts only that reason
        let needs_confirm = repos.iter().any(|item| engine_state::notify_truthy(item.get("needs_confirm")));
        self.close_worktrees(context, &json!({"confirm_compare_link": needs_confirm, "allow_missing_pr": true, "finished": true}))
    }

    fn close_worktrees(&self, context: &ProjectContext, close_body: &Value) -> Result<Value, AppError> {
        let result = self.app.loop_worktrees().close_loop(context, close_body)?;
        let failed: Vec<(&String, &Value)> = result["repos"].as_object().into_iter().flatten().filter(|(_, item)| item.is_object() && item["status"] != "closed").collect();
        if !failed.is_empty() {
            let reasons: Vec<Value> = failed
                .iter()
                .map(|(name, item)| {
                    let texts: Vec<String> = item["reasons"].as_array().into_iter().flatten().map(|r| py_str(r.get("text"))).collect();
                    let joined = texts.join("; ");
                    json!({"repo": name, "kind": "close_failed", "files": [], "commits": [], "message": if joined.is_empty() { "the worktree was kept".to_string() } else { joined }})
                })
                .collect();
            let message = format!("the close did not finish: {}", reasons.iter().map(|r| format!("{}: {}", r["repo"].as_str().unwrap_or(""), r["message"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("; "));
            let mut error = build_blocked(message, reasons);
            error.extras.insert("closed".into(), result.get("closed").cloned().unwrap_or(json!([])));
            error.extras.insert("backup".into(), result.get("backup").cloned().unwrap_or(Value::Null));
            return Err(error);
        }
        Ok(result)
    }

    // ------------------------------------------------ reasons and receipt
    /// One 409 reason per blocking check reason of the repo: `{repo, kind, files|commits, message}`.
    fn describe_blocked_repo(&self, context: &ProjectContext, item: &Value) -> Vec<Value> {
        let name = item["name"].as_str().unwrap_or("");
        let worktree = self.find_worktree(context, name);
        let mut reasons = Vec::new();
        for reason in blocking_reasons(item) {
            let code = reason["code"].as_str().unwrap_or("");
            let kind = reason_kind_by_code(code).unwrap_or(if code.is_empty() { "blocked" } else { code });
            let text = if engine_state::notify_truthy(reason.get("text")) { py_str(reason.get("text")) } else { kind.to_string() };
            let mut entry = json!({"repo": name, "kind": kind, "files": [], "commits": [], "message": format!("{name}: {text}")});
            if kind == REASON_KIND_UNCOMMITTED {
                let files = self.list_uncommitted_files(&worktree);
                entry["message"] = json!(format!("{name}: {} uncommitted file(s) would be lost", files.len()));
                entry["files"] = json!(files);
            } else if kind == REASON_KIND_UNPUSHED {
                entry["commits"] = json!(self.list_unpushed_commits(&worktree, item));
            }
            reasons.push(entry);
        }
        reasons
    }

    fn find_worktree(&self, context: &ProjectContext, repo_name: &str) -> String {
        let summary = self.app.loop_status().get_loop_summary(context);
        match summary["meta"]["repos"].get(repo_name).and_then(|r| r.get("worktree")).and_then(Value::as_str).filter(|w| !w.is_empty()) {
            Some(worktree) => worktree.to_string(),
            None => context.work_dir(),
        }
    }

    fn list_uncommitted_files(&self, worktree: &str) -> Vec<String> {
        let (code, output, _) = self.app.git.run_git(worktree, &["status", "--porcelain", "--untracked-files=all", "--", ".", ":(exclude).specs"]);
        if code != 0 {
            return vec![];
        }
        pycompat::splitlines(&output).into_iter().filter(|l| !l.trim().is_empty()).map(|l| l.chars().skip(3).collect::<String>().trim().trim_matches('"').to_string()).take(LISTED_FILES_LIMIT).collect()
    }

    /// `<sha> <subject>` of the commits the remote does not hold: against the remote branch, else against the base the loop forked from.
    fn list_unpushed_commits(&self, worktree: &str, item: &Value) -> Vec<String> {
        let (branch, base) = (item.get("branch").and_then(Value::as_str), item.get("base").and_then(Value::as_str));
        let candidates = [Some(format!("origin/{}", branch.unwrap_or("None"))), Some(format!("origin/{}", base.unwrap_or("None"))), base.map(String::from)];
        for upstream in candidates.into_iter().flatten() {
            if upstream.is_empty() || upstream.ends_with("/None") || upstream == "None" {
                continue;
            }
            let (code, output, _) = self.app.git.run_git(worktree, &["log", "--oneline", "-n", &LISTED_COMMITS_LIMIT.to_string(), &format!("{upstream}..HEAD")]);
            if code == 0 {
                return pycompat::splitlines(&output).into_iter().map(String::from).collect();
            }
        }
        vec![]
    }

    fn build_receipt(&self, context: &ProjectContext, meta: &Map<String, Value>, record: &Map<String, Value>, checked_repos: &[Value]) -> Map<String, Value> {
        let summary = self.build_summary(context, meta, record);
        let primary_branch = checked_repos.first().and_then(|p| p.get("branch")).filter(|b| engine_state::notify_truthy(Some(b)));
        let mut receipt = Map::new();
        receipt.insert("pr_url".into(), summary["pr_url"].clone());
        receipt.insert("compare_url".into(), summary["compare_url"].clone());
        receipt.insert("branch".into(), primary_branch.cloned().or_else(|| meta.get("branch").filter(|b| engine_state::notify_truthy(Some(b))).cloned()).unwrap_or(Value::Null));
        receipt.insert("backup_dir".into(), Value::Null);
        receipt.insert("cost_usd".into(), summary["cost_usd"].clone());
        receipt.insert("max_cost_usd".into(), summary["max_cost_usd"].clone());
        receipt.insert("turns".into(), summary["turns"].clone());
        receipt.insert("worktree_removed".into(), json!(false));
        receipt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_state_and_body_validation() {
        let record = |v: Value| v.as_object().cloned().unwrap();
        assert!(is_push_done(&record(json!({"push": "done: pushed"}))));
        assert!(!is_push_done(&record(json!({"push": "failed"}))));
        assert!(is_push_done(&record(json!({"repos": {"a": {"push": "done"}, "b": {"push": "done 2"}}}))));
        assert!(!is_push_done(&record(json!({"repos": {"a": {"push": "done"}, "b": {}}}))));
        assert!(is_delivery_done(&Map::new(), &[json!({"pushed": true})]) && !is_delivery_done(&Map::new(), &[]));
        assert_eq!(WrapUpService::validate_body(&json!({"mode": "nope"})).unwrap_err().message, "mode must be one of: safe, keep_worktree, remove_anyway");
        assert_eq!(WrapUpService::validate_body(&json!({"mode": "safe", "confirm_text": 1})).unwrap_err().message, "confirm_text must be text");
        assert_eq!(WrapUpService::validate_body(&json!({"mode": "safe", "confirm_text": " x ", "accept_unreviewed": true})).unwrap(), ("safe".into(), "x".into(), true));
    }

    #[test]
    fn finish_refuses_without_a_worktree_and_wrap_up_reads_the_summary() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        std::fs::write(repo.join(".specs/project/ROADMAP.md"), "- [a] status=verified\n- [b] status=pending\n").unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let service = app.wrap_up();
        let view = service.get_wrap_up(&context).unwrap();
        assert_eq!((view["summary"]["features_done"].clone(), view["summary"]["features_total"].clone(), view["check"].clone()), (json!(1), json!(2), json!({"repos": []})));
        assert_eq!((view["delivery"]["push_pr"].clone(), view["delivery"]["done"].clone(), view["unreviewed_critical"].clone()), (json!(true), json!(false), json!(0)));
        let error = service.finish(&context, &json!({"mode": "safe"})).unwrap_err();
        assert_eq!((error.kind, error.code.as_deref()), (Kind::RepoConflict, Some("no_worktree")));
    }
}
