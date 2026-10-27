// SPDX-License-Identifier: MIT
//! Routes of the repo list of a project (hub-level, keyed by project id), the repo catalog, the workspace / Close loop / wrap-up, "Commit setup files
//! and push", the follow-up rounds, reopen planning, the per-repo readiness guard and the quarantine of a blocked feature.

use super::route;
use crate::api::responses::{context_of, respond_converting_errors, respond_json, respond_with_project_context, guard_repos_free};
use crate::api::router::{Request, Response, Router};
use crate::domain::project::ProjectContext;
use crate::services::App;
use serde_json::{json, Value};
use std::sync::Arc;

const JOB_ACCEPTED_STATUS: u16 = 202;
const REPOS_ROUTE: &str = "/api/app/projects/{project_id}/repos";
const READINESS_ROUTE: &str = "/api/app/projects/{project_id}/repos-readiness";
const REPO_ROUTE: &str = "/api/app/projects/{project_id}/repos/{name}";
const PRIMARY_STANDARDS_ROUTE: &str = "/p/{project_id}/api/standards";
const PROJECT_PREFIX: &str = "/p/{project_id}/api";

pub fn register(router: &mut Router, app: &Arc<App>) {
    // RepoController
    route(router, app, "GET", REPOS_ROUTE, list_repos);
    route(router, app, "POST", REPOS_ROUTE, add_repo);
    route(router, app, "POST", &format!("{REPOS_ROUTE}/scope/confirm"), confirm_scope);
    route(router, app, "PATCH", &format!("{REPOS_ROUTE}/{{name}}"), update_repo);
    route(router, app, "PUT", &format!("{REPOS_ROUTE}/{{name}}/role"), set_role); // same handler, works with the current HTTP server
    route(router, app, "DELETE", &format!("{REPOS_ROUTE}/{{name}}"), remove_repo);
    route(router, app, "PUT", &format!("{REPOS_ROUTE}/{{name}}/copy"), set_copy_list);
    route(router, app, "PUT", &format!("{REPOS_ROUTE}/{{name}}/setup"), set_setup);
    route(router, app, "GET", &format!("{REPOS_ROUTE}/{{name}}/copy-suggestions"), list_copy_suggestions);
    // RepoCatalogController
    route(router, app, "GET", "/api/app/catalog", get_catalog_index);
    route(router, app, "GET", "/api/app/catalog/sheet", get_catalog_sheet);
    route(router, app, "POST", "/api/app/catalog/scan", start_catalog_scan);
    route(router, app, "GET", "/api/app/catalog/job/{job_id}", get_catalog_job);
    // WorkspaceController
    route(router, app, "GET", &format!("{PROJECT_PREFIX}/workspace"), get_workspace);
    route(router, app, "POST", &format!("{PROJECT_PREFIX}/workspace/create"), create_workspace);
    route(router, app, "GET", &format!("{PROJECT_PREFIX}/workspace/job/{{job_id}}"), get_create_job);
    route(router, app, "POST", &format!("{PROJECT_PREFIX}/workspace/repos/{{name}}/commit-push"), commit_and_push);
    route(router, app, "POST", &format!("{PROJECT_PREFIX}/workspace/repos"), add_repo); // the contract's repo list routes (same service as the hub's)
    route(router, app, "PATCH", &format!("{PROJECT_PREFIX}/workspace/repos/{{name}}"), update_repo);
    route(router, app, "DELETE", &format!("{PROJECT_PREFIX}/workspace/repos/{{name}}"), remove_repo);
    route(router, app, "GET", &format!("{PROJECT_PREFIX}/close-loop/check"), check_close_loop);
    route(router, app, "POST", &format!("{PROJECT_PREFIX}/close-loop"), close_loop);
    route(router, app, "GET", &format!("{PROJECT_PREFIX}/wrap-up"), get_wrap_up);
    route(router, app, "POST", &format!("{PROJECT_PREFIX}/wrap-up/finish"), finish_wrap_up);
    route(router, app, "GET", "/api/app/overview", get_overview);
    // SetupCommitController
    route(router, app, "POST", &format!("{PROJECT_PREFIX}/workspace/repos/{{name}}/commit-setup"), commit_setup);
    // FollowUpController
    route(router, app, "GET", &format!("{PROJECT_PREFIX}/follow-up"), get_follow_up);
    route(router, app, "POST", &format!("{PROJECT_PREFIX}/follow-up/send"), send_follow_up);
    route(router, app, "POST", &format!("{PROJECT_PREFIX}/follow-up/run"), run_follow_up);
    // PlanningReopenController
    route(router, app, "GET", &format!("{PROJECT_PREFIX}/planning/reopen-check"), check_reopen);
    route(router, app, "POST", &format!("{PROJECT_PREFIX}/planning/reopen"), reopen_planning);
    // RepoReadinessController
    route(router, app, "GET", READINESS_ROUTE, describe_repos_readiness);
    route(router, app, "PUT", &format!("{REPO_ROUTE}/conformance"), set_repo_conformance);
    route(router, app, "POST", &format!("{REPO_ROUTE}/map"), map_repo);
    route(router, app, "GET", &format!("{REPO_ROUTE}/standards"), get_standards);
    route(router, app, "PUT", &format!("{REPO_ROUTE}/standards"), set_standards);
    route(router, app, "GET", PRIMARY_STANDARDS_ROUTE, get_standards);
    route(router, app, "PUT", PRIMARY_STANDARDS_ROUTE, set_standards);
    // QuarantineController
    let quarantine = format!("{PROJECT_PREFIX}/quarantine/{{slug}}");
    route(router, app, "GET", &quarantine, get_quarantine_detail);
    route(router, app, "POST", &format!("{quarantine}/summary"), start_quarantine_summary);
    route(router, app, "POST", &format!("{quarantine}/retry"), retry_quarantined);
    route(router, app, "POST", &format!("{quarantine}/skip"), skip_quarantined);
    route(router, app, "POST", &format!("{quarantine}/revert"), revert_quarantined);
}

