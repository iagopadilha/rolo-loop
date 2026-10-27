// SPDX-License-Identifier: MIT
//! The AI decisions of a project (`.specs/project/loop/VALIDATE.md`, grammar owned by the engine's `decisions`); the engine file is never edited
//! here.
//!
//! What the user did with a decision (reviewed, asked for a change) lives in `internal/decisions-review.json`
//! (`{"items": {"<id>": {"reviewed", "reviewed_at", "change_requested"}}}`, the id is the hash of the raw line). "Ask for a change" also adds
//! the id to the follow-up draft (`internal/follow-up-draft.json`, key `decisions`; the `text` key belongs to the follow-up service, so this
//! service reads, merges and never clobbers it).

use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use crate::repositories::loop_command_file::{get_loop_internal_dir, now_iso, read_json_object, write_json_atomic};
use engine_state::decisions as decisions_engine;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

pub const REVIEW_FILE_NAME: &str = "decisions-review.json";
pub const FOLLOW_UP_DRAFT_FILE_NAME: &str = "follow-up-draft.json";
pub const LEVEL_CRITICAL: &str = "critical";
pub const LEVEL_IMPORTANT: &str = "important";

pub struct DecisionsService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn decisions(&self) -> DecisionsService<'_> {
        DecisionsService { app: self }
    }
}

fn truthy(v: Option<&Value>) -> bool {
    engine_state::notify_truthy(v)
}

