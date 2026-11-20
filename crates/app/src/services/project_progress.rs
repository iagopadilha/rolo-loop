// SPDX-License-Identifier: MIT
//! Checklist "where this project stands and what the next action is" (`GET /p/<id>/api/progress`).
//!
//! Reads everything from disk on each call (cheap: small files and one `git rev-parse`). The checks the engine makes in the preflight are NOT
//! rewritten: approval = `loop_state::roadmap_approval` (the same as `loop --check`), test environment = `test_environment_problems`, grammar =
//! `loop_state::parse_roadmap` + `loop_state::check_fields`. The planning gates (what "Approve roadmap" requires) are built from the same facts
//! (`planning_gates::build_planning_gates`).

use super::planning_gates::{
    are_gates_ok, build_planning_gates, build_subproject_conformance_gate, build_subproject_map_gate, build_subprojects_gates, check_sent_imports_coverage,
    describe_map_documents, read_subproject_state, GATE_CONFORMANCE, GATE_FLOWS, GATE_GIT, GATE_MAP, GATE_REPOS, GATE_STRUCTURE, GATE_SUBPROJECTS,
};
use super::roadmap::extract_open_questions;
use super::App;
use crate::domain::errors::AppError;
use crate::domain::flows_document::check_flow_coverage;
use crate::domain::project::ProjectContext;
use crate::repositories::specs_file::list_test_environment_problems;
use engine_state::roadmap_settings::parse_roadmap_e2e;
use engine_state::{loop_state, run_hints, subprojects};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

pub const MODE_NEW: &str = "new";
pub const MODE_NO_SPECS: &str = "no_specs";
pub const MODE_PLANNING: &str = "planning";
pub const MODE_READY_FOR_LOOP: &str = "ready_for_loop";
pub const MODE_IN_LOOP: &str = "in_loop";
pub const MODE_DONE: &str = "done";
pub const ACTION_STRUCTURE: &str = "structure";
pub const ACTION_SUBPROJECTS: &str = "subprojects";
pub const ACTION_CONFORMANCE: &str = "conformance";
pub const ACTION_MAP: &str = "map";
pub const ACTION_ROADMAP: &str = "roadmap";
pub const ACTION_FLOWS: &str = "flows";
pub const ACTION_APPROVE: &str = "approve";
pub const ACTION_TEST_ENVIRONMENT: &str = "test_environment";
pub const ACTION_CHECK: &str = "check";
pub const ACTION_START_LOOP: &str = "start_loop";
pub const ACTION_FOLLOW: &str = "follow";
pub const ACTION_REPOS: &str = "repos";
const LOOP_STATUS_RUNNING: &str = "running";
const LOOP_STATUS_STOPPED: &str = "stopped";
const LOOP_STATUS_ERROR: &str = "error";
const LOOP_STATUS_NO_LOOP: &str = "no_loop";
const LOOP_STATUS_CONTINUE: &str = "continue";
const LOOP_STATUS_DONE: &str = "done";
const LOOP_STATUSES_IN_PROGRESS: [&str; 4] = [LOOP_STATUS_CONTINUE, "awaiting_user", LOOP_STATUS_STOPPED, LOOP_STATUS_ERROR];
const CLOSED_FEATURE_STATUSES: [&str; 2] = ["verified", "blocked"];
const MAP_JOB_TYPE: &str = "map";
const ROADMAP_JOB_TYPE_PREFIX: &str = "roadmap";
const MISSING_ITEMS_SHOWN: usize = 6;
/// How long the last gates may answer a chat send's prerequisites refusal without a new check.
pub const PLANNING_GATES_CACHE_SECONDS: u64 = 10;
const NAMES_THAT_ARE_NOT_CODE: [&str; 11] =
    [".git", ".specs", ".claude", ".vscode", ".idea", "claude.md", "readme.md", ".gitignore", ".gitattributes", "license", "license.md"];
static REGRESSION_REQUIRED: LazyLock<Re> = LazyLock::new(|| Re::new(r"\bregression=(yes|baseline)\b"));

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

fn t(value: Option<&Value>) -> bool {
    engine_state::notify_truthy(value)
}

fn clip(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// `{slug: {field: text}}` in ROADMAP order (the dict the Python facts carry).
pub fn features_to_value(features: &loop_state::Features) -> Value {
    Value::Object(features.iter().map(|(slug, fields)| (slug.clone(), Value::Object(fields.0.iter().map(|(k, v)| (k.clone(), json!(v))).collect()))).collect())
}

/// `[dict(fields, slug=slug)]` for the roadmap payload.
pub fn features_with_slug(features: &loop_state::Features) -> Value {
    Value::Array(
        features
            .iter()
            .map(|(slug, fields)| {
                let mut row: Map<String, Value> = fields.0.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
                row.insert("slug".into(), json!(slug));
                Value::Object(row)
            })
            .collect(),
    )
}

pub fn create_step(step_id: &str, title: &str, ok: bool, detail: String, optional: bool) -> Value {
    json!({"id": step_id, "title": title, "ok": ok, "optional": optional, "detail": detail})
}

pub struct ProjectProgressService<'a> {
    pub app: &'a App,
}