fn reply(pair: (u16, Value)) -> Response {
    Response::json(pair.0, pair.1)
}

fn name_param(request: &Request) -> &str {
    request.param("name")
}

// ------------------------------------------------ repo list
fn list_repos(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.repo_registry().list_repos(&context_of(app, request)?)))
}

fn add_repo(app: &App, request: &Request) -> Response {
    respond_json(|| app.repo_registry().add_repo(&context_of(app, request)?, &request.value("path"), request.opt("copy"), request.opt("setup"), request.opt("role"), request.opt("reason")))
}

fn set_role(app: &App, request: &Request) -> Response {
    respond_json(|| app.repo_registry().set_role(&context_of(app, request)?, name_param(request), &request.value("role")))
}

fn update_repo(app: &App, request: &Request) -> Response {
    respond_json(|| app.repo_registry().update_repo(&context_of(app, request)?, name_param(request), request.opt("role"), request.opt("reason")))
}

fn confirm_scope(app: &App, request: &Request) -> Response {
    respond_json(|| app.repo_registry().confirm_scope(&context_of(app, request)?))
}

fn remove_repo(app: &App, request: &Request) -> Response {
    respond_json(|| app.repo_registry().remove_repo(&context_of(app, request)?, name_param(request)))
}

fn set_copy_list(app: &App, request: &Request) -> Response {
    // explicit null = automatic; a missing key stays invalid
    let copy = request.field("copy").cloned().unwrap_or(json!(""));
    respond_json(|| app.repo_registry().set_copy_list(&context_of(app, request)?, name_param(request), &copy))
}

fn set_setup(app: &App, request: &Request) -> Response {
    respond_json(|| app.repo_registry().set_setup(&context_of(app, request)?, name_param(request), &request.value("setup")))
}

fn list_copy_suggestions(app: &App, request: &Request) -> Response {
    respond_json(|| app.repo_registry().list_copy_suggestions(&context_of(app, request)?, name_param(request)))
}

// ------------------------------------------------ catalog
fn get_catalog_index(app: &App, _: &Request) -> Response {
    Response::json(200, app.repo_catalog().get_index())
}

fn get_catalog_sheet(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| match app.repo_catalog().get_sheet(&json!(request.get_query("path")))? {
        Some(sheet) => Ok(Response::json(200, sheet)),
        None => Ok(Response::error(404, "this repo has not been scanned")),
    })
}

fn start_catalog_scan(app: &App, request: &Request) -> Response {
    respond_json(|| app.repo_catalog().start_scan(&request.value("paths")))
}

fn get_catalog_job(app: &App, request: &Request) -> Response {
    respond_json(|| app.repo_catalog().get_job(request.param("job_id")))
}

// ------------------------------------------------ workspace, close loop, wrap-up
fn get_workspace(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.workspace().get_workspace(&context_of(app, request)?)))
}

fn create_workspace(app: &App, request: &Request) -> Response {
    respond_json(|| app.workspace().create_workspace(&context_of(app, request)?, request.field("repos")))
}

fn commit_and_push(app: &App, request: &Request) -> Response {
    respond_json(|| app.workspace().commit_and_push(&context_of(app, request)?, name_param(request), request.field("message"), request.field("push")))
}

