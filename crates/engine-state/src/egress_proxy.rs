// SPDX-License-Identifier: MIT
//! Run-time egress layer of the loop: a local proxy plus the language preloads, driven by the project's `network-allow.json`.
//!
//! The guard reads the TEXT of a command; a script can still build a URL at run time. This layer judges what the processes the agent starts
//! REALLY connect to, by host (HTTPS hides path and method from anything but the process):
//!   * [`EgressProxy`]  a proxy on 127.0.0.1:<free port>. `CONNECT host:port` passes only when the host (and the port, when the entry has one)
//!                      matches an effective allow entry, or a local host, and is not denied (a `tcp` entry matches host:port). An absolute-URI
//!                      plain HTTP request gets the full network check (host + path + method, `cred` when it carries Authorization/Cookie/userinfo)
//!                      and is forwarded. A deny answers `403` with a JSON body and one JSON line in `<data dir>/egress.log`. The list file is read
//!                      on every connection (cached by mtime/size): a list change applies without a restart, and a missing or broken file fails
//!                      closed for every non-local host.
//!   * `net_preload/`   `sitecustomize.py` (PYTHONPATH) and `preload.cjs` (NODE_OPTIONS=--require): libraries that ignore proxy variables are
//!                      refused too. They are payloads for the Python and Node processes of the user's project and ship as embedded assets.
//!   * [`acquire`]/[`release`]  one proxy per project folder (reference counted: the loop and the app's `claude -p` jobs); the port goes to
//!                      `<project>/.specs/project/loop/internal/network-runtime.json`, which the guard reads to build the variables of the commands.
//!   * [`layer_environment`]  the variables. They are delivered by the guard's PreToolUse `updatedInput` to Bash and PowerShell commands ONLY:
//!                      never put them in the environment of `claude` itself or of an MCP server (they would also hit the Anthropic API).
//!   * [`runtime_self_test`]  the probe for `doctor --access`.
//!
//! Residual: HTTPS through the proxy is judged by host only; .NET/Java/Go processes that ignore proxy variables are covered only by the
//! guard text rules.

use crate::loop_io::{iso_seconds, local_now, which};
use crate::network_policy;
use cfg::Env;
use egress_policy::{host_matches, Effective, ORIGIN_LOCAL};
use pycompat::json::{py_json_dumps, py_json_dumps_indent};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

pub const RUNTIME_FILE: &str = "network-runtime.json";
pub const ALLOW_FILE: &str = "network-allow.json";
pub const LOG_NAME: &str = "egress.log";
const LOCAL_NAMES: [&str; 4] = ["localhost", "127.0.0.1", "::1", "0.0.0.0"];
const NO_PROXY_VALUE: &str = "localhost,127.0.0.1,::1,.localhost";
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const CONNECT_TIMEOUT_SECONDS: u64 = 15;
const HEADER_TIMEOUT_SECONDS: u64 = 30;
const CRED_HEADERS: [&str; 3] = ["authorization", "cookie", "x-api-key"];

const SITECUSTOMIZE: &str = include_str!("../assets/net_preload/sitecustomize.py");
const PRELOAD_CJS: &str = include_str!("../assets/net_preload/preload.cjs");

pub fn clean_host(host: &str) -> String {
    let mut text = host.trim().to_lowercase();
    if text.starts_with('[') && text.ends_with(']') {
        text = text[1..text.len() - 1].to_string();
    }
    if text.ends_with('.') && !text.ends_with("..") {
        text.pop();
    }
    text
}

pub fn is_local(host: &str) -> bool {
    LOCAL_NAMES.contains(&host) || host.ends_with(".localhost")
}

// ---- the list file ------------------------------------------------------------------------------------------------------------------------

/// `network-allow.json` read on demand, cached by (mtime, size). [`AllowList::read`] is the effective list or `None` when the file is
/// missing or unusable (callers then fail closed for every non-local host).
pub struct AllowList {
    path: PathBuf,
    cache: Mutex<(Option<(SystemTime, u64)>, Option<Value>)>,
}

impl AllowList {
    pub fn new(path: &Path) -> Self {
        AllowList { path: path.to_path_buf(), cache: Mutex::new((None, None)) }
    }

    pub fn read(&self) -> Option<Value> {
        let mut cache = self.cache.lock().ok()?;
        let loaded = (|| -> Option<((SystemTime, u64), Value)> {
            let info = std::fs::metadata(&self.path).ok()?;
            let stamp = (info.modified().ok()?, info.len());
            if cache.0 == Some(stamp) {
                return Some((stamp, cache.1.clone()?));
            }
            let text = String::from_utf8(std::fs::read(&self.path).ok()?).ok()?;
            let content: Value = serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text)).ok()?;
            content.get("allow").and_then(Value::as_array)?;
            Some((stamp, content))
        })();
        match loaded {
            Some((stamp, content)) => {
                *cache = (Some(stamp), Some(content.clone()));
                Some(content)
            }
            None => {
                *cache = (None, None);
                None
            }
        }
    }
}

