// SPDX-License-Identifier: MIT
//! Network section of the Test environment tab: the project's allowed hosts (`.specs/project/NETWORK.json`), the inherited lists (always local,
//! built-in read, global `egress_hosts`, global `egress_never`), and the suggestions the planning chat wrote (`NETWORK.suggested.json`; only the
//! user's click moves them into NETWORK.json).
//!
//! The contract (grammar, validation, files, merge) lives in the engine's `network_policy`; this service only reads and writes through it. Reads and
//! writes go to the REPO (`context.credentials_dir`, WT-02), never to the loop worktree: the engine copies NETWORK.json into the worktree at every
//! turn start. Entries are host names, not secrets, so they travel in clear.

use super::App;
use crate::domain::errors::AppError;
use crate::domain::project::ProjectContext;
use egress_policy::read_project_entries;
use engine_state::network_policy;
use serde_json::{json, Value};
use std::path::Path;

pub const INVALID_ENTRY_CODE: &str = "invalid_network_entry";
pub const MAX_ENTRIES: usize = 200;
const SUGGESTION_ACTIONS: [&str; 2] = ["apply", "dismiss"];

pub struct NetworkService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn network(&self) -> NetworkService<'_> {
        NetworkService { app: self }
    }
}

fn require_confirm(confirm: &Value) -> Result<(), AppError> {
    if confirm != &Value::Bool(true) {
        return Err(AppError::invalid_request("confirm must be true"));
    }
    Ok(())
}

impl NetworkService<'_> {
    /// `{ok, effective: [{entry, origin}], deny, project, suggestions, suggestions_exists, notes, broken?}`. `effective` is the union the guard enforces
    /// (local + built-in + global + project, deny not applied: it is listed apart); `project` is the raw text of every NETWORK.json entry (an invalid one
    /// stays visible so the user can fix it).
    pub fn get_network(&self, context: &ProjectContext) -> Value {
        let root = context.credentials_dir();
        let effective = network_policy::load(root, &self.app.config.user_data_dir, None);
        let (texts, _) = read_project_entries(Path::new(root));
        let suggestion = network_policy::read_suggestions(root);
        let mut notes: Vec<String> = effective.notes.clone();
        notes.extend(suggestion.notes.clone());
        let mut body = json!({
            "ok": true,
            "effective": effective.allow.iter().map(|e| json!({"entry": e.text, "origin": e.origin})).collect::<Vec<_>>(),
            "deny": effective.deny,
            "project": texts.iter().filter(|t| t.is_string()).collect::<Vec<_>>(),
            "suggestions": suggestion.allow,
            "suggestions_exists": suggestion.exists,
            "notes": notes,
        });
        if !effective.broken.is_empty() {
            body["broken"] = json!(effective.broken);
        }
        body
    }

    /// Replaces the project's entries. Every entry is validated first; the first invalid one refuses the whole save (nothing written).
    pub fn save_network(&self, context: &ProjectContext, allow: &Value, confirm: &Value) -> Result<Value, AppError> {
        require_confirm(confirm)?;
        let Some(list) = allow.as_array() else { return Err(AppError::invalid_request("allow must be a list of entries")) };
        if list.len() > MAX_ENTRIES {
            return Err(AppError::invalid_request(format!("too many entries (at most {MAX_ENTRIES})")));
        }
        let stored = {
            let _guard = self.app.network_lock.lock().unwrap_or_else(|e| e.into_inner());
            network_policy::write_project(context.credentials_dir(), list)
        };
        match stored {
            Ok(stored) => {
                self.app.log.info("network_saved", &[("project", json!(context.project.id)), ("entries", json!(stored.len()))]);
                Ok(json!({"ok": true, "project": stored}))
            }
            Err(error) => Err(AppError::invalid_request(format!("entry {} is not valid: {}", error.index + 1, error.reason))
                .with_code(INVALID_ENTRY_CODE)
                .with_extra("index", json!(error.index))
                .with_extra("entry", json!(error.text))
                .with_extra("reason", json!(error.reason))),
        }
    }

    /// `apply` merges the valid suggested entries into NETWORK.json and deletes the suggestion file; `dismiss` only deletes it.
    pub fn answer_suggestion(&self, context: &ProjectContext, action: &Value, confirm: &Value) -> Result<Value, AppError> {
        require_confirm(confirm)?;
        let Some(action) = action.as_str().filter(|a| SUGGESTION_ACTIONS.contains(a)) else {
            return Err(AppError::invalid_request("action must be 'apply' or 'dismiss'"));
        };
        let root = context.credentials_dir();
        let _guard = self.app.network_lock.lock().unwrap_or_else(|e| e.into_inner());
        if action == "dismiss" {
            let dismissed = network_policy::dismiss_suggestions(root);
            self.app.log.info("network_suggestion_dismissed", &[("project", json!(context.project.id)), ("dismissed", json!(dismissed))]);
            return Ok(json!({"ok": true, "action": action, "dismissed": dismissed}));
        }
        let (added, allow) = network_policy::apply_suggestions(root).map_err(AppError::invalid_request)?;
        self.app.log.info("network_suggestion_applied", &[("project", json!(context.project.id)), ("added", json!(added.len()))]);
        Ok(json!({"ok": true, "action": action, "added": added, "project": allow}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::errors::Kind;

    #[test]
    fn entries_are_saved_validated_and_suggestions_applied() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let service = app.network();
        assert_eq!(service.save_network(&context, &json!(["api.example.com"]), &json!(false)).unwrap_err().message, "confirm must be true");
        assert_eq!(service.save_network(&context, &json!("x"), &json!(true)).unwrap_err().message, "allow must be a list of entries");
        let invalid = service.save_network(&context, &json!(["api.example.com", "bad host!"]), &json!(true)).unwrap_err();
        assert_eq!((invalid.kind, invalid.code.as_deref(), invalid.extras["index"].clone()), (Kind::InvalidRequest, Some("invalid_network_entry"), json!(1)));
        assert!(invalid.message.starts_with("entry 2 is not valid: "));
        let saved = service.save_network(&context, &json!(["api.example.com", "db.example.com:5432 tcp"]), &json!(true)).unwrap();
        assert_eq!(saved["project"].as_array().unwrap().len(), 2);
        let view = service.get_network(&context);
        assert_eq!(view["project"].as_array().unwrap().len(), 2);
        assert!(view["effective"].as_array().unwrap().iter().any(|e| e["entry"] == "api.example.com" && e["origin"] == "project"));
        std::fs::write(repo.join(".specs/project/NETWORK.suggested.json"), r#"{"allow": ["new.example.com", "api.example.com"]}"#).unwrap();
        assert_eq!(service.get_network(&context)["suggestions_exists"], true);
        assert_eq!(service.answer_suggestion(&context, &json!("nope"), &json!(true)).unwrap_err().message, "action must be 'apply' or 'dismiss'");
        let applied = service.answer_suggestion(&context, &json!("apply"), &json!(true)).unwrap();
        assert_eq!(applied["added"], json!(["new.example.com"]));
        assert_eq!(service.answer_suggestion(&context, &json!("dismiss"), &json!(true)).unwrap()["dismissed"], false);
    }
}
