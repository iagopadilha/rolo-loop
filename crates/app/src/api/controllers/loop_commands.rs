// SPDX-License-Identifier: MIT
//! Loop command routes of a project (`/p/<project_id>/api/...`): answer, resume, note, pause, loop/check, loop/start, instruction, loop/settings,
//! loop/raise-limit, whiteboard and the legacy shutdown (a no-op: shutting down belongs to the app), the read-only chat and the Act mode. POST requires
//! the token (the server checks it). In planning mode every POST here except the legacy shutdown is refused with 409 `code:"planning_required"`.

use super::{route, route_with_limit};
use crate::api::responses::{guard_repos_free, parse_unsigned, respond_with_project_context};
use crate::api::router::{Request, Response, Router};
use crate::domain::project::ProjectContext;
use crate::services::App;
use serde_json::json;
use std::sync::Arc;

const WHITEBOARD_BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;

pub fn register(router: &mut Router, app: &Arc<App>) {
    let prefix = "/p/{project_id}/api";
    route(router, app, "POST", &format!("{prefix}/answer"), answer_question);
    route(router, app, "POST", &format!("{prefix}/resume"), resume_loop);
    route(router, app, "POST", &format!("{prefix}/note"), write_note);
    route(router, app, "POST", &format!("{prefix}/pause"), request_pause);
    route(router, app, "POST", &format!("{prefix}/loop/check"), check_preflight);
    route(router, app, "POST", &format!("{prefix}/loop/start"), start_loop);
    route(router, app, "POST", &format!("{prefix}/instruction"), send_instruction);
    route(router, app, "POST", &format!("{prefix}/loop/raise-limit"), raise_limit);
    route(router, app, "POST", &format!("{prefix}/approvals/{{approval_id}}/approve"), approve_request);
    route(router, app, "POST", &format!("{prefix}/approvals/{{approval_id}}/deny"), deny_request);
    route(router, app, "POST", &format!("{prefix}/policy"), set_policy);
    route(router, app, "POST", &format!("{prefix}/dependencies/apply"), apply_dependencies);
    route(router, app, "POST", &format!("{prefix}/scripts/answer"), answer_script);
    route(router, app, "POST", &format!("{prefix}/shutdown"), ignore_legacy_shutdown);
    route(router, app, "GET", &format!("{prefix}/loop/settings"), get_loop_settings);
    route(router, app, "GET", &format!("{prefix}/whiteboard"), get_whiteboard);
    route_with_limit(router, app, "POST", &format!("{prefix}/whiteboard"), save_whiteboard, WHITEBOARD_BODY_LIMIT_BYTES);
    // ChatController
    route(router, app, "POST", &format!("{prefix}/chat/send"), send_question);
    route(router, app, "GET", &format!("{prefix}/chat/job/{{job_id}}"), get_chat_job);
    route(router, app, "GET", &format!("{prefix}/chat/threads"), list_threads);
    route(router, app, "GET", &format!("{prefix}/chat/thread/{{thread_id}}"), get_thread);
    // ActionController
    route(router, app, "GET", &format!("{prefix}/actions"), list_actions);
    route(router, app, "POST", &format!("{prefix}/action/propose"), propose_action);
    route(router, app, "GET", &format!("{prefix}/action/job/{{job_id}}"), get_proposal_job);
    route(router, app, "POST", &format!("{prefix}/action/run"), run_action);
}

/// Loop commands and whiteboard writes: refused in planning mode.
fn after_planning(app: &App, request: &Request, action: impl FnOnce(&ProjectContext) -> Response) -> Response {
    respond_with_project_context(app, request, true, action)
}

fn reply(pair: (u16, serde_json::Value)) -> Response {
    Response::json(pair.0, pair.1)
}

fn answer_question(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| guard_repos_free(app, context, || reply(app.loop_control().answer_loop_question(context, &request.body))))
}

fn resume_loop(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| guard_repos_free(app, context, || reply(app.loop_control().resume_loop(context, Some(&request.body)))))
}