/// `(allowed, reason)` for a tunnel to host:port (CONNECT): host level, the same rule as the preloads for tcp entries; an HTTP entry matches
/// its own port, or any port when it has none.
pub fn host_decision(content: Option<&Value>, host: &str, port: u32) -> (bool, String) {
    let host = clean_host(host);
    if let Some(denies) = content.and_then(|c| c.get("deny")).and_then(Value::as_array) {
        for pattern in denies.iter().filter_map(Value::as_str) {
            if host_matches(pattern, &host) {
                return (false, format!("host {host} is denied (egress_never {pattern})"));
            }
        }
    }
    if is_local(&host) {
        return (true, "local host".into());
    }
    let Some(content) = content.filter(|c| crate::notify_truthy(Some(c))) else {
        return (false, "network list unreadable: every non-local connection is refused".into());
    };
    if let Some(broken) = content.get("broken").and_then(Value::as_str).filter(|b| !b.is_empty()) {
        return (false, format!("{broken} is unreadable: every non-local connection is refused"));
    }
    let defined: Vec<&str> = content.get("env_names").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    for entry in content.get("allow").and_then(Value::as_array).into_iter().flatten() {
        if !entry.is_object() || entry.get("origin").and_then(Value::as_str) == Some(ORIGIN_LOCAL) {
            continue;
        }
        if let Some(missing) = entry.get("if_missing").and_then(Value::as_str).filter(|m| !m.is_empty()) {
            if defined.contains(&missing) {
                continue;
            }
        }
        if !host_matches(entry.get("host").and_then(Value::as_str).unwrap_or(""), &host) {
            continue;
        }
        if let Some(entry_port) = entry.get("port").and_then(Value::as_u64) {
            if entry_port != u64::from(port) {
                continue;
            }
        }
        let text = entry.get("text").and_then(Value::as_str).or_else(|| entry.get("host").and_then(Value::as_str)).unwrap_or("");
        return (true, format!("matches {text} ({})", entry.get("origin").and_then(Value::as_str).unwrap_or("")));
    }
    (false, format!("{host}:{port} is not in the project's network list"))
}

// ---- the proxy ----------------------------------------------------------------------------------------------------------------------------

struct Shared {
    allow_list: AllowList,
    log_file: String,
    project: String,
    turn: AtomicI64,
    stop: AtomicBool,
    open: Mutex<Vec<TcpStream>>,
}

impl Shared {
    fn track(&self, stream: &TcpStream) {
        if let (Ok(clone), Ok(mut open)) = (stream.try_clone(), self.open.lock()) {
            open.push(clone);
        }
    }

    fn record_denial(&self, host: &str, port: u32, method: &str, path: &str, reason: &str) {
        append_log(
            &self.log_file,
            &json!({"time": iso_seconds(local_now()), "project": self.project, "turn": self.turn.load(Ordering::Relaxed), "layer": "proxy", "host": host,
                    "port": port, "method": method, "path": path, "reason": reason}),
        );
    }
}

/// One JSON line in egress.log (best effort; rotated past 5 MB).
pub fn append_log(log_file: &str, line: &Value) {
    if log_file.is_empty() {
        return;
    }
    let path = Path::new(log_file);
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
    if std::fs::metadata(path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(path, format!("{log_file}.1"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", py_json_dumps(line, false));
    }
}

fn reply(stream: &mut TcpStream, status: u16, text: &str, body: &str) {
    let head = format!("HTTP/1.1 {status} {text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn deny(shared: &Shared, stream: &mut TcpStream, host: &str, port: u32, method: &str, path: &str, reason: &str) {
    shared.record_denial(host, port, method, path, reason);
    let message = format!("[egress-runtime] {host}:{port} {reason}").replace('"', "'");
    reply(stream, 403, "Forbidden", &format!("{{\"error\":\"egress-denied\",\"message\":\"{message}\"}}"));
}

/// Reads the request head: `(request line, headers, bytes read past the head)`.
fn read_head(stream: &mut TcpStream) -> Option<(String, Vec<(String, String)>, Vec<u8>)> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let (end, separator) = loop {
        if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break (i, 4);
        }
        if let Some(i) = buffer.windows(2).position(|w| w == b"\n\n") {
            break (i, 2);
        }
        if buffer.len() > MAX_HEADER_BYTES {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..n]);
    };
    let head: String = buffer[..end].iter().map(|b| *b as char).collect();
    let rest = buffer[end + separator..].to_vec();
    let mut lines = head.lines();
    let request_line = lines.next()?.trim().to_string();
    if request_line.is_empty() {
        return None;
    }
    let headers = lines.filter_map(|l| l.split_once(':').map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))).collect();
    Some((request_line, headers, rest))
}

