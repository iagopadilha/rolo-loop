// SPDX-License-Identifier: MIT
//! Planning readiness: the check the loop runs at start (`loop <repo> --check --planning`), run in the background while the user plans, cached in
//! `.specs/project/loop/internal/planning/readiness.json`.
//!
//! `{state: ready|problems|stale|running|error, at, problems:[...], warnings:[...], uncommitted:[...], fingerprint, error?}`
//!
//! `uncommitted` (RD-01): one block per repo with uncommitted changes, `{kind: "uncommitted", repo, setup_files:[paths], other_changes:[paths]}`; the
//! check's text line for it is not repeated in `warnings`.
//!
//! One run at a time per project (a second request waits for the first), 120 s timeout, never on the 2 s poll: `get` only reads the cache. A cached
//! `ready`/`problems` whose fingerprint (mtimes of ROADMAP, REPOS.json, TEST-ENVIRONMENT, .env.test, the machine `.env`, the toolkit and user shared
//! `.env` and `~/.claude.json`) no longer matches is reported as `stale`. A check that could not run is `error` ("readiness check did not run"),
//! which keeps the approval blocked.

use super::loop_control::parse_check_output;
use super::App;
use crate::domain::project::ProjectContext;
use crate::infrastructure::claude_client::redact_secrets;
use crate::infrastructure::process_runner::{START_FAILURE_CODE, TIMEOUT_CODE};
use crate::repositories::loop_command_file::now_iso;
use engine_state::{context_files, credential_check, loop_state, project_repos, readiness_checks, subprojects};
use pycompat::json::py_json_dumps_indent;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub const STATE_READY: &str = "ready";
pub const STATE_PROBLEMS: &str = "problems";
pub const STATE_STALE: &str = "stale";
pub const STATE_RUNNING: &str = "running";
pub const STATE_ERROR: &str = "error";
/// The engine's text line of a block (`setup_files::block_text`).
const UNCOMMITTED_WARNING_PREFIX: &str = "uncommitted changes in ";
const READINESS_FILE_PARTS: [&str; 2] = ["planning", "readiness.json"];
pub const CHECK_TIMEOUT_SECONDS: u64 = 120;
const MESSAGE_LIMIT: usize = 300;
const FINGERPRINT_RELATIVE_PATHS: [&str; 4] =
    [".specs/project/ROADMAP.md", ".specs/project/REPOS.json", ".specs/project/TEST-ENVIRONMENT.md", ".specs/project/.env.test"];
const UNCOMMITTED_FIX: &str = "Commit these files (the worktree is born from the last commit)";
const JSON_LINE_PREFIX: &str = "[check] JSON: ";

/// Readiness checks scheduled or running per work dir, and one run lock per work dir.
#[derive(Default)]
pub struct ReadinessState {
    pending: Mutex<BTreeMap<String, i64>>,
    run_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

fn list_of(value: Option<&Value>) -> Vec<Value> {
    value.and_then(Value::as_array).cloned().unwrap_or_default()
}

/// RDY-02: merges every `uncommitted` item of the same repo into one (files deduplicated, order kept); other items pass through in order.
pub fn group_items(items: &[Value]) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    let mut grouped: Vec<(Value, usize)> = Vec::new();
    for item in items {
        if !item.is_object() || item.get("kind").and_then(Value::as_str) != Some("uncommitted") {
            result.push(item.clone());
            continue;
        }
        let repo = item.get("repo").cloned().unwrap_or(Value::Null);
        if let Some((_, position)) = grouped.iter().find(|(r, _)| *r == repo) {
            let mut files = list_of(result[*position].get("files"));
            files.extend(list_of(item.get("files")));
            let mut seen: Vec<Value> = Vec::new();
            for file in files {
                if !seen.contains(&file) {
                    seen.push(file);
                }
            }
            result[*position]["files"] = json!(seen);
            continue;
        }
        let mut unique: Vec<Value> = Vec::new();
        for file in list_of(item.get("files")) {
            if !unique.contains(&file) {
                unique.push(file);
            }
        }
        let mut merged = item.clone();
        merged["files"] = json!(unique);
        grouped.push((repo, result.len()));
        result.push(merged);
    }
    result
}