fn get_create_job(app: &App, request: &Request) -> Response {
    respond_json(|| app.workspace().get_create_job(&context_of(app, request)?, request.param("job_id")))
}

fn check_close_loop(app: &App, request: &Request) -> Response {
    respond_json(|| app.loop_worktrees().check_close_loop(&context_of(app, request)?))
}

/// Forcing and finishing go through wrap-up/finish only (typed confirmation).
fn close_loop(app: &App, request: &Request) -> Response {
    let body: serde_json::Map<String, Value> = request.body_map().iter().filter(|(name, _)| !matches!(name.as_str(), "force_remove" | "finished")).map(|(k, v)| (k.clone(), v.clone())).collect();
    respond_json(|| app.loop_worktrees().close_loop(&context_of(app, request)?, &Value::Object(body)))
}

fn get_wrap_up(app: &App, request: &Request) -> Response {
    respond_json(|| app.wrap_up().get_wrap_up(&context_of(app, request)?))
}

fn finish_wrap_up(app: &App, request: &Request) -> Response {
    respond_json(|| app.wrap_up().finish(&context_of(app, request)?, &request.body))
}

fn get_overview(app: &App, _: &Request) -> Response {
    Response::json(200, json!({"ok": true, "at": engine_state::loop_io::now_iso(), "loops": app.loop_status().list_running_loops()}))
}

fn commit_setup(app: &App, request: &Request) -> Response {
    respond_json(|| app.setup_commit().commit_setup(&context_of(app, request)?, name_param(request), &request.value("confirm_branch")))
}

// ------------------------------------------------ follow-up
fn get_follow_up(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.follow_up().get_follow_up(&context_of(app, request)?)))
}

fn send_follow_up(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| app.follow_up().send_follow_up(&context_of(app, request)?, &request.body).map(|job| Response::json(JOB_ACCEPTED_STATUS, job)))
}

/// The start of a round: the answer is the one `loop/start` gives (202 started, 400/409 refusals of the start itself).
fn run_follow_up(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| {
        let context = context_of(app, request)?;
        app.loop_status().ensure_repos_free(&context)?;
        app.follow_up().run_follow_up(&context).map(reply)
    })
}

// ------------------------------------------------ reopen planning
fn check_reopen(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.planning_reopen().check_reopen(&context_of(app, request)?)))
}

fn reopen_planning(app: &App, request: &Request) -> Response {
    respond_json(|| app.planning_reopen().reopen(&context_of(app, request)?, &request.body))
}

// ------------------------------------------------ repo readiness and standards
fn describe_repos_readiness(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.repo_readiness().describe_repos(&context_of(app, request)?)))
}

fn set_repo_conformance(app: &App, request: &Request) -> Response {
    respond_json(|| app.repo_readiness().set_conformance(&context_of(app, request)?, name_param(request), request.text("value")))
}

fn map_repo(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| app.repo_readiness().map_repo(&context_of(app, request)?, name_param(request)).map(|job| Response::json(JOB_ACCEPTED_STATUS, job)))
}

fn standards_repo(request: &Request) -> Option<&str> {
    Some(request.param("name")).filter(|n| !n.is_empty())
}

fn get_standards(app: &App, request: &Request) -> Response {
    respond_json(|| app.standards().get_repo(&context_of(app, request)?, standards_repo(request)))
}

fn set_standards(app: &App, request: &Request) -> Response {
    respond_json(|| app.standards().put_repo(&context_of(app, request)?, standards_repo(request), &request.value("bindings")))
}

// ------------------------------------------------ quarantine
fn with_context_after_planning(app: &App, request: &Request, action: impl FnOnce(&ProjectContext) -> Response) -> Response {
    respond_with_project_context(app, request, true, action)
}

fn get_quarantine_detail(app: &App, request: &Request) -> Response {
    respond_with_project_context(app, request, false, |context| reply(app.quarantine().get_detail(context, request.param("slug"))))
}

fn start_quarantine_summary(app: &App, request: &Request) -> Response {
    with_context_after_planning(app, request, |context| reply(app.quarantine().start_summary(context, request.param("slug"))))
}

fn retry_quarantined(app: &App, request: &Request) -> Response {
    with_context_after_planning(app, request, |context| guard_repos_free(app, context, || reply(app.quarantine().retry_feature(context, request.param("slug"), &request.body))))
}

fn skip_quarantined(app: &App, request: &Request) -> Response {
    with_context_after_planning(app, request, |context| guard_repos_free(app, context, || reply(app.quarantine().skip_feature(context, request.param("slug")))))
}

fn revert_quarantined(app: &App, request: &Request) -> Response {
    with_context_after_planning(app, request, |context| reply(app.quarantine().revert_files(context, request.param("slug"), &request.body)))
}
