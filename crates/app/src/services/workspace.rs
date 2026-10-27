// SPDX-License-Identifier: MIT
//! Workspace of a project: what exists on disk for the loop (a git worktree per repo) and the job that creates it.
//!
//! `get_workspace` tells, per repo, the folder, the worktree, the loop branch, the base branch and a state
//! (`not_created | ready | loop_running | dirty | missing`) plus the plain-language explanation. `create_workspace` starts a job (registry of the
//! claude jobs, type `workspace-create`, no claude involved) that runs the ENGINE routine and turns each engine step line into a step the panel
//! polls. The worktree is always created by the engine (the same code the loop start uses); nothing here runs `git worktree add`. An existing
//! worktree is never recreated.

use super::claude_jobs::JOB_TYPE_WORKSPACE_CREATE;
use super::loop_control::LAUNCH_MARKER_PREFIX;
use super::planning_gates::get_integration_branch;
use super::planning_repos::load_last_rejections;
use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use crate::repositories::loop_command_file::RELAUNCH_LOG_FILE_NAME;
use crate::repositories::loop_read_formatting::now_iso;
use crate::repositories::project_files::Read;
use engine_state::project_repos::{self, RepoRef};
use engine_state::{context_files, loop_state, setup_files, subprojects};
use pycompat::{nt_join, Re};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

pub const BUSY_CODE: &str = "busy";
pub const UNKNOWN_REPO_CODE: &str = "unknown_repo";
pub const COMMIT_FAILED_CODE: &str = "commit_push_failed";
const COMMIT_MESSAGE_LIMIT: usize = 200;
const DEFAULT_COMMIT_MESSAGE: &str = "chore: save work before the loop worktree is created";
const STATE_NOT_CREATED: &str = "not_created";
const STATE_READY: &str = "ready";
const STATE_LOOP_RUNNING: &str = "loop_running";
const STATE_DIRTY: &str = "dirty";
const STATE_MISSING: &str = "missing";
/// A reference repository: read by the agents, never given a worktree.
const STATE_READ_ONLY: &str = "read_only";
pub const USER_FOLDER_REFUSED_CODE: &str = "user_folder_refused";
pub const NOT_LOOP_BRANCH_CODE: &str = "not_loop_branch";
const LOOP_BRANCH_PREFIX: &str = "loop/";
const PROTECTED_BRANCHES: [&str; 2] = ["main", "master"];
const USER_FOLDER_REFUSAL: &str = "this route only saves a loop worktree: for your own folder use Commit setup files and push in Workspace";
const SETUP_NOT_SET_UP: &str = "not_set_up";
const SETUP_SET_UP: &str = "set_up";
const SETUP_MAP_MISSING: &str = "map_missing";
const SETUP_MAPPING: &str = "mapping";
const MAP_JOB_TYPE: &str = "map";
const EXPLANATION: &str = "The loop works in a separate folder (a git worktree) on a new loop/<date> branch, so your folder and branch are never touched. Nothing is pushed until you deliver.";
const UNCOMMITTED_NOTE: &str = "Your folder has uncommitted changes. The worktree is born from the last commit and does not carry them.";
const MISSING_NOTE: &str = "The repo folder does not exist anymore; restore it or remove the repo from the list.";
/// Copied at creation by the engine; not the user's copy list.
const STANDARD_COPIED_NAMES: [&str; 3] = [".specs", "CLAUDE.md", ".claude"];
const SPECS_PREFIX: &str = ".specs/";
const CREATE_TIMEOUT_SECONDS: u64 = 600;
const POLL_SECONDS: f64 = 0.3;
const CREATE_LOG_FILE_NAME: &str = "workspace-create.log";
const LOG_OUTPUT_LIMIT: usize = 300;
/// Read-only job built from the loop's own output while it creates worktrees at start.
pub const LOOP_START_JOB_ID: &str = "loop-start";
const START_LOG_TAIL_BYTES: u64 = 256 * 1024;

static STEP_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\[loop\] (?:\[([^\]]+)\] )?(check|warn|worktree-add|reattach|copy|setup|ready|reuse|error): (.*)\z"));
static LEGACY_REUSE_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\[loop\] reusing worktree (.+) on branch (\S+)\z"));
static LEGACY_CREATED_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\[loop\] worktree created: (.+?) \(branch (\S+), base ([^;]+);"));
static LEGACY_SETUP_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\[loop\] one-time setup: (.*)\z"));
static READY_TEXT: LazyLock<Re> = LazyLock::new(|| Re::new(r"^path=(.*) branch=(\S+) base=(\S+)\z"));
static TURN_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\[loop\] turn \d+"));

fn to_posix(path: &str) -> String {
    path.replace('\\', "/")
}

/// One step line the engine printed.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub repo: String,
    pub kind: String,
    pub text: String,
}

