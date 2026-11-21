// SPDX-License-Identifier: MIT
//! Routes of Settings v3 (`/api/app/settings/{providers,notifications,agents,quality}`, `/api/app/onboarding`). The POSTs require the token
//! (router default). No route ever returns a secret: tokens, keys, passwords and webhook URLs come back masked.

use super::route;
use crate::api::responses::respond_json;
use crate::api::router::{Request, Response, Router};
use crate::services::App;
use std::sync::Arc;

pub fn register(router: &mut Router, app: &Arc<App>) {
    route(router, app, "GET", "/api/app/settings/providers", |app, _| Response::json(200, app.settings_v3().list_providers()));
    route(router, app, "POST", "/api/app/settings/providers", save_providers);
    route(router, app, "POST", "/api/app/settings/providers/{provider_id}/secret", save_provider_secret);
    route(router, app, "POST", "/api/app/settings/providers/{provider_id}/test", test_provider);
    route(router, app, "GET", "/api/app/settings/notifications", |app, _| Response::json(200, app.settings_v3().get_notifications()));
    route(router, app, "POST", "/api/app/settings/notifications", save_notifications);
    route(router, app, "GET", "/api/app/settings/agents", |app, _| Response::json(200, app.settings_v3().list_agents()));
    route(router, app, "POST", "/api/app/settings/agents", save_agents);
    route(router, app, "GET", "/api/app/settings/quality", |app, _| Response::json(200, app.settings_v3().get_quality()));
    route(router, app, "POST", "/api/app/settings/quality", save_quality);
    route(router, app, "GET", "/api/app/onboarding", |app, _| Response::json(200, app.settings_v3().get_onboarding()));
    route(router, app, "POST", "/api/app/onboarding", save_onboarding);
}

fn save_providers(app: &App, request: &Request) -> Response {
    respond_json(|| app.settings_v3().save_providers(&request.body))
}

fn save_provider_secret(app: &App, request: &Request) -> Response {
    respond_json(|| app.settings_v3().save_provider_secret(request.param("provider_id"), request.text("value")))
}

fn test_provider(app: &App, request: &Request) -> Response {
    respond_json(|| app.settings_v3().test_provider(request.param("provider_id")))
}

fn save_notifications(app: &App, request: &Request) -> Response {
    respond_json(|| app.settings_v3().save_notifications(&request.body))
}

fn save_agents(app: &App, request: &Request) -> Response {
    respond_json(|| app.settings_v3().save_agents(&request.body))
}

fn save_quality(app: &App, request: &Request) -> Response {
    respond_json(|| app.settings_v3().save_quality(&request.body))
}

fn save_onboarding(app: &App, request: &Request) -> Response {
    respond_json(|| app.settings_v3().save_onboarding(&request.body))
}
