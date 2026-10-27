// SPDX-License-Identifier: MIT
//! Planning mode rule: a project whose roadmap was never approved AND that never ran a loop opens in planning mode.
//!
//! While it is on, the loop commands, the chat, the Act mode, the whiteboard writes and the one-shot roadmap jobs are refused with 409
//! `code:"planning_required"` (reads, structure, conformance, the map job, the planning endpoints and the approval stay open). A project that
//! already ran loops and lost the approval (rows changed) keeps the old behaviour (banner).
//!
//! Reopen planning (`POST planning/reopen`) writes a marker (`PLANNING-REOPENED.json`) next to the approval: while the roadmap is not approved AND
//! the marker exists the project is in planning mode even though it ran loops before. The roadmap approval removes the marker again.
//!
//! "Ran a loop" = any trace the engine leaves: the loop worktree (or a project opened on it), LOOP.json with a status, `meta.json` with turns, a turn
//! file or a live `LOOP.lock`. Everything is read from disk on each call (small files).

use super::App;
use crate::domain::errors::{AppError, Kind, PLANNING_REQUIRED_CODE};
use crate::domain::project::{ProjectContext, LOOP_WORKTREE_SUFFIX};
use engine_state::loop_state;
use std::path::Path;

/// Lives in `project/loop/json/` (the `loop_path` rule for `*.json`).
pub const REOPENED_MARKER_NAME: &str = "PLANNING-REOPENED.json";
pub const PLANNING_REQUIRED_MESSAGE: &str =
    "planning mode: approve the roadmap in the planning screen first (the loop, the chat and the actions open after the approval)";

pub struct PlanningRequirementService<'a> {
    pub app: &'a App,
}

impl PlanningRequirementService<'_> {
    pub fn is_planning_required(&self, context: &ProjectContext) -> bool {
        if loop_state::roadmap_approval(&context.work_dir())["approved"] == true {
            return false;
        }
        self.is_reopened(context) || !self.has_ever_run_loop(context)
    }

    // ------------------------------------------------ reopened planning marker
    pub fn is_reopened(&self, context: &ProjectContext) -> bool {
        loop_state::loop_path(&context.work_dir(), REOPENED_MARKER_NAME).is_file()
    }

    /// Marker in the current work dir, the repo and any extra folder (a worktree that could not be removed).
    pub fn mark_reopened(&self, context: &ProjectContext, now_iso: &str, extra_work_dirs: &[String]) {
        let mut folders: Vec<String> = Vec::new();
        for folder in [context.work_dir(), context.repo_path().to_string()].into_iter().chain(extra_work_dirs.iter().cloned()) {
            if !folders.contains(&folder) {
                folders.push(folder);
            }
        }
        for folder in folders {
            if !Path::new(&folder).is_dir() {
                continue; // a removed worktree is never recreated by the marker
            }
            let path = loop_state::loop_path(&folder, REOPENED_MARKER_NAME);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, pycompat::json::py_json_dumps(&serde_json::json!({"at": now_iso}), true));
        }
    }

    pub fn clear_reopened(&self, context: &ProjectContext) {
        for folder in [context.work_dir(), context.repo_path().to_string()] {
            let _ = std::fs::remove_file(loop_state::loop_path(&folder, REOPENED_MARKER_NAME));
        }
    }

    /// Errors while the project is in planning mode.
    pub fn ensure_planning_done(&self, context: &ProjectContext) -> Result<(), AppError> {
        if self.is_planning_required(context) {
            return Err(AppError::new(Kind::PlanningRequired, PLANNING_REQUIRED_MESSAGE).with_code(PLANNING_REQUIRED_CODE));
        }
        Ok(())
    }

    pub fn has_ever_run_loop(&self, context: &ProjectContext) -> bool {
        let work_dir = context.work_dir();
        if pycompat::last_segment(&pycompat::nt_normpath(context.repo_path())).ends_with(LOOP_WORKTREE_SUFFIX) {
            return true; // the project was opened on the loop worktree itself
        }
        let loop_json = self.app.specs.read_json(&loop_state::state_file(&work_dir, "LOOP.json"));
        if loop_json.get("status").and_then(|s| s.as_str()).is_some_and(|s| loop_state::LOOP_STATUS.contains(&s)) {
            return true;
        }
        let turns = self.app.specs.read_json(&loop_state::loop_path(&work_dir, "meta.json")).get("turns").cloned();
        if turns.as_ref().is_some_and(|t| (t.is_i64() || t.is_u64()) && t.as_i64().unwrap_or(0) > 0) {
            return true;
        }
        if !self.app.specs.list_json_files(&loop_state::turns_dir(&work_dir)).is_empty() {
            return true;
        }
        self.app.processes.is_alive(self.app.specs.read_loop_lock_pid(&work_dir))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::project::Project;

    fn context(dir: &Path, name: &str) -> ProjectContext {
        let repo = dir.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        ProjectContext::new(Project {
            id: "p".into(),
            name: name.into(),
            repo_path: repo.display().to_string(),
            opened_at: String::new(),
            last_opened_at: String::new(),
            favorite: false,
        })
    }

    #[test]
    fn planning_is_required_until_approval_or_a_loop_ran() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let service = PlanningRequirementService { app: &app };
        let ctx = context(dir.path(), "app");
        assert!(service.is_planning_required(&ctx));
        let error = service.ensure_planning_done(&ctx).unwrap_err();
        assert_eq!((error.kind, error.code.as_deref()), (Kind::PlanningRequired, Some("planning_required")));
        let work = ctx.work_dir();
        let meta = loop_state::loop_path(&work, "meta.json");
        std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
        std::fs::write(&meta, r#"{"turns": 0}"#).unwrap();
        assert!(service.is_planning_required(&ctx));
        std::fs::write(&meta, r#"{"turns": 3}"#).unwrap();
        assert!(!service.is_planning_required(&ctx));
        service.mark_reopened(&ctx, "2026-01-01T00:00:00", &[]);
        assert!(service.is_reopened(&ctx) && service.is_planning_required(&ctx));
        service.clear_reopened(&ctx);
        assert!(!service.is_reopened(&ctx) && !service.is_planning_required(&ctx));
        let on_worktree = context(dir.path(), "x-worktree-loop");
        assert!(service.has_ever_run_loop(&on_worktree));
    }
}