/// One engine stdout line -> a step, or `None` (a line that is not a step still goes to the log). The three legacy single-repo lines (no `[repo]`
/// tag) map to reuse / ready / setup for the primary repo.
pub fn parse_step_line(line: &str, primary_name: &str) -> Option<Step> {
    let line = line.trim();
    let step = |repo: &str, kind: &str, text: String| Some(Step { repo: repo.into(), kind: kind.into(), text });
    if let Some(found) = STEP_LINE.match_start(line) {
        return step(found.group(1).filter(|r| !r.is_empty()).unwrap_or(primary_name), &found[2], found[3].to_string());
    }
    if let Some(found) = LEGACY_REUSE_LINE.match_start(line) {
        return step(primary_name, "reuse", format!("reusing worktree {} on branch {}", &found[1], &found[2]));
    }
    if let Some(found) = LEGACY_CREATED_LINE.match_start(line) {
        return step(primary_name, "ready", format!("path={} branch={} base={}", &found[1], &found[2], found[3].trim()));
    }
    if let Some(found) = LEGACY_SETUP_LINE.match_start(line) {
        return step(primary_name, "setup", found[1].to_string());
    }
    None
}

/// Thin adapter over the engine routine (`ensure_worktrees` of the loop). Runs `harness loop <primary repo> --ensure-worktrees [--repos a,b]`
/// detached, its output going to a log file that this adapter tails line by line (so the panel sees each step while it happens). The process prints
/// the step lines of the contract (`[loop] [<repo>] <kind>: <text>`) and exits.
pub struct EngineWorktreeAdapter<'a> {
    pub app: &'a App,
}

impl EngineWorktreeAdapter<'_> {
    /// Starts the routine and calls `on_output_line(line)` for every output line while it runs. Returns `None`, or an error text.
    pub fn run(&self, primary_repo: &str, repo_names: &[String], log_path: &Path, on_output_line: &mut dyn FnMut(&str), timeout_seconds: u64) -> Option<String> {
        let mut argv = self.app.engine_argv("run_loop");
        argv.extend([primary_repo.to_string(), "--ensure-worktrees".into()]);
        if !repo_names.is_empty() {
            argv.extend(["--repos".to_string(), repo_names.join(",")]);
        }
        let _ = std::fs::create_dir_all(log_path.parent().unwrap_or(Path::new(".")));
        let _ = std::fs::remove_file(log_path);
        let pid = match self.app.processes.spawn_detached(&argv, primary_repo, &log_path.display().to_string(), &[]) {
            Ok(pid) => pid,
            Err(failure) => return Some(format!("could not start the engine: {failure}")),
        };
        let (mut position, mut pending) = (0u64, String::new());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_seconds);
        loop {
            let alive = self.app.processes.is_alive(pid);
            (position, pending) = Self::read_new_lines(log_path, position, pending, on_output_line, !alive);
            if !alive {
                return None;
            }
            if std::time::Instant::now() > deadline {
                self.app.processes.kill_tree(pid);
                return Some(format!("timed out after {} minutes", timeout_seconds / 60));
            }
            std::thread::sleep(std::time::Duration::from_secs_f64(POLL_SECONDS));
        }
    }

    fn read_new_lines(log_path: &Path, position: u64, pending: String, on_output_line: &mut dyn FnMut(&str), flush: bool) -> (u64, String) {
        use std::io::{Read as _, Seek, SeekFrom};
        let Ok(mut handle) = std::fs::File::open(log_path) else { return (position, pending) };
        let mut data = Vec::new();
        if handle.seek(SeekFrom::Start(position)).is_err() || handle.read_to_end(&mut data).is_err() {
            return (position, pending);
        }
        let text = format!("{pending}{}", String::from_utf8_lossy(&data));
        let mut lines: Vec<&str> = text.split('\n').collect();
        let pending = if flush { String::new() } else { lines.pop().unwrap_or("").to_string() };
        for line in lines.iter().filter(|l| !l.trim().is_empty()) {
            on_output_line(line.trim_end_matches('\r'));
        }
        (position + data.len() as u64, pending)
    }
}

pub struct WorkspaceService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn workspace(&self) -> WorkspaceService<'_> {
        WorkspaceService { app: self }
    }
}

fn repo_conflict(message: impl Into<String>, code: &str) -> AppError {
    AppError::new(Kind::RepoConflict, message).with_code(code)
}

fn invalid_repo(message: impl Into<String>) -> AppError {
    AppError::new(Kind::InvalidRepo, message)
}