fn open_upstream(stream: &mut TcpStream, host: &str, port: u32) -> Option<TcpStream> {
    let addrs: Vec<SocketAddr> = (host, port as u16).to_socket_addrs().map(|a| a.collect()).unwrap_or_default();
    let mut last = "Error".to_string();
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(CONNECT_TIMEOUT_SECONDS)) {
            Ok(s) => return Some(s),
            Err(e) => last = format!("{:?}", e.kind()),
        }
    }
    reply(stream, 502, "Bad Gateway", &format!("{{\"error\":\"upstream\",\"message\":\"{last}\"}}"));
    None
}

fn pipe(mut source: TcpStream, mut target: TcpStream) {
    let mut buffer = [0u8; 65536];
    while let Ok(n) = source.read(&mut buffer) {
        if n == 0 || target.write_all(&buffer[..n]).is_err() {
            break;
        }
    }
    let _ = target.shutdown(Shutdown::Write);
}

fn bridge(shared: &Shared, client: &mut TcpStream, upstream: TcpStream, rest: &[u8]) {
    shared.track(&upstream);
    let _ = client.set_read_timeout(None);
    let mut upstream = upstream;
    if !rest.is_empty() && upstream.write_all(rest).is_err() {
        return;
    }
    let (Ok(client_reader), Ok(upstream_writer), Ok(upstream_reader), Ok(client_writer)) = (client.try_clone(), upstream.try_clone(), upstream.try_clone(), client.try_clone()) else { return };
    let worker = std::thread::spawn(move || pipe(client_reader, upstream_writer));
    pipe(upstream_reader, client_writer);
    let _ = worker.join();
}

fn serve(shared: &Shared, client: &mut TcpStream) {
    let _ = client.set_read_timeout(Some(Duration::from_secs(HEADER_TIMEOUT_SECONDS)));
    let Some((request_line, headers, rest)) = read_head(client) else { return };
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() != 3 {
        return reply(client, 400, "Bad Request", "");
    }
    let (method, target, version) = (parts[0], parts[1], parts[2]);
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host_text, port_text) = target.rsplit_once(':').unwrap_or(("", target));
        let host = clean_host(host_text);
        if host.is_empty() || port_text.is_empty() || !port_text.chars().all(|c| c.is_ascii_digit()) {
            return reply(client, 400, "Bad Request", "");
        }
        let port: u32 = port_text.parse().unwrap_or(0);
        let (allowed, reason) = host_decision(shared.allow_list.read().as_ref(), &host, port);
        if !allowed {
            return deny(shared, client, &host, port, "CONNECT", "", &reason);
        }
        let Some(upstream) = open_upstream(client, &host, port) else { return };
        let _ = client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n");
        return bridge(shared, client, upstream, &rest);
    }
    if !target.to_lowercase().starts_with("http://") {
        return reply(client, 400, "Bad Request", "{\"error\":\"this is a proxy: send absolute-URI or CONNECT requests\"}");
    }
    forward(shared, client, method, target, version, &headers, &rest);
}

fn forward(shared: &Shared, client: &mut TcpStream, method: &str, target: &str, version: &str, headers: &[(String, String)], rest: &[u8]) {
    let Ok(parts) = pycompat::urlsplit(target) else { return reply(client, 400, "Bad Request", "") };
    let host = clean_host(parts.hostname.as_deref().unwrap_or(""));
    let port = parts.port.unwrap_or(80);
    let cred = parts.username.as_deref().is_some_and(|u| !u.is_empty()) || parts.password.as_deref().is_some_and(|u| !u.is_empty()) || headers.iter().any(|(n, _)| CRED_HEADERS.contains(&n.to_lowercase().as_str()));
    let content = shared.allow_list.read();
    let (mut allowed, reason) = match &content {
        Some(c) if crate::notify_truthy(Some(c)) => network_policy::allows(&network_policy::effective_from_json(c), method, target, cred),
        _ => (false, "network list unreadable: every non-local request is refused".to_string()),
    };
    if let Some(c) = &content {
        let denied = c.get("deny").and_then(Value::as_array).is_some_and(|d| d.iter().filter_map(Value::as_str).any(|p| host_matches(p, &host)));
        if is_local(&host) && !denied {
            allowed = true;
        }
    }
    let path_only = if parts.path.is_empty() { "/".to_string() } else { parts.path.clone() };
    if !allowed {
        return deny(shared, client, &host, port, &method.to_uppercase(), &path_only, &reason);
    }
    let Some(mut upstream) = open_upstream(client, &host, port) else { return };
    let path = format!("{path_only}{}", if parts.query.is_empty() { String::new() } else { format!("?{}", parts.query) });
    let mut lines = vec![format!("{method} {path} {version}")];
    lines.extend(headers.iter().filter(|(n, _)| !["proxy-connection", "proxy-authorization", "connection"].contains(&n.to_lowercase().as_str())).map(|(n, v)| format!("{n}: {v}")));
    lines.push("Connection: close".into());
    if upstream.write_all(format!("{}\r\n\r\n", lines.join("\r\n")).as_bytes()).is_err() {
        return;
    }
    bridge(shared, client, upstream, rest);
}

