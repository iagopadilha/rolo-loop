// SPDX-License-Identifier: MIT
//! The app's HTTP server: 127.0.0.1 only, Host check (DNS rebinding), token on the methods that change state, body limit per route and serving of the
//! front end (SPA) with the token injected. It knows no business rule: everything goes through the [`Router`].
//!
//! Plain `std::net`: one thread per connection. A successful GET without a body keeps the connection open for the next request (HTTP/1.1
//! keep-alive, idle [`KEEP_ALIVE_IDLE_SECONDS`]): the panel polls every couple of seconds, and one connection per request leaves hundreds of
//! sockets in TIME_WAIT (Windows then refuses new ones: ERR_NO_BUFFER_SPACE). Anything else (a body, an error, `Connection: close`) is answered
//! with `Connection: close` and closed gracefully, as before.

use super::router::{Body, Found, Request, Response, Router};
use crate::services::App;
use pycompat::{unquote, Re};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

const BODY_DISCARD_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const HEADER_LIMIT_BYTES: usize = 64 * 1024;
const SOCKET_TIMEOUT_SECONDS: u64 = 30;
/// How long a kept-alive connection waits for its next request.
const KEEP_ALIVE_IDLE_SECONDS: u64 = 5;
/// Requests served on one connection before it is closed anyway.
const KEEP_ALIVE_MAX_REQUESTS: usize = 500;
/// "/" (hub) and "/p/<id>/" (project panel) serve the same index.html.
static SPA_PAGE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^/(p/[^/]+/?)?\z"));
static TOKEN_TAG: LazyLock<Re> = LazyLock::new(|| Re::ci(r#"<meta\s+name=["']monitor-token["'][^>]*>"#));
static HEAD_CLOSE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"</head>"));

pub fn inject_token_into_page(html: &str, token: &str) -> String {
    let tag = format!("<meta name=\"monitor-token\" content=\"{token}\">");
    if let Some(found) = TOKEN_TAG.search(html) {
        return format!("{}{tag}{}", &html[..found.start], &html[found.end..]);
    }
    match HEAD_CLOSE.search(html) {
        Some(found) => format!("{}  {tag}\n</head>{}", &html[..found.start], &html[found.end..]),
        None => format!("{tag}{html}"),
    }
}

/// `urllib.parse.parse_qs`: percent-decoded, `+` as space, blank values dropped, keys in order of first appearance.
pub fn parse_query(query: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for pair in query.split(['&', ';']).filter(|p| !p.is_empty()) {
        let Some((key, value)) = pair.split_once('=') else { continue };
        let (key, value) = (unquote(&key.replace('+', " ")), unquote(&value.replace('+', " ")));
        if value.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some((_, values)) => values.push(value),
            None => out.push((key, vec![value])),
        }
    }
    out
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

fn mime_of(path: &Path) -> String {
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let guessed = match extension.as_str() {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/vnd.microsoft.icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "xml" => "text/xml",
        _ => "application/octet-stream",
    };
    if guessed.starts_with("text/") || guessed == "application/json" || guessed == "image/svg+xml" {
        format!("{guessed}; charset=utf-8")
    } else {
        guessed.to_string()
    }
}

/// Constant-time comparison of two texts (the token never leaks through the time of a failed check).
fn same_text(first: &str, second: &str) -> bool {
    let (a, b) = (first.as_bytes(), second.as_bytes());
    let mut difference = a.len() ^ b.len();
    for index in 0..a.len().max(b.len()) {
        difference |= (*a.get(index).unwrap_or(&0) ^ *b.get(index).unwrap_or(&0)) as usize;
    }
    difference == 0
}

pub struct AppHttpServer {
    pub app: Arc<App>,
    pub router: Arc<Router>,
    pub frontend_dir: String,
    pub token: String,
    pub port: Mutex<u16>,
    pub url: Mutex<String>,
    listener: Mutex<Option<TcpListener>>,
    stopping: AtomicBool,
}

