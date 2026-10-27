// SPDX-License-Identifier: MIT
//! Follow-up of a finished loop, as a CHAT: a planning-like `claude -p` job (type `follow_up`) turns the user's messages (and the decisions they asked
//! to change) into NEW features of ONE stage `Follow-up <n>` appended to the ROADMAP, with FLOWS entries; the user reviews the stage and **Continue
//! loop** starts a new loop round on the same worktree and branch, with every guard of a normal round.
//!
//! One thread per round whose messages continue one claude session (`--resume`). The job: `--model sonnet` (the job model ceiling), hook profile
//! `planning` (Write/Edit only inside `.specs/`), `Edit` allowed only on ROADMAP.md and FLOWS.md, no MCP, US$ 1 per message and US$ 5 per round, run in
//! the loop worktree. Before each turn the app snapshots the work folder and keeps the text of ROADMAP.md and FLOWS.md; afterwards (finalize) it undoes
//! anything outside those two files and accepts ONLY edits of the round's `Follow-up <n>` stage plus appended FLOWS text (`validate_round_stage`
//! against the round's baseline): otherwise both files go back to their text BEFORE THIS TURN (earlier good turns survive) and the job ends as `error`
//! with the reason.
//!
//! The stage id is the first word of the heading, so the heading is written `## Stage Follow-up-<n> - Follow-up <n>`: one id per round. `Follow-up <n>`
//! with a space is accepted too. Refused (409) while the loop runs, while any claude job runs, without a loop worktree, or (code `follow_up_budget`)
//! once the round spent US$ 5. Run ("Continue loop") closes the round (`thread.closed_at`): the next send is round n+1 with spend 0.

use super::chat::is_number;
use super::claude_jobs::{ClaudeJobCommand, ClaudeJobError, JOB_MODEL_CEILING};
use super::loop_control::Reply;
use super::planning::GUARD_IGNORED_PATHS;
use super::App;
use crate::domain::errors::{AppError, Kind, LOOP_RUNNING_CODE};
use crate::domain::follow_up_stage::{describe_stage_change, list_follow_up_stages, normalize_newlines, validate_round_stage, OPEN_FEATURE_STATUSES};
use crate::domain::project::ProjectContext;
use crate::infrastructure::claude_client::{call_kind, redact_secrets, CommandOptions};
use crate::infrastructure::workspace_file_guard::{WorkspaceFileGuard, WorkspaceSnapshot, DEFAULT_IGNORED_NAMES};
use crate::repositories::loop_read_formatting::{now_iso, text_or_empty};
use crate::repositories::small_stores::{current_follow_up_round, read_follow_up_thread, write_follow_up_thread};
use engine_state::{decisions as decisions_engine, loop_state};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const JOB_TYPE_FOLLOW_UP: &str = "follow_up";
/// Per message; the body can no longer change it.
const FOLLOW_UP_BUDGET_USD: f64 = 1.0;
const FOLLOW_UP_ROUND_BUDGET_USD: f64 = 5.0;
pub const FOLLOW_UP_BUDGET_CODE: &str = "follow_up_budget";
const FOLLOW_UP_TIMEOUT_SECONDS: u64 = 900;
const FOLLOW_UP_TEXT_LIMIT: usize = 8000;
const FOLLOW_UP_REPLY_LIMIT: usize = 20000;
const FOLLOW_UP_PERMISSION_MODE: &str = "dontAsk";
const FOLLOW_UP_HOOK_PROFILE: &str = "planning";
const FOLLOW_UP_DISALLOWED_TOOLS: [&str; 5] = ["Bash", "NotebookEdit", "Agent", "WebFetch", "WebSearch"];
const ROADMAP_PATH: &str = ".specs/project/ROADMAP.md";
const FLOWS_PATH: &str = ".specs/project/FLOWS.md";
const EDITABLE_PATHS: [&str; 2] = [ROADMAP_PATH, FLOWS_PATH];
const DRAFT_RELATIVE_PATH: &str = ".specs/project/loop/internal/follow-up-draft.json";
pub const NO_WORKTREE_CODE: &str = "no_worktree";
pub const NO_STAGE_CODE: &str = "no_follow_up_stage";
pub const FOLLOW_UP_BUSY_CODE: &str = "follow_up_busy";
const FOLLOW_UP_BUDGET_REACHED_REASON: &str = "Follow-up budget of this round reached (US$ 5)";
const FOLLOW_UP_NO_STAGE_REASON: &str = "The follow-up stage has no feature yet";
const FOLLOW_UP_FINISHED_REASON: &str = "The loop worktree is gone (the work was finished): use Back to planning for new work";
const FOLLOW_UP_LOOP_RUNNING_REASON: &str = "The loop is running: wait for it to stop";
const FOLLOW_UP_JOB_RUNNING_REASON: &str = "A claude job is running in this project: wait for it to finish";
const SUMMARIZED_NOTE: &str = "Earlier messages were summarized";
const RETRY_HISTORY_MESSAGES: usize = 6;
const NO_EXTRA_TEXT: &str = "(no extra text: change the decisions listed above)";
/// A resumed turn that fails with one of these (lower case) fragments in `error` lost its session (or the claude build does not know the flag): the
/// turn is retried ONCE as a fresh session. Any other failure (timeout, budget, a real error) is not retried.
const RESUME_FAILURE_MARKERS: [&str; 5] = ["no conversation found", "session not found", "session id", "--resume", "unknown option"];