/// The proxy of one project. [`EgressProxy::start`] returns the port; [`EgressProxy::stop`] closes the listener and the open tunnels.
pub struct EgressProxy {
    shared: Arc<Shared>,
    pub port: u16,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EgressProxy {
    pub fn new(allow_file: &Path, log_file: &str, project: &str, turn: i64) -> Self {
        let shared = Arc::new(Shared {
            allow_list: AllowList::new(allow_file),
            log_file: log_file.to_string(),
            project: project.to_string(),
            turn: AtomicI64::new(turn),
            stop: AtomicBool::new(false),
            open: Mutex::new(Vec::new()),
        });
        EgressProxy { shared, port: 0, thread: None }
    }

    pub fn start(&mut self) -> std::io::Result<u16> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        self.port = listener.local_addr()?.port();
        let shared = Arc::clone(&self.shared);
        self.thread = Some(std::thread::Builder::new().name("egress-proxy".into()).spawn(move || {
            while !shared.stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        shared.track(&stream);
                        let handler = Arc::clone(&shared);
                        std::thread::spawn(move || {
                            serve(&handler, &mut stream);
                            let _ = stream.shutdown(Shutdown::Both);
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break,
                }
            }
        })?);
        Ok(self.port)
    }

    pub fn set_turn(&self, turn: i64) {
        self.shared.turn.store(turn, Ordering::Relaxed);
    }

    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Ok(mut open) = self.shared.open.lock() {
            for stream in open.drain(..) {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
    }
}

// ---- variables of the agent's commands ----------------------------------------------------------------------------------------------------

/// Windows-style paths with forward slashes survive Git Bash (no MSYS conversion) and are read by Windows Python and Node alike.
pub fn forward_slashes(path: &Path) -> String {
    pycompat::nt_abspath(&path.display().to_string()).replace('\\', "/")
}

/// The variables that put one command under the layer, as pairs. `existing` supplies the PYTHONPATH and NODE_OPTIONS to extend: ours goes
/// first in PYTHONPATH, `--require` is appended to NODE_OPTIONS. The guard builds the same names as shell text (`guard_network.runtime_prefix`);
/// a test pins the two to the same set. NEVER add these to the environment of `claude`.
pub fn layer_environment(port: u16, allow_file: &Path, preload_dir: &Path, node_preload: &Path, existing: &Env) -> Vec<(String, String)> {
    let proxy = format!("http://127.0.0.1:{port}");
    let mut values: Vec<(String, String)> = Vec::new();
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        values.push((name.into(), proxy.clone()));
        values.push((name.to_lowercase(), proxy.clone()));
    }
    values.push(("NO_PROXY".into(), NO_PROXY_VALUE.into()));
    values.push(("no_proxy".into(), NO_PROXY_VALUE.into()));
    let (old_path, old_options) = (existing.text("PYTHONPATH"), existing.text("NODE_OPTIONS"));
    values.push(("PYTHONPATH".into(), format!("{}{}", forward_slashes(preload_dir), if old_path.is_empty() { String::new() } else { format!(";{old_path}") })));
    values.push(("NODE_OPTIONS".into(), format!("{}--require \"{}\"", if old_options.is_empty() { String::new() } else { format!("{old_options} ") }, forward_slashes(node_preload))));
    values.push(("NODE_USE_ENV_PROXY".into(), "1".into()));
    values.push(("LOOP_NETWORK_ALLOW_FILE".into(), forward_slashes(allow_file)));
    values
}

// ---- one project's layer ------------------------------------------------------------------------------------------------------------------

fn internal_dir_of(work: &str) -> PathBuf {
    [".specs", "project", "loop", "internal"].iter().fold(PathBuf::from(pycompat::nt_abspath(work)), |acc, p| acc.join(p))
}

fn write_json_atomically(path: &Path, content: &Value) -> std::io::Result<()> {
    crate::loop_io::atomic_write_text(path, &format!("{}\n", py_json_dumps_indent(content, false, 2)))
}

/// Writes the embedded preload payloads under `<data dir>/runtime/net_preload/` (only when they differ) and returns that folder and the
/// node preload file.
pub fn install_preloads(data_dir: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    let dir = data_dir.join("runtime").join("net_preload");
    std::fs::create_dir_all(&dir)?;
    for (name, text) in [("sitecustomize.py", SITECUSTOMIZE), ("preload.cjs", PRELOAD_CJS)] {
        let path = dir.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(text.as_bytes()) {
            std::fs::write(&path, text)?;
        }
    }
    let node = dir.join("preload.cjs");
    Ok((dir, node))
}

/// `network-allow.json` of the project from its lists (the loop writes the same file at every turn start; an app job has no turn start).
pub fn write_allow_file(env: &Env, work: &str, data_dir: &Path) -> std::io::Result<PathBuf> {
    let effective = network_policy::load(work, data_dir, None);
    let path = internal_dir_of(work).join(ALLOW_FILE);
    let _ = env;
    write_json_atomically(&path, &network_policy::to_jsonable(&effective))?;
    Ok(path)
}

