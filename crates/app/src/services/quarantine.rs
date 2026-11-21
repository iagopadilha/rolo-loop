// SPDX-License-Identifier: MIT
//! Quarantine of ONE feature (the loop paused because a feature failed twice): the detail the pause dialog shows, an AI summary of it, and the
//! three things the user can do about it (try again with guidance, skip, throw away uncommitted files).
//!
//! Reads: the ROADMAP (blocked feature, tries, dependents), the pipeline events of the feature (its failed attempts), the verifier transcript
//! (failing tests) and `git status` of the work folder. Writes: only through the existing mechanics (an instruction/answer to the loop for retry
//! and skip, a read-only `claude -p` job for the summary, a git restore of listed files for revert). Texts leave redacted.

use super::agent_history::HISTORY_ID;
use super::chat::{
    busy_jobs_error, is_number, ChatService, CHAT_ALLOWED_TOOLS, CHAT_BUDGET_USD, CHAT_DISALLOWED_TOOLS, CHAT_MODEL, CHAT_TIMEOUT_SECONDS, JOB_STATE_ERROR, JOB_STATE_OK,
    MAX_CONCURRENT_JOBS, READ_ONLY_PREFIX, REPLY_LIMIT, UNEXPECTED_FAILURE,
};
use super::loop_control::Reply;
use super::loop_panel::QUARANTINE_OPTIONS;
use super::loop_parsers::parse_failed_tests;
use super::pipeline::{step_key, GATE_NOTE_PREFIX, STEP_REVIEW, STEP_VERIFY};
use super::App;
use crate::domain::project::ProjectContext;
use crate::infrastructure::claude_client::{call_kind, redact_secrets, CommandOptions};
use crate::repositories::loop_read_formatting::{clip_redacted_text, now_iso, redact_credentials, text_or_empty};
use crate::repositories::project_files::THREAD_BUSY_REASON;
use crate::repositories::small_stores::{read_summary, write_summary};
use engine_state::loop_state;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

const BLOCKED_STATUS: &str = "blocked";
pub const NOT_QUARANTINED_ERROR: &str = "feature is not in quarantine";
const ATTEMPTS_LIMIT: usize = 10;
const REASON_LIMIT: usize = 1200;
const OUT_LIMIT: usize = 300;
const SUMMARY_LIMIT: usize = 1200;
const GUIDANCE_LIMIT: usize = 4000;
const REVERT_PATHS_LIMIT: usize = 200;
const SPECS_FOLDER_PREFIX: &str = ".specs/";
const DEFAULT_RETRY_OPTION: &str = "try the feature again";
const DEFAULT_SKIP_OPTION: &str = "skip the feature and carry on with the others";
const RETRY_OPTION_WORDS: [&str; 2] = ["again", "retry"];
const SKIP_OPTION_WORDS: [&str; 1] = ["skip"];
const GUIDANCE_LABEL: &str = "Guidance from the user: ";
const SUMMARY_JOB_THREAD_PREFIX: &str = "quarantine-summary:";
const SUMMARY_INSTRUCTION: &str = "Explain this quarantine in 2 or 3 plain sentences, no headings and no bullet lists: the root cause as far as the facts show it, and what the user has to decide (try again, with which guidance, or skip). Say so when the facts do not show the cause. You may read the files named above with Read, Grep and Glob.";
const SOURCE_AI: &str = "ai";
const SOURCE_FACTS: &str = "facts";

fn failure(status: u16, message: impl Into<String>) -> Reply {
    (status, json!({"ok": false, "error": message.into()}))
}

