// SPDX-License-Identifier: MIT
//! The `claude -p` map job: a safety snapshot of the repository, the mapping run, the undo of every change outside the allowed folders and, after
//! it, the app (not the AI) writing the map index and the `CODEBASE` block of `CLAUDE.md`.
//!
//! Used by the root map (`RoadmapService::map_codebase`) and reusable by a per-subproject map: the folder that holds `docs/codebase` and `CLAUDE.md`
//! (`map_root`), the prompt, the allowed folders, the model and the hook profile are parameters. The HTTP request only writes the job record: the
//! snapshot is taken in `before_start`, inside the job thread.

use super::claude_jobs::{ClaudeJobCommand, ClaudeJobError, JOB_MODEL_CEILING, JOB_TYPE_MAP};
use super::code_inventory::inventory_name;
use super::App;
use crate::domain::errors::AppError;
use crate::infrastructure::claude_client::CommandOptions;
use crate::infrastructure::workspace_file_guard::{WorkspaceFileGuard, WorkspaceSnapshot, DEFAULT_IGNORED_NAMES};
use engine_state::run_hints;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The same default as the loop (`--permission-mode auto`).
pub const JOB_PERMISSION_MODE: &str = "auto";
pub const MAP_CODEBASE_PROMPT: &str = "/orchestrator map";
/// The map job may only change files here; anything else is undone after the job.
pub const MAP_ALLOWED_FOLDERS: [&str; 2] = [".specs", run_hints::MAP_FOLDER];
pub const MAP_GUARD_IGNORED_PATHS: [&str; 4] = [".git", "node_modules", ".specs/project/loop/internal", ".specs/project/loop/json/MONITOR.json"];
const MAP_GUARD_IGNORED_TRASH: &str = ".specs/.trash";
pub const MAP_NOTHING_WRITTEN: &str = "The AI finished but wrote no document in docs/codebase, so nothing was mapped. The usual cause: the /orchestrator skill is not loaded in that claude session (update the plugin: claude plugin update agent-maestro@agent-maestro) and the AI answered as a plain chat.";
pub const MAP_GUARD_REFUSALS_SHOWN: usize = 3;

/// The failure text when the guard refused actions during a job that then wrote nothing: the real cause, not the skill.
pub fn guard_blocked_message(refusals: &[String]) -> String {
    format!(
        "The AI finished but wrote no document in docs/codebase, so nothing was mapped. The guard refused {} action(s) during the job: {}. Fix or release that rule (PERMISSIONS.json) and map again.",
        refusals.len(),
        refusals.join(" | ")
    )
}
pub const ROOT_INVENTORY_NAME: &str = "_root";
pub const MAP_GUARD_NOTE: &str = "\n\n(The app undid changes outside .specs/ and docs/codebase/: {})";
pub const MAP_GUARD_FILES_SHOWN: usize = 8;

/// The paths the map guards leave alone (the five of the Python tuple).
pub fn map_guard_ignored_paths() -> Vec<&'static str> {
    let mut paths = MAP_GUARD_IGNORED_PATHS.to_vec();
    paths.push(MAP_GUARD_IGNORED_TRASH);
    paths
}

pub fn guard_note(touched: &[String]) -> String {
    MAP_GUARD_NOTE.replace("{}", &touched.iter().take(MAP_GUARD_FILES_SHOWN).cloned().collect::<Vec<_>>().join(", "))
}

pub type ExtraBeforeStart = Box<dyn FnOnce(&dyn Fn(&str)) -> Result<(), ClaudeJobError> + Send>;

/// Parameters of [`MapJobRunner::start`].
pub struct MapStart {
    /// The repository, where the job is registered and the snapshot is taken (so a write into a sibling is caught).
    pub repo_path: String,
    pub project_id: String,
    /// `None` = no `--max-budget-usd`.
    pub budget_usd: Option<f64>,
    pub prompt: String,
    /// Where claude runs (default = `repo_path`).
    pub work_dir: Option<String>,
    /// Relative paths claude may change.
    pub allowed_folders: Vec<String>,
    pub ignored_paths: Vec<String>,
    /// Folder, relative to `repo_path` ("" = the repository root), whose `docs/codebase` receives the index and whose `CLAUDE.md` receives the
    /// `CODEBASE` block.
    pub map_root: String,
    pub model: String,
    pub hook_profile: String,
    pub job_type: String,
    /// Optional, runs after the snapshot in the job thread (e.g. a later inventory); an error ends the job as error.
    pub extra_before_start: Option<ExtraBeforeStart>,
    pub parallel_key: Option<String>,
}