/// Proxy + runtime file of one project folder.
pub struct RuntimeLayer {
    pub work: String,
    data_dir: PathBuf,
    project: String,
    pub allow_file: PathBuf,
    pub runtime_file: PathBuf,
    pub log_file: PathBuf,
    preload_dir: PathBuf,
    node_preload: PathBuf,
    proxy: Option<EgressProxy>,
    turn: i64,
}

impl RuntimeLayer {
    pub fn new(work: &str, data_dir: &Path, project: &str) -> Self {
        let work = pycompat::nt_abspath(work);
        let internal = internal_dir_of(&work);
        RuntimeLayer {
            project: if project.is_empty() { pycompat::last_segment(&work).to_string() } else { project.to_string() },
            allow_file: internal.join(ALLOW_FILE),
            runtime_file: internal.join(RUNTIME_FILE),
            log_file: data_dir.join(LOG_NAME),
            data_dir: data_dir.to_path_buf(),
            preload_dir: PathBuf::new(),
            node_preload: PathBuf::new(),
            work,
            proxy: None,
            turn: 0,
        }
    }

    pub fn port(&self) -> u16 {
        self.proxy.as_ref().map(|p| p.port).unwrap_or(0)
    }

    pub fn start(&mut self, env: &Env) -> std::io::Result<()> {
        if !self.allow_file.is_file() {
            write_allow_file(env, &self.work, &self.data_dir)?;
        }
        let (dir, node) = install_preloads(&self.data_dir)?;
        self.preload_dir = dir;
        self.node_preload = node;
        let mut proxy = EgressProxy::new(&self.allow_file, &self.log_file.display().to_string(), &self.project, self.turn);
        proxy.start()?;
        self.proxy = Some(proxy);
        if let Err(error) = self.write_runtime() {
            if let Some(mut p) = self.proxy.take() {
                p.stop();
            }
            return Err(error);
        }
        Ok(())
    }

    fn write_runtime(&self) -> std::io::Result<()> {
        write_json_atomically(
            &self.runtime_file,
            &json!({"version": 1, "port": self.port(), "pid": std::process::id(), "started_at": iso_seconds(local_now()), "project": self.project, "turn": self.turn,
                    "allow_file": forward_slashes(&self.allow_file), "egress_log": forward_slashes(&self.log_file),
                    "preload_dir": forward_slashes(&self.preload_dir), "node_preload": forward_slashes(&self.node_preload)}),
        )
    }

    /// Stamps the denials of the next turn.
    pub fn set_turn(&mut self, turn: i64) {
        self.turn = turn;
        if let Some(proxy) = &self.proxy {
            proxy.set_turn(turn);
            let _ = self.write_runtime();
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut proxy) = self.proxy.take() {
            proxy.stop();
        }
        let ours = std::fs::read(&self.runtime_file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).is_some_and(|v| v.get("pid").and_then(Value::as_u64) == Some(u64::from(std::process::id())));
        if ours {
            let _ = std::fs::remove_file(&self.runtime_file); // no runtime file = the guard points the proxy variables at a closed port: fail closed
        }
    }
}

type Layers = Vec<(String, Arc<Mutex<RuntimeLayer>>, usize)>;
static LAYERS: Mutex<Layers> = Mutex::new(Vec::new());

/// A running layer of a project folder, shared by the loop and the app's jobs of that folder.
#[derive(Clone)]
pub struct Layer(Arc<Mutex<RuntimeLayer>>);

impl Layer {
    pub fn port(&self) -> u16 {
        self.0.lock().map(|l| l.port()).unwrap_or(0)
    }

    pub fn log_file(&self) -> PathBuf {
        self.0.lock().map(|l| l.log_file.clone()).unwrap_or_default()
    }

    pub fn set_turn(&self, turn: i64) {
        if let Ok(mut l) = self.0.lock() {
            l.set_turn(turn);
        }
    }
}

/// The running layer of the project folder (started on first use, shared by the loop and the app's jobs of that folder).
pub fn acquire(env: &Env, work: &str, project: &str) -> std::io::Result<Layer> {
    let key = pycompat::nt_normcase(&pycompat::nt_abspath(work));
    let mut layers = LAYERS.lock().map_err(|_| std::io::Error::other("layers lock"))?;
    if let Some(entry) = layers.iter_mut().find(|(k, _, _)| *k == key) {
        entry.2 += 1;
        return Ok(Layer(Arc::clone(&entry.1)));
    }
    let mut layer = RuntimeLayer::new(work, &env.user_data_dir(), project);
    layer.start(env)?;
    let shared = Arc::new(Mutex::new(layer));
    layers.push((key, Arc::clone(&shared), 1));
    Ok(Layer(shared))
}