/// Slugs of the features that depend on `slug`, directly or not, and are not verified yet (ROADMAP order). `dir:` deps are ignored.
pub fn list_transitive_dependents(features: &loop_state::Features, slug: &str) -> Vec<String> {
    let direct: Vec<(String, Vec<String>)> = features.iter().map(|(name, fields)| (name.clone(), loop_state::split_deps(fields).0)).collect();
    let mut found: BTreeSet<String> = BTreeSet::new();
    let mut pending = vec![slug.to_string()];
    while let Some(current) = pending.pop() {
        for (name, dependencies) in &direct {
            if dependencies.contains(&current) && !found.contains(name) && name != slug {
                found.insert(name.clone());
                pending.push(name.clone());
            }
        }
    }
    features.iter().filter(|(name, f)| found.contains(*name) && f.get("status") != Some("verified")).map(|(name, _)| name.clone()).collect()
}

fn step_text(detail: &Value) -> Option<String> {
    detail["step"].as_u64().map(|step| format!("{} ({})", step_key(step as usize), detail["step_agent"].as_str().unwrap_or("")))
}

/// A plain sentence made only of the facts of the detail (used while there is no AI summary).
pub fn build_facts_sentence(detail: &Value) -> String {
    let mut first = format!("Feature {} is in quarantine after {} failed attempt(s)", detail["id"].as_str().unwrap_or(""), detail["failures"].as_i64().unwrap_or(0));
    if let Some(step) = detail["step"].as_u64() {
        first.push_str(&format!(" at the {} step ({})", step_key(step as usize), detail["step_agent"].as_str().unwrap_or("")));
        if let Some(turn) = detail["turn"].as_i64() {
            first.push_str(&format!(" in turn {turn}"));
        }
    }
    first.push('.');
    let mut parts = vec![first];
    let attempts = detail["attempts"].as_array().cloned().unwrap_or_default();
    if let Some(last_out) = attempts.iter().rev().filter_map(|a| a["out"].as_str()).find(|o| !o.is_empty()) {
        parts.push(format!("Last result: {}.", last_out.trim_end_matches('.')));
    }
    let tests = detail["failed_tests"].as_array().cloned().unwrap_or_default();
    if !tests.is_empty() {
        let names: Vec<&str> = tests.iter().take(3).map(|t| t["name"].as_str().unwrap_or("")).collect();
        parts.push(format!("{} failing test(s): {}{}.", tests.len(), names.join(", "), if tests.len() > 3 { ", ..." } else { "" }));
    }
    let uncommitted = detail["uncommitted"].as_array().map_or(0, Vec::len);
    if uncommitted > 0 {
        parts.push(format!("{uncommitted} uncommitted file(s) were left in the work folder."));
    }
    let impact: Vec<&str> = detail["skip_impact"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !impact.is_empty() {
        parts.push(format!("Skipping it also blocks: {}.", impact.join(", ")));
    }
    parts.join(" ")
}

/// Facts of the quarantine as text for a read-only claude run (summary job and the `quarantine` chat scope).
pub fn build_context_text(detail: &Value, work_dir: &str) -> String {
    let mut lines = vec![
        format!("Quarantined feature: {}.", detail["id"].as_str().unwrap_or("")),
        format!("Reason: {}", detail["reason"].as_str().unwrap_or("")),
        format!("Failures: {}. Step: {}.", detail["failures"].as_i64().unwrap_or(0), step_text(detail).unwrap_or_else(|| "unknown".into())),
    ];
    let attempts = detail["attempts"].as_array().cloned().unwrap_or_default();
    if !attempts.is_empty() {
        lines.push("Failed attempts:".into());
        for attempt in &attempts {
            let out = attempt["out"].as_str().filter(|o| !o.is_empty()).unwrap_or("(no text)");
            lines.push(format!(
                "- turn {} {} {}: {out}",
                text_or_none(&attempt["turn"]),
                attempt["agent"].as_str().unwrap_or(""),
                attempt["status"].as_str().unwrap_or("")
            ));
        }
    }
    let tests = detail["failed_tests"].as_array().cloned().unwrap_or_default();
    if !tests.is_empty() {
        lines.push("Failed tests:".into());
        for test in &tests {
            lines.push(format!(
                "- {} | why: {} | expected: {} | got: {}",
                text_or_none(&test["name"]),
                text_or_none(&test["why"]),
                text_or_none(&test["expected"]),
                text_or_none(&test["got"])
            ));
        }
    }
    let files: Vec<&str> = detail["uncommitted"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !files.is_empty() {
        lines.push(format!("Uncommitted files in the work folder: {}", files.join(", ")));
    }
    let impact: Vec<&str> = detail["skip_impact"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !impact.is_empty() {
        lines.push(format!("If skipped, these features stay blocked too: {}", impact.join(", ")));
    }
    lines.push(format!("Feature files: {}", ChatService::describe_feature_dir(work_dir, detail["id"].as_str().unwrap_or(""))));
    lines.push(ChatService::build_general_context(work_dir));
    redact_secrets(&lines.join("\n"), None)
}

/// Python `str(value)` of a JSON value (`None`, numbers, text).
fn text_or_none(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => pycompat::json::py_json_dumps(other, false),
    }
}

pub struct QuarantineService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn quarantine(&self) -> QuarantineService<'_> {
        QuarantineService { app: self }
    }
}