const FOLLOW_UP_RULES: &str = "You are the planner of a project whose loop already ran. The user wants a FOLLOW-UP: new work on top of what was built.
Turn the request below into NEW features appended to .specs/project/ROADMAP.md as ONE new stage, and add matching entries to .specs/project/FLOWS.md.
Rules:
- Read .specs/project/ROADMAP.md, FLOWS.md, PROJECT.md and RULES.md first (and the decisions listed below, if any). Edit ONLY ROADMAP.md and FLOWS.md.
- APPEND at the end of ROADMAP.md a stage with exactly this heading (the id is the first word, so it carries the number): `## Stage Follow-up-%(n)s - Follow-up %(n)s`
- Never edit, move, reword, reorder or delete any existing line of ROADMAP.md or FLOWS.md above your stage; never edit another stage; never change a status. The app restores both files if you do.
- This is a conversation: in a later message of the same round you may add, edit or remove features of YOUR stage `Follow-up-%(n)s` (and append to FLOWS.md). Slugs of earlier stages are taken.
- Under the new heading write one line per new feature in the ROADMAP grammar already used by the file: `- [slug] status=pending scope=<Small|Medium|Large> kind=<back|front|generic> paths=<paths> deps=<slugs> title=\"<what the user gets>\"`.
  A slug is unique in the whole file (kebab-case, 3-6 words). `refactor-*` slugs follow the refactor rules of the file (`regression=` only when the user asked for it).
  `deps=` may name features of earlier stages (already verified). In a multi-repo project a path carries its repo: `paths=<repo>:<path>` (a path without prefix belongs to the primary repo).
- Append to FLOWS.md one entry per new feature, in the format the file already uses.
- Do not write code, specs or tests; the loop does that. Do not approve anything.
- Reply short: the slugs you added and one line on what each does.";

/// What one chat turn needs in its finalize (job thread): the round, the text before the turn, the round baseline and how it started.
struct FollowUpTurn {
    round_number: i64,
    snapshot: Mutex<Option<WorkspaceSnapshot>>,
    before: [String; 2],
    baseline_roadmap: String,
    baseline_flows: String,
    executed_slugs: Vec<String>,
    resumed: bool,
    /// (context block, request text) to rebuild a fresh prompt on a resume failure.
    fresh_prompt_parts: (String, String),
    /// Set by send right after the job starts (best effort: a very fast job may finish first).
    job_id: Mutex<String>,
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::invalid_request(message)
}

fn repo_conflict(message: impl Into<String>, code: &str) -> AppError {
    AppError::new(Kind::RepoConflict, message).with_code(code)
}

fn float_of(value: Option<&Value>) -> f64 {
    value.and_then(Value::as_f64).unwrap_or(0.0)
}

pub struct FollowUpService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn follow_up(&self) -> FollowUpService<'_> {
        FollowUpService { app: self }
    }
}

fn path_of(work_dir: &str, relative: &str) -> PathBuf {
    relative.split('/').fold(Path::new(work_dir).to_path_buf(), |p, part| p.join(part))
}