/// Gives one use back; the last one stops the proxy and removes the runtime file. Never fails.
pub fn release(layer: Option<&Layer>) {
    let Some(layer) = layer else { return };
    let Ok(mut layers) = LAYERS.lock() else { return };
    let Some(index) = layers.iter().position(|(_, l, _)| Arc::ptr_eq(l, &layer.0)) else { return };
    layers[index].2 -= 1;
    if layers[index].2 > 0 {
        return;
    }
    let (_, shared, _) = layers.remove(index);
    drop(layers);
    let stopped = shared.lock().map(|mut l| l.stop());
    let _ = stopped;
}

// ---- self test ----------------------------------------------------------------------------------------------------------------------------

const DENIED_HOST_CANDIDATES: [&str; 3] = ["denied.selftest.example.net", "denied-2.selftest.example.org", "denied-3.selftest.example.invalid"];
const PYTHON_DENIED: &str = "import socket, sys\ntry:\n    socket.create_connection((sys.argv[1], 443), timeout=5)\nexcept ConnectionRefusedError as error:\n    print(error); sys.exit(0)\nexcept Exception as error:\n    print('other:', type(error).__name__, error); sys.exit(3)\nsys.exit(4)\n";
const PYTHON_LOCAL: &str = "import socket, sys\ntry:\n    socket.create_connection(('127.0.0.1', int(sys.argv[1])), timeout=5).close(); print('connected'); sys.exit(0)\nexcept Exception as error:\n    print('other:', type(error).__name__, error); sys.exit(3)\n";
const NODE_DENIED: &str = "const net = require('net');\nnet.connect({ host: process.argv[1], port: 443 })\n  .on('connect', () => process.exit(4))\n  .on('error', (error) => { console.log(error.message); process.exit(/egress-runtime/.test(error.message) ? 0 : 3); });\n";
const NODE_LOCAL: &str = "const net = require('net');\nnet.connect({ host: '127.0.0.1', port: Number(process.argv[1]) })\n  .on('connect', () => { console.log('connected'); process.exit(0); })\n  .on('error', (error) => { console.log(error.message); process.exit(3); });\n";