impl DecisionsService<'_> {
    pub fn review_path(work_dir: &str) -> PathBuf {
        get_loop_internal_dir(work_dir).join(REVIEW_FILE_NAME)
    }

    pub fn draft_path(work_dir: &str) -> PathBuf {
        get_loop_internal_dir(work_dir).join(FOLLOW_UP_DRAFT_FILE_NAME)
    }

    fn read_review_items(&self, work_dir: &str) -> Map<String, Value> {
        match read_json_object(&Self::review_path(work_dir)).get("items") {
            Some(Value::Object(items)) => items.clone(),
            _ => Map::new(),
        }
    }

    // ------------------------------------------------ read
    /// `(items with the review state merged, unparsed lines)` of the work folder, in file order.
    pub fn read_decisions(&self, context: &ProjectContext) -> (Vec<Value>, Vec<Value>) {
        let work_dir = context.work_dir();
        let (parsed, unparsed) = decisions_engine::parse(&decisions_engine::read_file(&work_dir));
        let reviews = self.read_review_items(&work_dir);
        let items = parsed
            .iter()
            .map(|decision| {
                let state = decision["id"].as_str().and_then(|id| reviews.get(id)).and_then(Value::as_object);
                let get = |key: &str| state.and_then(|s| s.get(key));
                json!({
                    "id": decision["id"], "slug": decision["slug"], "turn": decision["turn"], "level": decision["level"],
                    "doubt": decision["doubt"], "chose": decision["chose"], "revert": decision["revert"],
                    "reviewed": truthy(get("reviewed")), "reviewed_at": get("reviewed_at").cloned().unwrap_or(Value::Null),
                    "change_requested": truthy(get("change_requested")),
                })
            })
            .collect();
        (items, unparsed)
    }

    /// Open = not marked as reviewed yet.
    pub fn count_open(items: &[Value]) -> Value {
        let open = |level: &str| items.iter().filter(|i| i["reviewed"] != true && i["level"] == level).count();
        json!({"open_critical": open(LEVEL_CRITICAL), "open_important": open(LEVEL_IMPORTANT)})
    }

    /// `GET decisions[?turn=N]`. The counts always cover the whole loop, not only the filtered turn.
    pub fn list_decisions(&self, context: &ProjectContext, turn: Option<i64>) -> Value {
        let (mut items, unparsed) = self.read_decisions(context);
        let counts = Self::count_open(&items);
        if let Some(turn) = turn {
            items.retain(|item| item["turn"].as_i64() == Some(turn));
        }
        json!({"items": items, "unparsed": unparsed, "counts": counts})
    }

    /// `{open_critical, open_important}`: consumed by the panel state and by the wrap-up.
    pub fn open_counts(&self, context: &ProjectContext) -> Value {
        Self::count_open(&self.read_decisions(context).0)
    }

    /// Critical decisions the user has not marked as reviewed (they block Finish).
    pub fn unreviewed_critical(&self, context: &ProjectContext) -> Vec<Value> {
        self.read_decisions(context).0.into_iter().filter(|i| i["level"] == LEVEL_CRITICAL && i["reviewed"] != true).collect()
    }

    // ------------------------------------------------ write
    fn ensure_decision_exists(&self, context: &ProjectContext, decision_id: &str) -> Result<(), AppError> {
        let (items, unparsed) = self.read_decisions(context);
        if items.iter().chain(unparsed.iter()).any(|row| row["id"] == decision_id) {
            return Ok(());
        }
        Err(AppError::new(Kind::DecisionNotFound, "decision not found"))
    }

    fn update_review(&self, work_dir: &str, decision_id: &str, fields: &[(&str, Value)]) {
        let mut data = read_json_object(&Self::review_path(work_dir));
        let mut items = match data.get("items") {
            Some(Value::Object(items)) => items.clone(),
            _ => Map::new(),
        };
        let mut merged = Map::new();
        merged.insert("reviewed".into(), json!(false));
        merged.insert("reviewed_at".into(), Value::Null);
        merged.insert("change_requested".into(), json!(false));
        if let Some(Value::Object(current)) = items.get(decision_id) {
            for (k, v) in current {
                merged.insert(k.clone(), v.clone());
            }
        }
        for (k, v) in fields {
            merged.insert((*k).to_string(), v.clone());
        }
        items.insert(decision_id.to_string(), Value::Object(merged));
        data.insert("items".into(), Value::Object(items));
        let _ = write_json_atomic(&Self::review_path(work_dir), &Value::Object(data));
    }

    pub fn mark_reviewed(&self, context: &ProjectContext, decision_id: &str) -> Result<Value, AppError> {
        self.ensure_decision_exists(context, decision_id)?;
        let _guard = self.app.decisions_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.update_review(&context.work_dir(), decision_id, &[("reviewed", json!(true)), ("reviewed_at", json!(now_iso()))]);
        Ok(json!({"ok": true}))
    }

    /// Adds the id to the follow-up draft (no duplicates, `text` kept) and flags the decision; returns the draft.
    pub fn request_change(&self, context: &ProjectContext, decision_id: &str) -> Result<Value, AppError> {
        self.ensure_decision_exists(context, decision_id)?;
        let _guard = self.app.decisions_lock.lock().unwrap_or_else(|e| e.into_inner());
        let work_dir = context.work_dir();
        self.update_review(&work_dir, decision_id, &[("change_requested", json!(true))]);
        let mut draft = read_json_object(&Self::draft_path(&work_dir));
        let mut chosen = match draft.get("decisions") {
            Some(Value::Array(list)) => list.clone(),
            _ => vec![],
        };
        if !chosen.iter().any(|c| c == decision_id) {
            chosen.push(json!(decision_id));
        }
        draft.insert("decisions".into(), Value::Array(chosen));
        if !draft.get("text").is_some_and(Value::is_string) {
            draft.insert("text".into(), json!(""));
        }
        let draft = Value::Object(draft);
        let _ = write_json_atomic(&Self::draft_path(&work_dir), &draft);
        Ok(json!({"ok": true, "draft": draft}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, std::sync::Arc<App>, ProjectContext) {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project/loop")).unwrap();
        std::fs::write(
            repo.join(".specs/project/loop/VALIDATE.md"),
            "- [a] turn=1 level=critical which db? | chose: pg | revert: drop\n- [b] turn=2 level=important which lib? | chose: x | revert: y\n- [broken line\n- [c] nonsense\n",
        )
        .unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        (dir, app, context)
    }

    #[test]
    fn review_state_is_merged_and_counted() {
        let (_dir, app, context) = setup();
        let service = app.decisions();
        let listing = service.list_decisions(&context, None);
        assert_eq!(listing["counts"], json!({"open_critical": 1, "open_important": 1}));
        let id = listing["items"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(service.mark_reviewed(&context, &id).unwrap(), json!({"ok": true}));
        assert_eq!(service.open_counts(&context), json!({"open_critical": 0, "open_important": 1}));
        assert!(service.unreviewed_critical(&context).is_empty());
        let filtered = service.list_decisions(&context, Some(2));
        assert_eq!((filtered["items"].as_array().unwrap().len(), filtered["counts"]["open_critical"].clone()), (1, json!(0)));
        assert_eq!(service.mark_reviewed(&context, "nope").unwrap_err().kind, Kind::DecisionNotFound);
    }

    #[test]
    fn change_request_keeps_draft_text_and_has_no_duplicates() {
        let (_dir, app, context) = setup();
        let service = app.decisions();
        let id = service.read_decisions(&context).0[1]["id"].as_str().unwrap().to_string();
        let draft_path = DecisionsService::draft_path(&context.work_dir());
        std::fs::create_dir_all(draft_path.parent().unwrap()).unwrap();
        std::fs::write(&draft_path, r#"{"text": "keep me"}"#).unwrap();
        service.request_change(&context, &id).unwrap();
        let out = service.request_change(&context, &id).unwrap();
        assert_eq!(out["draft"], json!({"text": "keep me", "decisions": [id]}));
        assert_eq!(service.read_decisions(&context).0[1]["change_requested"], true);
    }
}
