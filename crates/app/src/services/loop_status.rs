// SPDX-License-Identifier: MIT
//! Loop status of a project and app occupancy (a loop running in some project).
//!
//! Derivation rules (docs/API.md): `LOOP.lock` with a live PID = `running`; no lock and LOOP.json at `continue` = `stopped` (the script died), or
//! `error` when `meta.json` recorded an error stop after LOOP.json; no LOOP.json = `no_loop`.

use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::{Project, ProjectContext};
use engine_state::{loop_state, project_repos};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

pub const STATUS_RUNNING: &str = "running";
pub const STATUS_STOPPED: &str = "stopped";
pub const STATUS_ERROR: &str = "error";
pub const STATUS_NO_LOOP: &str = "no_loop";
pub const STATUS_CONTINUE: &str = "continue";
const STOPPED_BY_ERROR: &str = "error";
pub const STATUS_AWAITING_USER: &str = "awaiting_user";
pub const REPO_IN_USE_CODE: &str = "repo_in_use";
static STAGE_HEADING: LazyLock<Re> = LazyLock::new(|| Re::new(r"^#{1,6}\s+"));

pub fn normalize_repo_path(path: &str) -> String {
    pycompat::nt_normcase(&pycompat::nt_realpath(&pycompat::nt_abspath(path)))
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

/// A list of non-blank texts; a non-blank string is a one-item list.
pub fn to_text_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().map(|i| text_of(Some(i))).filter(|s| !s.trim().is_empty()).collect(),
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.clone()],
        _ => vec![],
    }
}

/// `dict.setdefault` on an insertion-ordered list.
fn set_default(map: &mut Vec<(String, String)>, key: String, value: String) {
    if !map.iter().any(|(k, _)| *k == key) {
        map.push((key, value));
    }
}

/// `{normalized path: repo name}` of the listed repos (a later duplicate path overwrites the name, keeping its place) plus the repo itself.
fn repo_map(context: &ProjectContext) -> Vec<(String, String)> {
    let mut map: Vec<(String, String)> = Vec::new();
    for repo in project_repos::load_repos(context.repo_path()) {
        let key = normalize_repo_path(&repo.path);
        match map.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = repo.name,
            None => map.push((key, repo.name)),
        }
    }
    set_default(&mut map, normalize_repo_path(context.repo_path()), pycompat::last_segment(&pycompat::nt_normpath(context.repo_path())).to_string());
    map
}

fn is_number(value: Option<&Value>) -> Option<f64> {
    value.filter(|v| v.is_number()).and_then(Value::as_f64)
}

pub struct LoopStatusService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn loop_status(&self) -> LoopStatusService<'_> {
        LoopStatusService { app: self }
    }
}

