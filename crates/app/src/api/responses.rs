// SPDX-License-Identifier: MIT
//! Conversion of the domain errors into HTTP responses, in one place for every controller, plus the helpers the controllers share.

use super::router::{Request, Response};
use crate::domain::errors::{AppError, Kind, PLANNING_REQUIRED_CODE};
use crate::domain::project::{Project, ProjectContext};
use crate::services::App;
use serde_json::{json, Value};

pub const PROJECT_NOT_FOUND_MESSAGE: &str = "project not found";
pub const PROJECT_ROUTE_PREFIX: &str = "/p/{project_id}/api";

/// The HTTP status of each kind of error.
pub fn status_of(kind: Kind) -> u16 {
    match kind {
        Kind::ProjectNotFound | Kind::SpecFileNotFound | Kind::JobNotFound | Kind::RepoNotFound | Kind::DecisionNotFound => 404,
        Kind::InvalidPath
        | Kind::InvalidRequest
        | Kind::PathOutsideSpecs
        | Kind::FileEditRefused
        | Kind::InvalidRepo
        | Kind::UnknownCredentialName
        | Kind::InvalidSetting
        | Kind::SettingUnavailable => 400,
        Kind::SubprojectsNotConfirmed | Kind::PlanningGates => 422,
        Kind::Internal => 500,
        Kind::FileVersionConflict
        | Kind::LoopRunning
        | Kind::JobInProgress
        | Kind::RoadmapSessionMissing
        | Kind::LoopWorktreeExists
        | Kind::InvalidRoadmap
        | Kind::ProjectAlreadyExists
        | Kind::PlanningRequired
        | Kind::RepoConflict
        | Kind::RepoInUse
        | Kind::ConfigFileUnreadable => 409,
    }
}

/// An error as the response the old API gave: `{ok: false, error, ...extras, code?}`; a version conflict carries `current_version`, a planning gate
/// refusal carries `code` and `gates`.
pub fn response_of(error: &AppError) -> Response {
    match error.kind {
        Kind::ProjectNotFound => Response::error(404, PROJECT_NOT_FOUND_MESSAGE),
        Kind::FileVersionConflict => {
            Response::error_with(409, error.message.clone(), &[("current_version", error.current_version.as_ref().map_or(Value::Null, |v| json!(v)))])
        }
        Kind::PlanningGates => {
            let status = if error.code.as_deref() == Some(crate::domain::errors::PLANNING_PREREQUISITES_CODE) { 409 } else { 422 };
            Response::error_with(
                status,
                error.message.clone(),
                &[("code", error.code.as_ref().map_or(Value::Null, |c| json!(c))), ("gates", error.gates.clone().unwrap_or(Value::Null))],
            )
        }
        Kind::Internal => Response::error(500, format!("internal error: {}", error.message)),
        kind => {
            let mut extras: Vec<(&str, Value)> = error.extras.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
            if let Some(code) = error.code.as_ref().filter(|c| !c.is_empty()) {
                extras.retain(|(k, _)| *k != "code");
                extras.push(("code", json!(code)));
            }
            Response::error_with(status_of(kind), error.message.clone(), &extras)
        }
    }
}

/// Runs `action` and returns its Response; an error becomes the right status.
pub fn respond_converting_errors(action: impl FnOnce() -> Result<Response, AppError>) -> Response {
    action().unwrap_or_else(|error| response_of(&error))
}

/// `respond_converting_errors` for the common `200 + JSON` answer.
pub fn respond_json(action: impl FnOnce() -> Result<Value, AppError>) -> Response {
    respond_converting_errors(|| action().map(|value| Response::json(200, value)))
}

pub fn project_to_dict(project: &Project) -> Value {
    json!({
        "id": project.id, "name": project.name, "repo_path": project.repo_path, "opened_at": project.opened_at, "last_opened_at": project.last_opened_at,
        "favorite": project.favorite, "url": format!("/p/{}/", project.id),
    })
}

/// The context of the project in the route; an unknown project = 404.
pub fn context_of(app: &App, request: &Request) -> Result<ProjectContext, AppError> {
    app.project_service().get_project_context(request.param("project_id"))
}

/// Resolves the route's project and calls `action(context)`; an unknown project = 404 `project not found`. With `planning_check`, a project in
/// planning mode = 409 `code:"planning_required"` (nothing runs).
pub fn respond_with_project_context(app: &App, request: &Request, planning_check: bool, action: impl FnOnce(&ProjectContext) -> Response) -> Response {
    let context = match context_of(app, request) {
        Ok(context) => context,
        Err(_) => return Response::error(404, PROJECT_NOT_FOUND_MESSAGE),
    };
    if planning_check && app.planning_requirement().is_planning_required(&context) {
        return Response::error_with(409, crate::services::planning_requirement::PLANNING_REQUIRED_MESSAGE, &[("code", json!(PLANNING_REQUIRED_CODE))]);
    }
    action(&context)
}

/// Start-like commands (new loop, resume, answer = relaunch): refused with 409 `repo_in_use` when another project's running loop uses one of this
/// project's repos.
pub fn guard_repos_free(app: &App, context: &ProjectContext, action: impl FnOnce() -> Response) -> Response {
    match app.loop_status().ensure_repos_free(context) {
        Ok(()) => action(),
        Err(error) => response_of(&error),
    }
}

/// Query value as a non-negative integer (`isdigit()` rule); `None` for anything else.
pub fn parse_unsigned(text: &str) -> Option<i64> {
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
        text.parse().ok()
    } else {
        None
    }
}
