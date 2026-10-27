// SPDX-License-Identifier: MIT
//! Context of the chat scopes the panel's dialogs use (`error` and `quarantine`), built on the server from the same data the panel shows.
//!
//! The chat never receives text from the browser as context: it only names an error id or a feature, and this service looks the facts up.
//! Everything is redacted before it reaches `claude -p`.

use super::chat::ChatService;
use super::pipeline::step_key;
use super::App;
use crate::domain::project::ProjectContext;
use crate::infrastructure::claude_client::redact_secrets;
use crate::repositories::loop_read_formatting::text_or_empty;
use serde_json::Value;

const TURN_EVENTS_LIMIT: usize = 40;

pub struct ChatScopeContextService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn chat_scope(&self) -> ChatScopeContextService<'_> {
        ChatScopeContextService { app: self }
    }
}

fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::String(s) => s.clone(),
        other => pycompat::json::py_json_dumps(other, false),
    }
}

impl ChatScopeContextService<'_> {
    /// Title, detail, hint, turn and the events of that turn for one item of `GET errors`; `None` when the id is not in the list.
    pub fn build_error_context(&self, context: &ProjectContext, error_id: &str) -> Option<String> {
        let errors = self.app.error_center().build_errors(context);
        let item = errors["items"].as_array()?.iter().find(|row| row["id"] == error_id)?;
        let or = |value: &Value, fallback: &str| {
            let text = text_or_empty(Some(value));
            if text.is_empty() { fallback.to_string() } else { text }
        };
        let mut lines = vec![
            format!("Error shown in the loop panel: {}", item["title"].as_str().unwrap_or("")),
            format!(
                "Kind: {}. Severity: {}. When: {}. Turn: {}.",
                item["kind"].as_str().unwrap_or(""),
                item["severity"].as_str().unwrap_or(""),
                or(&item["when"], "unknown"),
                if item["turn"].is_null() { "n/a".to_string() } else { py_str(&item["turn"]) }
            ),
            format!("Detail:\n{}", or(&item["detail"], "(none)")),
            format!("What the panel suggests: {}", or(&item["hint"], "(nothing)")),
        ];
        let recover: Vec<&str> = item["recover"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        if !recover.is_empty() {
            lines.push(format!("Recovery buttons offered: {}", recover.join(", ")));
        }
        let events = self.list_turn_events(context, item["turn"].as_i64());
        if !events.is_empty() {
            lines.push(format!("Events of turn {}:", py_str(&item["turn"])));
            lines.extend(events);
        }
        lines.push(ChatService::build_general_context(&context.work_dir()));
        Some(redact_secrets(&lines.join("\n"), None))
    }

    fn list_turn_events(&self, context: &ProjectContext, turn_number: Option<i64>) -> Vec<String> {
        let Some(turn_number) = turn_number else { return vec![] };
        let pipeline = self.app.pipeline().build_pipeline(context);
        let Some(turn) = pipeline["turns"].as_array().into_iter().flatten().find(|row| row["n"].as_i64() == Some(turn_number)) else { return vec![] };
        turn["events"]
            .as_array()
            .into_iter()
            .flatten()
            .take(TURN_EVENTS_LIMIT)
            .map(|event| {
                let out = event["out"].as_str().filter(|o| !o.is_empty()).unwrap_or("(no text)");
                format!("- {} | {} | {} | {out}", py_str(&event["feature"]), step_key(event["step"].as_u64().unwrap_or(0) as usize), event["status"].as_str().unwrap_or(""))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_context_names_the_item_and_the_files_to_read() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        std::fs::write(repo.join(".specs/project/ROADMAP.md"), "- [a] status=blocked tries=2 note=flaky\n").unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        assert!(app.chat_scope().build_error_context(&context, "nope").is_none());
        let text = app.chat_scope().build_error_context(&context, "feature-a").unwrap();
        assert!(text.starts_with("Error shown in the loop panel: Feature a is blocked (quarantine)\nKind: feature. Severity: error. When: unknown. Turn: n/a.\nDetail:\n2 failures. flaky"));
        assert!(text.contains("General loop context. Read what you need with Read:"));
    }
}
