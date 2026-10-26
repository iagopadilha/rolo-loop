// SPDX-License-Identifier: MIT
//! Project roadmap: structured read, approval (only by the user's click) and the headless `/orchestrator` jobs that generate and refine the roadmap
//! and map the codebase.
//!
//! Approval requires `confirm:true`, refuses with the loop running or with a planning gate not ok (422 `planning_gates`, see `planning_gates`) and
//! writes through the engine function (`loop_state::approve_roadmap`), the same signature `loop --check` verifies. The app never approves on its
//! own: a job only generates; the user's POST approves. In planning mode (never approved, never looped) the one-shot generate/refine jobs are refused
//! (`planning_required`): the planning chat replaces them.

use super::claude_jobs::{BeforeStart, ClaudeJobCommand, JOB_TYPE_ROADMAP, JOB_TYPE_ROADMAP_REFINE, ROADMAP_JOB_TYPES};
use super::map_job::{map_guard_ignored_paths, MapStart};
use super::planning_gates::{create_readiness_gate, GATE_READY};
use super::App;
use crate::domain::errors::{AppError, Kind, PLANNING_GATES_CODE};
use crate::domain::project::ProjectContext;
use crate::infrastructure::claude_client::CommandOptions;
use crate::infrastructure::workspace_file_guard::{WorkspaceFileGuard, WorkspaceSnapshot, DEFAULT_IGNORED_NAMES};
use crate::repositories::loop_command_file::now_iso;
use engine_state::roadmap_settings::parse_roadmap_e2e;
use engine_state::{loop_state, run_hints};
use pycompat::Re;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

pub const GOAL_FILE_NAME: &str = "roadmap-goal.md";
pub const GOAL_RELATIVE_PATH: &str = ".specs/project/loop/internal/roadmap-goal.md";
pub const GENERATE_ROADMAP_PROMPT: &str = "/orchestrator roadmap --headless --goal-file .specs/project/loop/internal/roadmap-goal.md";
pub const MAP_FIRST_FLAG: &str = "--map-first";
pub const REFINE_ROADMAP_PREFIX: &str = "Continue in this session's `/orchestrator roadmap --headless` mode: adjust .specs/project/ROADMAP.md and .specs/project/FLOWS.md (one flow per feature), and PROJECT.md if needed, following the request below, in the grammar of references/state.md. Do not ask (doubts go to the `## Open questions` section of the ROADMAP), never approve the roadmap, and finish.\n\nUser request:\n";
/// What the rules do not pre-approve is denied (nobody answers a prompt in -p).
pub const ROADMAP_PERMISSION_MODE: &str = "dontAsk";
/// Guard profile: Write/Edit only inside .specs/.
pub const ROADMAP_HOOK_PROFILE: &str = "planning";
pub const ROADMAP_EDITABLE_PATHS: [&str; 5] = [".specs/project/ROADMAP.md", ".specs/project/FLOWS.md", ".specs/project/PROJECT.md", ".specs/project/RULES.md", ".specs/project/TEST-ENVIRONMENT.md"];
/// Map-first variant: Phase 1 dispatches brownfield-mapper agents (Agent) and sizes them with `harness scale` (Bash), and writes docs/codebase/.
/// The guard profile still confines the writes: `planning` rules + `map-docs`, and must exist in the plugin's guard profiles.
pub const ROADMAP_MAP_FIRST_HOOK_PROFILE: &str = "roadmapmap";
pub const DEFAULT_JOB_MODEL: &str = ""; // empty = the account's default model, as in the loop
pub const DEFAULT_BUDGET_USD: f64 = 5.0;
pub const MAX_BUDGET_USD: f64 = 50.0;
pub const GOAL_TEXT_LIMIT: usize = 20000;
static STAGE_TITLE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^#{1,6}\s*Stage\s+(\S+)\s*(?:[—–:-]+\s*)?(.*)$"));
static SECTION_TITLE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^#{1,6}\s+(.*)$"));
const OPEN_QUESTIONS_TITLE: &str = "open questions";
const NO_QUESTION_ANSWERS: [&str; 3] = ["none", "none.", "-"];
static HTML_COMMENT_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?s)<!--.*?-->"));

pub fn roadmap_allowed_tools(map_first: bool) -> Vec<String> {
    let mut tools: Vec<String> = ["Read", "Grep", "Glob", "Skill"].iter().map(|s| s.to_string()).collect();
    tools.extend(ROADMAP_EDITABLE_PATHS.iter().map(|p| format!("Edit(/{p})")));
    if map_first {
        tools.extend(["Agent".to_string(), "Bash".to_string(), format!("Edit(/{}/**)", run_hints::MAP_FOLDER)]);
    }
    tools
}