/// Status code the proxy answers to `CONNECT host:port` (0 when nothing answers).
pub fn raw_connect_status(proxy_port: u16, host: &str, port: u16) -> u16 {
    let Ok(mut client) = TcpStream::connect(("127.0.0.1", proxy_port)) else { return 0 };
    let _ = client.set_read_timeout(Some(Duration::from_secs(10)));
    if client.write_all(format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").as_bytes()).is_err() {
        return 0;
    }
    let mut buffer = [0u8; 256];
    let n = client.read(&mut buffer).unwrap_or(0);
    String::from_utf8_lossy(&buffer[..n]).split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn run_probe(command: &[String], env: &[(String, String)], timeout: u64) -> (Option<i32>, String) {
    let result = crate::loop_io::run_process(command, &std::env::temp_dir().display().to_string(), env, "", timeout);
    if result.timed_out || (result.code == 1 && result.stdout.is_empty() && result.stderr.contains("os error")) {
        return (None, result.stderr.chars().take(60).collect());
    }
    (Some(result.code), format!("{}{}", result.stdout, result.stderr).trim().chars().take(200).collect())
}

/// The probe for `doctor --access`: inside the run-time layer a Python and a Node one-liner to a denied host fail (refused by the preload
/// before any DNS or TCP) and to a local listener succeed; the proxy answers 403 to a CONNECT to the denied host (and writes egress.log) and
/// 200 to the local one. No external traffic: the denied host never leaves this machine. Returns `{"ok", "checks": [{id, ok, detail}],
/// "skipped": [{id, reason}]}`; `ok` is false when any check failed (a skipped check, e.g. `node` not installed, does not fail it).
pub fn runtime_self_test(env: &Env, allow_file: Option<&Path>, timeout: u64) -> Value {
    let (mut checks, mut skipped, mut all_ok) = (Vec::<Value>::new(), Vec::<Value>::new(), true);
    let mut add = |id: &str, ok: bool, detail: String| {
        checks.push(json!({"id": id, "ok": ok, "detail": detail}));
        all_ok = all_ok && ok;
    };
    let scratch = match tempfile::Builder::new().prefix("egress-selftest-").tempdir() {
        Ok(d) => d,
        Err(e) => return json!({"ok": false, "checks": [{"id": "self-test", "ok": false, "detail": format!("OSError: {e}")}], "skipped": []}),
    };
    let list_path = match allow_file.filter(|p| p.is_file()) {
        Some(p) => p.to_path_buf(),
        None => {
            let path = scratch.path().join(ALLOW_FILE);
            let effective = egress_policy::effective_from(&json!(["selftest-allowed.example.com"]), &json!([]), &[], true, &[], vec![], "");
            let _ = write_json_atomically(&path, &network_policy::to_jsonable(&effective));
            path
        }
    };
    let content = AllowList::new(&list_path).read();
    let effective: Option<Effective> = content.as_ref().map(network_policy::effective_from_json);
    let denied = DENIED_HOST_CANDIDATES.iter().find(|h| effective.as_ref().is_some_and(|e| !network_policy::allows(e, "GET", &format!("https://{h}/"), false).0)).copied().unwrap_or("");
    let (preload_dir, node_preload) = match install_preloads(&env.user_data_dir()) {
        Ok(p) => p,
        Err(_) => (PathBuf::new(), PathBuf::new()),
    };
    let files_ok = preload_dir.join("sitecustomize.py").is_file() && node_preload.is_file();
    add("preload-files", files_ok, format!("net_preload/sitecustomize.py and preload.cjs {}", if files_ok { "found" } else { "missing" }));
    if denied.is_empty() || !files_ok {
        if denied.is_empty() {
            add("list", false, "the list file is unreadable or allows every probe host".into());
        }
        return json!({"ok": all_ok, "checks": checks, "skipped": skipped});
    }
    let Ok(listener) = TcpListener::bind(("127.0.0.1", 0)) else {
        add("self-test", false, "OSError: no local listener".into());
        return json!({"ok": false, "checks": checks, "skipped": skipped});
    };
    let local_port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    std::thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    });
    let log_path = scratch.path().join(LOG_NAME);
    let mut proxy = EgressProxy::new(&list_path, &log_path.display().to_string(), "self-test", 0);
    let proxy_port = match proxy.start() {
        Ok(p) => p,
        Err(e) => {
            add("self-test", false, format!("OSError: {e}"));
            return json!({"ok": false, "checks": checks, "skipped": skipped});
        }
    };
    let denied_status = raw_connect_status(proxy_port, denied, 443);
    let local_status = raw_connect_status(proxy_port, "127.0.0.1", local_port);
    let logged = std::fs::read_to_string(&log_path).map(|t| t.contains(denied)).unwrap_or(false);
    add("proxy-denied", denied_status == 403 && logged, format!("CONNECT to a denied host answered {denied_status}, egress.log {}", if logged { "has the line" } else { "has no line" }));
    add("proxy-local", local_status == 200, format!("CONNECT to localhost answered {local_status}"));
    let mut base: Vec<(String, String)> = env.vars();
    for (k, v) in layer_environment(proxy_port, &list_path, &preload_dir, &node_preload, env) {
        match base.iter_mut().find(|(n, _)| *n == k) {
            Some(slot) => slot.1 = v,
            None => base.push((k, v)),
        }
    }
    let mut probes: Vec<(&str, Vec<String>, &str)> = Vec::new();
    if let Some(python) = which("python") {
        let python = python.display().to_string();
        probes.push(("python-denied", vec![python.clone(), "-c".into(), PYTHON_DENIED.into(), denied.into()], "[egress-runtime]"));
        probes.push(("python-local", vec![python, "-c".into(), PYTHON_LOCAL.into(), local_port.to_string()], "connected"));
    } else {
        skipped.extend([json!({"id": "python-denied", "reason": "python not found on PATH"}), json!({"id": "python-local", "reason": "python not found on PATH"})]);
    }
    if let Some(node) = which("node") {
        let node = node.display().to_string();
        probes.push(("node-denied", vec![node.clone(), "-e".into(), NODE_DENIED.into(), denied.into()], "[egress-runtime]"));
        probes.push(("node-local", vec![node, "-e".into(), NODE_LOCAL.into(), local_port.to_string()], "connected"));
    } else {
        skipped.extend([json!({"id": "node-denied", "reason": "node not found on PATH"}), json!({"id": "node-local", "reason": "node not found on PATH"})]);
    }
    for (id, command, expected) in probes {
        let (code, output) = run_probe(&command, &base, timeout);
        add(id, code == Some(0) && output.contains(expected), format!("exit {}: {}", code.map(|c| c.to_string()).unwrap_or_else(|| "None".into()), output.replace('\n', " ")));
    }
    proxy.stop();
    json!({"ok": all_ok, "checks": checks, "skipped": skipped})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(entries: Value) -> Value {
        let effective = egress_policy::effective_from(&entries, &json!([]), &[json!("blocked.example.com")], true, &["HAS_KEY".to_string()], vec![], "");
        network_policy::to_jsonable(&effective)
    }

    #[test]
    fn host_decisions() {
        let content = list(json!(["api.example.com", "db.internal.example.com:5432 tcp", "*.cdn.example.com", "fallback.example.com if-missing=HAS_KEY"]));
        let decide = |h: &str, p: u32| host_decision(Some(&content), h, p);
        assert_eq!(decide("API.example.com.", 443), (true, "matches api.example.com (project)".to_string()));
        assert_eq!(decide("db.internal.example.com", 5432).0, true);
        assert_eq!(decide("db.internal.example.com", 5433).0, false);
        assert_eq!(decide("x.cdn.example.com", 443).0, true);
        assert_eq!(decide("cdn.example.com", 443).0, false);
        assert_eq!(decide("blocked.example.com", 443), (false, "host blocked.example.com is denied (egress_never blocked.example.com)".to_string()));
        assert_eq!(decide("localhost", 3000), (true, "local host".to_string()));
        assert_eq!(decide("fallback.example.com", 443).0, false);
        assert_eq!(decide("nope.example.com", 443), (false, "nope.example.com:443 is not in the project's network list".to_string()));
        assert_eq!(host_decision(None, "x.example.com", 443), (false, "network list unreadable: every non-local connection is refused".to_string()));
        assert_eq!(host_decision(None, "localhost", 80).0, true);
        let broken = json!({"allow": [], "broken": "config.json", "deny": [], "env_names": []});
        assert_eq!(host_decision(Some(&broken), "x.example.com", 80), (false, "config.json is unreadable: every non-local connection is refused".to_string()));
    }

    #[test]
    fn environment_and_files() {
        let env = Env::from_pairs([("PYTHONPATH", "C:\\old"), ("NODE_OPTIONS", "--max-old-space-size=4096")]);
        let values = layer_environment(4242, Path::new("C:\\p\\allow.json"), Path::new("C:\\pre"), Path::new("C:\\pre\\preload.cjs"), &env);
        let get = |k: &str| values.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap();
        assert_eq!((get("HTTP_PROXY").as_str(), get("https_proxy").as_str(), get("NO_PROXY").as_str()), ("http://127.0.0.1:4242", "http://127.0.0.1:4242", NO_PROXY_VALUE));
        assert_eq!(get("PYTHONPATH"), "C:/pre;C:\\old");
        assert_eq!(get("NODE_OPTIONS"), "--max-old-space-size=4096 --require \"C:/pre/preload.cjs\"");
        assert_eq!(get("LOOP_NETWORK_ALLOW_FILE"), "C:/p/allow.json");
        let dir = tempfile::tempdir().unwrap();
        let (preload_dir, node) = install_preloads(dir.path()).unwrap();
        assert!(preload_dir.join("sitecustomize.py").is_file() && node.is_file());
        assert!(std::fs::read_to_string(node).unwrap().contains("LOOP_NETWORK_ALLOW_FILE"));
    }

    #[test]
    fn the_proxy_denies_logs_and_tunnels_to_local() {
        let dir = tempfile::tempdir().unwrap();
        let allow = dir.path().join(ALLOW_FILE);
        write_json_atomically(&allow, &list(json!(["api.example.com"]))).unwrap();
        let log = dir.path().join(LOG_NAME);
        let mut proxy = EgressProxy::new(&allow, &log.display().to_string(), "t", 7);
        let port = proxy.start().unwrap();
        let echo = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = echo.accept() {
                let _ = s.write_all(b"hello");
            }
        });
        assert_eq!(raw_connect_status(port, "denied.example.net", 443), 403);
        let line: Value = serde_json::from_str(std::fs::read_to_string(&log).unwrap().lines().next().unwrap()).unwrap();
        assert_eq!((line["host"].as_str(), line["method"].as_str(), line["layer"].as_str(), line["turn"].as_i64()), (Some("denied.example.net"), Some("CONNECT"), Some("proxy"), Some(7)));
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.write_all(format!("CONNECT localhost:{echo_port} HTTP/1.1\r\n\r\n").as_bytes()).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut got = Vec::new();
        let mut buffer = [0u8; 256];
        while let Ok(n) = client.read(&mut buffer) {
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buffer[..n]);
            if String::from_utf8_lossy(&got).contains("hello") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&got).to_string();
        assert!(text.starts_with("HTTP/1.1 200 Connection Established") && text.ends_with("hello"), "{text}");
        let mut plain = TcpStream::connect(("127.0.0.1", port)).unwrap();
        plain.write_all(b"GET http://nope.example.net/x HTTP/1.1\r\nHost: nope.example.net\r\n\r\n").unwrap();
        let mut answer = String::new();
        plain.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 403 Forbidden") && answer.contains("egress-denied") && answer.contains("is not in the network list"), "{answer}");
        proxy.stop();
    }

    #[test]
    fn the_layer_writes_and_removes_the_runtime_file() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let env = Env::from_pairs([("LOOP_DATA_DIR", dir.path().join("data").display().to_string())]);
        let w = work.display().to_string();
        let first = acquire(&env, &w, "proj").unwrap();
        let second = acquire(&env, &w, "proj").unwrap();
        assert_eq!(first.port(), second.port());
        let runtime = internal_dir_of(&w).join(RUNTIME_FILE);
        let info: Value = serde_json::from_str(&std::fs::read_to_string(&runtime).unwrap()).unwrap();
        assert_eq!((info["port"].as_u64(), info["project"].as_str(), info["pid"].as_u64()), (Some(u64::from(first.port())), Some("proj"), Some(u64::from(std::process::id()))));
        first.set_turn(3);
        let info: Value = serde_json::from_str(&std::fs::read_to_string(&runtime).unwrap()).unwrap();
        assert_eq!(info["turn"], 3);
        assert!(internal_dir_of(&w).join(ALLOW_FILE).is_file());
        release(Some(&second));
        assert!(runtime.is_file());
        release(Some(&first));
        assert!(!runtime.exists());
        release(None);
    }
}
