// SPDX-License-Identifier: MIT
//! Routes of the app hub (`/api/app/*`): health, info (diagnostics), test notification and shutdown, the Settings screen and its "MCP servers"
//! section. The POSTs require the token (router default). `info` never returns a variable value: only channel names and whether they are configured.

use super::route;
use crate::api::responses::{respond_json, response_of};
use crate::api::router::{Request, Response, Router};
use crate::services::App;
use serde_json::{json, Value};
use std::sync::Arc;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const SETTINGS_ROUTE: &str = "/api/app/settings";
const MCP_SERVERS_ROUTE: &str = "/api/app/settings/mcp-servers";

pub fn register(router: &mut Router, app: &Arc<App>) {
    route(router, app, "GET", "/api/app/health", get_health);
    route(router, app, "GET", "/api/app/info", get_diagnostics);
    route(router, app, "POST", "/api/app/notify/test", send_test_notification);
    route(router, app, "POST", "/api/app/shutdown", shut_down_app);
    route(router, app, "GET", SETTINGS_ROUTE, get_settings);
    route(router, app, "POST", SETTINGS_ROUTE, save_setting);
    route(router, app, "GET", MCP_SERVERS_ROUTE, get_mcp_servers);
    route(router, app, "POST", MCP_SERVERS_ROUTE, save_mcp_servers);
}

fn get_health(app: &App, _: &Request) -> Response {
    Response::json(200, json!({"ok": true, "pid": std::process::id(), "version": APP_VERSION, "url": app.app_url()}))
}

/// Work folder and repo folder of the most recently opened project: where the loop wrote the usage of each command (empty if none).
fn list_usage_folders(app: &App) -> Vec<String> {
    let projects = app.project_service().list_projects();
    let Some(latest) = projects.into_iter().max_by(|a, b| a.last_opened_at.cmp(&b.last_opened_at)) else { return vec![] };
    let context = crate::domain::project::ProjectContext::new(latest);
    let mut folders = vec![context.work_dir()];
    if !folders.contains(&context.repo_path().to_string()) {
        folders.push(context.repo_path().to_string());
    }
    folders
}

fn get_diagnostics(app: &App, _: &Request) -> Response {
    let mut diagnostics = app.diagnostics().build_diagnostics(&list_usage_folders(app));
    diagnostics["ok"] = json!(true);
    diagnostics["version"] = json!(APP_VERSION);
    diagnostics["pid"] = json!(std::process::id());
    diagnostics["url"] = json!(app.app_url());
    diagnostics["seconds_until_shutdown"] = json!(app.activity.get().and_then(|m| m.seconds_until_shutdown()));
    diagnostics["open_projects"] = json!(app.project_service().list_projects().len());
    Response::json(200, diagnostics)
}

fn send_test_notification(app: &App, request: &Request) -> Response {
    let channels: Vec<Value> = match request.field("channels") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(list)) if list.iter().all(Value::is_string) => list.clone(),
        Some(_) => return Response::error(400, "channels must be a list of names"),
    };
    let result = app.diagnostics().send_test_notification(&channels);
    let names: Vec<&str> = result["results"].as_object().map(|r| r.keys().map(String::as_str).collect()).unwrap_or_default();
    app.log.info("test_notification_sent", &[("ok", result["ok"].clone()), ("channels", json!(names.join(",")))]);
    let no_results = names.is_empty();
    if engine_state::notify_truthy(result.get("error")) && no_results {
        return Response::json(400, result);
    }
    Response::json(200, result)
}

fn shut_down_app(app: &App, _: &Request) -> Response {
    app.log.info("shutdown_requested", &[("origin", json!("api"))]);
    if let Some(shutdown) = app.shutdown.get() {
        shutdown();
    }
    Response::json(200, json!({"ok": true}))
}

// ------------------------------------------------ Settings: `{key, value, confirm: true}` or `{key, reset: true, confirm: true}`
fn get_settings(app: &App, _: &Request) -> Response {
    Response::json(200, app.app_settings().get_settings())
}

fn save_setting(app: &App, request: &Request) -> Response {
    if request.field("confirm") != Some(&Value::Bool(true)) {
        return Response::error(400, "confirm must be true");
    }
    let key = request.value("key");
    let wants_reset = request.field("reset") == Some(&Value::Bool(true));
    if !wants_reset && request.field("value").is_none() {
        return Response::error(400, "value is required (or reset: true)");
    }
    let outcome = if wants_reset { app.app_settings().reset_setting(&key) } else { app.app_settings().update_setting(&key, &request.value("value")) };
    match outcome {
        Ok(item) => {
            app.log.info("app_setting_saved", &[("key", key), ("reset", json!(wants_reset))]);
            Response::json(200, json!({"ok": true, "setting": item}))
        }
        Err(error) => response_of(&error),
    }
}

// ------------------------------------------------ MCP servers: `{servers, extra, confirm: true}` replaces both lists
fn get_mcp_servers(app: &App, request: &Request) -> Response {
    Response::json(200, app.mcp_servers().get_servers(&request.get_query("project")))
}

fn save_mcp_servers(app: &App, request: &Request) -> Response {
    if request.field("confirm") != Some(&Value::Bool(true)) {
        return Response::error(400, "confirm must be true");
    }
    let mut unknown: Vec<String> = request.body_map().keys().filter(|k| !matches!(k.as_str(), "servers" | "extra" | "confirm")).cloned().collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Response::error(400, format!("unknown field(s): {}", unknown.join(", ")));
    }
    let servers = request.field("servers").cloned().unwrap_or(json!({}));
    let extra = request.field("extra").cloned().unwrap_or(json!([]));
    respond_json(|| {
        let result = app.mcp_servers().save_servers(&servers, &extra)?;
        let count = |v: &Value| match v {
            Value::Object(m) => m.len(),
            Value::Array(a) => a.len(),
            _ => 0,
        };
        app.log.info("mcp_servers_saved", &[("servers", json!(count(&servers))), ("extra", json!(count(&extra)))]);
        Ok(result)
    })
}