impl FollowUpService<'_> {
    // ------------------------------------------------ read
    pub fn get_follow_up(&self, context: &ProjectContext) -> Value {
        let work_dir = context.work_dir();
        let draft = self.read_draft(&work_dir);
        let jobs: Vec<Value> = self.app.jobs().list_jobs(context.repo_path()).into_iter().filter(|j| j["type"] == JOB_TYPE_FOLLOW_UP).collect();
        let roadmap_text = self.read_text(&work_dir, ROADMAP_PATH);
        let round_number = current_follow_up_round(&work_dir);
        let thread = read_follow_up_thread(&work_dir, round_number);
        let stage = Self::find_round_stage(&roadmap_text, round_number).map(|mut stage| {
            stage["name"] = if stage["n"].as_i64().is_some_and(|n| n != 0) { json!(format!("Follow-up {}", stage["n"])) } else { json!("Follow-up") };
            stage
        });
        let finished = !self.has_loop_worktree(context);
        json!({
            "round": round_number,
            "thread": {"messages": thread["messages"], "spent_usd": float_of(thread.get("spent_usd")), "summary_note": text_or_empty(thread.get("summary_note"))},
            "stage": stage, "last_job": jobs.first(),
            "caps": {"message_usd": FOLLOW_UP_BUDGET_USD as i64, "round_usd": FOLLOW_UP_ROUND_BUDGET_USD as i64},
            "can_send": self.decide_can_send(context, &thread, finished),
            "can_continue": self.decide_can_continue(context, stage.as_ref(), finished),
            "finished": finished,
            "draft": {"text": text_or_empty(draft.get("text")), "decisions": draft.get("decisions").and_then(Value::as_array).cloned().unwrap_or_default()},
        })
    }

    /// Why neither send nor continue is possible right now ('' when nothing blocks).
    fn decide_busy_reason(&self, context: &ProjectContext, finished: bool) -> &'static str {
        if finished {
            return FOLLOW_UP_FINISHED_REASON;
        }
        if self.app.loop_status().is_loop_running(context) {
            return FOLLOW_UP_LOOP_RUNNING_REASON;
        }
        if self.app.jobs().get_running_job(context.repo_path()).is_some() {
            return FOLLOW_UP_JOB_RUNNING_REASON;
        }
        ""
    }

    fn decide_can_send(&self, context: &ProjectContext, thread: &Value, finished: bool) -> Value {
        let mut reason = self.decide_busy_reason(context, finished);
        if reason.is_empty() && float_of(thread.get("spent_usd")) >= FOLLOW_UP_ROUND_BUDGET_USD {
            reason = FOLLOW_UP_BUDGET_REACHED_REASON;
        }
        json!({"ok": reason.is_empty(), "reason": reason})
    }

    fn decide_can_continue(&self, context: &ProjectContext, stage: Option<&Value>, finished: bool) -> Value {
        let mut reason = self.decide_busy_reason(context, finished);
        let has_open =
            stage.is_some_and(|s| s["features"].as_array().into_iter().flatten().any(|f| f["status"].as_str().is_some_and(|st| OPEN_FEATURE_STATUSES.contains(&st))));
        if reason.is_empty() && !has_open {
            reason = FOLLOW_UP_NO_STAGE_REASON;
        }
        json!({"ok": reason.is_empty(), "reason": reason})
    }

    fn find_round_stage(roadmap_text: &str, round_number: i64) -> Option<Value> {
        list_follow_up_stages(roadmap_text).into_iter().find(|stage| stage["n"].as_i64() == Some(round_number))
    }

    fn has_loop_worktree(&self, context: &ProjectContext) -> bool {
        let worktree = context.project.loop_worktree_path();
        Path::new(&worktree).is_dir() && pycompat::nt_normcase(&pycompat::nt_abspath(&context.work_dir())) == pycompat::nt_normcase(&pycompat::nt_abspath(&worktree))
    }

    fn read_draft(&self, work_dir: &str) -> Map<String, Value> {
        self.app.specs.read_json(&path_of(work_dir, DRAFT_RELATIVE_PATH))
    }

    /// Merges into the draft file: the `decisions` key belongs to the decisions service and is never touched here.
    fn write_draft_text(&self, work_dir: &str, text: &str) {
        let mut draft = self.read_draft(work_dir);
        draft.insert("text".into(), json!(text));
        draft.entry("decisions").or_insert(json!([]));
        let _ = self.app.specs.write_json_atomic(&path_of(work_dir, DRAFT_RELATIVE_PATH), &Value::Object(draft));
    }

    fn read_text(&self, work_dir: &str, relative: &str) -> String {
        self.app.specs.read_text(&path_of(work_dir, relative)).unwrap_or_default()
    }

    // ------------------------------------------------ send
    pub fn send_follow_up(&self, context: &ProjectContext, body: &Value) -> Result<Value, AppError> {
        let empty = json!({});
        let body = if body.is_object() { body } else { &empty }; // `budget_usd` in the body is ignored: the cap is fixed (US$ 1 per message)
        let text = Self::validate_text(body.get("text"))?;
        let decision_ids = Self::validate_decision_ids(body.get("decisions"))?;
        let work_dir = context.work_dir();
        Self::describe_decisions(&work_dir, &decision_ids, true)?; // an unknown id of the body is a 400
        if text.is_empty() && decision_ids.is_empty() {
            return Err(invalid("write what the follow-up should do, or pick decisions to change"));
        }
        self.ensure_can_start(context)?;
        let before = [self.read_text(&work_dir, ROADMAP_PATH), self.read_text(&work_dir, FLOWS_PATH)];
        let round_number = current_follow_up_round(&work_dir);
        let mut thread = read_follow_up_thread(&work_dir, round_number);
        if float_of(thread.get("spent_usd")) >= FOLLOW_UP_ROUND_BUDGET_USD {
            return Err(repo_conflict(FOLLOW_UP_BUDGET_REACHED_REASON, FOLLOW_UP_BUDGET_CODE));
        }
        if !thread["baseline"].get("roadmap").is_some_and(|r| engine_state::notify_truthy(Some(r))) {
            thread["baseline"] = json!({"roadmap": before[0], "flows": before[1]});
        }
        let (baseline_roadmap, baseline_flows) = (text_or_empty(thread["baseline"].get("roadmap")), text_or_empty(thread["baseline"].get("flows")));
        let marked_ids: Vec<String> =
            self.read_draft(&work_dir).get("decisions").and_then(Value::as_array).into_iter().flatten().map(|i| text_or_empty(Some(i))).collect();
        let mut all_ids = decision_ids.clone();
        for id in marked_ids {
            if !all_ids.contains(&id) {
                all_ids.push(id);
            }
        }
        let decisions_text = Self::describe_decisions(&work_dir, &all_ids, false)?;
        let context_block = self.compose_context_block(context, round_number, &before[0], &baseline_roadmap, &decisions_text);
        let session_id = text_or_empty(thread.get("session_id"));
        let resumed = !session_id.is_empty();
        let prompt = if resumed {
            Self::compose_resumed_prompt(round_number, &text, &context_block)
        } else {
            Self::compose_fresh_prompt(round_number, &text, &context_block, &[], "")
        };
        if let Some(Value::Array(messages)) = thread.get_mut("messages") {
            messages.push(json!({"role": "user", "text": text, "decisions": decision_ids, "at": now_iso()}));
        }
        write_follow_up_thread(&work_dir, round_number, &mut thread);
        let turn = Arc::new(FollowUpTurn {
            round_number,
            snapshot: Mutex::new(None),
            before,
            executed_slugs: Self::list_executed_slugs(&baseline_roadmap),
            baseline_roadmap,
            baseline_flows,
            resumed,
            fresh_prompt_parts: (context_block, text.clone()),
            job_id: Mutex::new(String::new()),
        });
        let (snapshot_turn, snapshot_dir) = (turn.clone(), work_dir.clone());
        let (finish_app, finish_context, finish_turn) = (self.app.shared(), context.clone(), turn.clone());
        let command = ClaudeJobCommand {
            argv: self.build_argv(&session_id),
            stdin_text: prompt,
            timeout_seconds: Some(FOLLOW_UP_TIMEOUT_SECONDS),
            work_dir: Some(work_dir.clone()),
            // the safety snapshot is taken in the job thread, never in the request
            before_start: Some(Box::new(move |_update_progress| -> Result<Option<super::claude_jobs::Prepared>, ClaudeJobError> {
                let ignored: Vec<&str> = GUARD_IGNORED_PATHS.to_vec();
                *snapshot_turn.snapshot.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(WorkspaceFileGuard::default().take_snapshot(&snapshot_dir, &ignored, &DEFAULT_IGNORED_NAMES));
                Ok(None)
            })),
            finalize: Some(Box::new(move |result| finish_app.follow_up().finish_job(&finish_context, &finish_turn, result))),
            ..Default::default()
        };
        self.write_draft_text(&work_dir, &text); // kept if the job fails, so the user does not retype it
        let job = self.app.jobs().start_job(context.repo_path(), JOB_TYPE_FOLLOW_UP, command)?;
        *turn.job_id.lock().unwrap_or_else(|e| e.into_inner()) = job["id"].as_str().unwrap_or("").to_string();
        self.app.log.info(
            "follow_up_sent",
            &[
                ("project", json!(context.project.id)),
                ("job", job["id"].clone()),
                ("number", json!(round_number)),
                ("resumed", json!(resumed)),
                ("decisions", json!(decision_ids.len())),
            ],
        );
        Ok(json!({"ok": true, "job": job}))
    }

    fn build_argv(&self, session_id: &str) -> Vec<String> {
        let mut allowed: Vec<String> = ["Read", "Grep", "Glob"].iter().map(|t| t.to_string()).collect();
        allowed.extend(EDITABLE_PATHS.iter().map(|p| format!("Edit(/{p})")));
        let options = CommandOptions {
            model: JOB_MODEL_CEILING.into(),
            budget_usd: Some(FOLLOW_UP_BUDGET_USD),
            allowed_tools: allowed,
            disallowed_tools: FOLLOW_UP_DISALLOWED_TOOLS.iter().map(|t| t.to_string()).collect(),
            no_mcp: true,
            permission_mode: FOLLOW_UP_PERMISSION_MODE.into(),
            hook_profile: FOLLOW_UP_HOOK_PROFILE.into(),
            resume_session: session_id.into(),
            ..CommandOptions::default()
        };
        self.app.claude.build_command(&options)
    }

    fn validate_text(text: Option<&Value>) -> Result<String, AppError> {
        let text = match text {
            None | Some(Value::Null) => return Ok(String::new()),
            Some(Value::String(s)) => s.trim().to_string(),
            Some(_) => return Err(invalid("text must be text")),
        };
        if text.chars().count() > FOLLOW_UP_TEXT_LIMIT {
            return Err(invalid(format!("text is too long (at most {FOLLOW_UP_TEXT_LIMIT} characters)")));
        }
        Ok(text)
    }

    fn validate_decision_ids(value: Option<&Value>) -> Result<Vec<String>, AppError> {
        let list = match value {
            None | Some(Value::Null) => return Ok(vec![]),
            Some(Value::Array(list)) if list.iter().all(|i| i.as_str().is_some_and(|s| !s.trim().is_empty())) => list,
            Some(_) => return Err(invalid("decisions must be a list of decision ids")),
        };
        let mut ids: Vec<String> = Vec::new();
        for id in list.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// The VALIDATE.md lines the user picked, as prompt text; an unknown id is a 400 (the list changed under the user). `strict=false` (ids marked
    /// earlier in the draft) skips the ids that no longer exist.
    fn describe_decisions(work_dir: &str, decision_ids: &[String], strict: bool) -> Result<String, AppError> {
        if decision_ids.is_empty() {
            return Ok(String::new());
        }
        let (items, unparsed) = decisions_engine::parse(&decisions_engine::read_file(work_dir));
        let all: Vec<&Value> = items.iter().chain(unparsed.iter()).collect();
        let find = |id: &str| all.iter().find(|i| i["id"] == id).copied();
        if strict {
            if let Some(unknown) = decision_ids.iter().find(|id| find(id).is_none()) {
                return Err(invalid(format!("unknown decision id: {unknown}")));
            }
        }
        let lines: Vec<String> =
            decision_ids.iter().filter_map(|id| find(id)).map(|item| format!("- {}", item["raw"].as_str().unwrap_or("").trim_start_matches(['-', ' ']).trim())).collect();
        Ok(lines.join("\n"))
    }

    /// Slugs of the baseline ROADMAP: every earlier stage is already executed (the round's own stage is not in the baseline).
    fn list_executed_slugs(baseline_roadmap: &str) -> Vec<String> {
        loop_state::parse_roadmap(&normalize_newlines(baseline_roadmap)).0.slugs()
    }

    /// Executed stages (read-only), the round's current stage, the loop status and the decisions the user asked to change.
    fn compose_context_block(&self, context: &ProjectContext, round_number: i64, roadmap_before: &str, baseline_roadmap: &str, decisions_text: &str) -> String {
        let (features, _) = loop_state::parse_roadmap(&normalize_newlines(baseline_roadmap));
        let executed: Vec<String> = features.iter().map(|(slug, fields)| format!("{slug} ({})", fields.get("status").unwrap_or("None"))).collect();
        let stage = Self::find_round_stage(roadmap_before, round_number);
        let lines: Vec<String> =
            stage.as_ref().and_then(|s| s["lines"].as_object()).map(|m| m.values().map(|l| format!("- {}", l.as_str().unwrap_or(""))).collect()).unwrap_or_default();
        let current = if lines.is_empty() { "(none yet)".to_string() } else { lines.join("\n") };
        let summary = self.app.loop_status().get_loop_summary(context);
        let py = |v: Option<&Value>| match v {
            None | Some(Value::Null) => "None".to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => pycompat::json::py_json_dumps(other, false),
        };
        let turns = match summary["meta"].get("turns") {
            Some(turns) => py(Some(turns)),
            None => "?".to_string(),
        };
        let mut parts = vec![
            format!("Features of earlier stages (already executed, read-only): {}", if executed.is_empty() { "(none)".to_string() } else { executed.join(", ") }),
            format!("Current stage Follow-up-{round_number} (you may edit it):\n{current}"),
            format!("Loop: status={} turns={turns}", py(summary.get("status"))),
        ];
        if !decisions_text.is_empty() {
            parts.push(format!(
                "Decisions the user asked to CHANGE (from .specs/project/loop/VALIDATE.md; each line: doubt | what was chosen | how to revert):\n{decisions_text}"
            ));
        }
        parts.join("\n\n")
    }

    fn compose_fresh_prompt(number: i64, text: &str, context_block: &str, history: &[Value], summary_note: &str) -> String {
        let mut parts = vec![FOLLOW_UP_RULES.replace("%(n)s", &number.to_string()), context_block.to_string()];
        if !history.is_empty() {
            let lines: Vec<String> =
                history.iter().map(|m| format!("{}: {}", text_or_none(m.get("role")), text_or_empty(m.get("text")).chars().take(1000).collect::<String>())).collect();
            parts.push(format!("{summary_note}. Last messages of this conversation:\n{}", lines.join("\n")));
        }
        parts.push(format!("FOLLOW-UP-NUMBER: {number}\n\nUser request:\n{}", if text.is_empty() { NO_EXTRA_TEXT } else { text }));
        parts.join("\n\n")
    }

    /// The session already holds the rules: a short status preface (the stage may have changed by hand) and the new message.
    fn compose_resumed_prompt(number: i64, text: &str, context_block: &str) -> String {
        format!("Status before this message:\n{context_block}\n\nFOLLOW-UP-NUMBER: {number}\n\nUser request:\n{}", if text.is_empty() { NO_EXTRA_TEXT } else { text })
    }

    /// Shared by send and run: a loop or a claude job running, or no loop worktree, refuses with 409.
    fn ensure_can_start(&self, context: &ProjectContext) -> Result<(), AppError> {
        if self.app.loop_status().is_loop_running(context) {
            return Err(AppError::loop_running("the loop is running: wait for it to stop before a follow-up").with_code(LOOP_RUNNING_CODE));
        }
        if self.app.jobs().get_running_job(context.repo_path()).is_some() {
            return Err(AppError::new(Kind::JobInProgress, "a claude job is already running in this project: wait for it to finish").with_code(FOLLOW_UP_BUSY_CODE));
        }
        if !self.has_loop_worktree(context) {
            return Err(repo_conflict("there is no loop worktree: a follow-up needs the finished loop's worktree (use Back to planning for new work)", NO_WORKTREE_CODE));
        }
        Ok(())
    }

    // ------------------------------------------------ finalize (job thread)
    /// Keeps the turn's stage edit, or restores ROADMAP.md and FLOWS.md to their text BEFORE THIS TURN and ends the job as an error.
    fn finish_job(&self, context: &ProjectContext, turn: &FollowUpTurn, result: Value) -> Value {
        let work_dir = context.work_dir();
        let mut thread = read_follow_up_thread(&work_dir, turn.round_number);
        let mut result = result;
        if turn.resumed && result["ok"] != true && Self::is_resume_failure(&result) {
            result = self.retry_as_fresh_session(context, turn, &mut thread, &result);
        }
        if let Some(snapshot) = turn.snapshot.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let report = WorkspaceFileGuard::default().revert_unexpected_changes(snapshot, &EDITABLE_PATHS);
            let list = |key: &str| report[key].as_array().cloned().unwrap_or_default();
            if !list("reverted").is_empty() || !list("removed").is_empty() || !list("unrestorable").is_empty() {
                self.app.log.warn(
                    "follow_up_guard_reverted",
                    &[
                        ("project", json!(context.project.id)),
                        ("reverted", json!(list("reverted").into_iter().take(50).collect::<Vec<_>>())),
                        ("removed", json!(list("removed").into_iter().take(50).collect::<Vec<_>>())),
                    ],
                );
            }
        }
        let after = [self.read_text(&work_dir, ROADMAP_PATH), self.read_text(&work_dir, FLOWS_PATH)];
        let problem: Option<String> = if result["ok"] != true {
            Some("the follow-up job did not finish cleanly".to_string())
        } else {
            validate_round_stage(&turn.baseline_roadmap, &after[0], &turn.baseline_flows, &after[1], turn.round_number, &turn.executed_slugs)
        };
        let reply = redact_secrets(&text_or_empty(result.get("reply")), Some(FOLLOW_UP_REPLY_LIMIT));
        let cost = if is_number(result.get("cost_usd")) { result["cost_usd"].clone() } else { json!(0) };
        let mut message = Map::new();
        message.insert("role".into(), json!("assistant"));
        message.insert("text".into(), json!(reply));
        message.insert("cost_usd".into(), cost.clone());
        message.insert("job_id".into(), json!(turn.job_id.lock().unwrap_or_else(|e| e.into_inner()).clone()));
        message.insert("at".into(), json!(now_iso()));
        match &problem {
            None => {
                self.write_draft_text(&work_dir, "");
                let changed = describe_stage_change(
                    Self::find_round_stage(&turn.before[0], turn.round_number).as_ref(),
                    Self::find_round_stage(&after[0], turn.round_number).as_ref(),
                );
                message.insert("state".into(), json!("ok"));
                message.insert("error".into(), json!(""));
                message.insert("changed".into(), changed);
                result["reply"] = json!(reply);
            }
            Some(problem) => {
                for (index, path) in EDITABLE_PATHS.iter().enumerate() {
                    let _ = self.app.specs.write_text_atomic(&path_of(&work_dir, path), &turn.before[index]);
                }
                self.app.log.warn("follow_up_reverted", &[("project", json!(context.project.id)), ("reason", json!(problem.chars().take(200).collect::<String>()))]);
                message.insert("state".into(), json!("error"));
                message.insert("error".into(), json!(problem));
                message.insert("changed".into(), json!({"added": [], "removed": [], "edited": []}));
                if result["ok"] == true {
                    result["ok"] = json!(false);
                    result["error"] = json!(problem);
                    result["reply"] = json!(format!("{reply}\n\n(The app restored ROADMAP.md and FLOWS.md: {problem})"));
                }
            }
        }
        if let Some(Value::Array(messages)) = thread.get_mut("messages") {
            messages.push(Value::Object(message));
        }
        let spent = float_of(thread.get("spent_usd")) + cost.as_f64().unwrap_or(0.0); // a failed turn still cost money
        thread["spent_usd"] = json!((spent * 1e6).round_ties_even() / 1e6);
        if engine_state::notify_truthy(result.get("session_id")) {
            thread["session_id"] = result["session_id"].clone();
        }
        write_follow_up_thread(&work_dir, turn.round_number, &mut thread);
        result
    }

    fn is_resume_failure(result: &Value) -> bool {
        let error = text_or_empty(result.get("error")).to_lowercase();
        RESUME_FAILURE_MARKERS.iter().any(|marker| error.contains(marker))
    }

    /// The session is gone: ONE more run as a fresh session with the context block plus the last messages.
    fn retry_as_fresh_session(&self, context: &ProjectContext, turn: &FollowUpTurn, thread: &mut Value, failed: &Value) -> Value {
        let messages = thread["messages"].as_array().cloned().unwrap_or_default();
        let without_last = &messages[..messages.len().saturating_sub(1)]; // the last message is this turn's own user message
        let history = &without_last[without_last.len().saturating_sub(RETRY_HISTORY_MESSAGES)..];
        let (context_block, text) = &turn.fresh_prompt_parts;
        let prompt = Self::compose_fresh_prompt(turn.round_number, text, context_block, history, SUMMARIZED_NOTE);
        thread["summary_note"] = json!(SUMMARIZED_NOTE);
        thread["session_id"] = Value::Null;
        self.app.log.warn(
            "follow_up_resume_failed",
            &[("project", json!(context.project.id)), ("error", json!(text_or_empty(failed.get("error")).chars().take(200).collect::<String>()))],
        );
        let _kind = call_kind("follow_up");
        self.app.claude.run_prompt(&self.build_argv(""), &context.work_dir(), &prompt, FOLLOW_UP_TIMEOUT_SECONDS, &[])
    }

    // ------------------------------------------------ run
    /// A new loop round on the same worktree and branch, started the way the Start dialog starts one: the delivery choices of the first start
    /// (meta.delivery) go in the body, the loop settings come from the ROADMAP header. Clicking Run is the approval of the reviewed stage, so the
    /// approval file is renewed for the new feature list. Once the loop starts the round is closed.
    pub fn run_follow_up(&self, context: &ProjectContext) -> Result<Reply, AppError> {
        self.ensure_can_start(context)?;
        let work_dir = context.work_dir();
        let roadmap_text = self.read_text(&work_dir, ROADMAP_PATH);
        let stages = list_follow_up_stages(&roadmap_text);
        let has_open = stages
            .last()
            .is_some_and(|s| s["features"].as_array().into_iter().flatten().any(|f| f["status"].as_str().is_some_and(|st| OPEN_FEATURE_STATUSES.contains(&st))));
        let Some(last_stage) = stages.last().filter(|_| has_open) else {
            return Err(repo_conflict("there is no Follow-up stage with open features: send a follow-up first", NO_STAGE_CODE));
        };
        let delivery = self.app.loop_control().read_delivery(context); // meta.delivery, then delivery-pending.json, then on/on
        let truthy_or = |key: &str| delivery.get(key).is_none_or(|v| engine_state::notify_truthy(Some(v)));
        let body = json!({"push": truthy_or("push_pr"), "remove_worktree_on_finish": truthy_or("remove_worktree_on_finish")});
        loop_state::approve_roadmap(&work_dir, &now_iso()).map_err(|failure| AppError::new(Kind::InvalidRoadmap, failure))?;
        let round_number = current_follow_up_round(&work_dir);
        self.app.log.info("follow_up_run", &[("project", json!(context.project.id)), ("stage", last_stage["id"].clone())]);
        let (status, payload) = self.app.loop_control().start_new_loop(context, &body);
        if status < 300 {
            let mut thread = read_follow_up_thread(&work_dir, round_number);
            thread["closed_at"] = json!(now_iso());
            write_follow_up_thread(&work_dir, round_number, &mut thread);
        }
        Ok((status, payload))
    }
}