/// A summary younger than this is served without computing it again.
pub const SUMMARY_FRESH: Duration = Duration::from_secs(5);
/// The hub list never waits longer than this for the summaries (a project in a huge folder can take seconds).
pub const SUMMARY_WAIT: Duration = Duration::from_millis(1500);

impl App {
    /// Progress summaries for the hub list, by project id: each stale one is computed on its own thread; the list waits at most
    /// [`SUMMARY_WAIT`]. A slower project gives its last known summary, or `None` (the front shows it as still loading); its result is
    /// kept for the next poll. One project computing never starts a second computation of itself.
    pub fn progress_summaries_within_budget(&self, ids: &[String]) -> std::collections::HashMap<String, Option<Value>> {
        use std::collections::HashMap;
        let mut out: HashMap<String, Option<Value>> = HashMap::new();
        let (sender, receiver) = std::sync::mpsc::channel::<(String, Value)>();
        let mut waiting = 0usize;
        for id in ids {
            let cached = self.progress_summaries.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned();
            if let Some((summary, at)) = &cached {
                if at.elapsed() < SUMMARY_FRESH {
                    out.insert(id.clone(), Some(summary.clone()));
                    continue;
                }
            }
            if self.progress_refreshing.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone()) {
                waiting += 1;
                let (app, id, sender) = (self.shared(), id.clone(), sender.clone());
                let _ = std::thread::Builder::new().name("progress-summary".into()).spawn(move || {
                    let summary = match app.project_service().get_project_context(&id) {
                        Ok(context) => app.progress().build_progress_summary(&context),
                        Err(_) => Value::Null, // one unreadable project does not take the list down
                    };
                    app.progress_summaries.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), (summary.clone(), Instant::now()));
                    app.progress_refreshing.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                    let _ = sender.send((id, summary));
                });
            }
            out.insert(id.clone(), cached.map(|(summary, _)| summary));
        }
        drop(sender);
        let deadline = Instant::now() + SUMMARY_WAIT;
        while waiting > 0 {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else { break };
            match receiver.recv_timeout(left) {
                Ok((id, summary)) => {
                    waiting -= 1;
                    out.insert(id, Some(summary));
                }
                Err(_) => break,
            }
        }
        out
    }

    pub fn progress(&self) -> ProjectProgressService<'_> {
        ProjectProgressService { app: self }
    }
}

