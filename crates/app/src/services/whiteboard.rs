// SPDX-License-Identifier: MIT
//! Free-form project board (whiteboard), persisted on the server with a version (optimistic control between tabs).
//!
//! `GET` -> `{"ok":true,"data":<object|null>,"version":N,"updated_at":"ISO|null"}` (version 0 and data null when never saved).
//! `POST {"data":<object>,"base_version":N}` -> `{"ok":true,"version":N+1,"updated_at":"ISO"}`; a different base = 409 with the current one.

use super::App;
use crate::domain::project::ProjectContext;
use crate::repositories::loop_command_file::now_iso;
use crate::repositories::small_stores::{read_board, write_board};
use serde_json::{json, Map, Value};

const NEVER_SAVED_BOARD_VERSION: i64 = 0;

fn is_integer(value: Option<&Value>) -> Option<i64> {
    value.filter(|v| v.is_i64() || v.is_u64()).and_then(Value::as_i64)
}

fn current_version(board: &Map<String, Value>) -> i64 {
    is_integer(board.get("version")).unwrap_or(NEVER_SAVED_BOARD_VERSION)
}

pub struct WhiteboardService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn whiteboard(&self) -> WhiteboardService<'_> {
        WhiteboardService { app: self }
    }
}

impl WhiteboardService<'_> {
    pub fn get_board(&self, context: &ProjectContext) -> (u16, Value) {
        let board = read_board(&context.work_dir());
        let data = board.get("data").filter(|d| d.is_object()).cloned().unwrap_or(Value::Null);
        (200, json!({"ok": true, "data": data, "version": current_version(&board), "updated_at": board.get("updated_at").cloned().unwrap_or(Value::Null)}))
    }

    pub fn save_board(&self, context: &ProjectContext, body: &Value) -> (u16, Value) {
        let Some(data) = body.get("data").filter(|d| d.is_object()) else { return (400, json!({"ok": false, "error": "data must be a JSON object"})) };
        let Some(base_version) = is_integer(body.get("base_version")) else { return (400, json!({"ok": false, "error": "base_version must be an integer"})) };
        let board = {
            let _guard = self.app.whiteboard_lock.lock().unwrap_or_else(|e| e.into_inner());
            let current = current_version(&read_board(&context.work_dir()));
            if base_version != current {
                return (409, json!({"ok": false, "error": format!("the board changed in another tab (version {current}); reload or overwrite"), "version": current}));
            }
            let board = json!({"data": data, "version": current + 1, "updated_at": now_iso()});
            let _ = write_board(&context.work_dir(), &board);
            board
        };
        self.app.log.info("whiteboard_saved", &[("project", json!(context.project.id)), ("version", board["version"].clone())]);
        (200, json!({"ok": true, "version": board["version"], "updated_at": board["updated_at"]}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_guard_concurrent_saves() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(&repo).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let service = app.whiteboard();
        assert_eq!(service.get_board(&context), (200, json!({"ok": true, "data": null, "version": 0, "updated_at": null})));
        assert_eq!(service.save_board(&context, &json!({"data": [], "base_version": 0})).0, 400);
        assert_eq!(service.save_board(&context, &json!({"data": {}, "base_version": "0"})).1["error"], "base_version must be an integer");
        let saved = service.save_board(&context, &json!({"data": {"a": 1}, "base_version": 0}));
        assert_eq!((saved.0, saved.1["version"].clone()), (200, json!(1)));
        let stale = service.save_board(&context, &json!({"data": {"a": 2}, "base_version": 0}));
        assert_eq!((stale.0, stale.1["version"].clone()), (409, json!(1)));
        assert_eq!(service.get_board(&context).1["data"], json!({"a": 1}));
    }
}