fn text_or_none(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(worktree: bool) -> (tempfile::TempDir, std::sync::Arc<App>, ProjectContext) {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(repo.join(".specs/project/loop")).unwrap();
        std::fs::write(repo.join(".specs/project/ROADMAP.md"), "## Stage 1\n- [a] status=verified\n").unwrap();
        std::fs::write(repo.join(".specs/project/loop/VALIDATE.md"), "- [a] turn=1 level=important Which? | chose: x | revert: y\n").unwrap();
        if worktree {
            std::fs::create_dir_all(dir.path().join("proj-worktree-loop/.specs/project/loop")).unwrap();
            std::fs::write(dir.path().join("proj-worktree-loop/.specs/project/ROADMAP.md"), "## Stage 1\n- [a] status=verified\n").unwrap();
            std::fs::write(dir.path().join("proj-worktree-loop/.specs/project/loop/VALIDATE.md"), "- [a] turn=1 level=important Which? | chose: x | revert: y\n")
                .unwrap();
            std::fs::create_dir_all(dir.path().join("proj-worktree-loop/.specs/project/loop/json")).unwrap();
            std::fs::write(dir.path().join("proj-worktree-loop/.specs/project/loop/json/LOOP.json"), r#"{"status": "done"}"#).unwrap();
        }
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        (dir, app, context)
    }

    #[test]
    fn prompts_carry_the_round_number_and_the_context() {
        let rules = FOLLOW_UP_RULES.replace("%(n)s", "2");
        assert!(rules.contains("`## Stage Follow-up-2 - Follow-up 2`") && !rules.contains("%(n)s"));
        let fresh = FollowUpService::compose_fresh_prompt(2, "", "CTX", &[json!({"role": "user", "text": "hi"})], SUMMARIZED_NOTE);
        assert!(
            fresh.contains("CTX")
                && fresh.contains("Earlier messages were summarized. Last messages of this conversation:\nuser: hi")
                && fresh.ends_with(&format!("FOLLOW-UP-NUMBER: 2\n\nUser request:\n{NO_EXTRA_TEXT}"))
        );
        assert!(FollowUpService::compose_resumed_prompt(3, "more", "CTX").starts_with("Status before this message:\nCTX\n\nFOLLOW-UP-NUMBER: 3"));
    }

    #[test]
    fn input_is_validated() {
        assert_eq!(FollowUpService::validate_text(Some(&json!(5))).unwrap_err().message, "text must be text");
        assert_eq!(FollowUpService::validate_text(Some(&json!("  hi "))).unwrap(), "hi");
        assert!(FollowUpService::validate_text(Some(&json!("x".repeat(8001)))).unwrap_err().message.starts_with("text is too long"));
        assert_eq!(FollowUpService::validate_decision_ids(Some(&json!([" a ", "a", "b"]))).unwrap(), ["a", "b"]);
        assert_eq!(FollowUpService::validate_decision_ids(Some(&json!(["a", ""]))).unwrap_err().message, "decisions must be a list of decision ids");
        assert!(
            FollowUpService::is_resume_failure(&json!({"error": "No conversation found with session ID"}))
                && !FollowUpService::is_resume_failure(&json!({"error": "timeout"}))
        );
    }

    #[test]
    fn read_view_and_refusals() {
        let (_dir, app, context) = setup(false);
        let service = app.follow_up();
        let view = service.get_follow_up(&context);
        assert_eq!((view["round"].clone(), view["finished"].clone(), view["can_send"]["reason"].clone()), (json!(1), json!(true), json!(FOLLOW_UP_FINISHED_REASON)));
        assert_eq!(view["caps"], json!({"message_usd": 1, "round_usd": 5}));
        assert_eq!(service.send_follow_up(&context, &json!({})).unwrap_err().message, "write what the follow-up should do, or pick decisions to change");
        assert_eq!(service.send_follow_up(&context, &json!({"decisions": ["nope"]})).unwrap_err().message, "unknown decision id: nope");
        assert_eq!(service.send_follow_up(&context, &json!({"text": "more"})).unwrap_err().code.as_deref(), Some("no_worktree"));
        assert_eq!(service.run_follow_up(&context).unwrap_err().code.as_deref(), Some("no_worktree"));
    }

    #[test]
    fn with_a_worktree_a_stage_is_needed_to_continue() {
        let (_dir, app, context) = setup(true);
        let service = app.follow_up();
        let view = service.get_follow_up(&context);
        assert_eq!(
            (view["finished"].clone(), view["can_send"]["ok"].clone(), view["can_continue"]["reason"].clone()),
            (json!(false), json!(true), json!(FOLLOW_UP_NO_STAGE_REASON))
        );
        assert_eq!(service.run_follow_up(&context).unwrap_err().code.as_deref(), Some("no_follow_up_stage"));
        let id = app.decisions().read_decisions(&context).0[0]["id"].as_str().unwrap().to_string();
        let text = FollowUpService::describe_decisions(&context.work_dir(), &[id], true).unwrap();
        assert_eq!(text, "- [a] turn=1 level=important Which? | chose: x | revert: y");
        let block = service.compose_context_block(&context, 1, "", "- [a] status=verified\n", &text);
        assert!(block.starts_with(
            "Features of earlier stages (already executed, read-only): a (verified)\n\nCurrent stage Follow-up-1 (you may edit it):\n(none yet)\n\nLoop: status="
        ));
    }
}