/// The `items` of the check's `[check] JSON: {...}` line; `None` when the line is missing or unreadable (the caller falls back to text).
pub fn parse_json_items(output: &str) -> Option<Vec<Value>> {
    for line in pycompat::splitlines(output).into_iter().rev() {
        if let Some(rest) = line.strip_prefix(JSON_LINE_PREFIX) {
            let items = serde_json::from_str::<Value>(rest).ok()?.get("items").cloned()?;
            return items.as_array().map(|a| a.iter().filter(|i| i.is_object()).cloned().collect());
        }
    }
    None
}

/// The panel's `items`: one grouped `uncommitted` item per repo (from the structured blocks), then every other line as `other`.
pub fn build_items(problems: &[Value], warnings: &[Value], uncommitted: &[Value]) -> Vec<Value> {
    let mut items: Vec<Value> = uncommitted
        .iter()
        .map(|block| {
            let mut files = list_of(block.get("setup_files"));
            files.extend(list_of(block.get("other_changes")));
            json!({"kind": "uncommitted", "level": "warning", "repo": block.get("repo").cloned().unwrap_or(Value::Null), "files": files,
                   "text": format!("uncommitted changes in {}", text_of(block.get("repo"))), "fix": UNCOMMITTED_FIX})
        })
        .collect();
    items.extend(problems.iter().map(|t| json!({"kind": "other", "level": "problem", "repo": null, "files": [], "text": t, "fix": ""})));
    items.extend(warnings.iter().map(|t| json!({"kind": "other", "level": "warning", "repo": null, "files": [], "text": t, "fix": ""})));
    group_items(&items)
}

fn modified_nanos(path: &Path) -> Option<u128> {
    std::fs::metadata(path).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_nanos())
}

pub struct ReadinessService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn readiness(&self) -> ReadinessService<'_> {
        ReadinessService { app: self }
    }

    /// Starts a background readiness check (waits behind a running one). Returns the thread (tests join it).
    pub fn readiness_schedule(&self, context: &ProjectContext) -> Option<JoinHandle<()>> {
        self.readiness().schedule(context)
    }
}