impl AppHttpServer {
    pub fn new(app: Arc<App>, router: Router, frontend_dir: &str, token: Option<String>) -> Arc<Self> {
        let token = token.unwrap_or_else(|| winproc::random_bytes(32).iter().map(|b| format!("{b:02x}")).collect());
        Arc::new(AppHttpServer {
            app,
            router: Arc::new(router),
            frontend_dir: pycompat::nt_abspath(frontend_dir),
            token,
            port: Mutex::new(0),
            url: Mutex::new(String::new()),
            listener: Mutex::new(None),
            stopping: AtomicBool::new(false),
        })
    }

    /// Binds the first free port of the range (or the fixed one) on 127.0.0.1 and returns the URL.
    pub fn open_port(&self, port_range: (u16, u16), fixed_port: u16) -> std::io::Result<String> {
        let ports: Vec<u16> = if fixed_port != 0 { vec![fixed_port] } else { (port_range.0..=port_range.1).collect() };
        for port in ports {
            if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
                let bound = listener.local_addr().map(|a| a.port()).unwrap_or(port);
                *self.port.lock().unwrap_or_else(|e| e.into_inner()) = bound;
                // the bound address: `localhost` tries ::1 first, and on Windows a refused connect costs 2 s
                let url = format!("http://127.0.0.1:{bound}/");
                *self.url.lock().unwrap_or_else(|e| e.into_inner()) = url.clone();
                *self.listener.lock().unwrap_or_else(|e| e.into_inner()) = Some(listener);
                return Ok(url);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            if fixed_port != 0 { format!("port {fixed_port} is busy") } else { format!("no free port in {}..{}", port_range.0, port_range.1) },
        ))
    }

    pub fn port(&self) -> u16 {
        *self.port.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn url(&self) -> String {
        self.url.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Serves until `request_shutdown`; every connection runs in its own thread.
    pub fn serve_until_stopped(self: &Arc<Self>) {
        let Some(listener) = self.listener.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        for connection in listener.incoming() {
            if self.stopping.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = connection else { continue };
            let server = self.clone();
            let _ = std::thread::Builder::new().name("http".into()).spawn(move || server.handle_connection(stream));
        }
    }

    /// Ends `serve_until_stopped`: the flag is set and a throw-away connection wakes the accept loop.
    pub fn request_shutdown(&self) {
        if !self.stopping.swap(true, Ordering::SeqCst) {
            let port = self.port();
            let _ = TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_secs(2));
        }
    }

    fn handle_connection(&self, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(SOCKET_TIMEOUT_SECONDS)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(SOCKET_TIMEOUT_SECONDS)));
        let _ = stream.set_nodelay(true);
        for served in 1..=KEEP_ALIVE_MAX_REQUESTS {
            let Some((method, target, headers, leftover)) = read_head(&mut stream) else { return };
            let reusable = can_keep_alive(&method, &headers, &leftover) && served < KEEP_ALIVE_MAX_REQUESTS && !self.stopping.load(Ordering::SeqCst);
            let response = self.serve_request(&mut stream, &method, &target, headers, leftover);
            let keep = reusable && response.status < 400;
            self.send(&mut stream, response, keep);
            if !keep {
                break;
            }
            // the next request of this connection, or the end of it after a short idle
            let _ = stream.set_read_timeout(Some(Duration::from_secs(KEEP_ALIVE_IDLE_SECONDS)));
        }
        // Graceful close: the answer goes first (FIN), then whatever the client still sends is read and dropped. Closing with unread data would send a
        // reset, and on Windows a reset can overtake the answer (an early 403 or 413 would then never reach the client).
        let _ = stream.shutdown(Shutdown::Write);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut drained = 0usize;
        let mut buffer = [0u8; 65536];
        while drained < BODY_DISCARD_LIMIT_BYTES {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => drained += count,
            }
        }
    }

    fn send(&self, stream: &mut TcpStream, response: Response, keep_alive: bool) {
        let (body, content_type) = match response.body {
            Body::Bytes(bytes, content_type) => (bytes, content_type),
            Body::Json(value) => (pycompat::json::py_json_dumps(&value, false).into_bytes(), "application/json; charset=utf-8".to_string()),
        };
        let head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{}\r\n",
            response.status,
            reason(response.status),
            body.len(),
            if keep_alive { format!("Connection: keep-alive\r\nKeep-Alive: timeout={KEEP_ALIVE_IDLE_SECONDS}\r\n") } else { "Connection: close\r\n".to_string() }
        );
        let _ = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(&body)).and_then(|_| stream.flush());
    }

    fn is_host_allowed(&self, headers: &[(String, String)]) -> bool {
        let host = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("host")).map(|(_, v)| v.trim().to_lowercase()).unwrap_or_default();
        let port = self.port();
        host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
    }

    fn record_activity(&self) {
        if let Some(monitor) = self.app.activity.get() {
            monitor.record_activity();
        }
    }

    fn serve_request(&self, stream: &mut TcpStream, method: &str, target: &str, headers: Vec<(String, String)>, leftover: Vec<u8>) -> Response {
        if !self.is_host_allowed(&headers) {
            return Response::error(403, "invalid Host");
        }
        let (raw_path, raw_query) = target.split_once('?').map(|(p, q)| (p, q.split('#').next().unwrap_or(""))).unwrap_or((target.split('#').next().unwrap_or(""), ""));
        let path = unquote(raw_path);
        let query = parse_query(raw_query);
        if method == "GET" && SPA_PAGE.match_start(&path).is_some() {
            return self.serve_spa_page();
        }
        if method == "GET" {
            if let Some(relative) = path.strip_prefix("/static/") {
                return self.serve_static_file(relative);
            }
        }
        let (route, params) = match self.router.find_route(method, &path) {
            Found::Route(route, params) => (route, params),
            Found::MethodNotAllowed => return Response::error(405, "method not allowed"),
            Found::NotFound => return Response::error(404, "route not found"),
        };
        if route.counts_as_activity || (method == "GET" && query.iter().any(|(k, v)| k == "visible" && v.first().map(String::as_str) == Some("1"))) {
            self.record_activity();
        }
        let header = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).unwrap_or_default();
        if route.requires_token && !same_text(&header("X-Monitor-Token"), &self.token) {
            return Response::error(403, "missing or invalid token");
        }
        let body = match read_json_body(stream, method, &header("Content-Length"), route.body_limit_bytes, leftover) {
            Ok(body) => body,
            Err(response) => return response,
        };
        let request = Request { method: method.to_string(), path, route_params: params, query, body, headers };
        // the API never brings the server down; the front end shows the banner
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (route.handler)(&request))) {
            Ok(response) => response,
            Err(_) => {
                self.app.log.error(
                    "route_failed",
                    &[("route", Value::from(route.pattern.clone())), ("type", Value::from("panic")), ("message", Value::from("the handler panicked"))],
                );
                Response::error(500, "internal error: panic")
            }
        }
    }

    fn serve_spa_page(&self) -> Response {
        self.record_activity();
        let Ok(bytes) = std::fs::read(Path::new(&self.frontend_dir).join("index.html")) else {
            return Response::error(404, "frontend/index.html does not exist yet");
        };
        // read as text, like the reference does: universal newlines (CRLF and a lone CR become LF)
        let html = String::from_utf8_lossy(&bytes).replace("\r\n", "\n").replace('\r', "\n");
        Response::bytes(200, inject_token_into_page(&html, &self.token).into_bytes(), "text/html; charset=utf-8")
    }

    fn serve_static_file(&self, relative: &str) -> Response {
        let root = pycompat::nt_normcase(&pycompat::nt_realpath(&self.frontend_dir));
        let target_path = pycompat::nt_realpath(&Path::new(&self.frontend_dir).join(relative).display().to_string());
        let target = pycompat::nt_normcase(&target_path);
        let inside = target == root || target.starts_with(&format!("{}\\", root.trim_end_matches('\\')));
        if !inside || !Path::new(&target_path).is_file() {
            return Response::error(404, "file not found");
        }
        match std::fs::read(&target_path) {
            Ok(bytes) => Response::bytes(200, bytes, &mime_of(Path::new(&target_path))),
            Err(_) => Response::error(404, "file not found"),
        }
    }
}