impl QuarantineService<'_> {
    // ------------------------------------------------ detail
    fn read_roadmap_features(&self, work_dir: &str) -> loop_state::Features {
        loop_state::parse_roadmap(&self.app.loop_files.read_roadmap(work_dir)).0
    }

    /// `(200, detail)` of a blocked feature; 404 when the feature does not exist or is not blocked.
    pub fn get_detail(&self, context: &ProjectContext, slug: &str) -> Reply {
        match self.build_detail(context, slug) {
            Some(detail) => (200, detail),
            None => failure(404, NOT_QUARANTINED_ERROR),
        }
    }

    pub fn build_detail(&self, context: &ProjectContext, slug: &str) -> Option<Value> {
        let work_dir = context.work_dir();
        let features = self.read_roadmap_features(&work_dir);
        let fields = features.get(slug).filter(|f| f.get("status") == Some(BLOCKED_STATUS))?;
        let tries_text = fields.text("tries");
        let tries: i64 = if !tries_text.is_empty() && tries_text.chars().all(|c| c.is_ascii_digit()) { tries_text.parse().unwrap_or(0) } else { 0 };
        let attempts = self.list_attempts(context, slug);
        let last = attempts.last();
        let step: Option<u64> = match last {
            Some(last) => last["step"].as_u64(),
            None if fields.text("note").starts_with(GATE_NOTE_PREFIX) => Some(STEP_REVIEW as u64),
            None => None,
        };
        let mut detail = json!({
            "id": slug, "reason": self.build_reason(context, slug, fields, tries), "step": step,
            "step_agent": step.map(|s| super::pipeline::step_agent(s as usize)), "failures": tries,
            "turn": last.map(|l| l["turn"].clone()),
            "attempts": attempts.iter().map(|a| json!({"turn": a["turn"], "agent": a["agent"], "status": a["status"], "out": a["out"], "agent_id": a["agent_id"]})).collect::<Vec<_>>(),
            "failed_tests": self.read_failed_tests(&work_dir, &attempts), "uncommitted": self.list_uncommitted(&work_dir),
            "skip_impact": list_transitive_dependents(&features, slug), "options": QUARANTINE_OPTIONS,
        });
        let summary = self.read_summary(&work_dir, &detail);
        detail["summary"] = summary;
        Some(detail)
    }

    /// The pause reason of LOOP.json when it names this feature, else the ROADMAP note with the failure count.
    fn build_reason(&self, context: &ProjectContext, slug: &str, fields: &loop_state::Fields, tries: i64) -> String {
        let summary = self.app.loop_status().get_loop_summary(context);
        let reason = text_or_empty(summary.get("reason"));
        if summary["status"] == "awaiting_user" && reason.contains(slug) {
            return redact_credentials(&reason, Some(REASON_LIMIT)).trim().to_string();
        }
        let note = fields.text("note");
        clip_redacted_text(&if note.is_empty() { format!("{tries} failures") } else { format!("{note} ({tries} failures)") }, REASON_LIMIT)
    }

    /// Runs of this feature that failed or blocked it, oldest first (the last ones only).
    fn list_attempts(&self, context: &ProjectContext, slug: &str) -> Vec<Value> {
        let rows: Vec<Value> = self.app.pipeline().list_feature_events(context, slug).into_iter().filter(|e| e["status"] == "FAIL" || e["status"] == "BLOCKED").collect();
        rows[rows.len().saturating_sub(ATTEMPTS_LIMIT)..].to_vec()
    }

    /// Failing tests of the latest verifier run that shows any: its final return first, else the output of its tool calls.
    fn read_failed_tests(&self, work_dir: &str, attempts: &[Value]) -> Vec<Value> {
        for attempt in attempts.iter().rev() {
            let Some(found) = HISTORY_ID.match_start(attempt["agent_id"].as_str().unwrap_or("")) else { continue };
            if attempt["step"].as_u64() != Some(STEP_VERIFY as u64) {
                continue;
            }
            let Some(transcript) = self.read_agent_transcript(work_dir, found[1].parse().unwrap_or(0), &found[2]) else { continue };
            let mut tests = parse_failed_tests(&text_or_empty(transcript.get("final_return")));
            if tests.is_empty() {
                let output: Vec<String> = transcript["log"].as_array().into_iter().flatten().map(|item| text_or_empty(item.get("result"))).collect();
                tests = parse_failed_tests(&output.join("\n"));
            }
            if !tests.is_empty() {
                return tests;
            }
        }
        vec![]
    }

    fn read_agent_transcript(&self, work_dir: &str, turn_number: u64, agent_name: &str) -> Option<Value> {
        let record = self.app.loop_files.read_turn(work_dir, turn_number);
        let (_, subagents) = self.app.transcripts.locate_session_files(work_dir, record.get("session_id").and_then(Value::as_str).unwrap_or(""));
        let path = subagents.iter().find(|item| item.0 == agent_name).map(|item| item.1.clone())?;
        self.app.transcripts.read_transcript(&path).filter(|t| t.as_object().is_some_and(|o| !o.is_empty()))
    }

    /// Files `git status` shows as changed in the work folder, outside `.specs/` (those are the loop's own state, never thrown away).
    fn list_uncommitted(&self, work_dir: &str) -> Vec<String> {
        let entries = if std::path::Path::new(work_dir).is_dir() { self.app.git.list_status_entries(work_dir) } else { None };
        entries.unwrap_or_default().into_iter().map(|e| e.path).filter(|p| !p.starts_with(SPECS_FOLDER_PREFIX)).collect()
    }

    fn read_summary(&self, work_dir: &str, detail: &Value) -> Value {
        let cached = read_summary(work_dir, detail["id"].as_str().unwrap_or(""));
        let has_text = cached.get("text").is_some_and(|t| engine_state::notify_truthy(Some(t)));
        if has_text && cached.get("failures") == Some(&detail["failures"]) && cached.get("turn").unwrap_or(&Value::Null) == &detail["turn"] {
            return json!({"text": redact_credentials(&text_or_empty(cached.get("text")), Some(SUMMARY_LIMIT)), "source": SOURCE_AI, "at": cached.get("at")});
        }
        json!({"text": clip_redacted_text(&build_facts_sentence(detail), SUMMARY_LIMIT), "source": SOURCE_FACTS, "at": null})
    }

    // ------------------------------------------------ summary job (read-only claude, polled through chat/job/<job>)
    /// Answers at once: only the cheap checks run here; the detail (transcripts, `git status`), the argv and the prompt are built in the job thread.
    pub fn start_summary(&self, context: &ProjectContext, slug: &str) -> Reply {
        if self.build_detail_or_none(context, slug).is_none() {
            return failure(404, NOT_QUARANTINED_ERROR);
        }
        let thread = format!("{SUMMARY_JOB_THREAD_PREFIX}{slug}");
        let job_id = match self.app.chat_jobs.reserve_job(&context.project.id, Some(&thread), MAX_CONCURRENT_JOBS, None) {
            Ok(id) => id,
            Err(reason) => {
                return failure(409, if reason == THREAD_BUSY_REASON { "a summary of this feature is already being written".to_string() } else { busy_jobs_error() })
            }
        };
        self.app.log.info("quarantine_summary_started", &[("project", json!(context.project.id)), ("job", json!(job_id)), ("feature", json!(slug))]);
        let (app, job_context, job, feature) = (self.app.shared(), context.clone(), job_id.clone(), slug.to_string());
        if std::thread::Builder::new().name(format!("quarantine-{job_id}")).spawn(move || app.quarantine().run_summary_job(&job_context, &job, &feature)).is_err() {
            let result = json!({"state": JOB_STATE_ERROR, "reply": null, "cost_usd": null, "error": UNEXPECTED_FAILURE, "thread": null});
            self.app.chat_jobs.finish_job(&job_id, result.as_object().unwrap());
            return failure(500, "the summary thread cannot start");
        }
        (202, json!({"ok": true, "job": job_id}))
    }

    pub fn run_summary_job(&self, context: &ProjectContext, job_id: &str, slug: &str) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let error = |message: &str| json!({"state": JOB_STATE_ERROR, "reply": null, "cost_usd": null, "error": message, "thread": null});
            let Some(detail) = self.build_detail(context, slug) else { return error(NOT_QUARANTINED_ERROR) }; // unblocked between the request and the job
            let options = CommandOptions {
                model: CHAT_MODEL.into(),
                budget_usd: Some(CHAT_BUDGET_USD),
                allowed_tools: CHAT_ALLOWED_TOOLS.iter().map(|t| t.to_string()).collect(),
                disallowed_tools: CHAT_DISALLOWED_TOOLS.iter().map(|t| t.to_string()).collect(),
                no_mcp: true,
                ..CommandOptions::default()
            };
            let argv = self.app.claude.build_command(&options);
            let work_dir = context.work_dir();
            let stdin_text = format!("{READ_ONLY_PREFIX}\n\n{}\n\n{SUMMARY_INSTRUCTION}", build_context_text(&detail, &work_dir));
            let run_dir = if std::path::Path::new(&work_dir).is_dir() { work_dir.clone() } else { context.repo_path().to_string() };
            let returned = {
                let _kind = call_kind("quarantine");
                self.app.claude.run_prompt(&argv, &run_dir, &stdin_text, CHAT_TIMEOUT_SECONDS, &[])
            };
            if returned["ok"] != true {
                return error(returned.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()).unwrap_or(UNEXPECTED_FAILURE));
            }
            let text = redact_secrets(returned.get("reply").and_then(Value::as_str).unwrap_or(""), Some(REPLY_LIMIT)).trim().to_string();
            let cost = if is_number(returned.get("cost_usd")) { returned["cost_usd"].clone() } else { Value::Null };
            write_summary(
                &work_dir,
                detail["id"].as_str().unwrap_or(""),
                &json!({
                    "text": text.chars().take(SUMMARY_LIMIT).collect::<String>(), "at": now_iso(), "failures": detail["failures"], "turn": detail["turn"], "cost_usd": cost,
                }),
            );
            json!({"state": JOB_STATE_OK, "reply": text, "cost_usd": cost, "error": null, "thread": null})
        }));
        let result =
            outcome.unwrap_or_else(|_| json!({"state": JOB_STATE_ERROR, "reply": null, "cost_usd": null, "error": "failed to run claude: panic", "thread": null}));
        self.app.chat_jobs.finish_job(job_id, result.as_object().unwrap());
        self.app.log.info("quarantine_summary_finished", &[("project", json!(context.project.id)), ("job", json!(job_id)), ("state", result["state"].clone())]);
    }

    // ------------------------------------------------ decisions (through the answer/instruction mechanics of the loop)
    /// The wording of the option the loop listed (LOOP.json `options`) so the answer reads like the click on it; else the default wording.
    fn pick_option_text(&self, context: &ProjectContext, words: &[&str], default: &str) -> String {
        let summary = self.app.loop_status().get_loop_summary(context);
        let options: Vec<String> = match summary["json"].get("options") {
            Some(Value::Array(list)) => list.iter().map(text_or_none).collect(),
            _ => vec![],
        };
        options.into_iter().find(|option| words.iter().any(|w| option.to_lowercase().contains(w))).unwrap_or_else(|| default.to_string())
    }

    pub fn retry_feature(&self, context: &ProjectContext, slug: &str, body: &Value) -> Reply {
        let guidance = match body.get("guidance") {
            None | Some(Value::Null) => "",
            Some(Value::String(s)) => s.as_str(),
            Some(_) => return failure(400, "guidance must be text"),
        };
        if guidance.chars().count() > GUIDANCE_LIMIT {
            return failure(400, format!("guidance is longer than {GUIDANCE_LIMIT} characters"));
        }
        if self.build_detail_or_none(context, slug).is_none() {
            return failure(404, NOT_QUARANTINED_ERROR);
        }
        let mut text = self.pick_option_text(context, &RETRY_OPTION_WORDS, DEFAULT_RETRY_OPTION);
        if !guidance.trim().is_empty() {
            text.push_str(&format!(". {GUIDANCE_LABEL}{}", guidance.trim()));
        }
        self.app.loop_control().send_instruction_to_loop(context, &json!({"text": text, "feature": slug}))
    }

    pub fn skip_feature(&self, context: &ProjectContext, slug: &str) -> Reply {
        if self.build_detail_or_none(context, slug).is_none() {
            return failure(404, NOT_QUARANTINED_ERROR);
        }
        let text = self.pick_option_text(context, &SKIP_OPTION_WORDS, DEFAULT_SKIP_OPTION);
        self.app.loop_control().send_instruction_to_loop(context, &json!({"text": text, "feature": slug}))
    }

    /// The blocked feature's ROADMAP fields (cheap check for the commands, which do not need the whole detail); `None` = not quarantined.
    fn build_detail_or_none(&self, context: &ProjectContext, slug: &str) -> Option<loop_state::Fields> {
        self.read_roadmap_features(&context.work_dir()).get(slug).filter(|f| f.get("status") == Some(BLOCKED_STATUS)).cloned()
    }

    // ------------------------------------------------ revert
    pub fn revert_files(&self, context: &ProjectContext, slug: &str, body: &Value) -> Reply {
        let paths = match body.get("paths") {
            Some(Value::Array(list)) if !list.is_empty() && list.len() <= REVERT_PATHS_LIMIT && list.iter().all(|p| p.as_str().is_some_and(|s| !s.trim().is_empty())) => {
                list.iter().filter_map(Value::as_str).collect::<Vec<_>>()
            }
            _ => return failure(400, format!("paths: a list of 1 to {REVERT_PATHS_LIMIT} file paths from the uncommitted list")),
        };
        if self.build_detail_or_none(context, slug).is_none() {
            return failure(404, NOT_QUARANTINED_ERROR);
        }
        if self.app.loop_control().is_loop_alive(context) {
            return failure(409, "the loop is running: nothing can be reverted until it stops");
        }
        let work_dir = context.work_dir();
        let entries: Vec<_> = self.app.git.list_status_entries(&work_dir).unwrap_or_default().into_iter().filter(|e| !e.path.starts_with(SPECS_FOLDER_PREFIX)).collect();
        let wanted: Vec<String> = paths.iter().map(|p| p.trim().replace('\\', "/").trim_start_matches("./").to_string()).collect();
        let outside: Vec<&String> = wanted.iter().filter(|p| !entries.iter().any(|e| &e.path == *p)).collect();
        if !outside.is_empty() {
            return failure(
                400,
                format!("not in the uncommitted list: {}", outside.iter().take(5).map(|p| clip_redacted_text(p, OUT_LIMIT)).collect::<Vec<_>>().join(", ")),
            );
        }
        let (mut reverted, mut failed): (Vec<String>, Vec<Value>) = (vec![], vec![]);
        let mut done_paths: Vec<&String> = Vec::new();
        for path in &wanted {
            if done_paths.contains(&path) {
                continue;
            }
            done_paths.push(path);
            let entry = entries.iter().find(|e| &e.path == path).unwrap();
            let (done, error) = self.app.git.discard_path_changes(&work_dir, entry);
            if done {
                reverted.push(path.clone());
            } else {
                failed.push(json!({"path": path, "error": clip_redacted_text(&error, OUT_LIMIT)}));
            }
        }
        self.app.log.info(
            "quarantine_revert",
            &[("project", json!(context.project.id)), ("feature", json!(slug)), ("reverted", json!(reverted.len())), ("failed", json!(failed.len()))],
        );
        let mut out: Map<String, Value> = Map::new();
        out.insert("ok".into(), json!(failed.is_empty()));
        out.insert("reverted".into(), json!(reverted));
        out.insert("failed".into(), json!(failed));
        if failed.is_empty() {
            return (200, Value::Object(out));
        }
        out.insert("error".into(), json!("some files could not be reverted"));
        (500, Value::Object(out))
    }

    // ------------------------------------------------ chat scope
    /// Text for the `quarantine` chat scope; `None` when the feature is not in quarantine.
    pub fn build_chat_context(&self, context: &ProjectContext, slug: &str) -> Option<String> {
        self.build_detail(context, slug).map(|detail| build_context_text(&detail, &context.work_dir()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(roadmap: &str) -> (tempfile::TempDir, std::sync::Arc<App>, ProjectContext) {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        std::fs::write(repo.join(".specs/project/ROADMAP.md"), roadmap).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        (dir, app, context)
    }

    #[test]
    fn dependents_are_transitive_and_skip_verified() {
        let (features, _) = loop_state::parse_roadmap(
            "- [a] status=blocked\n- [b] status=pending deps=a\n- [c] status=pending deps=b,dir:x\n- [d] status=verified deps=a\n- [e] status=pending\n",
        );
        assert_eq!(list_transitive_dependents(&features, "a"), ["b", "c"]);
    }

    #[test]
    fn detail_summary_and_commands_for_a_quarantined_feature() {
        let (_dir, app, context) = setup("- [a] status=blocked tries=2 note=flaky\n- [b] status=pending deps=a\n");
        let service = app.quarantine();
        assert_eq!(service.get_detail(&context, "b"), failure(404, NOT_QUARANTINED_ERROR));
        let (status, detail) = service.get_detail(&context, "a");
        assert_eq!(status, 200);
        assert_eq!(
            (detail["failures"].clone(), detail["reason"].clone(), detail["skip_impact"].clone(), detail["step"].clone()),
            (json!(2), json!("flaky (2 failures)"), json!(["b"]), Value::Null)
        );
        assert_eq!(detail["summary"]["source"], "facts");
        assert_eq!(detail["summary"]["text"], "Feature a is in quarantine after 2 failed attempt(s). Skipping it also blocks: b.");
        write_summary(&context.work_dir(), "a", &json!({"text": "AI says", "at": "t", "failures": 2, "turn": null}));
        assert_eq!(service.get_detail(&context, "a").1["summary"], json!({"text": "AI says", "source": "ai", "at": "t"}));
        assert_eq!(service.retry_feature(&context, "a", &json!({"guidance": 5})), failure(400, "guidance must be text"));
        assert_eq!(service.retry_feature(&context, "zzz", &json!({})), failure(404, NOT_QUARANTINED_ERROR));
        assert_eq!(service.revert_files(&context, "a", &json!({"paths": []})).0, 400);
        assert!(service.build_chat_context(&context, "a").unwrap().starts_with("Quarantined feature: a.\nReason: flaky (2 failures)\nFailures: 2. Step: unknown."));
        assert!(service.build_chat_context(&context, "b").is_none());
    }
}