impl ReadinessService<'_> {
    /// Whether the approval depends on this check (looked up on every call: the gate `ready` exists only while it does).
    pub fn enforced(&self) -> bool {
        self.app.config.planning_readiness_enforced
    }

    // ------------------------------------------------ paths and fingerprint
    fn key_of(context: &ProjectContext) -> String {
        pycompat::nt_normcase(&pycompat::nt_abspath(&context.work_dir()))
    }

    fn cache_path(context: &ProjectContext) -> PathBuf {
        READINESS_FILE_PARTS.iter().fold(loop_state::internal_dir(&context.work_dir()), |a, p| a.join(p))
    }

    /// Machine-level files that change what the check finds: machine `.env`, the user shared `.env` (only when configured), `~/.claude.json` (MCP).
    fn default_machine_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![self.app.env.home_dir().join(".claude.json"), self.app.config.user_data_dir.join(".env")];
        let shared = credential_check::user_shared_env_path(&self.app.env);
        if !shared.is_empty() {
            paths.push(PathBuf::from(shared));
        }
        paths
    }

    pub fn compute_fingerprint(&self, context: &ProjectContext) -> String {
        let work = context.work_dir();
        let mut paths: Vec<PathBuf> = FINGERPRINT_RELATIVE_PATHS.iter().map(|r| r.split('/').fold(PathBuf::from(&work), |a, p| a.join(p))).collect();
        paths.extend(self.default_machine_paths());
        paths.extend(Self::subproject_paths(context));
        let mut parts: Vec<String> = paths
            .iter()
            .map(|path| {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                match modified_nanos(path) {
                    Some(nanos) => format!("{name}={nanos}"),
                    None => format!("{name}=-"),
                }
            })
            .collect();
        parts.push(format!("context={}", Self::context_files_signature(context)));
        parts.join("|")
    }

    /// SUBPROJECTS.json, the subprojects' CLAUDE.md files and the code inventories: what the subprojects checks read.
    fn subproject_paths(context: &ProjectContext) -> Vec<PathBuf> {
        let root = context.work_dir();
        let mut paths = vec![Path::new(&root).join(".specs").join("project").join("SUBPROJECTS.json")];
        let (entry, _) = subprojects::load(&root, None);
        for project in subprojects::entry_projects(entry.as_ref()) {
            let base = subprojects::normalize(&text_of(project.get("path")));
            paths.push(base.split('/').fold(PathBuf::from(&root), |a, p| a.join(p)).join("CLAUDE.md"));
        }
        let inventory = Path::new(&root).join(".specs").join("project").join("loop").join("internal").join("inventory");
        if let Ok(entries) = std::fs::read_dir(&inventory) {
            let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".json")).collect();
            names.sort();
            paths.extend(names.into_iter().map(|n| inventory.join(n)));
        }
        paths
    }

    /// The uncommitted agent-context files (git porcelain paths and states): committing one makes the warning stale.
    fn context_files_signature(context: &ProjectContext) -> String {
        let items = context_files::uncommitted_context(context.repo_path(), None);
        let joined = items.iter().map(|item| format!("{}:{}", text_of(item.get("path")), text_of(item.get("state")))).collect::<Vec<_>>().join(";");
        if joined.is_empty() {
            "-".into()
        } else {
            joined
        }
    }

    // ------------------------------------------------ read
    /// The cached result with the live state: running while a check is scheduled, stale when the fingerprint moved.
    pub fn get(&self, context: &ProjectContext) -> Value {
        let mut cached = self.read_cache(context);
        let running = self.app.readiness.pending.lock().unwrap_or_else(|e| e.into_inner()).get(&Self::key_of(context)).copied().unwrap_or(0) > 0;
        if running {
            cached["state"] = json!(STATE_RUNNING);
            return cached;
        }
        let state = text_of(cached.get("state"));
        if (state == STATE_READY || state == STATE_PROBLEMS) && text_of(cached.get("fingerprint")) != self.compute_fingerprint(context) {
            cached["state"] = json!(STATE_STALE);
        }
        cached
    }

    pub fn read_cache(&self, context: &ProjectContext) -> Value {
        let data = std::fs::read(Self::cache_path(context)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).unwrap_or(Value::Null);
        let state = text_of(data.get("state"));
        if !data.is_object() || ![STATE_READY, STATE_PROBLEMS, STATE_ERROR].contains(&state.as_str()) {
            return json!({"state": STATE_STALE, "at": null, "problems": [], "warnings": [], "uncommitted": [], "fingerprint": ""});
        }
        let (problems, warnings, uncommitted) = (list_of(data.get("problems")), list_of(data.get("warnings")), list_of(data.get("uncommitted")));
        let items = match data.get("items").and_then(Value::as_array) {
            Some(list) => group_items(list),
            None => build_items(&problems, &warnings, &uncommitted),
        };
        json!({"state": state, "at": data.get("at").cloned().unwrap_or(Value::Null), "problems": problems, "warnings": warnings, "uncommitted": uncommitted,
               "items": items, "fingerprint": text_of(data.get("fingerprint")), "error": text_of(data.get("error"))})
    }

    // ------------------------------------------------ run
    /// Starts a background check (waits behind a running one). Returns the thread (tests join it).
    pub fn schedule(&self, context: &ProjectContext) -> Option<JoinHandle<()>> {
        let key = Self::key_of(context);
        let run_lock = {
            *self.app.readiness.pending.lock().unwrap_or_else(|e| e.into_inner()).entry(key.clone()).or_insert(0) += 1;
            self.app.readiness.run_locks.lock().unwrap_or_else(|e| e.into_inner()).entry(key.clone()).or_default().clone()
        };
        let (app, context, error_key) = (self.app.shared(), context.clone(), key.clone());
        let tail: String = key.chars().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect();
        let spawned = std::thread::Builder::new().name(format!("readiness-{tail}")).spawn(move || app.readiness().run_in_thread(&context, &key, &run_lock));
        match spawned {
            Ok(handle) => Some(handle),
            Err(_) => {
                let mut pending = self.app.readiness.pending.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(count) = pending.get_mut(&error_key) {
                    *count = (*count - 1).max(0);
                }
                None
            }
        }
    }

    pub fn run(&self, context: &ProjectContext, wait: bool) -> Value {
        let handle = self.schedule(context);
        if wait {
            if let Some(handle) = handle {
                let _ = handle.join();
            }
        }
        self.get(context)
    }

    fn run_in_thread(&self, context: &ProjectContext, key: &str, run_lock: &Arc<Mutex<()>>) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = run_lock.lock().unwrap_or_else(|e| e.into_inner());
            let result = self.execute_check(context);
            let path = Self::cache_path(context);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(failure) = std::fs::write(&path, py_json_dumps_indent(&result, false, 2)) {
                self.app.log.warn("readiness_cache_not_written", &[("message", json!(failure.to_string().chars().take(200).collect::<String>()))]);
            }
        }));
        if outcome.is_err() {
            // the thread must never die silently with the item stuck on running
            self.app.log.warn("readiness_check_crashed", &[("message", json!("the check panicked"))]);
        }
        let mut pending = self.app.readiness.pending.lock().unwrap_or_else(|e| e.into_inner());
        let count = pending.get(key).copied().unwrap_or(1);
        pending.insert(key.to_string(), (count - 1).max(0));
    }

    fn execute_check(&self, context: &ProjectContext) -> Value {
        let fingerprint = self.compute_fingerprint(context); // before the run: a change during it leaves the result stale
        let mut argv = self.app.engine_argv("run_loop");
        argv.extend([context.repo_path().to_string(), "--check".into(), "--planning".into(), "--json".into()]);
        let (code, output, error) = self.app.processes.run_and_wait(&argv, context.repo_path(), CHECK_TIMEOUT_SECONDS, None, &[], None);
        if code == TIMEOUT_CODE {
            return Self::build_error(&format!("the check took longer than {CHECK_TIMEOUT_SECONDS} s"), &fingerprint);
        }
        if code == START_FAILURE_CODE && output.is_empty() {
            return Self::build_error(&error, &fingerprint);
        }
        let parsed = parse_check_output(&output);
        if parsed["ready"] != true && parsed["problems"].as_array().is_some_and(|p| p.is_empty()) {
            // no verdict at all: the script failed before checking (bad flag, crash)
            let source = if error.is_empty() { &output } else { &error };
            let last = pycompat::splitlines(source.trim()).last().map(|l| l.to_string());
            return Self::build_error(&last.unwrap_or_else(|| format!("no READY or PROBLEM line (code {code})")), &fingerprint);
        }
        let problems: Vec<String> = list_of(parsed.get("problems")).iter().map(|i| redact_secrets(i.as_str().unwrap_or(""), Some(MESSAGE_LIMIT))).collect();
        let warnings: Vec<String> = list_of(parsed.get("warnings"))
            .iter()
            .filter(|i| !i.as_str().unwrap_or("").starts_with(UNCOMMITTED_WARNING_PREFIX))
            .map(|i| redact_secrets(i.as_str().unwrap_or(""), Some(MESSAGE_LIMIT)))
            .collect();
        let mut result = json!({"state": if problems.is_empty() { STATE_READY } else { STATE_PROBLEMS }, "at": now_iso(), "problems": problems, "warnings": warnings,
                                "uncommitted": Self::read_uncommitted_blocks(context), "fingerprint": fingerprint, "error": ""});
        if let Some(items) = parse_json_items(&output) {
            result["items"] = json!(group_items(&items));
        }
        result
    }

    /// The structured twin of the engine's uncommitted line: one `{kind, repo, setup_files, other_changes}` per repo (`[]` on any failure).
    fn read_uncommitted_blocks(context: &ProjectContext) -> Vec<Value> {
        let repos = project_repos::load_repos(context.repo_path());
        readiness_checks::uncommitted_blocks(&repos, context.repo_path())
            .into_iter()
            .map(|mut block| {
                block["kind"] = json!("uncommitted");
                block
            })
            .collect()
    }

    fn build_error(detail: &str, fingerprint: &str) -> Value {
        json!({"state": STATE_ERROR, "at": now_iso(), "problems": [], "warnings": [], "uncommitted": [], "fingerprint": fingerprint, "error": redact_secrets(detail, Some(MESSAGE_LIMIT))})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::project::Project;
    use crate::infrastructure::process_runner::ProcessRunner;

    struct Scripted(Mutex<(i32, String, String)>);

    impl ProcessRunner for Scripted {
        fn run_and_wait(&self, _: &[String], _: &str, _: u64, _: Option<&str>, _: &[(String, String)], _: Option<&[(String, String)]>) -> (i32, String, String) {
            self.0.lock().unwrap().clone()
        }
        fn spawn_detached(&self, _: &[String], _: &str, _: &str, _: &[(String, String)]) -> std::io::Result<i64> {
            Ok(1)
        }
        fn is_alive(&self, _: i64) -> bool {
            false
        }
        fn kill_tree(&self, _: i64) {}
    }

    fn setup(dir: &Path, reply: (i32, &str, &str)) -> (Arc<App>, ProjectContext) {
        let app = App::for_tests_with(dir, Arc::new(Scripted(Mutex::new((reply.0, reply.1.to_string(), reply.2.to_string())))));
        let repo = dir.join("proj");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        std::fs::write(repo.join(".specs/project/ROADMAP.md"), "x").unwrap();
        let context = ProjectContext::new(Project {
            id: "p".into(),
            name: "proj".into(),
            repo_path: repo.display().to_string(),
            opened_at: String::new(),
            last_opened_at: String::new(),
            favorite: false,
        });
        (app, context)
    }

    #[test]
    fn items_are_grouped_per_repo_and_text_lines_become_other_items() {
        let items = vec![
            json!({"kind": "uncommitted", "repo": "a", "files": ["1", "2"]}),
            json!({"kind": "other", "text": "t"}),
            json!({"kind": "uncommitted", "repo": "a", "files": ["2", "3"]}),
            json!({"kind": "uncommitted", "repo": "b", "files": ["9", "9"]}),
        ];
        let grouped = group_items(&items);
        assert_eq!(grouped.len(), 3);
        assert_eq!((grouped[0]["files"].clone(), grouped[2]["files"].clone()), (json!(["1", "2", "3"]), json!(["9"])));
        let built = build_items(&[json!("p")], &[json!("w")], &[json!({"repo": "r", "setup_files": ["s"], "other_changes": ["o"]})]);
        assert_eq!(
            (built[0]["files"].clone(), built[0]["text"].clone(), built[1]["level"].clone(), built[2]["level"].clone()),
            (json!(["s", "o"]), json!("uncommitted changes in r"), json!("problem"), json!("warning"))
        );
        assert_eq!(parse_json_items("x\n[check] JSON: {\"items\": [{\"a\": 1}, 5]}\n"), Some(vec![json!({"a": 1})]));
        assert_eq!(parse_json_items("[check] JSON: nope"), None);
        assert_eq!(parse_json_items("nothing"), None);
    }

    #[test]
    fn a_check_is_cached_goes_stale_with_the_fingerprint_and_reports_failures() {
        let dir = tempfile::tempdir().unwrap();
        let (app, context) = setup(dir.path(), (0, "[check] WARNING: slow\n[check] WARNING: uncommitted changes in x\n[check] READY. Command: go\n", ""));
        let service = app.readiness();
        assert_eq!(service.get(&context)["state"], "stale");
        let result = service.run(&context, true);
        assert_eq!((result["state"].clone(), result["warnings"].clone(), result["error"].clone()), (json!("ready"), json!(["slow"]), json!("")));
        assert!(Path::new(&context.work_dir()).join(".specs/project/loop/internal/planning/readiness.json").is_file());
        std::fs::write(Path::new(&context.work_dir()).join(".specs/project/ROADMAP.md"), "changed later").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let touched = Path::new(&context.work_dir()).join(".specs/project/REPOS.json");
        std::fs::write(&touched, "{}").unwrap();
        assert_eq!(service.get(&context)["state"], "stale");
        let (app, context) = setup(dir.path().join("two").as_path(), (0, "[check] PROBLEM: no git\n", ""));
        assert_eq!(app.readiness().run(&context, true)["problems"], json!(["no git"]));
        let (app, context) = setup(dir.path().join("three").as_path(), (1, "", "crash: boom"));
        let failed = app.readiness().run(&context, true);
        assert_eq!((failed["state"].clone(), failed["error"].clone()), (json!("error"), json!("crash: boom")));
        let (app, context) = setup(dir.path().join("four").as_path(), (TIMEOUT_CODE, "", ""));
        assert_eq!(app.readiness().run(&context, true)["error"], "the check took longer than 120 s");
        assert!(app.readiness().enforced());
    }
}
