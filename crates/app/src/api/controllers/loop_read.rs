// SPDX-License-Identifier: MIT
//! GET routes that read a project's loop (`/p/<project_id>/api/...`): health, state, history, agent log, turn, graph, errors, pipeline, feature and
//! codebase maps, plus the AI decisions routes. No token for the reads (read only). Unknown project = 404 `{"ok":false,"error":"project not found"}`.

use super::route;
use crate::api::responses::{parse_unsigned, respond_json, respond_with_project_context};
use crate::api::router::{Request, Response, Router};
use crate::domain::project::ProjectContext;
use crate::services::App;
use serde_json::json;
use std::sync::Arc;

const TURN_WITHOUT_SNAPSHOT_MESSAGE: &str = "turn without snapshot";
const FEATURE_NOT_FOUND_MESSAGE: &str = "feature not found";
const INVALID_TURN_MESSAGE: &str = "turn must be a non-negative integer";

pub fn register(router: &mut Router, app: &Arc<App>) {
    let prefix = "/p/{project_id}/api";
    route(router, app, "GET", &format!("{prefix}/health"), get_health);
    route(router, app, "GET", &format!("{prefix}/state"), get_state);
    route(router, app, "GET", &format!("{prefix}/history"), get_history);
    route(router, app, "GET", &format!("{prefix}/agent/{{agent_id*}}/log"), get_agent_log); // raw id (may contain ':')
    route(router, app, "GET", &format!("{prefix}/turn/{{number}}"), get_turn);
    route(router, app, "GET", &format!("{prefix}/graph"), get_graph);
    route(router, app, "GET", &format!("{prefix}/errors"), get_errors);
    route(router, app, "GET", &format!("{prefix}/pipeline"), get_pipeline);
    route(router, app, "GET", &format!("{prefix}/feature/{{slug}}"), get_feature_detail);
    route(router, app, "GET", &format!("{prefix}/codebase/tree"), get_codebase_tree);
    route(router, app, "GET", &format!("{prefix}/codebase/file"), get_codebase_file);
    // DecisionsController
    route(router, app, "GET", &format!("{prefix}/decisions"), list_decisions);
    route(router, app, "POST", &format!("{prefix}/decisions/{{decision_id}}/review"), mark_reviewed);
    route(router, app, "POST", &format!("{prefix}/decisions/{{decision_id}}/change"), request_change);
}

fn with_context(app: &App, request: &Request, action: impl FnOnce(&ProjectContext) -> Response) -> Response {
    respond_with_project_context(app, request, false, action)
}

/// URL of this project's panel from the Host already validated by the server (127.0.0.1:<port> or localhost:<port>).
fn build_project_url(request: &Request, context: &ProjectContext) -> Option<String> {
    let host = request.header("host").trim().to_string();
    if host.is_empty() {
        None
    } else {
        Some(format!("http://{host}/p/{}/", context.project.id))
    }
}

fn get_codebase_tree(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| Response::json(200, app.loop_panel().build_codebase_tree(context)))
}

fn get_codebase_file(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| respond_json(|| app.loop_panel().read_codebase_file(context, &request.get_query("path"))))
}

fn get_errors(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| Response::json(200, app.error_center().build_errors(context)))
}

fn get_pipeline(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| Response::json(200, app.pipeline().build_pipeline(context)))
}

fn get_feature_detail(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| match app.feature_detail().build_feature_detail(context, request.param("slug")) {
        Some(detail) => Response::json(200, detail),
        None => Response::error(404, FEATURE_NOT_FOUND_MESSAGE),
    })
}

fn get_health(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| Response::json(200, json!({"ok": true, "repo": context.repo_path(), "work": context.work_dir(), "pid": std::process::id()})))
}

fn get_state(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| {
        let url = build_project_url(request, context);
        Response::json(200, app.loop_panel().build_state(context, url.as_deref()))
    })
}

fn get_history(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| Response::json(200, app.agent_history().build_history(context)))
}

fn get_agent_log(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| Response::json(200, app.agent_history().get_agent_log(context, request.param("agent_id"))))
}

fn get_turn(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| {
        let detail = parse_unsigned(request.param("number")).and_then(|n| app.turns().get_turn_detail(context, n as u64));
        match detail {
            Some(detail) => Response::json(200, detail),
            None => Response::error(404, TURN_WITHOUT_SNAPSHOT_MESSAGE),
        }
    })
}

fn get_graph(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| Response::json(200, app.loop_panel().build_graph(context)))
}

// ------------------------------------------------ decisions
fn list_decisions(app: &App, request: &Request) -> Response {
    let raw_turn = request.get_query("turn");
    let turn = if raw_turn.is_empty() {
        None
    } else {
        match parse_unsigned(&raw_turn) {
            Some(n) => Some(n),
            None => return Response::error(400, INVALID_TURN_MESSAGE),
        }
    };
    with_context(app, request, |context| Response::json(200, app.decisions().list_decisions(context, turn)))
}

fn mark_reviewed(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| respond_json(|| app.decisions().mark_reviewed(context, request.param("decision_id"))))
}

fn request_change(app: &App, request: &Request) -> Response {
    with_context(app, request, |context| respond_json(|| app.decisions().request_change(context, request.param("decision_id"))))
}