impl MapStart {
    pub fn new(repo_path: &str, project_id: &str) -> Self {
        MapStart {
            repo_path: repo_path.into(),
            project_id: project_id.into(),
            budget_usd: None,
            prompt: MAP_CODEBASE_PROMPT.into(),
            work_dir: None,
            allowed_folders: MAP_ALLOWED_FOLDERS.iter().map(|s| s.to_string()).collect(),
            ignored_paths: map_guard_ignored_paths().iter().map(|s| s.to_string()).collect(),
            map_root: String::new(),
            model: JOB_MODEL_CEILING.into(),
            hook_profile: "spec".into(),
            job_type: JOB_TYPE_MAP.into(),
            extra_before_start: None,
            parallel_key: None,
        }
    }
}

pub struct MapJobRunner<'a> {
    pub app: &'a App,
}

impl App {
    pub fn map_jobs(&self) -> MapJobRunner<'_> {
        MapJobRunner { app: self }
    }
}

fn path_refs(items: &[String]) -> Vec<&str> {
    items.iter().map(String::as_str).collect()
}

impl MapJobRunner<'_> {
    /// Registers the job (202 path) and returns its public record at once.
    pub fn start(&self, params: MapStart) -> Result<Value, AppError> {
        let MapStart {
            repo_path,
            project_id,
            budget_usd,
            mut prompt,
            work_dir,
            allowed_folders,
            ignored_paths,
            map_root,
            model,
            hook_profile,
            job_type,
            extra_before_start,
            parallel_key,
        } = params;
        let started_at = engine_state::loop_io::now_iso(); // same local format as the `t` of guard.log
        let holder: Arc<Mutex<Option<WorkspaceSnapshot>>> = Arc::new(Mutex::new(None));
        let map_root_path = if map_root.is_empty() { PathBuf::from(&repo_path) } else { Path::new(&repo_path).join(&map_root) };
        let map_folder = run_hints::MAP_FOLDER.split('/').fold(map_root_path.clone(), |a, p| a.join(p));
        let use_inventory = map_root.is_empty() && prompt == MAP_CODEBASE_PROMPT;
        if use_inventory {
            // The path is known from the name, so the prompt is built before the thread. If the inventory then fails the record says so (progress)
            // and the mapper simply does not find the file.
            prompt.push_str(&self.app.inventory.prompt_argument(ROOT_INVENTORY_NAME));
        }
        let app = self.app.shared();
        let (snapshot_slot, snapshot_repo, snapshot_ignored) = (holder.clone(), repo_path.clone(), ignored_paths.clone());
        let before_start: super::claude_jobs::BeforeStart = Box::new(move |update_progress| {
            let guard = WorkspaceFileGuard::default();
            *snapshot_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(guard.take_snapshot(&snapshot_repo, &path_refs(&snapshot_ignored), &DEFAULT_IGNORED_NAMES));
            if use_inventory {
                app.inventory.run_for_job(&snapshot_repo, ".", ROOT_INVENTORY_NAME, update_progress);
            }
            if let Some(extra) = extra_before_start {
                extra(update_progress)?;
            }
            Ok(None)
        });
        let options =
            CommandOptions { model, budget_usd, no_mcp: true, prompt_argument: prompt, permission_mode: JOB_PERMISSION_MODE.into(), hook_profile, ..Default::default() };
        let argv = self.app.claude.build_command(&options);
        let finish_app = self.app.shared();
        let (finish_holder, finish_project, finish_repo, finish_root, finish_folder) = (holder, project_id, repo_path.clone(), map_root_path, map_folder);
        let mut command = ClaudeJobCommand::new(argv, "");
        command.work_dir = work_dir;
        command.parallel_key = parallel_key;
        command.before_start = Some(before_start);
        command.finalize = Some(Box::new(move |result| {
            finish_app.map_jobs().finish(&finish_project, &finish_repo, &finish_root, &finish_folder, &finish_holder, &allowed_folders, &started_at, result)
        }));
        self.app.jobs().start_job(&repo_path, &job_type, command)
    }

    /// Runs in the job thread: undoes changes outside the allowed folders, fails the job when no map document was written and writes the index and
    /// the `CODEBASE` block.
    fn finish(
        &self,
        project_id: &str,
        repo_path: &str,
        map_root_path: &Path,
        map_folder: &Path,
        holder: &Arc<Mutex<Option<WorkspaceSnapshot>>>,
        allowed_folders: &[String],
        started_at: &str,
        result: Value,
    ) -> Value {
        let snapshot = holder.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(snapshot) = snapshot else { return result };
        let report = WorkspaceFileGuard::default().revert_unexpected_changes(&snapshot, &path_refs(allowed_folders));
        let list = |key: &str| -> Vec<String> { report[key].as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default() };
        let (reverted, removed, unrestorable) = (list("reverted"), list("removed"), list("unrestorable"));
        let touched: Vec<String> = reverted.iter().chain(&removed).chain(&unrestorable).cloned().collect();
        if !touched.is_empty() {
            let head = |items: &[String]| json!(items.iter().take(50).collect::<Vec<_>>());
            self.app.log.warn(
                "map_guard_reverted",
                &[("project", json!(project_id)), ("reverted", head(&reverted)), ("removed", head(&removed)), ("unrestorable", head(&unrestorable))],
            );
        }
        let mut result = result;
        let ok = engine_state::notify_truthy(result.get("ok"));
        if ok && !touched.is_empty() {
            let reply = result.get("reply").and_then(Value::as_str).unwrap_or("").to_string();
            result["reply"] = json!(format!("{reply}{}", guard_note(&touched)));
        }
        if ok && run_hints::list_map_documents(map_folder).is_empty() {
            result["ok"] = json!(false);
            let refusals = self.guard_refusals_since(&[repo_path, &map_root_path.display().to_string()], started_at);
            result["error"] = json!(if refusals.is_empty() { MAP_NOTHING_WRITTEN.to_string() } else { guard_blocked_message(&refusals) });
        }
        if engine_state::notify_truthy(result.get("ok")) {
            // after the undo above: this is the app (not the AI) writing the index and pointing CLAUDE.md at the map
            run_hints::write_map_index(&map_root_path.display().to_string(), self.read_short_head(repo_path).as_deref(), None);
            run_hints::write_codebase_block(&map_root_path.display().to_string());
            if pycompat::nt_abspath(&map_root_path.display().to_string()) == pycompat::nt_abspath(repo_path) {
                self.app.context_blocks().refresh_folder(repo_path, None, "", Some(repo_path));
            }
        }
        result
    }

    /// M4. `rule: detail` of the guard blocks logged since `since` in any of `folders` (at most `MAP_GUARD_REFUSALS_SHOWN`, redacted and clipped by
    /// the same code as the panel's guard list).
    pub fn guard_refusals_since(&self, folders: &[&str], since: &str) -> Vec<String> {
        let mut found: Vec<(String, String)> = Vec::new();
        for folder in folders {
            for line in self.app.loop_files.read_guard_lines(folder) {
                let decision = line.get("decision").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
                let stamp = line.get("t").and_then(Value::as_str).unwrap_or("");
                if !super::guard_events::BLOCK_DECISIONS.contains(&decision.as_str()) || stamp < since {
                    continue;
                }
                let event = super::guard_events::build_guard_event(&line);
                let item = format!("{}: {}", event["rule"].as_str().unwrap_or(""), event["detail"].as_str().unwrap_or(""));
                if !found.iter().any(|(_, existing)| *existing == item) {
                    found.push((stamp.to_string(), item));
                }
            }
        }
        found.sort();
        found.into_iter().map(|(_, item)| item).take(MAP_GUARD_REFUSALS_SHOWN).collect()
    }

    /// Short sha of HEAD for the "mapped at" marker; `None` when git cannot tell (the marker then says it is unknown).
    pub fn read_short_head(&self, repo_path: &str) -> Option<String> {
        Some(self.app.git.get_short_head(repo_path)).filter(|s| !s.is_empty())
    }
}

