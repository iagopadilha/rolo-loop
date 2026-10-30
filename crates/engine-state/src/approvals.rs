// SPDX-License-Identifier: MIT
//! Pending approvals of the loop: a standard command that the project policy asks about (apply or create a migration, start the app in
//! development) does NOT stop the run. The guard refuses that one command and writes one request; the agents carry on with the work that does not
//! depend on it; the panel shows ONE card per request; the user answers with one button; the next turn reads the answer and retries (or not).
//!
//! * [`list`] merges the guard's request file with the user's answers.
//! * [`decide`] records an answer: `approve` (this request), `approve` + `always` (this class is added to the project's `commands`, see
//!   [`crate::project_policy`]) or deny. Every answer is journaled in `PERMISSIONS-LOG.json` (kind `command_approval`, no secret: the command text
//!   was masked by the guard). A request that never had a card (production is refused for good and never asked) cannot be answered.
//! * [`write_turn_notes`] writes `loop/APPROVALS.md`, which the turn prompt names with `--approvals` while it exists.

use crate::access_grant::{self, AccessContext, OUTCOME_GRANTED, OUTCOME_REFUSED};
use crate::loop_io::{atomic_write_text, load_json, now_iso};
use crate::loop_state::loop_path;
use crate::project_policy;
use guard_core::policy::{self, APPROVED, DENIED};

/// The guard's own policy module (request ids, classes), for callers that only depend on this crate.
pub use guard_core::policy as guard_policy;
use guard_core::state::loop_file;
use pycompat::json::py_json_dumps_indent;
use serde_json::{json, Map, Value};
use std::path::Path;

pub const KIND_APPROVAL: &str = "command_approval";
pub const STATE_PENDING: &str = "pending";
pub const NOTES_NAME: &str = "APPROVALS.md";
const NOTES_LIMIT: usize = 20;

fn specs_of(work: &str) -> String {
    Path::new(work).join(".specs").display().to_string()
}

/// Every request of the run with its state (`pending`, `approved`, `denied`), oldest first.
pub fn list(work: &str) -> Vec<Value> {
    let specs = specs_of(work);
    let answers = policy::read_decisions(&specs);
    policy::read_requests(&specs)
        .into_iter()
        .map(|mut request| {
            let answer = request["id"].as_str().and_then(|id| answers.get(id));
            let state = answer.and_then(|a| a.get("decision")).and_then(Value::as_str).unwrap_or(STATE_PENDING).to_string();
            let scope = answer.and_then(|a| a.get("scope")).cloned().unwrap_or(Value::Null);
            if let Some(map) = request.as_object_mut() {
                map.insert("state".into(), json!(state));
                map.insert("scope".into(), scope);
                map.insert("answered_at".into(), answer.and_then(|a| a.get("at")).cloned().unwrap_or(Value::Null));
            }
            request
        })
        .collect()
}

pub fn pending(work: &str) -> Vec<Value> {
    list(work).into_iter().filter(|r| r["state"] == STATE_PENDING).collect()
}

/// What the panel and the state payload carry: the pending cards and the counts.
pub fn view(work: &str) -> Value {
    let all = list(work);
    let count = |state: &str| all.iter().filter(|r| r["state"] == state).count();
    let pending: Vec<&Value> = all.iter().filter(|r| r["state"] == STATE_PENDING).collect();
    json!({"pending": pending, "counts": {"pending": count(STATE_PENDING), "approved": count(APPROVED), "denied": count(DENIED)}})
}