pub fn roadmap_disallowed_tools(map_first: bool) -> Vec<String> {
    let names: &[&str] = if map_first { &["PowerShell", "NotebookEdit", "WebFetch", "WebSearch"] } else { &["Bash", "PowerShell", "NotebookEdit", "Agent", "WebFetch", "WebSearch"] };
    names.iter().map(|s| s.to_string()).collect()
}

fn editable_paths(map_first: bool) -> Vec<String> {
    let mut paths: Vec<String> = ROADMAP_EDITABLE_PATHS.iter().map(|s| s.to_string()).collect();
    if map_first {
        paths.push(run_hints::MAP_FOLDER.to_string());
    }
    paths
}

/// `[{"n","title","features":[slug]}]` in file order (the number comes from the engine; the title, from the rest of the line).
pub fn list_roadmap_stages(text: &str, features: &loop_state::Features) -> Vec<Value> {
    let mut titles: Vec<(String, String)> = Vec::new();
    for line in pycompat::splitlines(text) {
        if let Some(found) = STAGE_TITLE_PATTERN.match_start(line.trim()) {
            if !titles.iter().any(|(n, _)| *n == found[1]) {
                titles.push((found[1].to_string(), found[2].trim().to_string()));
            }
        }
    }
    let mut stages: Vec<(String, Vec<String>)> = Vec::new();
    for (slug, fields) in features.iter() {
        let number = fields.text("stage").to_string();
        match stages.iter_mut().find(|(n, _)| *n == number) {
            Some((_, slugs)) => slugs.push(slug.clone()),
            None => stages.push((number, vec![slug.clone()])),
        }
    }
    stages
        .into_iter()
        .map(|(number, slugs)| {
            let title = titles.iter().find(|(n, _)| *n == number).map(|(_, t)| t.clone()).unwrap_or_default();
            json!({"n": number, "title": title, "features": slugs})
        })
        .collect()
}

/// Items of the `## Open questions` section (the headless /orchestrator mode puts there what it cannot ask).
pub fn extract_open_questions(text: &str) -> Vec<String> {
    let mut questions = Vec::new();
    let mut inside = false;
    let cleaned = HTML_COMMENT_PATTERN.replace_lit(text, ""); // a (multi-line) comment is never a question
    for line in pycompat::splitlines(&cleaned) {
        if let Some(title) = SECTION_TITLE_PATTERN.match_start(line.trim()) {
            inside = title[1].trim().to_lowercase().starts_with(OPEN_QUESTIONS_TITLE);
            continue;
        }
        let item = line.trim().trim_start_matches(['-', '*']).trim();
        if inside && !item.is_empty() && !NO_QUESTION_ANSWERS.contains(&item.to_lowercase().as_str()) {
            questions.push(item.to_string());
        }
    }
    questions
}

pub struct RoadmapService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn roadmap(&self) -> RoadmapService<'_> {
        RoadmapService { app: self }
    }
}

type SnapshotSlot = Arc<Mutex<Option<WorkspaceSnapshot>>>;

