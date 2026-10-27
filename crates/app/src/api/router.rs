// SPDX-License-Identifier: MIT
//! Minimal HTTP router. Each controller registers its routes in `register(router, app)`.
//!
//! Route pattern with parameters in braces: `/p/{project_id}/api/state`. A parameter matches one segment (no `/`), except when it ends in `*`
//! (`{path*}`), which matches the rest of the path.

use pycompat::{escape, Re};
use serde_json::{json, Map, Value};

pub const DEFAULT_BODY_LIMIT_BYTES: usize = 64 * 1024;

pub struct Request {
    pub method: String,
    pub path: String,
    pub route_params: Vec<(String, String)>,
    /// `parse_qs`: key -> values, blank values dropped.
    pub query: Vec<(String, Vec<String>)>,
    /// The JSON object of the body (`{}` for a GET).
    pub body: Value,
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn param(&self, name: &str) -> &str {
        self.route_params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("")
    }

    /// First value of a query key, `""` when absent.
    pub fn get_query(&self, key: &str) -> String {
        self.query_first(key).unwrap_or_default()
    }

    pub fn query_first(&self, key: &str) -> Option<String> {
        self.query.iter().find(|(k, _)| k == key).and_then(|(_, values)| values.first().cloned())
    }

    pub fn header(&self, name: &str) -> String {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).unwrap_or_default()
    }

    /// `request.body.get(key)`.
    pub fn field(&self, key: &str) -> Option<&Value> {
        self.body.get(key)
    }

    /// `request.body.get(key)`, `None` when absent or `null` (Python's `None` either way).
    pub fn opt(&self, key: &str) -> Option<&Value> {
        self.body.get(key).filter(|v| !v.is_null())
    }

    /// `request.body.get(key)` as text; `""` when absent or not text.
    pub fn text(&self, key: &str) -> &str {
        self.body.get(key).and_then(Value::as_str).unwrap_or("")
    }

    /// `request.body.get(key)` as a value (`null` when absent).
    pub fn value(&self, key: &str) -> Value {
        self.body.get(key).cloned().unwrap_or(Value::Null)
    }

    pub fn body_map(&self) -> &Map<String, Value> {
        static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
        self.body.as_object().unwrap_or_else(|| EMPTY.get_or_init(Map::new))
    }
}

pub enum Body {
    Json(Value),
    Bytes(Vec<u8>, String),
}

pub struct Response {
    pub status: u16,
    pub body: Body,
}

impl Response {
    pub fn json(status: u16, data: Value) -> Self {
        Response { status, body: Body::Json(data) }
    }

    pub fn error(status: u16, message: impl Into<String>) -> Self {
        Self::error_with(status, message, &[])
    }

    pub fn error_with(status: u16, message: impl Into<String>, extras: &[(&str, Value)]) -> Self {
        let mut body = Map::new();
        body.insert("ok".into(), json!(false));
        body.insert("error".into(), json!(message.into()));
        for (key, value) in extras {
            body.insert((*key).to_string(), value.clone());
        }
        Response::json(status, Value::Object(body))
    }

    pub fn bytes(status: u16, data: Vec<u8>, content_type: &str) -> Self {
        Response { status, body: Body::Bytes(data, content_type.to_string()) }
    }
}

impl From<(u16, Value)> for Response {
    fn from(pair: (u16, Value)) -> Self {
        Response::json(pair.0, pair.1)
    }
}

pub type Handler = Box<dyn Fn(&Request) -> Response + Send + Sync>;

pub struct Route {
    pub method: String,
    pub pattern: String,
    expression: Re,
    names: Vec<String>,
    pub handler: Handler,
    pub requires_token: bool,
    pub body_limit_bytes: usize,
    pub counts_as_activity: bool,
}

/// Options of a route that differ from the default (token and activity apply to every method but GET).
#[derive(Clone, Copy, Default)]
pub struct RouteOptions {
    pub body_limit_bytes: Option<usize>,
}