impl ProjectProgressService<'_> {
    // ------------------------------------------------ loop status (same derivation as the panel's loop status)
    pub fn get_loop_status(&self, work_dir: &str) -> Value {
        let loop_json = self.app.specs.read_json(&loop_state::state_file(work_dir, "LOOP.json"));
        let meta = self.app.specs.read_json(&loop_state::loop_path(work_dir, "meta.json"));
        let pid = self.app.specs.read_loop_lock_pid(work_dir);
        let running = self.app.processes.is_alive(pid);
        let mut status: Option<String> = loop_json.get("status").and_then(Value::as_str).filter(|s| loop_state::LOOP_STATUS.contains(s)).map(String::from);
        let mut reason = loop_json.get("reason").cloned().unwrap_or(Value::Null);
        if running {
            status = Some(LOOP_STATUS_RUNNING.into());
        } else if status.as_deref() == Some(LOOP_STATUS_CONTINUE) {
            status = Some(LOOP_STATUS_STOPPED.into()); // the script died in the middle of the loop
            if meta.get("stopped").and_then(Value::as_str) == Some(LOOP_STATUS_ERROR) && text_of(meta.get("finished_at")) >= text_of(loop_json.get("at")) {
                status = Some(LOOP_STATUS_ERROR.into());
                reason = meta.get("reason").cloned().unwrap_or(Value::Null);
            }
        } else if status.is_none() {
            status = Some(LOOP_STATUS_NO_LOOP.into());
        }
        json!({"status": status, "running": running, "pid": if running { json!(pid) } else { Value::Null }, "reason": reason})
    }

    pub fn is_loop_running(&self, context: &ProjectContext) -> bool {
        self.app.processes.is_alive(self.app.specs.read_loop_lock_pid(&context.work_dir()))
    }

    // ------------------------------------------------ planning gates
    /// The planning chat edits TEST-ENVIRONMENT.md (and can only Edit, not create). Structure init already creates it; this is the idempotent
    /// fallback for projects initialized before that.
    pub fn ensure_test_environment_template(&self, context: &ProjectContext) -> bool {
        self.app.specs_structure().create_test_environment_template(&context.work_dir())
    }

    /// The planning checklist (gate ids in the contract order; see `planning_gates`).
    pub fn build_planning_gates(&self, context: &ProjectContext) -> Vec<Value> {
        let facts = self.read_project_facts(context);
        self.record_import_coverage(context, &facts);
        let gates = build_planning_gates(self.app, &facts);
        self.app.gates_cache.lock().unwrap_or_else(|e| e.into_inner()).insert(context.project.id.clone(), (Instant::now(), json!(gates)));
        gates
    }

    /// The last gates computed for this project when they are at most `max_age_seconds` old, else `None` (in memory only).
    pub fn read_cached_planning_gates(&self, context: &ProjectContext, max_age_seconds: u64) -> Option<Vec<Value>> {
        let cache = self.app.gates_cache.lock().unwrap_or_else(|e| e.into_inner());
        let (at, gates) = cache.get(&context.project.id)?;
        if at.elapsed() > Duration::from_secs(max_age_seconds) {
            return None;
        }
        gates.as_array().cloned()
    }

    /// Writes the coverage line of the newest import's IMPORT-LOG.md (when it was sent and IMPORT-COVERAGE.md exists).
    fn record_import_coverage(&self, context: &ProjectContext, facts: &Value) {
        let imports = facts.get("imports").and_then(Value::as_array).cloned().unwrap_or_default();
        let sent = imports.first().is_some_and(|i| t(i.get("sent_at")));
        if !sent || text_of(facts.get("import_coverage_text")).trim().is_empty() {
            return;
        }
        if let Some(first) = check_sent_imports_coverage(facts).first() {
            self.app.planning_imports().record_coverage(context, first);
        }
    }

    // ------------------------------------------------ checklist
    pub fn build_progress_summary(&self, context: &ProjectContext) -> Value {
        let progress = self.build_progress(context);
        json!({"mode": progress["mode"], "next_action": progress["next_action"], "loop_status": progress["loop_status"], "counts": progress["counts"]})
    }

    pub fn build_progress(&self, context: &ProjectContext) -> Value {
        let facts = self.read_project_facts(context);
        let steps = self.build_steps(&facts);
        let (mode, next_action) = self.decide_mode_and_next_action(&facts);
        let mut progress = json!({
            "mode": mode, "next_action": next_action, "steps": steps, "loop_status": facts["loop"]["status"], "counts": facts["counts"], "work_dir": context.work_dir(),
            "loop_worktree": context.work_dir() != context.repo_path(), "running_job": facts["running_job"],
        });
        if let Some(extra) = Self::describe_subprojects_progress(&facts) {
            for (key, value) in extra {
                progress[key] = value;
            }
        }
        progress
    }

    /// Additive payload for a confirmed monorepo: `monorepo: true` (the front end switches its steps on it) and the `subprojects` summary. Nothing
    /// for a single project or a list still waiting for the user (the key is absent, not false).
    fn describe_subprojects_progress(facts: &Value) -> Option<Map<String, Value>> {
        let (_, confirmed, active, _) = read_subproject_state(facts);
        if !confirmed {
            return None;
        }
        let rows: Vec<Value> = active
            .iter()
            .map(|project| {
                let stack = Some(text_of(project.get("stack"))).filter(|s| !s.is_empty()).unwrap_or_else(|| "unknown".into());
                let map_files = project.get("map").and_then(|m| m.get("files")).and_then(Value::as_array).map(Vec::len).unwrap_or(0);
                json!({"path": project["path"], "stack": stack, "conformance": project.get("conformance").cloned().unwrap_or(Value::Null), "has_code": t(project.get("has_code")), "map_files": map_files})
            })
            .collect();
        Some(Map::from_iter([("monorepo".to_string(), json!(true)), ("subprojects".to_string(), json!(rows))]))
    }

    /// Reads (without deciding anything) everything the checklist needs: one method per fact group.
    pub fn read_project_facts(&self, context: &ProjectContext) -> Value {
        let (repo, work) = (context.repo_path().to_string(), context.work_dir());
        let folder_exists = Path::new(&repo).is_dir();
        let project_dir = Path::new(&work).join(".specs").join("project");
        let roadmap_text = self.app.specs.read_text(&project_dir.join("ROADMAP.md"));
        let (features, parse_errors) = loop_state::parse_roadmap(roadmap_text.as_deref().unwrap_or(""));
        let mut facts = Map::new();
        for part in [
            self.read_repository_facts(context, folder_exists),
            Self::read_map_facts(&work, &repo),
            self.read_roadmap_facts(&work, &project_dir, roadmap_text.as_deref(), &features, &parse_errors, Some(&repo)),
            Self::read_environment_facts(&work, roadmap_text.as_deref(), folder_exists),
            self.read_loop_facts(context, folder_exists, &features),
            self.read_subproject_facts(context),
        ] {
            if let Value::Object(map) = part {
                facts.extend(map);
            }
        }
        let mut facts = Value::Object(facts);
        if read_subproject_state(&facts).1 {
            // D21: `docs/codebase` at the root of a confirmed monorepo holds SYSTEM.md (and maybe a leftover map): it is not "the map"
            facts["map_files"] = json!([]);
            facts["map_folder"] = json!("docs/codebase");
            facts["map_commit"] = Value::Null;
        }
        facts
    }

    /// `facts["subprojects"]` = the description of `describe_subprojects` (monorepo, confirmed, projects, error). A failure to read it never takes the
    /// checklist down: the facts carry the error and the repository counts as a single project.
    fn read_subproject_facts(&self, context: &ProjectContext) -> Value {
        match self.describe_subprojects(context) {
            Ok(Some(described)) => json!({"subprojects": described}),
            Ok(None) => json!({}),
            Err(failure) => {
                self.app.log.warn("subprojects_unreadable", &[("type", json!(format!("{:?}", failure.kind))), ("message", json!(clip(&failure.message, 200)))]);
                json!({"subprojects": {"monorepo": false, "confirmed": false, "projects": [], "error": format!("{:?}: {}", failure.kind, clip(&failure.message, 200))}})
            }
        }
    }

    /// `facts["subprojects"]`: detects and stores the list on the first read. Before the `.specs/` structure exists nothing is written: the detection
    /// is shown unconfirmed and stored once the structure is initialized.
    pub fn describe_subprojects(&self, context: &ProjectContext) -> Result<Option<Value>, AppError> {
        if !Path::new(context.repo_path()).is_dir() {
            return Ok(None);
        }
        let service = self.app.subprojects();
        if Path::new(&context.work_dir()).join(".specs").is_dir() {
            let mut description = service.describe(context, None)?;
            if t(description.get("monorepo")) && t(description.get("confirmed")) {
                match self.app.system_maps().status(context, None) {
                    Ok(system) => description["system"] = system, // the map gate waits for the system map when it is due
                    Err(failure) => {
                        self.app
                            .log
                            .warn("system_map_status_unreadable", &[("type", json!(format!("{:?}", failure.kind))), ("message", json!(clip(&failure.message, 200)))]);
                    }
                }
            }
            return Ok(Some(description));
        }
        let (key, folder) = service.resolve_target(context, None)?;
        let detected = subprojects::detect(&folder);
        let mut entry = Map::new();
        entry.insert("monorepo".into(), json!(t(detected.get("monorepo"))));
        entry.insert("confirmed".into(), json!(false));
        entry.insert("validated_by".into(), json!("script"));
        entry.insert("reason".into(), json!(text_of(detected.get("reason"))));
        entry.insert("projects".into(), detected.get("projects").cloned().unwrap_or(json!([])));
        entry.insert("notes".into(), detected.get("warnings").cloned().filter(|w| t(Some(w))).unwrap_or(json!([])));
        Ok(Some(service.build_description(&key, &folder, Some(&entry), "")))
    }

    /// Folder, git, code, `.specs/` structure, Conformance and the providers of the other repos / readiness.
    fn read_repository_facts(&self, context: &ProjectContext, folder_exists: bool) -> Value {
        let (repo, work) = (context.repo_path(), context.work_dir());
        let structure = self.app.specs_structure();
        json!({
            "work_dir": work,
            "folder_exists": folder_exists,
            "is_git": folder_exists && self.app.git.is_git_repo(repo),
            "has_specs": Path::new(&work).join(".specs").is_dir(),
            "has_code": folder_exists && Self::repo_has_code(repo),
            "structure": structure.check_structure(&work),
            "conformance": structure.get_conformance(&work),
            "repos_unready": self.read_unready_repos(context),
            "repos_reference_warnings": self.read_reference_warnings(context),
            "readiness": self.read_readiness(context),
            "loop_running": self.is_loop_running(context),
        })
    }

    /// ROADMAP, PROJECT.md, FLOWS.md and the approval.
    fn read_roadmap_facts(
        &self,
        work: &str,
        project_dir: &Path,
        roadmap_text: Option<&str>,
        features: &loop_state::Features,
        parse_errors: &[String],
        repo: Option<&str>,
    ) -> Value {
        let flows_text = self.app.specs.read_text(&project_dir.join("FLOWS.md"));
        let field_errors = loop_state::check_fields(features, None);
        let slugs = features.slugs();
        let mut roadmap_errors = parse_errors.to_vec();
        roadmap_errors.extend(field_errors.clone());
        json!({
            "roadmap_exists": roadmap_text.is_some(), "features": features_to_value(features), "roadmap_errors": roadmap_errors,
            "roadmap_parse_errors": parse_errors, "roadmap_field_errors": field_errors,
            "open_questions": extract_open_questions(roadmap_text.unwrap_or("")),
            "project_text": self.app.specs.read_text(&project_dir.join("PROJECT.md")),
            "flows_exists": flows_text.is_some(), "flow_coverage": check_flow_coverage(&slugs, flows_text.as_deref().unwrap_or("")),
            "approval": loop_state::roadmap_approval_for_run(work, repo.unwrap_or("")),
            "imports": self.read_import_metas(work),
            "import_coverage_text": self.app.specs.read_text(&project_dir.join("IMPORT-COVERAGE.md")).unwrap_or_default(),
        })
    }

    /// The `import.json` of every spec import of the project, newest first (what the import coverage gate checks).
    fn read_import_metas(&self, work: &str) -> Vec<Value> {
        let root = super::planning_import::imported_root(work);
        let Ok(entries) = std::fs::read_dir(&root) else { return vec![] };
        let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names.reverse();
        names
            .into_iter()
            .filter_map(|entry| {
                let meta = self.app.specs.read_json(&root.join(&entry).join(super::planning_import::META_FILE_NAME));
                (meta.get("import_id").and_then(Value::as_str) == Some(entry.as_str())).then_some(Value::Object(meta))
            })
            .collect()
    }

    /// TEST-ENVIRONMENT.md problems (as required and as if required), E2E and regression flags.
    fn read_environment_facts(work: &str, roadmap_text: Option<&str>, folder_exists: bool) -> Value {
        let text = roadmap_text.unwrap_or("");
        let e2e = parse_roadmap_e2e(text).unwrap_or(false);
        let problems = if !text.is_empty() { list_test_environment_problems(work, e2e) } else { vec![] };
        json!({
            "environment_problems": problems,
            "environment_all_problems": if folder_exists { list_test_environment_problems(work, true) } else { vec![] }, // as if required
            "e2e": e2e, // `**E2E:** yes` in the ROADMAP header (read-only in the planning payload)
            "regression_required": !text.is_empty() && (REGRESSION_REQUIRED.search(text).is_some() || e2e),
            "environment_exists": Path::new(work).join(".specs").join("project").join("TEST-ENVIRONMENT.md").is_file(),
        })
    }

    /// Loop status, the running job and the feature counters.
    fn read_loop_facts(&self, context: &ProjectContext, folder_exists: bool, features: &loop_state::Features) -> Value {
        let job = if folder_exists { self.app.jobs().get_running_job(context.repo_path()) } else { None };
        let count = |status: &str| features.iter().filter(|(_, f)| f.status() == status).count();
        json!({
            "loop": self.get_loop_status(&context.work_dir()),
            "running_job": job.map(|j| json!({"id": j["id"], "type": j["type"], "started_at": j["started_at"]})),
            "counts": {"total": features.len(), "verified": count("verified"), "blocked": count("blocked")},
        })
    }

    /// The cached readiness result (`readiness.get`: only reads, never runs); `None` while the gate `ready` is off. An unreadable cache counts as "did
    /// not run", never as ready.
    fn read_readiness(&self, context: &ProjectContext) -> Value {
        let service = self.app.readiness();
        if !service.enforced() {
            return Value::Null;
        }
        service.get(context)
    }

    /// Extra repos of a multi-repo project that miss Conformance or a map. A failure to read them never takes the checklist down.
    fn read_unready_repos(&self, context: &ProjectContext) -> Vec<Value> {
        self.app.repo_readiness().list_unready(context)
    }

    fn read_reference_warnings(&self, context: &ProjectContext) -> Vec<String> {
        self.app.repo_readiness().list_reference_warnings(context)
    }

    /// The codebase map: `docs/codebase` first, then the legacy `.specs/codebase` (a project mapped before keeps working).
    pub fn read_map_facts(work: &str, repo: &str) -> Value {
        let (relative, root) = run_hints::find_map_folder(&[work, repo]);
        let folder = relative.split('/').fold(std::path::PathBuf::from(&root), |a, p| a.join(p));
        json!({"map_files": run_hints::list_root_map_documents(&root, &relative), "map_folder": relative, "map_commit": run_hints::read_mapped_commit(&folder)})
    }

    /// The checklist, in order; one method per step that needs more than a line.
    fn build_steps(&self, facts: &Value) -> Vec<Value> {
        let (structure, approval, loop_status) = (&facts["structure"], &facts["approval"], &facts["loop"]);
        let job = facts.get("running_job").filter(|j| t(Some(j)));
        let folder_exists = t(facts.get("folder_exists"));
        let is_git = t(facts.get("is_git"));
        let missing: Vec<String> = structure["missing"].as_array().map(|m| m.iter().take(MISSING_ITEMS_SHOWN).map(|i| text_of(Some(i))).collect()).unwrap_or_default();
        let structure_ok = t(structure.get("ok"));
        let features_present = t(facts.get("features"));
        let has_every_flow = Self::has_every_flow(facts);
        let approved = t(approval.get("approved"));
        let mut steps = vec![
            create_step("folder", "Project folder", folder_exists, if folder_exists { "exists".into() } else { "the folder no longer exists".into() }, false),
            create_step("git", "Git repository", is_git, if is_git { "ok".into() } else { "the folder is not a git repository (the loop needs git)".into() }, false),
            create_step(
                "structure",
                ".specs/ structure",
                structure_ok,
                if structure_ok && structure["missing"].as_array().is_some_and(Vec::is_empty) { "complete".into() } else { format!("missing: {}", missing.join(", ")) },
                false,
            ),
        ];
        steps.extend(
            build_subprojects_gates(facts).iter().map(|g| create_step(&text_of(g.get("id")), &text_of(g.get("label")), g["ok"] == true, text_of(g.get("detail")), false)),
        );
        steps.push(self.build_conformance_step(facts));
        steps.push(Self::build_map_step(facts, job));
        steps.push(create_step("roadmap", "Roadmap", features_present && !t(facts.get("roadmap_errors")), Self::describe_roadmap(facts), false));
        steps.push(create_step("flows", "Flows (FLOWS.md)", has_every_flow, Self::describe_flows(facts), approved && !has_every_flow));
        steps.push(create_step(
            "approval",
            "Approved roadmap",
            approved,
            if approved { format!("approved on {}", text_of(approval.get("at"))) } else { text_of(approval.get("reason")) },
            false,
        ));
        steps.push(Self::build_test_environment_step(facts));
        let loop_text = format!(
            "{}{}",
            text_of(loop_status.get("status")),
            if t(loop_status.get("reason")) { format!(": {}", text_of(loop_status.get("reason"))) } else { String::new() }
        );
        steps.push(create_step("loop", "Loop", loop_status["status"] == LOOP_STATUS_DONE, loop_text, false));
        steps
    }

    fn build_conformance_step(&self, facts: &Value) -> Value {
        if read_subproject_state(facts).0 {
            let gate = build_subproject_conformance_gate(facts);
            return create_step("conformance", &text_of(gate.get("label")), gate["ok"] == true, text_of(gate.get("detail")), false);
        }
        let conformance = text_of(facts.get("conformance"));
        let detail = if !conformance.is_empty() { format!("Conformance: {conformance}") } else { self.describe_conformance_suggestion(facts) };
        let mut step = create_step("conformance", "Conformance in CLAUDE.md", !conformance.is_empty(), detail, false);
        if conformance.is_empty() && t(facts.get("folder_exists")) {
            step["suggestion"] = self.read_conformance_suggestion(facts); // the installer preselects it (None = unknown)
        }
        step
    }

    fn build_map_step(facts: &Value, job: Option<&Value>) -> Value {
        let map_running = job.is_some_and(|j| j["type"] == MAP_JOB_TYPE);
        if read_subproject_state(facts).0 {
            let gate = build_subproject_map_gate(facts);
            return create_step(
                "map",
                &text_of(gate.get("label")),
                gate["ok"] == true,
                text_of(gate.get("detail")),
                gate["optional"] == true || t(facts.get("features")),
            );
        }
        let mut detail = if t(facts.get("map_files")) {
            describe_map_documents(facts)
        } else if t(facts.get("has_code")) {
            "repo with code and no map: map it before the roadmap".into()
        } else {
            "repo without code: nothing to map".into()
        };
        if map_running {
            detail = format!("mapping (job {})", text_of(job.and_then(|j| j.get("id"))));
        }
        create_step("map", "Codebase map", t(facts.get("map_files")) && !map_running, detail, !(t(facts.get("has_code")) && !t(facts.get("features"))))
    }

    fn build_test_environment_step(facts: &Value) -> Value {
        let problems: Vec<String> = facts["environment_problems"].as_array().map(|a| a.iter().map(|i| text_of(Some(i))).collect()).unwrap_or_default();
        let detail = if let Some(first) = problems.first() {
            first.clone()
        } else if t(facts.get("environment_exists")) {
            "filled in".into()
        } else {
            "not required (no feature asks for regression)".into()
        };
        create_step("test_environment", "Test environment", problems.is_empty(), detail, problems.is_empty())
    }

    fn decide_mode_and_next_action(&self, facts: &Value) -> (&'static str, Option<&'static str>) {
        let loop_status = text_of(facts["loop"].get("status"));
        if loop_status == LOOP_STATUS_RUNNING || LOOP_STATUSES_IN_PROGRESS.contains(&loop_status.as_str()) {
            return (MODE_IN_LOOP, Some(ACTION_FOLLOW));
        }
        let pending = self.find_first_pending_action(facts);
        let features = facts["features"].as_object().cloned().unwrap_or_default();
        let all_closed = !features.is_empty() && features.values().all(|f| CLOSED_FEATURE_STATUSES.contains(&text_of(f.get("status")).as_str()));
        if loop_status == LOOP_STATUS_DONE && all_closed {
            return (MODE_DONE, Some(ACTION_FOLLOW));
        }
        let Some(pending) = pending else { return (MODE_READY_FOR_LOOP, Some(if loop_status == LOOP_STATUS_NO_LOOP { ACTION_CHECK } else { ACTION_START_LOOP })) };
        if !t(facts.get("has_code")) && features.is_empty() {
            return (MODE_NEW, Some(pending));
        }
        if !t(facts.get("has_specs")) {
            return (MODE_NO_SPECS, Some(pending));
        }
        (MODE_PLANNING, Some(pending))
    }

    /// The flow order: structure -> conformance -> map (repo with code) -> roadmap -> approval -> test environment.
    fn find_first_pending_action(&self, facts: &Value) -> Option<&'static str> {
        if !t(facts["structure"].get("ok")) {
            return Some(ACTION_STRUCTURE);
        }
        let (detected, confirmed, _, _) = read_subproject_state(facts);
        let features_present = t(facts.get("features"));
        if detected && !confirmed {
            return Some(ACTION_SUBPROJECTS); // Conformance and the map are per subproject, behind the confirmed list
        }
        if detected {
            if build_subproject_conformance_gate(facts)["ok"] != true {
                return Some(ACTION_CONFORMANCE);
            }
            let map_gate = build_subproject_map_gate(facts);
            if map_gate["ok"] != true && map_gate["optional"] != true && !features_present {
                return Some(ACTION_MAP);
            }
        } else if !t(facts.get("conformance")) {
            return Some(ACTION_CONFORMANCE);
        } else if t(facts.get("has_code")) && !t(facts.get("map_files")) && !features_present {
            return Some(ACTION_MAP);
        }
        if !features_present || t(facts.get("roadmap_errors")) {
            return Some(ACTION_ROADMAP);
        }
        if !t(facts["approval"].get("approved")) {
            return Some(self.find_planning_action(facts));
        }
        if t(facts.get("environment_problems")) {
            return Some(ACTION_TEST_ENVIRONMENT);
        }
        None
    }

    /// Roadmap not approved: `approve` only when every planning gate is ok; otherwise the first failing gate's action (flows / map / ...), `roadmap`
    /// for the planning conversation (project context, fields, open questions). A failing git gate has no action here (the git step shows it).
    fn find_planning_action(&self, facts: &Value) -> &'static str {
        let gates = build_planning_gates(self.app, facts);
        if are_gates_ok(&gates) {
            return ACTION_APPROVE;
        }
        let failing: Vec<String> = gates.iter().filter(|g| g["ok"] != true && g["optional"] != true && g["id"] != GATE_GIT).map(|g| text_of(g.get("id"))).collect();
        let Some(first) = failing.first() else { return ACTION_APPROVE };
        match first.as_str() {
            GATE_STRUCTURE => ACTION_STRUCTURE,
            GATE_SUBPROJECTS => ACTION_SUBPROJECTS,
            GATE_CONFORMANCE => ACTION_CONFORMANCE,
            GATE_MAP => ACTION_MAP,
            GATE_REPOS => ACTION_REPOS,
            GATE_FLOWS => ACTION_FLOWS,
            _ => ACTION_ROADMAP,
        }
    }

    // ------------------------------------------------ support
    fn has_every_flow(facts: &Value) -> bool {
        t(facts.get("flows_exists")) && t(facts.get("features")) && facts["flow_coverage"]["missing"].as_array().is_none_or(Vec::is_empty)
    }

    fn describe_flows(facts: &Value) -> String {
        if !t(facts.get("flows_exists")) {
            return "FLOWS.md missing".into();
        }
        let missing: Vec<String> =
            facts["flow_coverage"]["missing"].as_array().map(|m| m.iter().take(MISSING_ITEMS_SHOWN).map(|i| text_of(Some(i))).collect()).unwrap_or_default();
        if !missing.is_empty() {
            return format!("without a flow: {}", missing.join(", "));
        }
        let count = facts["features"].as_object().map(|f| f.len()).unwrap_or(0);
        if count > 0 {
            format!("{count} flow(s), one per feature")
        } else {
            "no feature in the ROADMAP yet".into()
        }
    }

    fn describe_roadmap(facts: &Value) -> String {
        if let Some(job) = facts.get("running_job").filter(|j| t(Some(j))) {
            if text_of(job.get("type")).starts_with(ROADMAP_JOB_TYPE_PREFIX) {
                return format!("generating (job {})", text_of(job.get("id")));
            }
        }
        if !t(facts.get("roadmap_exists")) {
            return "ROADMAP.md missing".into();
        }
        if let Some(first) = facts["roadmap_errors"].as_array().and_then(|e| e.first()) {
            return format!("outside the grammar: {}", text_of(Some(first)));
        }
        let features = facts["features"].as_object().cloned().unwrap_or_default();
        if features.is_empty() {
            return "ROADMAP.md has no feature line".into();
        }
        let mut stages: Vec<String> = Vec::new();
        for fields in features.values() {
            let stage = text_of(fields.get("stage"));
            if !stages.contains(&stage) {
                stages.push(stage);
            }
        }
        format!("{} feature(s) in {} stage(s)", features.len(), stages.len())
    }

    /// `{suggestion, stack, evidence}` of the script, or `None`: the suggestion is only a help and never fails the progress.
    fn read_conformance_suggestion(&self, facts: &Value) -> Value {
        self.app.specs_structure().suggest_conformance(&text_of(facts.get("work_dir")))
    }

    fn describe_conformance_suggestion(&self, facts: &Value) -> String {
        if !t(facts.get("folder_exists")) {
            return "no Conformance line".into();
        }
        let suggestion = self.app.specs_structure().suggest_conformance(&text_of(facts.get("work_dir")));
        format!("no Conformance line (script suggestion: {}, stack {})", text_of(suggestion.get("suggestion")), text_of(suggestion.get("stack")))
    }

    pub fn repo_has_code(repo: &str) -> bool {
        let Ok(entries) = std::fs::read_dir(repo) else { return false };
        entries.flatten().any(|e| !NAMES_THAT_ARE_NOT_CODE.contains(&e.file_name().to_string_lossy().to_lowercase().as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::project::Project;

    fn context(app: &App, dir: &Path, name: &str) -> ProjectContext {
        let repo = dir.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        let project = app.project_service().open_existing_project(&repo.display().to_string()).unwrap();
        ProjectContext::new(Project { ..project })
    }

    #[test]
    fn an_empty_folder_starts_at_structure_and_a_filled_plan_reaches_approve() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let ctx = context(&app, dir.path(), "proj");
        let progress = app.progress().build_progress(&ctx);
        assert_eq!((progress["mode"].clone(), progress["next_action"].clone(), progress["loop_status"].clone()), (json!("new"), json!("structure"), json!("no_loop")));
        let ids: Vec<&str> = progress["steps"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["folder", "git", "structure", "conformance", "map", "roadmap", "flows", "approval", "test_environment", "loop"]);
        assert_eq!(progress["steps"][2]["detail"], "missing: project/, features/, project/PROJECT.md, project/ROADMAP.md, project/STATE.md, project/FLOWS.md");
        let work = ctx.work_dir();
        std::fs::write(Path::new(&work).join("main.rs"), "fn main(){}").unwrap();
        app.specs_structure().initialize_structure(&work, Some("standard"), "proj").unwrap();
        let progress = app.progress().build_progress(&ctx);
        assert_eq!(progress["next_action"], "map"); // a repo with code and no map
        assert_eq!(progress["steps"][3]["detail"], "Conformance: STANDARD");
        assert!(app.progress().read_cached_planning_gates(&ctx, 10).is_none());
        let gates = app.progress().build_planning_gates(&ctx);
        assert!(app.progress().read_cached_planning_gates(&ctx, 10).is_some() && !gates.is_empty());
        assert!(!app.progress().is_loop_running(&ctx));
    }

    #[test]
    fn facts_helpers_and_roadmap_description() {
        let (features, _) =
            loop_state::parse_roadmap("- [a] status=verified title=\"A\" scope=Small stage=1 paths=x\n- [b] status=blocked title=\"B\" scope=Small stage=2 paths=y\n");
        let value = features_to_value(&features);
        assert_eq!(value["a"]["status"], "verified");
        assert_eq!(features_with_slug(&features)[1]["slug"], "b");
        let facts = json!({"features": value, "roadmap_exists": true, "roadmap_errors": [], "running_job": null});
        assert_eq!(ProjectProgressService::describe_roadmap(&facts), "2 feature(s) in 2 stage(s)");
        assert_eq!(ProjectProgressService::describe_roadmap(&json!({"features": {}, "roadmap_exists": false})), "ROADMAP.md missing");
        assert_eq!(ProjectProgressService::describe_roadmap(&json!({"features": {}, "roadmap_exists": true, "roadmap_errors": ["bad"]})), "outside the grammar: bad");
        assert_eq!(
            ProjectProgressService::describe_flows(&json!({"flows_exists": true, "features": value, "flow_coverage": {"missing": ["a", "b"]}})),
            "without a flow: a, b"
        );
        assert!(ProjectProgressService::repo_has_code(&std::env::current_dir().unwrap().display().to_string()));
    }
}