impl LoopStatusService<'_> {
    /// `{"status","running","pid","json","meta","reason"}`.
    pub fn get_loop_summary(&self, context: &ProjectContext) -> Value {
        let work_dir = context.work_dir();
        let files = &self.app.loop_files;
        let loop_json = files.read_loop_json(&work_dir);
        let meta = files.read_meta(&work_dir);
        let pid = files.read_lock_pid(&work_dir);
        let running = files.is_process_alive(pid);
        let mut status: Option<String> = loop_json.get("status").and_then(Value::as_str).filter(|s| loop_state::LOOP_STATUS.contains(s)).map(String::from);
        let mut reason = loop_json.get("reason").cloned().unwrap_or(Value::Null);
        if running {
            status = Some(STATUS_RUNNING.into());
        } else if status.as_deref() == Some(STATUS_CONTINUE) {
            status = Some(STATUS_STOPPED.into()); // the script died in the middle of the loop
                                                  // finish(error) writes only meta.json; when it is newer than LOOP.json, it says why the loop stopped
            if meta.get("stopped").and_then(Value::as_str) == Some(STOPPED_BY_ERROR) && text_of(meta.get("finished_at")) >= text_of(loop_json.get("at")) {
                status = Some(STATUS_ERROR.into());
                reason = meta.get("reason").cloned().unwrap_or(Value::Null);
            }
        } else if status.is_none() {
            status = Some(STATUS_NO_LOOP.into());
        }
        json!({"status": status, "running": running, "pid": if running { json!(pid) } else { Value::Null }, "json": loop_json, "meta": meta, "reason": reason})
    }

    /// Only the lock (cheap): used by the occupancy sweep.
    pub fn is_loop_running(&self, context: &ProjectContext) -> bool {
        let pid = self.app.loop_files.read_lock_pid(&context.work_dir());
        self.app.loop_files.is_process_alive(pid)
    }

    pub fn is_any_loop_running(&self) -> bool {
        let service = self.app.project_service();
        for project in service.list_projects() {
            let Ok(context) = service.get_project_context(&project.id) else { continue };
            if self.is_loop_running(&context) {
                return true;
            }
        }
        false
    }

    // ------------------------------------------------ repos in use (several projects, several repos)
    /// `{normalized path: repo name}` of everything this project's loop uses or listed: its repos and `meta.repos`.
    pub fn list_used_repo_paths(&self, context: &ProjectContext) -> Vec<(String, String)> {
        let mut used = repo_map(context);
        if let Some(Value::Object(repos)) = self.app.loop_files.read_meta(&context.work_dir()).get("repos") {
            for (name, item) in repos {
                if let Some(path) = item.get("path").filter(|p| engine_state::notify_truthy(Some(p))) {
                    set_default(&mut used, normalize_repo_path(&text_of(Some(path))), name.clone());
                }
            }
        }
        used
    }

    /// `(repo name, other project)` of the first repo of this project that another RUNNING loop uses; `None` when free.
    pub fn find_repo_in_use(&self, context: &ProjectContext) -> Option<(String, Project)> {
        let mine = repo_map(context);
        let service = self.app.project_service();
        for project in service.list_projects() {
            if project.id == context.project.id {
                continue;
            }
            let Ok(other) = service.get_project_context(&project.id) else { continue };
            if !self.is_loop_running(&other) {
                continue;
            }
            for (path, _) in self.list_used_repo_paths(&other) {
                if let Some((_, name)) = mine.iter().find(|(p, _)| *p == path) {
                    return Some((name.clone(), project));
                }
            }
        }
        None
    }

    pub fn ensure_repos_free(&self, context: &ProjectContext) -> Result<(), AppError> {
        let Some((repo_name, project)) = self.find_repo_in_use(context) else { return Ok(()) };
        Err(AppError::new(Kind::RepoInUse, format!("Repo {repo_name} is in use by the running loop of project '{}'.", project.name))
            .with_code(REPO_IN_USE_CODE)
            .with_extra("repo", json!(repo_name))
            .with_extra("project", json!({"id": project.id, "name": project.name})))
    }

    // ------------------------------------------------ overview of the running loops
    pub fn list_running_loops(&self) -> Vec<Value> {
        let service = self.app.project_service();
        let mut loops = Vec::new();
        for project in service.list_projects() {
            let Ok(context) = service.get_project_context(&project.id) else { continue };
            if self.is_loop_running(&context) {
                loops.push(self.build_running_loop_item(&project, &context));
            }
        }
        loops
    }

    fn build_running_loop_item(&self, project: &Project, context: &ProjectContext) -> Value {
        let work_dir = context.work_dir();
        let loop_json = self.app.loop_files.read_loop_json(&work_dir);
        let meta = self.app.loop_files.read_meta(&work_dir);
        let awaiting = loop_json.get("status").and_then(Value::as_str) == Some(STATUS_AWAITING_USER);
        let features = to_text_list(loop_json.get("feature"));
        let repo_names: Vec<String> = match meta.get("repos") {
            Some(Value::Object(repos)) if !repos.is_empty() => repos.keys().cloned().collect(),
            _ => project_repos::load_repos(context.repo_path()).into_iter().take(1).map(|r| r.name).collect(),
        };
        let question = if awaiting {
            json!({"reason": loop_json.get("reason").cloned().unwrap_or(Value::Null), "options": to_text_list(loop_json.get("options")), "since": loop_json.get("at").cloned().unwrap_or(Value::Null)})
        } else {
            Value::Null
        };
        let cost = meta.get("cost_usd").filter(|v| v.is_number()).cloned().unwrap_or(Value::Null);
        json!({
            "project_id": project.id, "name": project.name, "url": format!("/p/{}/", project.id), "running": true,
            "status": if awaiting { STATUS_AWAITING_USER } else { STATUS_RUNNING }, "stage": self.describe_stage(&work_dir, &features),
            "feature": features, "turn": self.compute_running_turn(&work_dir, &loop_json, &meta), "cost_usd": cost, "repos": repo_names, "question": question,
        })
    }

    /// `Stage 2: API` of the first running feature (its ROADMAP stage label + the matching `## Stage` title); `None` when unknown.
    pub fn describe_stage(&self, work_dir: &str, features: &[String]) -> Value {
        let text = self.app.loop_files.read_roadmap(work_dir);
        let (parsed, _) = loop_state::parse_roadmap(&text);
        let label = features.iter().find_map(|slug| parsed.get(slug).map(|f| f.text("stage").to_string()).filter(|s| !s.is_empty())).unwrap_or_default();
        if label.is_empty() {
            return Value::Null;
        }
        let pattern = Re::ci(&format!(r"^#{{1,6}}\s+(Stage\s+{}\b.*?)\s*#*\s*\z", pycompat::escape(&label)));
        for line in pycompat::splitlines(&text) {
            if let Some(found) = pattern.match_start(line.trim()) {
                return json!(found[1].trim());
            }
        }
        let _ = &STAGE_HEADING;
        json!(format!("Stage {label}"))
    }

    /// The turn in progress: the next one after the last one done (same rule as the panel while running).
    pub fn compute_running_turn(&self, work_dir: &str, loop_json: &Map<String, Value>, meta: &Map<String, Value>) -> i64 {
        let numbers = self.app.loop_files.list_turn_numbers(work_dir);
        let mut best: f64 = 0.0;
        for value in [meta.get("turns"), loop_json.get("turn")] {
            if let Some(n) = is_number(value) {
                best = best.max(n);
            }
        }
        if let Some(last) = numbers.last() {
            best = best.max(*last as f64);
        }
        best as i64 + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(app: &App, dir: &std::path::Path, name: &str) -> ProjectContext {
        let repo = dir.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        app.project_service().open_existing_project(&repo.display().to_string()).unwrap();
        app.project_service().get_project_context(&crate::repositories::app_config::compute_project_id(&repo.display().to_string())).unwrap()
    }

    #[test]
    fn status_follows_the_lock_loop_json_and_meta() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let ctx = context(&app, dir.path(), "app");
        let service = app.loop_status();
        let work = ctx.work_dir();
        assert_eq!(service.get_loop_summary(&ctx)["status"], "no_loop");
        let loop_json = loop_state::state_file(&work, "LOOP.json");
        std::fs::create_dir_all(loop_json.parent().unwrap()).unwrap();
        std::fs::write(&loop_json, r#"{"status": "continue", "at": "2026-01-01T10:00:00", "reason": "r"}"#).unwrap();
        assert_eq!(service.get_loop_summary(&ctx)["status"], "stopped");
        let meta = loop_state::loop_path(&work, "meta.json");
        std::fs::write(&meta, r#"{"stopped": "error", "finished_at": "2026-01-01T11:00:00", "reason": "boom"}"#).unwrap();
        let summary = service.get_loop_summary(&ctx);
        assert_eq!((summary["status"].clone(), summary["reason"].clone(), summary["running"].clone()), (json!("error"), json!("boom"), json!(false)));
        std::fs::write(&loop_json, r#"{"status": "done"}"#).unwrap();
        assert_eq!(service.get_loop_summary(&ctx)["status"], "done");
        assert!(!service.is_loop_running(&ctx) && !service.is_any_loop_running() && service.list_running_loops().is_empty());
        assert!(service.ensure_repos_free(&ctx).is_ok());
    }

    #[test]
    fn helpers_for_lists_stage_and_turn() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let ctx = context(&app, dir.path(), "app");
        let service = app.loop_status();
        assert_eq!(to_text_list(Some(&json!(["a", " ", 3]))), vec!["a", "3"]);
        assert_eq!(to_text_list(Some(&json!("x"))), vec!["x"]);
        assert!(to_text_list(Some(&json!(" "))).is_empty());
        let work = ctx.work_dir();
        let roadmap = std::path::Path::new(&work).join(".specs/project/ROADMAP.md");
        std::fs::create_dir_all(roadmap.parent().unwrap()).unwrap();
        std::fs::write(
            &roadmap,
            "## Stage 2: API ##\n\n- [a] status=building title=\"A\" scope=Small stage=2 paths=src\n- [b] status=pending title=\"B\" scope=Small paths=src2\n",
        )
        .unwrap();
        assert_eq!(service.describe_stage(&work, &["a".to_string()]), json!("Stage 2: API"));
        assert_eq!(service.describe_stage(&work, &["b".to_string()]), json!("Stage 2:")); // the heading label "2:" has no word boundary, so only the generic title is built
        assert_eq!(service.describe_stage(&work, &["zzz".to_string()]), Value::Null);
        let turns = loop_state::turns_dir(&work);
        std::fs::create_dir_all(&turns).unwrap();
        std::fs::write(turns.join("004.json"), "{}").unwrap();
        let loop_json = json!({"turn": 3}).as_object().unwrap().clone();
        assert_eq!(service.compute_running_turn(&work, &loop_json, &Map::new()), 5);
        assert_eq!(service.compute_running_turn(&work, &loop_json, json!({"turns": 9.0}).as_object().unwrap()), 10);
    }
}