impl RoadmapService<'_> {
    // ------------------------------------------------ read
    pub fn get_roadmap(&self, context: &ProjectContext) -> Value {
        let text = self.app.specs.read_text(&Path::new(&context.specs_dir()).join("project").join("ROADMAP.md")).unwrap_or_default();
        let (features, parse_errors) = loop_state::parse_roadmap(&text);
        let mut errors = parse_errors;
        errors.extend(loop_state::check_fields(&features, None));
        let approval = loop_state::roadmap_approval(&context.work_dir());
        json!({
            "text": text, "approved": approval["approved"], "not_approved_reason": approval["reason"], "approved_at": approval["at"],
            "stages": list_roadmap_stages(&text, &features), "features": super::project_progress::features_with_slug(&features), "errors": errors,
            "open_questions": extract_open_questions(&text), "e2e": parse_roadmap_e2e(&text).unwrap_or(false),
        })
    }

    // ------------------------------------------------ approval
    pub fn approve_roadmap(&self, context: &ProjectContext, confirm: &Value) -> Result<Value, AppError> {
        if confirm != &Value::Bool(true) {
            return Err(AppError::invalid_request("confirm must be true"));
        }
        if self.app.progress().is_loop_running(context) {
            return Err(AppError::loop_running("loop running: the roadmap was already approved"));
        }
        self.ensure_planning_gates_ok(context)?;
        self.ensure_ready_to_run(context)?;
        self.write_capabilities(context);
        let approval = loop_state::approve_roadmap(&context.work_dir(), &now_iso()).map_err(|failure| AppError::new(Kind::InvalidRoadmap, failure))?;
        self.app.planning_requirement().clear_reopened(context);
        self.app.log.info("roadmap_approved", &[("project", json!(context.project.id)), ("features", approval["features"].clone())]);
        Ok(approval)
    }

    fn ensure_planning_gates_ok(&self, context: &ProjectContext) -> Result<(), AppError> {
        let gates = self.app.progress().build_planning_gates(context);
        let failing: Vec<&Value> = gates.iter().filter(|g| g["ok"] != true && g["optional"] != true).collect();
        if failing.is_empty() {
            return Ok(());
        }
        self.app.log.info("roadmap_approval_refused", &[("project", json!(context.project.id)), ("gates", json!(failing.iter().map(|g| g["id"].clone()).collect::<Vec<_>>()))]);
        let parts: Vec<String> = failing
            .iter()
            .map(|g| {
                let label = g["label"].as_str().unwrap_or("");
                match g["detail"].as_str().filter(|d| !d.is_empty()) {
                    Some(detail) => format!("{label} ({detail})"),
                    None => label.to_string(),
                }
            })
            .collect();
        Err(AppError::planning_gates(format!("the roadmap cannot be approved yet: {}", parts.join("; ")), json!(gates), PLANNING_GATES_CODE))
    }

    /// Server side re-check of the `ready` gate (the last moment the user is present): refuses with the first problem.
    fn ensure_ready_to_run(&self, context: &ProjectContext) -> Result<(), AppError> {
        let readiness = self.app.readiness();
        if !readiness.enforced() {
            return Ok(());
        }
        let gate = create_readiness_gate(&readiness.get(context));
        if gate["ok"] == true {
            return Ok(());
        }
        self.app.log.info("roadmap_approval_refused", &[("project", json!(context.project.id)), ("gates", json!([GATE_READY]))]);
        let mut gates: Vec<Value> = self.app.progress().build_planning_gates(context).into_iter().filter(|g| g["id"] != GATE_READY).collect();
        gates.push(gate.clone());
        Err(AppError::planning_gates(format!("the roadmap cannot be approved yet: {} ({})", gate["label"].as_str().unwrap_or(""), gate["detail"].as_str().unwrap_or("")), json!(gates), PLANNING_GATES_CODE))
    }

    fn write_capabilities(&self, context: &ProjectContext) {
        // PROJECT.md is a guide for the agents: never block the approval for it
        self.app.capabilities().write(context);
    }

    // ------------------------------------------------ jobs
    pub fn generate_roadmap(&self, context: &ProjectContext, goal: &Value, map_first: bool, budget_usd: Option<&Value>) -> Result<Value, AppError> {
        self.app.planning_requirement().ensure_planning_done(context)?;
        let goal_text = Self::validate_text(goal, "goal")?;
        let budget = Self::validate_budget(budget_usd)?;
        self.ensure_repo_free_for_job(context)?;
        self.write_goal(context.repo_path(), &goal_text, map_first);
        let prompt = format!("{GENERATE_ROADMAP_PROMPT}{}", if map_first { format!(" {MAP_FIRST_FLAG}") } else { String::new() });
        let argv = self.build_confined_argv(budget, &prompt, "", map_first);
        let command = self.create_confined_command(context.repo_path(), argv, "", "", map_first);
        self.app.jobs().start_job(context.repo_path(), JOB_TYPE_ROADMAP, command)
    }

    pub fn refine_roadmap(&self, context: &ProjectContext, text: &Value, budget_usd: Option<&Value>) -> Result<Value, AppError> {
        self.app.planning_requirement().ensure_planning_done(context)?;
        let request = Self::validate_text(text, "text")?;
        let budget = Self::validate_budget(budget_usd)?;
        self.ensure_repo_free_for_job(context)?;
        let session = self.app.jobs().get_last_session(context.repo_path(), &ROADMAP_JOB_TYPES);
        if session.is_empty() {
            return Err(AppError::new(Kind::RoadmapSessionMissing, "no roadmap generated by the app in this project to refine: generate the roadmap first"));
        }
        let argv = self.build_confined_argv(budget, "", &session, false);
        let command = self.create_confined_command(context.repo_path(), argv, &format!("{REFINE_ROADMAP_PREFIX}{request}"), &session, false);
        self.app.jobs().start_job(context.repo_path(), JOB_TYPE_ROADMAP_REFINE, command)
    }

    /// Same confinement as the planning chat: hook profile `planning` (Write/Edit only inside .specs/), dontAsk, only the roadmap documents editable,
    /// no Bash/Agent/web, no MCP (the roadmap job reads files only).
    fn build_confined_argv(&self, budget: f64, prompt_argument: &str, resume_session: &str, map_first: bool) -> Vec<String> {
        let options = CommandOptions {
            model: DEFAULT_JOB_MODEL.into(),
            budget_usd: Some(budget),
            allowed_tools: roadmap_allowed_tools(map_first),
            disallowed_tools: roadmap_disallowed_tools(map_first),
            no_mcp: true,
            prompt_argument: prompt_argument.into(),
            resume_session: resume_session.into(),
            permission_mode: ROADMAP_PERMISSION_MODE.into(),
            hook_profile: if map_first { ROADMAP_MAP_FIRST_HOOK_PROFILE } else { ROADMAP_HOOK_PROFILE }.into(),
            ..Default::default()
        };
        self.app.claude.build_command(&options)
    }

    /// The snapshot is taken in the job thread (before claude) and everything outside the roadmap documents is undone afterwards.
    fn create_confined_command(&self, repo_path: &str, argv: Vec<String>, stdin_text: &str, resumed_session: &str, map_first: bool) -> ClaudeJobCommand {
        let holder: SnapshotSlot = Arc::new(Mutex::new(None));
        let editable = editable_paths(map_first);
        let (slot, snapshot_repo) = (holder.clone(), repo_path.to_string());
        let before_start: BeforeStart = Box::new(move |_update| {
            let ignored = map_guard_ignored_paths();
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(WorkspaceFileGuard::default().take_snapshot(&snapshot_repo, &ignored, &DEFAULT_IGNORED_NAMES));
            Ok(None)
        });
        let (app, finish_repo) = (self.app.shared(), repo_path.to_string());
        let mut command = ClaudeJobCommand::new(argv, stdin_text);
        command.resumed_session = resumed_session.to_string();
        command.before_start = Some(before_start);
        command.finalize = Some(Box::new(move |result| app.roadmap().revert_changes(&holder, &editable, &finish_repo, map_first, result)));
        command
    }

    fn revert_changes(&self, holder: &SnapshotSlot, editable: &[String], repo_path: &str, map_first: bool, result: Value) -> Value {
        let snapshot = holder.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(snapshot) = snapshot else { return result };
        let allowed: Vec<&str> = editable.iter().map(String::as_str).collect();
        let report = WorkspaceFileGuard::default().revert_unexpected_changes(&snapshot, &allowed);
        let list = |key: &str| -> Vec<Value> { report[key].as_array().cloned().unwrap_or_default() };
        if !list("reverted").is_empty() || !list("removed").is_empty() || !list("unrestorable").is_empty() {
            self.app.log.warn("roadmap_guard_reverted", &[("reverted", json!(list("reverted").iter().take(50).collect::<Vec<_>>())), ("removed", json!(list("removed").iter().take(50).collect::<Vec<_>>()))]);
        }
        let map_dir = run_hints::MAP_FOLDER.split('/').fold(std::path::PathBuf::from(repo_path), |a, p| a.join(p));
        if map_first && engine_state::notify_truthy(result.get("ok")) && !run_hints::list_map_documents(&map_dir).is_empty() {
            // the app (not the AI) writes the map index and the CLAUDE.md block, as after a map job
            run_hints::write_map_index(repo_path, self.app.map_jobs().read_short_head(repo_path).as_deref(), None);
            run_hints::write_codebase_block(repo_path);
        }
        result
    }

    /// Only validates and registers the job: the safety snapshot and claude run in the job thread (the map runner). The setup map has no spend cap:
    /// `budget_usd` is accepted and ignored (never a 400), the argv has no `--max-budget-usd`.
    pub fn map_codebase(&self, context: &ProjectContext) -> Result<Value, AppError> {
        self.app.log.info("map_requested", &[("project", json!(context.project.id))]);
        if let Err(refusal) = self.ensure_repo_free_for_job(context) {
            self.app.log.info("map_refused", &[("project", json!(context.project.id)), ("reason", json!(format!("{:?}", refusal.kind))), ("message", json!(refusal.message.chars().take(200).collect::<String>()))]);
            return Err(refusal);
        }
        self.app.map_jobs().start(MapStart::new(context.repo_path(), &context.project.id))
    }

    // ------------------------------------------------ support
    /// The job runs in the REPO. With the loop running, or with the loop worktree already created (the state lives there), it is refused.
    pub fn ensure_repo_free_for_job(&self, context: &ProjectContext) -> Result<(), AppError> {
        if self.app.progress().is_loop_running(context) {
            return Err(AppError::loop_running("loop running: generate or refine the roadmap with the loop stopped"));
        }
        if pycompat::nt_normcase(&context.work_dir()) != pycompat::nt_normcase(context.repo_path()) {
            return Err(AppError::new(Kind::LoopWorktreeExists, format!("the loop worktree already exists ({}): the loop roadmap lives there; edit it in Files or remove the worktree before generating another one", context.work_dir())));
        }
        if self.app.jobs().get_running_job(context.repo_path()).is_some() {
            return Err(AppError::new(Kind::JobInProgress, "a claude job is already running in this project: wait for it to finish"));
        }
        Ok(())
    }

    fn write_goal(&self, repo_path: &str, goal_text: &str, map_first: bool) {
        let path = loop_state::internal_dir(repo_path).join(GOAL_FILE_NAME);
        let content = format!("# Roadmap goal\n\n{}\n\n---\nMap first: {}\nWritten by the harness app on {}\n", goal_text.trim(), if map_first { "yes" } else { "no" }, now_iso());
        let _ = self.app.specs.write_text_atomic(&path, &content);
    }

    pub fn validate_text(text: &Value, field_name: &str) -> Result<String, AppError> {
        let Some(text) = text.as_str().filter(|t| !t.trim().is_empty()) else { return Err(AppError::invalid_request(format!("empty {field_name}"))) };
        if text.chars().count() > GOAL_TEXT_LIMIT {
            return Err(AppError::invalid_request(format!("{field_name} longer than {GOAL_TEXT_LIMIT} characters")));
        }
        Ok(text.to_string())
    }

    pub fn validate_budget(budget_usd: Option<&Value>) -> Result<f64, AppError> {
        match budget_usd {
            None | Some(Value::Null) => Ok(DEFAULT_BUDGET_USD),
            Some(Value::String(s)) if s.is_empty() => Ok(DEFAULT_BUDGET_USD),
            Some(value) => match value.as_f64().filter(|b| value.is_number() && *b > 0.0 && *b <= MAX_BUDGET_USD) {
                Some(budget) => Ok(budget),
                None => Err(AppError::invalid_request(format!("budget_usd must be a number between 0 and {}", MAX_BUDGET_USD as i64))),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_open_questions_and_validation() {
        let text = "## Stage 1 - Core\n- [a] status=pending title=\"A\" scope=Small stage=1 paths=src\n## Stage 2 - API ##\n- [b] status=pending title=\"B\" scope=Small stage=2 paths=api\n\n## Open questions\n- Which db? | recommended: sqlite\n- none\n<!--\n- hidden\n-->\n- Auth?\n\n## Other\n- not a question\n";
        let (features, _) = loop_state::parse_roadmap(text);
        let stages = list_roadmap_stages(text, &features);
        assert_eq!(stages[0], json!({"n": "1", "title": "Core", "features": ["a"]}));
        assert_eq!(stages[1]["features"], json!(["b"]));
        assert_eq!(extract_open_questions(text), vec!["Which db? | recommended: sqlite", "Auth?"]);
        assert_eq!(RoadmapService::validate_budget(None).unwrap(), 5.0);
        assert_eq!(RoadmapService::validate_budget(Some(&json!(7))).unwrap(), 7.0);
        for bad in [json!(0), json!(51), json!(true), json!("x"), json!(-1)] {
            assert_eq!(RoadmapService::validate_budget(Some(&bad)).unwrap_err().message, "budget_usd must be a number between 0 and 50");
        }
        assert_eq!(RoadmapService::validate_text(&json!("  "), "goal").unwrap_err().message, "empty goal");
        assert!(RoadmapService::validate_text(&json!("x".repeat(20001)), "goal").unwrap_err().message.contains("longer than 20000"));
        assert!(roadmap_allowed_tools(true).contains(&"Edit(/docs/codebase/**)".to_string()) && !roadmap_allowed_tools(false).contains(&"Bash".to_string()));
        assert_eq!(roadmap_disallowed_tools(false).len(), 6);
    }
}