/// A request after which the same connection can carry the next one: a GET without a body (nothing unread on the socket) and no
/// `Connection: close`.
fn can_keep_alive(method: &str, headers: &[(String, String)], leftover: &[u8]) -> bool {
    let header = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_ascii_lowercase());
    method == "GET"
        && leftover.is_empty()
        && !matches!(header("content-length"), Some(v) if v != "0")
        && header("transfer-encoding").is_none()
        && !matches!(header("connection"), Some(v) if v.contains("close"))
}

/// `(method, target, headers, bytes already read past the head)`; `None` when the client sent nothing usable.
fn read_head(stream: &mut TcpStream) -> Option<(String, String, Vec<(String, String)>, Vec<u8>)> {
    let mut data: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 8192];
    let end = loop {
        if let Some(position) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break position;
        }
        if data.len() > HEADER_LIMIT_BYTES {
            return None;
        }
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return None,
            Ok(count) => data.extend_from_slice(&buffer[..count]),
        }
    };
    let head = String::from_utf8_lossy(&data[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split_whitespace();
    let (method, target) = (request_line.next()?.to_uppercase(), request_line.next()?.to_string());
    let headers = lines.filter_map(|line| line.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    Some((method, target, headers, data[end + 4..].to_vec()))
}

fn read_exact_into(stream: &mut TcpStream, mut already: Vec<u8>, size: usize) -> Vec<u8> {
    let mut buffer = [0u8; 65536];
    while already.len() < size {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => already.extend_from_slice(&buffer[..count]),
        }
    }
    already.truncate(size);
    already
}

fn read_json_body(stream: &mut TcpStream, method: &str, content_length: &str, limit_bytes: usize, leftover: Vec<u8>) -> Result<Value, Response> {
    if method == "GET" {
        return Ok(serde_json::json!({}));
    }
    let size: usize = if content_length.trim().is_empty() { 0 } else { content_length.trim().parse().map_err(|_| Response::error(400, "invalid Content-Length"))? };
    if size > limit_bytes {
        // discard the body (with a cap) so the client gets the 413; without reading it all, Windows resets the connection before the response
        let mut remaining = size.min((4 * limit_bytes).max(BODY_DISCARD_LIMIT_BYTES)).saturating_sub(leftover.len());
        let mut buffer = [0u8; 65536];
        while remaining > 0 {
            match stream.read(&mut buffer[..remaining.min(65536)]) {
                Ok(0) | Err(_) => break,
                Ok(count) => remaining -= count,
            }
        }
        return Err(Response::error(413, format!("body larger than {} KB", limit_bytes / 1024)));
    }
    let raw = if size > 0 { read_exact_into(stream, leftover, size) } else { Vec::new() };
    let text = String::from_utf8(raw).map_err(|_| Response::error(400, "body is not valid JSON"))?;
    if text.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => Ok(value),
        Ok(_) => Err(Response::error(400, "body must be a JSON object")),
        Err(_) => Err(Response::error(400, "body is not valid JSON")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_is_injected_in_the_page() {
        assert_eq!(inject_token_into_page("<head><meta name=\"monitor-token\" content=\"x\"></head>", "T"), "<head><meta name=\"monitor-token\" content=\"T\"></head>");
        assert_eq!(inject_token_into_page("<html><HEAD></HEAD>", "T"), "<html><HEAD>  <meta name=\"monitor-token\" content=\"T\">\n</head>");
        assert_eq!(inject_token_into_page("<p>x</p>", "T"), "<meta name=\"monitor-token\" content=\"T\"><p>x</p>");
    }

    #[test]
    fn queries_follow_parse_qs() {
        assert_eq!(
            parse_query("a=1&a=2&b=&c=x%20y+z&flag"),
            vec![("a".to_string(), vec!["1".to_string(), "2".to_string()]), ("c".to_string(), vec!["x y z".to_string()])]
        );
        assert!(same_text("abc", "abc") && !same_text("abc", "abd") && !same_text("abc", "ab"));
    }

    #[test]
    fn the_server_answers_over_a_real_socket() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        std::fs::create_dir_all(dir.path().join("front")).unwrap();
        std::fs::write(dir.path().join("front/index.html"), "<html><head></head></html>").unwrap();
        std::fs::write(dir.path().join("front/app.js"), "x").unwrap();
        let mut router = Router::new();
        router.add("GET", "/api/ping", |_| Response::json(200, serde_json::json!({"ok": true})));
        router.add("POST", "/api/echo", |request| Response::json(200, request.body.clone()));
        let server = AppHttpServer::new(app, router, &dir.path().join("front").display().to_string(), Some("secret".into()));
        server.open_port((38000, 38100), 0).unwrap();
        let runner = server.clone();
        let handle = std::thread::spawn(move || runner.serve_until_stopped());
        let port = server.port();
        let ask = |raw: String| -> String {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream.write_all(raw.as_bytes()).unwrap();
            let mut out = String::new();
            stream.read_to_string(&mut out).unwrap();
            out
        };
        let get = |path: &str, host: &str| ask(format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"));
        let local = format!("127.0.0.1:{port}");
        assert!(get("/api/ping", &local).contains("200 OK") && get("/api/ping", &local).ends_with("{\"ok\": true}"));
        assert!(get("/api/ping", "evil.example:80").contains("403 Forbidden") && get("/api/ping", "evil.example:80").contains("invalid Host"));
        assert!(get("/api/nope", &local).contains("404") && get("/", &local).contains("monitor-token\" content=\"secret\""));
        assert!(
            get("/p/abc/", &local).contains("text/html")
                && get("/static/app.js", &local).contains("text/javascript")
                && get("/static/..%5Cindex.html", &local).contains("404")
        );
        let post = |token: &str, body: &str| {
            ask(format!("POST /api/echo HTTP/1.1\r\nHost: {local}\r\nX-Monitor-Token: {token}\r\nContent-Length: {}\r\n\r\n{body}", body.len()))
        };
        assert!(post("wrong", "{}").contains("403 Forbidden"));
        assert!(post("secret", "{\"a\": 1}").ends_with("{\"a\": 1}"));
        assert!(post("secret", "[1]").contains("body must be a JSON object") && post("secret", "{oops").contains("body is not valid JSON"));
        assert!(ask(format!("DELETE /api/ping HTTP/1.1\r\nHost: {local}\r\nX-Monitor-Token: secret\r\n\r\n")).contains("405"));
        assert!(post("secret", &format!("{{\"k\": \"{}\"}}", "x".repeat(70_000))).contains("413"));
        // keep-alive: two GETs on one connection, each answer read by its Content-Length; a POST closes it
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let read_answer = |stream: &mut TcpStream| -> String {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                head.push(byte[0]);
            }
            let head = String::from_utf8(head).unwrap();
            let length: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).unwrap().trim().parse().unwrap();
            let mut body = vec![0u8; length];
            stream.read_exact(&mut body).unwrap();
            head + &String::from_utf8(body).unwrap()
        };
        for _ in 0..2 {
            stream.write_all(format!("GET /api/ping HTTP/1.1\r\nHost: {local}\r\n\r\n").as_bytes()).unwrap();
            let answer = read_answer(&mut stream);
            assert!(answer.contains("Connection: keep-alive") && answer.ends_with("{\"ok\": true}"));
        }
        stream.write_all(format!("POST /api/echo HTTP/1.1\r\nHost: {local}\r\nX-Monitor-Token: secret\r\nContent-Length: 2\r\n\r\n{{}}").as_bytes()).unwrap();
        assert!(read_answer(&mut stream).contains("Connection: close"));
        let mut rest = String::new();
        assert_eq!(stream.read_to_string(&mut rest).unwrap_or(0), 0);
        server.request_shutdown();
        handle.join().unwrap();
    }
}
