// SPDX-License-Identifier: MIT
//! One module per group of routes; each has `register(router, app)`. The registration order is the old composition's, so the first matching route is
//! the same one.

pub mod app_hub;
pub mod loop_commands;
pub mod loop_read;
pub mod multi_repo;
pub mod projects;

use super::router::{Request, Response, RouteOptions, Router};
use crate::services::App;
use std::sync::Arc;

/// A route whose handler is a plain function of the app and the request.
pub type RouteFn = fn(&App, &Request) -> Response;

pub fn route(router: &mut Router, app: &Arc<App>, method: &str, pattern: &str, handler: RouteFn) {
    let app = app.clone();
    router.add(method, pattern, move |request| handler(&app, request));
}

pub fn route_with_limit(router: &mut Router, app: &Arc<App>, method: &str, pattern: &str, handler: RouteFn, body_limit_bytes: usize) {
    let app = app.clone();
    router.add_with(method, pattern, move |request| handler(&app, request), RouteOptions { body_limit_bytes: Some(body_limit_bytes) });
}

/// Every route of the app, in the old composition's order.
pub fn build_router(app: &Arc<App>) -> Router {
    let mut router = Router::new();
    projects::register(&mut router, app);
    loop_read::register(&mut router, app);
    loop_commands::register(&mut router, app);
    multi_repo::register(&mut router, app);
    app_hub::register(&mut router, app);
    router
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The route table (method, pattern, token, activity, body limit) in registration order equals the one the old composition builds
    /// (`tests/fixtures/routes.txt`, dumped from the reference).
    #[test]
    fn the_route_table_equals_the_references() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let router = build_router(&app);
        let rows: Vec<String> = router.routes.iter().map(|r| format!("{} {} token={} activity={} limit={}", r.method, r.pattern, r.requires_token as u8, r.counts_as_activity as u8, r.body_limit_bytes)).collect();
        let expected: Vec<&str> = include_str!("../../../tests/fixtures/routes.txt").lines().collect();
        assert_eq!(rows.len(), expected.len(), "route count");
        for (index, (got, want)) in rows.iter().zip(expected.iter()).enumerate() {
            assert_eq!(got, want, "route #{index}");
        }
    }
}