fn write_note(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| reply(app.loop_control().write_user_note(context, &request.body)))
}

fn request_pause(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| reply(app.loop_control().request_loop_pause(context)))
}

fn check_preflight(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| reply(app.loop_control().check_preflight(context, &request.body)))
}

fn start_loop(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| guard_repos_free(app, context, || reply(app.loop_control().start_new_loop(context, &request.body))))
}

/// Read-only (the Start dialog): works in any mode, planning included (the settings are decided there).
fn get_loop_settings(app: &App, request: &Request) -> Response {
    respond_with_project_context(app, request, false, |context| reply(app.loop_control().get_loop_settings(context)))
}

fn raise_limit(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| guard_repos_free(app, context, || reply(app.loop_control().raise_loop_limit(context, &request.body))))
}

/// Approve (once, or `{"always": true}` for this project) the command a pending card asks about. Production is never asked, so it has no card.
fn approve_request(app: &App, request: &Request) -> Response {
    respond_with_project_context(app, request, false, |context| reply(app.approvals().answer(context, request.param("approval_id"), true, &request.body)))
}

fn deny_request(app: &App, request: &Request) -> Response {
    respond_with_project_context(app, request, false, |context| reply(app.approvals().answer(context, request.param("approval_id"), false, &request.body)))
}

/// The simple per-project policy: the commands released without asking and the E2E DEV baseline opt-in.
fn set_policy(app: &App, request: &Request) -> Response {
    respond_with_project_context(app, request, false, |context| reply(app.approvals().set_policy(context, &request.body)))
}

fn apply_dependencies(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| reply(app.loop_control().apply_dependency_proposals(context)))
}

fn answer_script(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| reply(app.loop_control().answer_project_script(context, &request.body)))
}

fn send_instruction(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| reply(app.loop_control().send_instruction_to_loop(context, &request.body)))
}

/// Compatibility: the old front/engine called the panel's `/api/shutdown`; in the app, shutting down is `/api/app/shutdown`.
fn ignore_legacy_shutdown(app: &App, request: &Request) -> Response {
    respond_with_project_context(app, request, false, |_| Response::json(200, json!({"ok": true})))
}

fn get_whiteboard(app: &App, request: &Request) -> Response {
    respond_with_project_context(app, request, false, |context| reply(app.whiteboard().get_board(context)))
}

fn save_whiteboard(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| reply(app.whiteboard().save_board(context, &request.body)))
}

// ------------------------------------------------ chat (refused in planning mode: the planning screen has its own chat)
fn send_question(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| reply(app.chat().send_question(context, &request.body)))
}

fn get_chat_job(app: &App, request: &Request) -> Response {
    respond_with_project_context(app, request, false, |context| reply(app.chat().get_project_job(context, request.param("job_id"))))
}

fn list_threads(app: &App, request: &Request) -> Response {
    let agent_id = request.query_first("agent_id");
    let turn = parse_unsigned(&request.get_query("turn"));
    respond_with_project_context(app, request, false, |context| reply(app.chat().list_project_threads(context, agent_id.as_deref(), turn)))
}

fn get_thread(app: &App, request: &Request) -> Response {
    respond_with_project_context(app, request, false, |context| reply(app.chat().get_project_thread(context, request.param("thread_id"))))
}

// ------------------------------------------------ Act mode (the two POSTs are refused in planning mode)
fn list_actions(app: &App, request: &Request) -> Response {
    respond_with_project_context(app, request, false, |context| reply(app.actions().list_available_actions(context)))
}

fn propose_action(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| reply(app.actions().propose_action_from_text(context, &request.body)))
}

fn get_proposal_job(app: &App, request: &Request) -> Response {
    respond_with_project_context(app, request, false, |context| reply(app.chat().get_project_job(context, request.param("job_id"))))
}

fn run_action(app: &App, request: &Request) -> Response {
    after_planning(app, request, |context| reply(app.actions().run_confirmed_action(context, &request.body)))
}
