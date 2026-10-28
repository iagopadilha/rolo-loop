// SPDX-License-Identifier: MIT
//! Pending approvals and the simple project policy (routes `approvals` and `policy`). The engine owns the data (`engine_state::approvals`,
//! `engine_state::project_policy`); this service builds the context, answers the routes and, once the last waiting request is answered, resumes a
//! loop that stopped only because it waited (a stopped run is relaunched; a run that never stopped needs nothing: the next turn reads the answer).

use super::loop_control::Reply;
use super::App;
use crate::domain::project::ProjectContext;
use engine_state::access_grant::AccessContext;
use engine_state::project_policy::Change;
use engine_state::{approvals, project_policy};
use serde_json::{json, Value};

const WAIT_STATUSES: [&str; 3] = ["awaiting_user", "paused", "stopped"];

pub struct ApprovalsService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn approvals(&self) -> ApprovalsService<'_> {
        ApprovalsService { app: self }
    }
}

fn failure(status: u16, message: impl Into<String>) -> Reply {
    (status, json!({"ok": false, "error": message.into()}))
}

impl ApprovalsService<'_> {
    fn with_access<T>(&self, context: &ProjectContext, who: &str, run: impl FnOnce(&AccessContext) -> T) -> T {
        let work = context.work_dir();
        let data_dir = self.app.config.user_data_dir.clone();
        let toolkit = self.app.diagnostics().find_toolkit();
        let access = AccessContext { repo: context.repo_path(), work: &work, data_dir: &data_dir, toolkit_root: &toolkit, who };
        run(&access)
    }

    /// `GET approvals`: the cards waiting for the user, the counts and the project policy.
    pub fn view(&self, context: &ProjectContext) -> Value {
        let work = context.work_dir();
        let mut view = approvals::view(&work);
        view["policy"] = project_policy::view(&work);
        view
    }

    /// `POST approvals/<id>/approve {always?, reason?}` and `POST approvals/<id>/deny {reason?}`.
    pub fn answer(&self, context: &ProjectContext, id: &str, approve: bool, body: &Value) -> Reply {
        let always = approve && body.get("always").and_then(Value::as_bool) == Some(true);
        let reason = body.get("reason").and_then(Value::as_str).unwrap_or("");
        let answered = self.with_access(context, "panel", |access| approvals::decide(access, id, approve, always, reason));
        let card = match answered {
            Ok(card) => card,
            Err(error) => return failure(if error.starts_with("no such request") { 404 } else { 400 }, error),
        };
        let resumed = approve && self.resume_if_waiting(context);
        self.app
            .log
            .info("approval_answered", &[("project", json!(context.project.id)), ("approved", json!(approve)), ("always", json!(always)), ("resumed", json!(resumed))]);
        (200, json!({"ok": true, "approval": card, "resumed": resumed, "pending": approvals::pending(&context.work_dir()).len()}))
    }

    /// A loop that stopped while it waited for an approval carries on by itself once the last request is answered. A loop that is running needs
    /// nothing, a loop that stopped for another reason (an error, a real question) is left to the user.
    fn resume_if_waiting(&self, context: &ProjectContext) -> bool {
        let work = context.work_dir();
        if !approvals::pending(&work).is_empty() || self.app.loop_control().is_loop_alive(context) {
            return false;
        }
        let summary = self.app.loop_status().get_loop_summary(context);
        let status = summary["status"].as_str().unwrap_or("");
        let reason = summary["reason"].as_str().or_else(|| summary["json"]["reason"].as_str()).unwrap_or("").to_lowercase();
        let waited = status != "awaiting_user" || reason.contains("approv") || reason.contains("permission");
        if !WAIT_STATUSES.contains(&status) || !waited {
            return false;
        }
        self.app.loop_control().resume_loop(context, None).0 == 200
    }

    /// `GET policy`.
    pub fn policy(&self, context: &ProjectContext) -> Value {
        project_policy::view(&context.work_dir())
    }

    /// `POST policy {commands?, e2e_dev_baseline?: {enabled?, dev_hosts?, production_hosts?}, reason}`.
    pub fn set_policy(&self, context: &ProjectContext, body: &Value) -> Reply {
        let list = |value: Option<&Value>| -> Result<Option<Vec<String>>, String> {
            match value {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Array(items)) => {
                    items.iter().map(|i| i.as_str().map(String::from).ok_or_else(|| "lists hold text only".to_string())).collect::<Result<Vec<_>, _>>().map(Some)
                }
                Some(_) => Err("expected a list".into()),
            }
        };
        let baseline = body.get("e2e_dev_baseline");
        let change = (|| -> Result<Change, String> {
            Ok(Change {
                commands: list(body.get("commands"))?,
                e2e_enabled: match baseline.and_then(|b| b.get("enabled")) {
                    None | Some(Value::Null) => None,
                    Some(Value::Bool(b)) => Some(*b),
                    Some(_) => return Err("enabled must be true or false".into()),
                },
                dev_hosts: list(baseline.and_then(|b| b.get("dev_hosts")))?,
                production_hosts: list(baseline.and_then(|b| b.get("production_hosts")))?,
            })
        })();
        let change = match change {
            Ok(c) => c,
            Err(error) => return failure(400, error),
        };
        if change.commands.is_none() && change.e2e_enabled.is_none() && change.dev_hosts.is_none() && change.production_hosts.is_none() {
            return failure(400, "nothing to change: send commands and/or e2e_dev_baseline");
        }
        let reason = body.get("reason").and_then(Value::as_str).unwrap_or("");
        match self.with_access(context, "panel", |access| project_policy::save(access, &change, reason)) {
            Ok(view) => (200, json!({"ok": true, "policy": view})),
            Err(error) => failure(400, error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_state::approvals::guard_policy::{register_request, Request, CLASS_DEV_RUN, CLASS_MIGRATE_APPLY};

    fn setup() -> (tempfile::TempDir, std::sync::Arc<App>, ProjectContext) {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project/loop")).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        (dir, app, context)
    }

    fn ask(context: &ProjectContext, class: &'static str, command: &str) -> String {
        let request = Request::new(class, command);
        register_request(&format!("{}/.specs", context.work_dir()), &request, Some(1), Some("feat-a"), "executor");
        request.id
    }

    #[test]
    fn the_view_lists_one_card_per_request_and_the_default_policy_asks() {
        let (_dir, app, context) = setup();
        let empty = app.approvals().view(&context);
        assert_eq!(
            (empty["counts"]["pending"].clone(), empty["policy"]["default"].clone(), empty["policy"]["production"].clone()),
            (json!(0), json!("ask"), json!("never"))
        );
        let id = ask(&context, CLASS_MIGRATE_APPLY, "dotnet ef database update");
        ask(&context, CLASS_MIGRATE_APPLY, "dotnet ef database update");
        let view = app.approvals().view(&context);
        assert_eq!(view["pending"].as_array().unwrap().len(), 1);
        assert_eq!((view["pending"][0]["id"].clone(), view["pending"][0]["env"].clone()), (json!(id), json!("DEV")));
    }

    #[test]
    fn approve_and_deny_answer_the_card_and_are_journaled() {
        let (_dir, app, context) = setup();
        let first = ask(&context, CLASS_MIGRATE_APPLY, "dotnet ef database update");
        let second = ask(&context, CLASS_DEV_RUN, "npm run dev");
        let (code, body) = app.approvals().answer(&context, &first, true, &json!({}));
        assert_eq!(code, 200, "{body}");
        assert_eq!(
            (body["approval"]["state"].clone(), body["approval"]["scope"].clone(), body["pending"].clone(), body["resumed"].clone()),
            (json!("approved"), json!("once"), json!(1), json!(false))
        );
        let (code, body) = app.approvals().answer(&context, &second, false, &json!({"reason": "not now"}));
        assert_eq!((code, body["approval"]["state"].clone()), (200, json!("denied")));
        let journal = engine_state::access_grant::read_journal(&context.work_dir());
        assert_eq!((journal.len(), journal[0]["outcome"].clone(), journal[1]["outcome"].clone()), (2, json!("granted"), json!("refused")));
        assert!(engine_state::project_policy::read(&context.work_dir()).commands.is_empty(), "approving once does not change the policy");
    }

    #[test]
    fn approve_always_writes_the_simple_list_and_an_unknown_card_is_404() {
        let (_dir, app, context) = setup();
        let id = ask(&context, CLASS_DEV_RUN, "npm run dev");
        let (code, _) = app.approvals().answer(&context, &id, true, &json!({"always": true}));
        assert_eq!(code, 200);
        assert_eq!(app.approvals().policy(&context)["commands"], json!(["dev_run"]));
        assert_eq!(app.approvals().answer(&context, "ap-0000000000000000", true, &json!({})).0, 404);
    }

    #[test]
    fn the_policy_route_validates_and_never_releases_production() {
        let (_dir, app, context) = setup();
        let service = app.approvals();
        assert_eq!(service.set_policy(&context, &json!({})).0, 400);
        assert_eq!(service.set_policy(&context, &json!({"commands": ["migrations"]})).0, 400, "a reason is required");
        assert_eq!(service.set_policy(&context, &json!({"commands": "migrations", "reason": "x"})).0, 400);
        let (code, body) = service.set_policy(&context, &json!({"commands": ["dotnet ef database update --environment Production"], "reason": "x"}));
        assert_eq!(code, 400);
        assert!(body["error"].as_str().unwrap().contains("production is never released"), "{body}");
        let (code, body) = service.set_policy(&context, &json!({"commands": ["migrations", "npm run dev"], "reason": "the team runs these on DEV"}));
        assert_eq!(code, 200, "{body}");
        assert_eq!(body["policy"]["commands"], json!(["migrations", "npm run dev"]));
        let (code, body) = service.set_policy(
            &context,
            &json!({"e2e_dev_baseline": {"enabled": true, "dev_hosts": ["svc.live.example.com"], "production_hosts": ["*.live.example.com"]}, "reason": "x"}),
        );
        assert_eq!(code, 400, "{body}");
        let (code, _) = service.set_policy(
            &context,
            &json!({"e2e_dev_baseline": {"enabled": true, "dev_hosts": ["dev.example.com"], "production_hosts": ["prod.example.com"]}, "reason": "E2E on DEV"}),
        );
        assert_eq!(code, 200);
        assert_eq!(service.policy(&context)["e2e_dev_baseline"]["enabled"], true);
    }
}