/// Folder name used for the inventory of a subproject (`src/api` -> `src-api`).
pub fn subproject_inventory_name(path: &str) -> String {
    inventory_name(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::process_runner::ProcessRunner;
    use std::time::Duration;

    /// A fake claude that writes a map document when asked and answers with a result.
    struct Mapper {
        write_into: Option<PathBuf>,
        also_touch: Option<PathBuf>,
        /// Lines the guard "wrote" into this guard.log while the job ran.
        guard_log: Option<(PathBuf, String)>,
    }

    impl ProcessRunner for Mapper {
        fn run_and_wait(&self, argv: &[String], _: &str, _: u64, _: Option<&str>, _: &[(String, String)], _: Option<&[(String, String)]>) -> (i32, String, String) {
            if !argv.iter().any(|a| a == "-p") {
                return (1, String::new(), String::new()); // git and friends: not the model
            }
            if let Some(dir) = &self.write_into {
                std::fs::create_dir_all(dir).unwrap();
                std::fs::write(dir.join("STACK.md"), "x").unwrap();
            }
            if let Some((path, lines)) = &self.guard_log {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, lines).unwrap();
            }
            if let Some(file) = &self.also_touch {
                std::fs::write(file, "oops").unwrap();
            }
            (0, r#"{"type":"result","is_error":false,"result":"mapped","session_id":"s","total_cost_usd":0.1}"#.to_string(), String::new())
        }
        fn spawn_detached(&self, _: &[String], _: &str, _: &str, _: &[(String, String)]) -> std::io::Result<i64> {
            Ok(1)
        }
        fn is_alive(&self, _: i64) -> bool {
            false
        }
        fn kill_tree(&self, _: i64) {}
    }

    fn wait_finished(app: &App, repo: &str, id: &str) -> Value {
        for _ in 0..200 {
            let job = app.jobs().get_job(repo, id).unwrap();
            if job["state"] != "running" {
                return job;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("job did not finish");
    }

    #[test]
    fn a_map_job_undoes_stray_edits_and_writes_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.rs"), "original").unwrap();
        let runner = Arc::new(Mapper { write_into: Some(repo.join("docs/codebase")), also_touch: Some(repo.join("src/a.rs")), guard_log: None });
        let app = App::for_tests_with(dir.path(), runner);
        let repo_text = repo.display().to_string();
        let job = app.map_jobs().start(MapStart::new(&repo_text, "p1")).unwrap();
        let finished = wait_finished(&app, &repo_text, job["id"].as_str().unwrap());
        assert_eq!(finished["state"], "ok");
        assert!(finished["reply"].as_str().unwrap().starts_with("mapped\n\n(The app undid changes outside .specs/ and docs/codebase/: src/a.rs)"));
        assert_eq!(std::fs::read_to_string(repo.join("src/a.rs")).unwrap(), "original");
        let index = std::fs::read_to_string(repo.join("docs/codebase/README.md")).unwrap();
        assert!(index.contains("- [STACK.md](STACK.md): languages") && index.contains("Mapped at commit unknown on "));
        assert!(std::fs::read_to_string(repo.join("CLAUDE.md")).unwrap().contains("maestro:context"));
    }

    #[test]
    fn a_map_job_that_writes_nothing_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let app = App::for_tests_with(dir.path(), Arc::new(Mapper { write_into: None, also_touch: None, guard_log: None }));
        let repo_text = repo.display().to_string();
        let job = app.map_jobs().start(MapStart::new(&repo_text, "p1")).unwrap();
        let finished = wait_finished(&app, &repo_text, job["id"].as_str().unwrap());
        assert_eq!((finished["state"].clone(), finished["error"].clone()), (json!("error"), json!(MAP_NOTHING_WRITTEN)));
        assert!(guard_note(&["a".into(), "b".into()]).ends_with("(The app undid changes outside .specs/ and docs/codebase/: a, b)"));
    }

    #[test]
    fn a_job_that_wrote_nothing_names_the_guard_refusals_of_this_job_only() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let log = repo.join(".specs/project/loop/internal/guard.log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        // a refusal from an earlier day must not be blamed; the one written during the job must
        let lines = concat!(
            r#"{"t":"2001-01-01T00:00:00","rule":"old-rule","decision":"blocked","detail":"earlier job"}"#,
            "\n",
            r#"{"t":"2999-01-01T00:00:00","rule":"scope-specs-only","decision":"blocked","detail":"write outside the allowed folders"}"#,
            "\n",
            r#"{"t":"2999-01-01T00:00:01","rule":"fyi","decision":"info","detail":"not a block"}"#,
            "\n"
        );
        std::fs::write(&log, &lines[..lines.find("\n{\"t\":\"2999").unwrap() + 1]).unwrap();
        let runner = Arc::new(Mapper { write_into: None, also_touch: None, guard_log: Some((log.clone(), lines.to_string())) });
        let app = App::for_tests_with(dir.path(), runner);
        let repo_text = repo.display().to_string();
        let job = app.map_jobs().start(MapStart::new(&repo_text, "p1")).unwrap();
        let finished = wait_finished(&app, &repo_text, job["id"].as_str().unwrap());
        let error = finished["error"].as_str().unwrap();
        assert!(error.contains("scope-specs-only: write outside the allowed folders"), "{error}");
        assert!(!error.contains("old-rule") && !error.contains("not a block") && !error.contains("skill is not loaded"), "{error}");
    }
}