impl WorkspaceService<'_> {
    // ------------------------------------------------ read
    pub fn get_workspace(&self, context: &ProjectContext) -> Value {
        let (multi, stored) = self.app.repo_registry().read_stored(context);
        let stored_setup: Vec<(String, String)> = stored.iter().map(|r| (r.name.clone(), r.setup.clone())).collect();
        let loop_running = self.app.loop_status().is_loop_running(context);
        let mut meta = self.app.loop_files.read_meta(&context.work_dir());
        if meta.is_empty() {
            // the work dir is still the primary repo while no loop ran: the bare worktree holds the recorded branch and base
            meta = self.app.loop_files.read_meta(&context.project.loop_worktree_path());
        }
        let cli_setup = self.read_saved_setup(context); // the CLI --setup applies to the primary when REPOS.json has none for it
        let changed = self.app.repo_registry().read_changed_repo_names(context, &stored);
        let repos: Vec<Value> = project_repos::load_repos(context.repo_path())
            .iter()
            .map(|repo| {
                let own = stored_setup.iter().find(|(n, _)| *n == repo.name).map(|(_, s)| s.clone()).filter(|s| !s.is_empty());
                let setup = own.or(if repo.primary { cli_setup.clone() } else { None });
                self.describe_repo(context, repo, &meta, loop_running, setup, Some(&changed))
            })
            .collect();
        let job = self.find_running_create_job(context).or_else(|| self.read_loop_start_job(context));
        let defined = project_repos::is_scope_defined(context.repo_path()) || repos.iter().any(|r| r["worktree_exists"] == true);
        json!({
            "ok": true, "multi": multi, "explanation": EXPLANATION, "repos": repos, "job": job,
            "integration_branch": self.integration_branch(context), "plan_folder": nt_join(&nt_join(&context.work_dir(), ".specs"), "project"),
            "defined": defined, // a loop that already has a worktree keeps the scope it has
            "rejected": load_last_rejections(context), // refused suggestions of the planning chat [{path, reason}]
        })
    }

    fn integration_branch(&self, context: &ProjectContext) -> String {
        let text = std::fs::read(Path::new(&context.specs_dir()).join("project").join("ROADMAP.md")).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        get_integration_branch(self.app, &text)
    }

    fn describe_repo(&self, context: &ProjectContext, repo: &RepoRef, meta: &Map<String, Value>, loop_running: bool, setup: Option<String>, changed: Option<&BTreeSet<String>>) -> Value {
        let (folder, worktree) = (repo.path.clone(), repo.loop_worktree_path());
        let (folder_exists, worktree_exists) = (Path::new(&folder).is_dir(), Path::new(&worktree).is_dir());
        let recorded: Map<String, Value> = match meta.get("repos") {
            Some(Value::Object(repos)) => repos.get(&repo.name).and_then(Value::as_object).cloned().unwrap_or_default(),
            _ if repo.primary => meta.clone(),
            _ => Map::new(),
        };
        let git = &self.app.git;
        let non_empty = |text: String| if text.is_empty() { None } else { Some(text) };
        let recorded_text = |key: &str| recorded.get(key).and_then(Value::as_str).filter(|v| !v.is_empty()).map(String::from);
        let branch = if worktree_exists { non_empty(git.get_current_branch(&worktree)).or_else(|| recorded_text("branch")) } else { None };
        let base = if worktree_exists && recorded_text("base").is_some() {
            recorded_text("base")
        } else if folder_exists {
            non_empty(git.get_current_branch(&folder))
        } else {
            None
        };
        let (mut notes, mut dirty, mut uncommitted_context): (Vec<&str>, bool, Vec<Value>) = (vec![], false, vec![]);
        let read_only = repo.role == project_repos::ROLE_REFERENCE;
        let state;
        if !folder_exists {
            state = STATE_MISSING;
            notes.push(MISSING_NOTE);
        } else if read_only {
            state = STATE_READ_ONLY; // no worktree, nothing carried, nothing to commit for the loop: read only
        } else if !worktree_exists {
            state = STATE_NOT_CREATED;
            dirty = self.has_changes_outside_specs(&folder);
            uncommitted_context = self.describe_uncommitted_context(context, repo);
            if dirty {
                notes.push(UNCOMMITTED_NOTE);
            }
        } else {
            dirty = self.has_changes_outside_specs(&worktree);
            state = if loop_running { STATE_LOOP_RUNNING } else if dirty { STATE_DIRTY } else { STATE_READY };
        }
        let mut description = Map::new();
        for (key, value) in [
            ("name", json!(repo.name)),
            ("path", json!(folder)),
            ("primary", json!(repo.primary)),
            ("folder", json!(folder)),
            ("worktree", json!(worktree)),
            ("worktree_exists", json!(worktree_exists)),
            ("branch", json!(branch)),
            ("base", json!(base)),
            ("state", json!(state)),
            ("dirty", json!(dirty)),
            ("uncommitted_context", json!(uncommitted_context)),
            ("copy", json!(repo.copy)),
            ("setup_command", json!(setup.filter(|s| !s.is_empty()))),
            ("notes", json!(notes)),
            ("role", json!(repo.role)),
            ("reason", json!(Some(repo.reason.clone()).filter(|r| !r.is_empty()))),
            ("changed_by_feature", json!(changed.is_some_and(|names| names.contains(&repo.name)))),
            ("read_only", json!(read_only)),
        ] {
            description.insert(key.into(), value);
        }
        let branches = if folder_exists { self.describe_branches(&folder, &self.integration_branch(context)) } else { json!({"current_branch": null, "base_branch": null, "base_sha": null, "upstream": null}) };
        description.extend(branches.as_object().cloned().unwrap_or_default());
        description.insert("setup".into(), if folder_exists { self.describe_setup_state(context, repo) } else { json!({"state": SETUP_NOT_SET_UP, "monorepo": false}) });
        let changes = if folder_exists && !read_only { self.classify_user_folder(context, repo) } else { json!({"setup_files": [], "other_changes": []}) };
        description.extend(changes.as_object().cloned().unwrap_or_default());
        Value::Object(description)
    }

    /// Where the loop worktree of this repository would start (spec BR-01): the base branch (`integration`) when it exists locally, else the current
    /// branch. `upstream` = the remote branch the current branch tracks (`null` without one).
    fn describe_branches(&self, folder: &str, integration: &str) -> Value {
        let git = &self.app.git;
        let current = Some(git.get_current_branch(folder)).filter(|c| !c.is_empty());
        let (code, _, _) = git.run_git(folder, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{integration}")]);
        let base = if code == 0 { Some(integration.to_string()) } else { current.clone() };
        let base_sha = base.as_ref().and_then(|b| {
            let (code, output, _) = git.run_git(folder, &["rev-parse", "--short", &format!("refs/heads/{b}")]);
            if code == 0 && !output.is_empty() { Some(output) } else { None }
        });
        let upstream = current.as_ref().and_then(|c| {
            let (code, output, _) = git.run_git(folder, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", &format!("{c}@{{upstream}}")]);
            if code == 0 && !output.is_empty() { Some(output) } else { None }
        });
        json!({"current_branch": current, "base_branch": base, "base_sha": base_sha, "upstream": upstream})
    }

    /// `{"setup_files", "other_changes"}` of the user's folder (the setup files the app generated vs every other changed file).
    fn classify_user_folder(&self, context: &ProjectContext, repo: &RepoRef) -> Value {
        if Path::new(&repo.loop_worktree_path()).is_dir() {
            return json!({"setup_files": [], "other_changes": []}); // the loop works there: the user's folder is not carried again
        }
        let (entry, _) = subprojects::load(context.repo_path(), Some(&repo.name));
        let setup_items = setup_files::list_setup_files(&repo.path, entry.as_ref());
        let setup_paths: Vec<String> = setup_items.iter().filter_map(|i| i["path"].as_str().map(String::from)).collect();
        json!({"setup_files": setup_paths, "other_changes": setup_files::list_other_changes(&repo.path, &setup_paths)})
    }

    /// `{"state": not_set_up | set_up | map_missing | mapping, "monorepo": bool}`: the installer's progress of ONE repository. A reference repository
    /// needs no Conformance (its map is optional: a missing one is `map_missing`, never a block). A confirmed monorepo is judged per non-skipped
    /// subproject.
    fn describe_setup_state(&self, context: &ProjectContext, repo: &RepoRef) -> Value {
        let fallback = json!({"state": SETUP_NOT_SET_UP, "monorepo": false});
        let target = if repo.primary { None } else { Some(repo.name.as_str()) };
        let Ok((key, folder)) = self.app.subprojects().resolve_target(context, target) else { return fallback };
        let entry: Option<Map<String, Value>> = match self.app.subprojects_files().read_repo_entry(&context.work_dir(), &key) {
            Read::Data(entry) => Some(entry),
            _ => None,
        };
        let monorepo = entry.as_ref().is_some_and(|e| e.get("monorepo") == Some(&Value::Bool(true)));
        if self.is_mapping_now(context, repo, target) {
            return json!({"state": SETUP_MAPPING, "monorepo": monorepo});
        }
        let needs_conformance = repo.role != project_repos::ROLE_REFERENCE;
        
        let missing_map = if monorepo {
            let entry = entry.as_ref().unwrap();
            if entry.get("confirmed") != Some(&Value::Bool(true)) {
                return json!({"state": SETUP_NOT_SET_UP, "monorepo": true});
            }
            let description = self.app.subprojects().build_description(&key, &folder, Some(entry), "");
            let rows: Vec<&Value> = description["projects"].as_array().into_iter().flatten().filter(|row| row["skip"] != true).collect();
            let row_misses = |what: &str| rows.iter().any(|row| row["missing"].as_array().is_some_and(|m| m.iter().any(|x| x == what)));
            if needs_conformance && row_misses("conformance") {
                return json!({"state": SETUP_NOT_SET_UP, "monorepo": true});
            }
            row_misses("map")
        } else {
            if needs_conformance && self.app.specs_structure().get_conformance(&folder).is_none() {
                return json!({"state": SETUP_NOT_SET_UP, "monorepo": false});
            }
            super::project_progress::ProjectProgressService::repo_has_code(&folder)
                && !super::project_progress::ProjectProgressService::read_map_facts(&folder, &folder)["map_files"].as_array().is_some_and(|f| !f.is_empty())
        };
        json!({"state": if missing_map { SETUP_MAP_MISSING } else { SETUP_SET_UP }, "monorepo": monorepo})
    }

    fn is_mapping_now(&self, context: &ProjectContext, repo: &RepoRef, target: Option<&str>) -> bool {
        if self.app.jobs().get_running_job(&repo.path).is_some_and(|job| job["type"] == MAP_JOB_TYPE) {
            return true;
        }
        self.app.subproject_maps().read_queue(context, target).is_ok_and(|queue| queue["state"] == "running")
    }

    /// Agent-context files (maps, CLAUDE.md, SYSTEM.md) that are in the user's folder but not committed: the worktree is born from the last commit and
    /// would not carry them. Only asked while the worktree is not created. Read-only; empty when git fails or nothing is pending.
    fn describe_uncommitted_context(&self, context: &ProjectContext, repo: &RepoRef) -> Vec<Value> {
        let (entry, _) = subprojects::load(context.repo_path(), Some(&repo.name)); // every repo's list is stored in the primary's SUBPROJECTS.json
        context_files::uncommitted_context(&repo.path, entry.as_ref())
    }

    /// Uncommitted work (of a worktree or of the user folder) that is not plan state (`.specs/` changes every turn and does not count).
    fn has_changes_outside_specs(&self, worktree: &str) -> bool {
        let (code, output, _) = self.app.git.run_git(worktree, &["status", "--porcelain"]);
        if code != 0 {
            return false;
        }
        pycompat::splitlines(&output).into_iter().any(|line| {
            let tail: String = line.chars().skip(3).collect();
            !to_posix(tail.trim().trim_matches('"')).starts_with(SPECS_PREFIX)
        })
    }

    /// The `--setup` text of the last recorded loop command (LOOP-CMD.json); `None` when there was none.
    fn read_saved_setup(&self, context: &ProjectContext) -> Option<String> {
        let command = self.app.loop_files.read_loop_command(&context.work_dir());
        let argv: Vec<Value> = command.get("argv").and_then(Value::as_array).cloned().unwrap_or_default();
        for (index, item) in argv.iter().enumerate() {
            if item == "--setup" && index + 1 < argv.len() {
                return Some(crate::repositories::loop_read_formatting::text_or_empty(Some(&argv[index + 1]))).filter(|s| !s.is_empty());
            }
            if let Some(rest) = item.as_str().and_then(|t| t.strip_prefix("--setup=")) {
                return Some(rest.to_string()).filter(|s| !s.is_empty());
            }
        }
        None
    }

    fn find_running_create_job(&self, context: &ProjectContext) -> Option<Value> {
        self.app.jobs().get_running_job(context.repo_path()).filter(|job| job["type"] == JOB_TYPE_WORKSPACE_CREATE)
    }

    /// Read-only job of a creation done by the LOOP at start: the step lines in the detached loop's output (`internal/monitor-relaunch.log`, appended
    /// across launches; each launch starts with a `[loop-launch]` marker line, and only the lines after the LAST marker are read; a log with no marker,
    /// from an older app, keeps the old rule: the whole tail). Only while the loop is alive and has not reached its first turn (a `[loop] turn N` line
    /// closes the creation). `None` when nothing is being created.
    fn read_loop_start_job(&self, context: &ProjectContext) -> Option<Value> {
        let alive = self.app.loop_status().is_loop_running(context) || self.app.loop_control().has_live_relaunch(&context.project.id);
        if !alive {
            return None;
        }
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        for folder in [context.work_dir(), context.repo_path().to_string()] {
            let path = loop_state::loop_path(&folder, RELAUNCH_LOG_FILE_NAME);
            if !candidates.contains(&path) {
                candidates.push(path);
            }
        }
        let existing: Vec<&std::path::PathBuf> = candidates.iter().filter(|p| p.is_file()).collect(); // the launch wrote it in the primary before the worktree held loop state
        let mtime = |p: &&std::path::PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let log_path = existing.iter().copied().max_by_key(|p| mtime(p)).unwrap_or(&candidates[0]).clone();
        let (text, started_at) = {
            use std::io::{Read as _, Seek, SeekFrom};
            let mut handle = std::fs::File::open(&log_path).ok()?;
            let size = handle.seek(SeekFrom::End(0)).ok()?;
            handle.seek(SeekFrom::Start(size.saturating_sub(START_LOG_TAIL_BYTES))).ok()?;
            let mut data = Vec::new();
            handle.read_to_end(&mut data).ok()?;
            let created = std::fs::metadata(&log_path).ok().and_then(|m| m.created().or_else(|_| m.modified()).ok())?;
            let secs = created.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
            (String::from_utf8_lossy(&data).into_owned(), engine_state::loop_io::local_from_epoch(secs).map(engine_state::loop_io::iso_seconds)?)
        };
        let repos = project_repos::load_repos(context.repo_path());
        let primary_name = project_repos::primary_of(&repos).name.clone();
        let mut lines = pycompat::splitlines(&text);
        if let Some(last_marker) = lines.iter().rposition(|l| l.starts_with(LAUNCH_MARKER_PREFIX)) {
            lines = lines[last_marker + 1..].to_vec(); // only the CURRENT launch counts: what follows the last marker line
        }
        let mut steps: Vec<Value> = Vec::new();
        for line in lines {
            if TURN_LINE.match_start(line.trim()).is_some() {
                steps.clear(); // the creation of that launch is over
                continue;
            }
            if let Some(step) = parse_step_line(line, &primary_name) {
                steps.push(json!({"at": "", "repo": step.repo, "kind": step.kind, "text": step.text}));
            }
        }
        if steps.is_empty() {
            return None;
        }
        let error = steps.iter().find(|s| s["kind"] == "error").and_then(|s| s["text"].as_str()).unwrap_or("").to_string();
        Some(json!({
            "id": LOOP_START_JOB_ID, "type": JOB_TYPE_WORKSPACE_CREATE, "state": if error.is_empty() { "running" } else { "error" },
            "started_at": started_at, "finished_at": null, "error": error, "steps": steps, "result": null,
        }))
    }

    // ------------------------------------------------ save the loop worktree (commit + push on its loop branch)
    /// `git add -A`, commit and push in the LOOP WORKTREE of a repo (its `loop/*` branch). The user's own folder is refused (409 `user_folder_refused`:
    /// it is saved with Commit setup files and push) and so is `main`/`master`: nothing of the user's is ever committed here.
    pub fn commit_and_push(&self, context: &ProjectContext, repo_name: &str, message: Option<&Value>, push: Option<&Value>) -> Result<Value, AppError> {
        let push = match push {
            None => true,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(invalid_repo("push must be true or false")),
        };
        let repo = self.app.repo_registry().resolve(context, repo_name)?;
        if self.app.loop_status().is_loop_running(context) {
            return Err(repo_conflict("a loop is running in this project: stop it first", BUSY_CODE));
        }
        if !Path::new(&repo.path).is_dir() {
            return Err(invalid_repo(format!("the repo folder does not exist: {}", repo.path)));
        }
        let worktree = repo.loop_worktree_path();
        if !Path::new(&worktree).is_dir() {
            return Err(repo_conflict(USER_FOLDER_REFUSAL, USER_FOLDER_REFUSED_CODE));
        }
        let branch = self.app.git.get_current_branch(&worktree);
        if PROTECTED_BRANCHES.contains(&branch.to_lowercase().as_str()) {
            return Err(repo_conflict(format!("`{branch}` is a protected branch: the app never commits or pushes there"), NOT_LOOP_BRANCH_CODE));
        }
        if !branch.starts_with(LOOP_BRANCH_PREFIX) {
            return Err(repo_conflict(format!("{USER_FOLDER_REFUSAL}: the worktree is not on a {LOOP_BRANCH_PREFIX}* branch (it is on `{}`)", if branch.is_empty() { "detached HEAD" } else { &branch }), NOT_LOOP_BRANCH_CODE));
        }
        let text = match message {
            None | Some(Value::Null) => Some(String::new()),
            Some(Value::String(s)) => Some(s.trim().to_string()),
            Some(_) => None,
        };
        let Some(text) = text.filter(|t| t.chars().count() <= COMMIT_MESSAGE_LIMIT && !t.contains('\n')) else {
            return Err(invalid_repo(format!("the commit message must be one line of up to {COMMIT_MESSAGE_LIMIT} characters")));
        };
        let outcome = self.app.git.commit_all_and_push(&worktree, if text.is_empty() { DEFAULT_COMMIT_MESSAGE } else { &text }, push);
        self.app.log.info("workspace_commit_push", &[("project", json!(context.project.id)), ("repo", json!(repo.name)), ("ok", outcome["ok"].clone()), ("committed", outcome["committed"].clone()), ("pushed", outcome["pushed"].clone())]);
        if outcome["ok"] != true {
            return Err(repo_conflict(outcome["error"].as_str().unwrap_or(""), COMMIT_FAILED_CODE).with_extra("committed", outcome["committed"].clone()).with_extra("branch", outcome["branch"].clone()));
        }
        Ok(json!({"ok": true, "repo": repo.name, "branch": outcome["branch"], "committed": outcome["committed"], "pushed": outcome["pushed"], "commit": outcome["commit"]}))
    }

    // ------------------------------------------------ create
    /// Starts the create job. Existing worktrees are skipped (a `reuse` step), never recreated.
    pub fn create_workspace(&self, context: &ProjectContext, repo_names: Option<&Value>) -> Result<Value, AppError> {
        let repos = project_repos::load_repos(context.repo_path());
        let targets = Self::select_targets(&repos, repo_names)?;
        if self.app.loop_status().is_loop_running(context) {
            return Err(repo_conflict("a loop is running in this project: its worktrees already exist", BUSY_CODE));
        }
        let primary = project_repos::primary_of(&repos).clone();
        let (app, task_context, task_targets) = (self.app.shared(), context.clone(), targets.clone());
        let task: super::claude_jobs::Task = Box::new(move |add_step| app.workspace().run_create_task(&task_context, &primary, &task_targets, add_step));
        let job = match self.app.jobs().start_task_job(context.repo_path(), JOB_TYPE_WORKSPACE_CREATE, task) {
            Ok(job) => job,
            Err(failure) if failure.kind == Kind::JobInProgress => return Err(repo_conflict(failure.message, BUSY_CODE)),
            Err(failure) => return Err(failure),
        };
        self.app.log.info("workspace_create_started", &[("project", json!(context.project.id)), ("repos", json!(targets.iter().map(|r| r.name.clone()).collect::<Vec<_>>().join(",")))]);
        Ok(json!({"ok": true, "job_id": job["id"]}))
    }

    pub fn get_create_job(&self, context: &ProjectContext, job_id: &str) -> Result<Value, AppError> {
        let not_found = || AppError::new(Kind::JobNotFound, "job not found");
        if job_id == LOOP_START_JOB_ID {
            return self.read_loop_start_job(context).map(|job| json!({"ok": true, "job": job})).ok_or_else(not_found);
        }
        let job = self.app.jobs().get_job(context.repo_path(), job_id)?;
        if job["type"] != JOB_TYPE_WORKSPACE_CREATE {
            return Err(not_found());
        }
        Ok(json!({"ok": true, "job": job}))
    }

    /// The repos to create: the named ones, or (empty/absent) every repo whose worktree folder does not exist yet.
    fn select_targets(repos: &[RepoRef], repo_names: Option<&Value>) -> Result<Vec<RepoRef>, AppError> {
        let names: Option<Vec<&str>> = match repo_names {
            None | Some(Value::Null) => None,
            Some(Value::Array(list)) if list.iter().all(Value::is_string) => Some(list.iter().filter_map(Value::as_str).collect()),
            Some(_) => return Err(invalid_repo("repos must be a list of repo names")),
        };
        let Some(names) = names.filter(|n| !n.is_empty()) else {
            // a reference repository never gets a worktree
            return Ok(repos.iter().filter(|r| r.role != project_repos::ROLE_REFERENCE && !Path::new(&r.loop_worktree_path()).is_dir()).cloned().collect());
        };
        let unknown: Vec<&str> = names.iter().copied().filter(|n| !repos.iter().any(|r| r.name == *n)).collect();
        if !unknown.is_empty() {
            return Err(invalid_repo(format!("unknown repo: {}", unknown.join(", "))).with_code(UNKNOWN_REPO_CODE));
        }
        let mut chosen: Vec<RepoRef> = Vec::new();
        for name in names {
            if !chosen.iter().any(|r| r.name == name) {
                chosen.push(repos.iter().find(|r| r.name == name).cloned().unwrap());
            }
        }
        Ok(chosen)
    }

    /// Job body (runs in the job thread): engine routine, steps streamed, result collected from `ready`/`reuse`/`copy` lines.
    pub fn run_create_task(&self, context: &ProjectContext, primary: &RepoRef, targets: &[RepoRef], add_step: super::claude_jobs::AddStep) -> Value {
        let git = &self.app.git;
        let created: Mutex<Map<String, Value>> = Mutex::new(Map::new());
        let to_create: Vec<&RepoRef> = targets.iter().filter(|r| !Path::new(&r.loop_worktree_path()).is_dir()).collect();
        for repo in targets.iter().filter(|r| Path::new(&r.loop_worktree_path()).is_dir()) {
            // never recreated: say so, record where it is
            add_step(json!({"at": now_iso(), "repo": repo.name, "kind": "reuse", "text": format!("worktree already exists: {}", repo.loop_worktree_path())}));
            let branch = Some(git.get_current_branch(&repo.loop_worktree_path())).filter(|b| !b.is_empty());
            let base = Some(git.get_current_branch(&repo.path)).filter(|b| !b.is_empty());
            created.lock().unwrap_or_else(|e| e.into_inner()).insert(repo.name.clone(), json!({"worktree": repo.loop_worktree_path(), "branch": branch, "base": base, "reattached": false, "copied": []}));
        }
        if to_create.is_empty() {
            return json!({"ok": true, "result": {"repos": created.into_inner().unwrap_or_default()}, "error": ""});
        }
        let output_tail: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let mut on_output_line = |line: &str| {
            {
                let mut tail = output_tail.lock().unwrap_or_else(|e| e.into_inner());
                tail.push(line.to_string());
                let excess = tail.len().saturating_sub(5);
                tail.drain(..excess);
            }
            let Some(step) = parse_step_line(line, &primary.name) else { return };
            add_step(json!({"at": now_iso(), "repo": step.repo, "kind": step.kind, "text": step.text}));
            let mut created = created.lock().unwrap_or_else(|e| e.into_inner());
            let entry = created.entry(step.repo.clone()).or_insert_with(|| json!({"worktree": null, "branch": null, "base": null, "reattached": false, "copied": []}));
            match step.kind.as_str() {
                "ready" => {
                    if let Some(ready) = READY_TEXT.match_start(&step.text) {
                        entry["worktree"] = json!(&ready[1]);
                        entry["branch"] = json!(&ready[2]);
                        entry["base"] = json!(&ready[3]);
                    }
                }
                "reattach" => entry["reattached"] = json!(true),
                "copy" => {
                    entry["copied"] = json!(step.text.split(',').map(str::trim).filter(|n| !n.is_empty() && !STANDARD_COPIED_NAMES.contains(n)).collect::<Vec<_>>());
                }
                _ => {}
            }
        };
        let names: Vec<String> = to_create.iter().map(|r| r.name.clone()).collect();
        let failure = EngineWorktreeAdapter { app: self.app }.run(&primary.path, &names, &loop_state::loop_path(context.repo_path(), CREATE_LOG_FILE_NAME), &mut on_output_line, CREATE_TIMEOUT_SECONDS);
        let created = created.into_inner().unwrap_or_default();
        let missing: Vec<&str> = to_create.iter().filter(|r| !created.get(&r.name).is_some_and(|e| engine_state::notify_truthy(e.get("worktree")))).map(|r| r.name.as_str()).collect();
        if failure.is_some() || !missing.is_empty() {
            let last_line = output_tail.lock().unwrap_or_else(|e| e.into_inner()).iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
            let reason = failure.filter(|f| !f.is_empty()).or(Some(last_line).filter(|l| !l.is_empty())).unwrap_or_else(|| "the engine did not report a worktree".into());
            let clipped: String = reason.chars().take(LOG_OUTPUT_LIMIT).collect();
            let error = if missing.is_empty() { clipped } else { format!("{clipped} (not created: {})", missing.join(", ")) };
            return json!({"ok": false, "result": {"repos": created}, "error": error});
        }
        json!({"ok": true, "result": {"repos": created}, "error": ""})
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_lines_of_the_engine_are_parsed() {
        assert_eq!(parse_step_line("[loop] [api] ready: path=C:\\w branch=loop/x base=main", "web"), Some(Step { repo: "api".into(), kind: "ready".into(), text: "path=C:\\w branch=loop/x base=main".into() }));
        assert_eq!(parse_step_line("[loop] copy: .env, CLAUDE.md", "web").unwrap().repo, "web");
        assert_eq!(parse_step_line("[loop] reusing worktree C:\\w on branch loop/x", "web").unwrap().text, "reusing worktree C:\\w on branch loop/x");
        assert_eq!(parse_step_line("[loop] worktree created: C:\\w (branch loop/x, base main; setup)", "web").unwrap().text, "path=C:\\w branch=loop/x base=main");
        assert_eq!(parse_step_line("[loop] one-time setup: npm i", "web").unwrap().kind, "setup");
        assert!(parse_step_line("random line", "web").is_none());
    }

    #[test]
    fn workspace_of_a_plain_folder_and_save_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let service = app.workspace();
        let view = service.get_workspace(&context);
        assert_eq!((view["multi"].clone(), view["repos"][0]["state"].clone(), view["repos"][0]["worktree_exists"].clone(), view["defined"].clone()), (json!(false), json!("not_created"), json!(false), json!(true)));
        assert_eq!(view["repos"][0]["setup"], json!({"state": "not_set_up", "monorepo": false}));
        assert_eq!(service.commit_and_push(&context, "proj", None, Some(&json!("yes"))).unwrap_err().message, "push must be true or false");
        assert_eq!(service.commit_and_push(&context, "proj", None, None).unwrap_err().code.as_deref(), Some("user_folder_refused"));
        assert_eq!(service.create_workspace(&context, Some(&json!([1]))).unwrap_err().message, "repos must be a list of repo names");
        assert_eq!(service.create_workspace(&context, Some(&json!(["nope"]))).unwrap_err().code.as_deref(), Some("unknown_repo"));
        assert_eq!(service.get_create_job(&context, "loop-start").unwrap_err().kind, Kind::JobNotFound);
    }

    #[test]
    fn creating_over_an_existing_worktree_only_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        std::fs::create_dir_all(dir.path().join("proj-worktree-loop")).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let repos = project_repos::load_repos(context.repo_path());
        let steps = Mutex::new(Vec::new());
        let add = |step: Value| steps.lock().unwrap().push(step);
        let outcome = app.workspace().run_create_task(&context, &repos[0], &repos, &add);
        assert_eq!(outcome["ok"], true);
        assert_eq!(steps.lock().unwrap()[0]["kind"], "reuse");
        assert_eq!(outcome["result"]["repos"]["proj"]["reattached"], false);
    }
}