/// Records the user's answer to one request. Returns the request with its new state, or the reason it was refused.
pub fn decide(ctx: &AccessContext, id: &str, approve: bool, always: bool, reason: &str) -> Result<Value, String> {
    let reason = if reason.trim().is_empty() {
        if approve {
            "approved in the panel"
        } else {
            "denied in the panel"
        }
    } else {
        reason
    };
    let Some(request) = list(ctx.work).into_iter().find(|r| r["id"] == id) else {
        return Err("no such request: only a command the guard asked about can be answered (production is never asked)".into());
    };
    let class = request["class"].as_str().unwrap_or("");
    let value = format!("{} on DEV: {}", request["what"].as_str().unwrap_or(""), request["command"].as_str().unwrap_or(""));
    let mut detail = if approve {
        if always {
            "approved for this project"
        } else {
            "approved once"
        }
    } else {
        "denied by the user"
    }
    .to_string();
    if approve && always {
        if let Err(error) = project_policy::add_command(ctx, class, reason) {
            let _ = access_grant::record(ctx.work, KIND_APPROVAL, &value, reason, ctx.who, OUTCOME_REFUSED, &error, "");
            return Err(error);
        }
    }
    let specs = specs_of(ctx.work);
    let path = loop_file(&specs, policy::DECISIONS_FILE);
    let mut file = match load_json(Path::new(&path)) {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let mut items = file.get("items").and_then(Value::as_object).cloned().unwrap_or_default();
    items.insert(
        id.to_string(),
        json!({"decision": if approve { APPROVED } else { DENIED }, "scope": if approve && always { "always" } else { "once" }, "at": now_iso(), "who": ctx.who}),
    );
    file.insert("version".into(), json!(1));
    file.insert("items".into(), Value::Object(items));
    if let Some(parent) = Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut text = py_json_dumps_indent(&Value::Object(file), false, 2);
    text.push('\n');
    if let Err(error) = atomic_write_text(Path::new(&path), &text) {
        detail = format!("the answer was not saved ({:?})", error.kind());
        let _ = access_grant::record(ctx.work, KIND_APPROVAL, &value, reason, ctx.who, OUTCOME_REFUSED, &detail, "");
        return Err(detail);
    }
    let _ = access_grant::record(ctx.work, KIND_APPROVAL, &value, reason, ctx.who, if approve { OUTCOME_GRANTED } else { OUTCOME_REFUSED }, &detail, "");
    let _ = write_turn_notes(ctx.work);
    Ok(list(ctx.work).into_iter().find(|r| r["id"] == id).unwrap_or(Value::Null))
}

/// Writes (or removes) `loop/APPROVALS.md`: what is waiting, what was approved and what was denied, for the agents of the next turn. Returns true
/// when the file exists afterwards, which is when the turn prompt carries `--approvals`.
pub fn write_turn_notes(work: &str) -> bool {
    let path = loop_path(work, NOTES_NAME);
    let all = list(work);
    if all.is_empty() {
        let _ = std::fs::remove_file(&path);
        return false;
    }
    let line = |r: &Value| {
        format!(
            "- {} on DEV: `{}`{}\n",
            r["what"].as_str().unwrap_or(""),
            r["command"].as_str().unwrap_or(""),
            if r["slug"].as_str().is_some_and(|s| !s.is_empty()) { format!(" (feature {})", r["slug"].as_str().unwrap_or("")) } else { String::new() }
        )
    };
    let section = |state: &str| -> String { all.iter().rev().filter(|r| r["state"] == state).take(NOTES_LIMIT).map(line).collect::<String>() };
    let mut text = String::from("# Approvals of this run\n\nWritten by the engine from the user's answers in the panel. Do not edit.\n\n");
    for (state, title, rule) in [
        (APPROVED, "Approved: retry now", "The user approved these. Run the command again and go on with the task that was waiting for it."),
        (STATE_PENDING, "Waiting for the user: do not retry yet", "Work on the features and tasks that do not depend on these. Do not stop the run for them."),
        (DENIED, "Denied: do not retry or work around", "The user said no. Leave the task that needs it as not done and say so in your return."),
    ] {
        let body = section(state);
        if !body.is_empty() {
            text.push_str(&format!("## {title}\n{rule}\n\n{body}\n"));
        }
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    atomic_write_text(&path, &text).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard_core::policy::{register_request, Request, CLASS_DEV_RUN, CLASS_MIGRATE_APPLY};

    struct Fixture {
        _dir: tempfile::TempDir,
        repo: String,
        data: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        Fixture { repo: repo.display().to_string(), data, _dir: dir }
    }

    fn ctx(f: &Fixture) -> AccessContext<'_> {
        AccessContext { repo: &f.repo, work: &f.repo, data_dir: &f.data, toolkit_root: "", who: "tester" }
    }

    fn ask(f: &Fixture, class: &'static str, command: &str) -> String {
        let request = Request::new(class, command);
        register_request(&specs_of(&f.repo), &request, Some(2), Some("feat-a"), "executor");
        request.id
    }

    #[test]
    fn a_request_is_one_pending_card_until_the_user_answers() {
        let f = fixture();
        assert_eq!(view(&f.repo)["counts"], json!({"pending": 0, "approved": 0, "denied": 0}));
        let id = ask(&f, CLASS_MIGRATE_APPLY, "dotnet ef database update");
        ask(&f, CLASS_MIGRATE_APPLY, "dotnet ef database update");
        let shown = view(&f.repo);
        assert_eq!(shown["counts"]["pending"], 1);
        assert_eq!(
            (shown["pending"][0]["id"].as_str(), shown["pending"][0]["env"].as_str(), shown["pending"][0]["what"].as_str()),
            (Some(id.as_str()), Some("DEV"), Some("apply a database migration"))
        );
        assert!(write_turn_notes(&f.repo));
        let notes = std::fs::read_to_string(loop_path(&f.repo, NOTES_NAME)).unwrap();
        assert!(notes.contains("Waiting for the user") && notes.contains("dotnet ef database update") && notes.contains("feat-a"), "{notes}");
    }

    #[test]
    fn approve_once_leaves_the_policy_alone_and_is_journaled() {
        let f = fixture();
        let id = ask(&f, CLASS_MIGRATE_APPLY, "dotnet ef database update");
        let done = decide(&ctx(&f), &id, true, false, "").unwrap();
        assert_eq!((done["state"].as_str(), done["scope"].as_str()), (Some("approved"), Some("once")));
        assert!(project_policy::read(&f.repo).commands.is_empty());
        assert_eq!(policy::decision_of(&specs_of(&f.repo), &id).as_deref(), Some("approved"));
        let journal = access_grant::read_journal(&f.repo);
        let last = journal.last().unwrap();
        assert_eq!((last["kind"].as_str(), last["outcome"].as_str(), last["detail"].as_str()), (Some(KIND_APPROVAL), Some("granted"), Some("approved once")));
        assert!(last["value"].as_str().unwrap().contains("apply a database migration on DEV"));
        let notes = std::fs::read_to_string(loop_path(&f.repo, NOTES_NAME)).unwrap();
        assert!(notes.contains("Approved: retry now"), "{notes}");
    }

    #[test]
    fn approve_always_writes_the_simple_project_list() {
        let f = fixture();
        let id = ask(&f, CLASS_DEV_RUN, "npm run dev");
        decide(&ctx(&f), &id, true, true, "the team runs it on DEV").unwrap();
        assert_eq!(project_policy::read(&f.repo).commands, vec!["dev_run"]);
        let last = access_grant::read_journal(&f.repo).pop().unwrap();
        assert_eq!(last["detail"], "approved for this project");
    }

    #[test]
    fn deny_is_recorded_and_unknown_or_production_requests_cannot_be_answered() {
        let f = fixture();
        let id = ask(&f, CLASS_MIGRATE_APPLY, "dotnet ef database update");
        let done = decide(&ctx(&f), &id, false, false, "not now").unwrap();
        assert_eq!(done["state"], "denied");
        let last = access_grant::read_journal(&f.repo).pop().unwrap();
        assert_eq!((last["outcome"].as_str(), last["detail"].as_str(), last["reason"].as_str()), (Some("refused"), Some("denied by the user"), Some("not now")));
        assert!(project_policy::read(&f.repo).commands.is_empty());
        assert!(decide(&ctx(&f), "ap-0000000000000000", true, false, "").is_err());
        assert_eq!(view(&f.repo)["counts"], json!({"pending": 0, "approved": 0, "denied": 1}));
        assert!(std::fs::read_to_string(loop_path(&f.repo, NOTES_NAME)).unwrap().contains("Denied: do not retry"));
    }

    #[test]
    fn no_request_means_no_notes_file() {
        let f = fixture();
        assert!(!write_turn_notes(&f.repo));
        assert!(!loop_path(&f.repo, NOTES_NAME).exists());
    }
}