/// `(regex source, parameter names in order)` of a route pattern.
fn compile_route_pattern(pattern: &str) -> (String, Vec<String>) {
    let (mut source, mut names) = ("^".to_string(), Vec::new());
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}').map(|e| start + e) else { break };
        source.push_str(&escape(&rest[..start]));
        let name = &rest[start + 1..end];
        match name.strip_suffix('*') {
            Some(rest_name) => {
                source.push_str("(.+)");
                names.push(rest_name.to_string());
            }
            None => {
                source.push_str("([^/]+)");
                names.push(name.to_string());
            }
        }
        rest = &rest[end + 1..];
    }
    source.push_str(&escape(rest));
    source.push_str(r"\z");
    (source, names)
}

/// What `find_route` found: a route, a path that exists with other methods (405) or nothing (404).
pub enum Found<'a> {
    Route(&'a Route, Vec<(String, String)>),
    MethodNotAllowed,
    NotFound,
}

#[derive(Default)]
pub struct Router {
    pub routes: Vec<Route>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    /// `handler(request) -> Response`. Token and activity apply by default to POST/PUT/PATCH/DELETE.
    pub fn add(&mut self, method: &str, pattern: &str, handler: impl Fn(&Request) -> Response + Send + Sync + 'static) {
        self.add_with(method, pattern, handler, RouteOptions::default());
    }

    pub fn add_with(&mut self, method: &str, pattern: &str, handler: impl Fn(&Request) -> Response + Send + Sync + 'static, options: RouteOptions) {
        let mutating = !method.eq_ignore_ascii_case("GET");
        let (source, names) = compile_route_pattern(pattern);
        self.routes.push(Route {
            method: method.to_uppercase(),
            pattern: pattern.to_string(),
            expression: Re::new(&source),
            names,
            handler: Box::new(handler),
            requires_token: mutating,
            body_limit_bytes: options.body_limit_bytes.unwrap_or(DEFAULT_BODY_LIMIT_BYTES),
            counts_as_activity: mutating,
        });
    }

    pub fn find_route(&self, method: &str, path: &str) -> Found<'_> {
        let mut path_exists = false;
        for route in &self.routes {
            let Some(found) = route.expression.match_start(path) else { continue };
            if route.method.eq_ignore_ascii_case(method) {
                let params = route.names.iter().enumerate().map(|(i, name)| (name.clone(), found.group(i + 1).unwrap_or("").to_string())).collect();
                return Found::Route(route, params);
            }
            path_exists = true;
        }
        if path_exists {
            Found::MethodNotAllowed
        } else {
            Found::NotFound
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> Router {
        let mut router = Router::new();
        router.add("GET", "/p/{project_id}/api/state", |_| Response::json(200, json!({"a": 1})));
        router.add("GET", "/p/{project_id}/api/agent/{agent_id*}/log", |_| Response::json(200, json!({})));
        router.add("POST", "/p/{project_id}/api/state", |_| Response::json(200, json!({})));
        router.add_with("PUT", "/x", |_| Response::json(200, json!({})), RouteOptions { body_limit_bytes: Some(10) });
        router
    }

    #[test]
    fn parameters_match_one_segment_or_the_rest() {
        let router = router();
        let Found::Route(route, params) = router.find_route("GET", "/p/abc/api/state") else { panic!("route") };
        assert_eq!((route.requires_token, params), (false, vec![("project_id".to_string(), "abc".to_string())]));
        let Found::Route(_, params) = router.find_route("GET", "/p/abc/api/agent/v001:orch/log") else { panic!("route") };
        assert_eq!(params[1], ("agent_id".to_string(), "v001:orch".to_string()));
        assert!(matches!(router.find_route("GET", "/p/a/b/api/state"), Found::NotFound));
        assert!(matches!(router.find_route("DELETE", "/p/abc/api/state"), Found::MethodNotAllowed));
        let Found::Route(route, _) = router.find_route("POST", "/p/abc/api/state") else { panic!("route") };
        assert!(route.requires_token && route.counts_as_activity);
        let Found::Route(route, _) = router.find_route("PUT", "/x") else { panic!("route") };
        assert_eq!(route.body_limit_bytes, 10);
    }
}
