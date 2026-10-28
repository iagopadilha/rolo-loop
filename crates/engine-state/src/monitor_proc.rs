// SPDX-License-Identifier: MIT
//! Ensures the web panel (the Maestro app) for the loop and writes the command to relaunch the loop.
//!
//! The panel is the multi-project app (`harness serve`): one single process for every project, INDEPENDENT of the loop (it starts
//! detached, does not read stdin and does not die when the loop ends). Order of [`ensure_monitor`]:
//!   1. `LOOP_MONITOR_URL` (the panel that launched this loop) answering `<url>api/health` -> use it.
//!   2. The work's MONITOR.json with a live pid and `<url>api/health` returning the SAME work -> reuse it.
//!   3. app.json (LOOP_DATA_DIR or ~/.loop-harness) with a live pid and `GET <url>api/app/health` ok -> register the project:
//!      `POST <url>api/app/projects/open {"path": repo}` (header X-Monitor-Token from app.json) and use the url returned.
//!   4. App not alive -> start `harness serve --project <repo>` detached (`LOOP_MONITOR_SERVER` swaps the program, for tests) and read
//!      `MONITOR_URL=` from the log (or the MONITOR.json the app writes).
//!
//! Everything is best effort: any failure returns `None` and prints a warning; the loop never falls because of the panel.
//!
//! Files in `<work>/.specs/project/loop/`:
//!   json/MONITOR.json   written by the app when it opens the project (app pid, project url, repo, work); deleted by it on exit
//!   json/LOOP-CMD.json  written here on every run: `{"argv": [...], "cwd": "..."}`; without --answer/--option
//!   internal/monitor.log  output of the `harness serve` launched from here

use crate::loop_io::{kill_tree, pid_alive};
use crate::loop_state::loop_path;
use cfg::Env;
use pycompat::json::py_json_dumps_indent;
use pycompat::{nt_normcase, nt_realpath};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const DEFAULT_WAIT_SECONDS: f64 = 15.0;
const POLL_SECONDS: f64 = 0.2;
const HEALTH_TIMEOUT_SECONDS: u64 = 2;
const REGISTER_TIMEOUT_SECONDS: u64 = 10;
const LOCAL_HOSTS: [&str; 2] = ["127.0.0.1", "localhost"];
const DISABLED_VALUES: [&str; 4] = ["off", "0", "false", "no"];
/// Set by the panel when it launches the loop: reuse this panel.
pub const MONITOR_URL_VARIABLE: &str = "LOOP_MONITOR_URL";
/// Alternative program instead of this executable (tests and custom installs).
pub const SERVER_OVERRIDE_VARIABLE: &str = "LOOP_MONITOR_SERVER";
const MONITOR_URL_PREFIX: &str = "MONITOR_URL=";
const APP_REGISTRY_NAME: &str = "app.json";
const TOKEN_HEADER: &str = "X-Monitor-Token";

pub fn monitor_file(work: &str) -> PathBuf {
    loop_path(work, "MONITOR.json")
}

pub fn app_registry_path(env: &Env) -> PathBuf {
    env.user_data_dir().join(APP_REGISTRY_NAME)
}

/// `LOOP_MONITOR=off` (or 0/false/no) in the app `.env` turns the panel off for every loop on this machine.
pub fn monitor_disabled_by_environment(value: &str) -> bool {
    DISABLED_VALUES.contains(&value.trim().to_lowercase().as_str())
}

/// `LOOP_MONITOR_OPEN=off` (env or the app `.env`) starts the app with `--no-browser`.
pub fn browser_open_disabled(value: &str) -> bool {
    monitor_disabled_by_environment(value)
}

fn same_path(first: &str, second: &str) -> bool {
    nt_normcase(&nt_realpath(first)) == nt_normcase(&nt_realpath(second))
}

fn read_json_file(path: &Path) -> Option<Value> {
    let text = String::from_utf8(std::fs::read(path).ok()?).ok()?;
    serde_json::from_str::<Value>(&text).ok().filter(Value::is_object)
}

fn pid_of(value: Option<&Value>) -> i64 {
    value.and_then(Value::as_i64).unwrap_or(0)
}

pub fn read_monitor_info(work: &str) -> Option<Value> {
    read_json_file(&monitor_file(work)).filter(|i| crate::notify_truthy(i.get("url")) && crate::notify_truthy(i.get("pid")))
}

/// `<url>` + `relative` (project url `/p/<id>/` -> `/p/<id>/api/health`). `None` outside 127.0.0.1/localhost.
pub fn local_endpoint(url: &str, relative: &str) -> Option<String> {
    let text = url.trim();
    let (scheme, rest) = text.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].split(['?', '#']).next().unwrap_or("/")),
        Some(i) => (&rest[..i], "/"),
        None => (rest, "/"),
    };
    let host = authority.rsplit_once('@').map(|x| x.1).unwrap_or(authority);
    let host_only = if host.starts_with('[') { host.split(']').next().unwrap_or("").trim_start_matches('[') } else { host.split(':').next().unwrap_or("") };
    if !LOCAL_HOSTS.contains(&host_only.to_lowercase().as_str()) {
        return None;
    }
    let path = if path.ends_with('/') { path.to_string() } else { format!("{path}/") };
    let base = format!("{scheme}://{authority}{path}");
    // urljoin with a relative reference: the last segment of a path that ends with `/` is empty, so it is simply appended
    Some(if relative.starts_with('/') { format!("{scheme}://{authority}{relative}") } else { format!("{base}{relative}") })
}

/// GET (or POST with `body`) on a local endpoint, without a proxy. Returns the response object or `None`.
pub fn request_local_json(url: &str, relative: &str, body: Option<&Value>, token: &str, timeout_seconds: u64) -> Option<Value> {
    let address = local_endpoint(url, relative)?;
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(timeout_seconds)).build();
    let mut request = if body.is_some() { agent.post(&address) } else { agent.get(&address) };
    if !token.is_empty() {
        request = request.set(TOKEN_HEADER, token);
    }
    let response = match body {
        Some(b) => request.set("Content-Type", "application/json").send_string(&pycompat::json::py_json_dumps(b, true)),
        None => request.call(),
    }
    .ok()?;
    let answer: Value = serde_json::from_str(&response.into_string().ok()?).ok()?;
    answer.is_object().then_some(answer)
}

/// GET `<url>api/health` answers ok for the SAME work. Only talks to 127.0.0.1/localhost.
pub fn monitor_answers_health(info: &Value, work: &str) -> bool {
    let url = info.get("url").and_then(Value::as_str).unwrap_or("");
    request_local_json(url, "api/health", None, "", HEALTH_TIMEOUT_SECONDS)
        .is_some_and(|b| b.get("ok") == Some(&Value::Bool(true)) && same_path(b.get("work").and_then(Value::as_str).unwrap_or(""), work))
}

/// GET `<url>api/health` answers `ok=true`. Only 127.0.0.1/localhost, without a proxy.
pub fn local_url_answers_health(url: &str) -> bool {
    request_local_json(url, "api/health", None, "", HEALTH_TIMEOUT_SECONDS).is_some_and(|b| b.get("ok") == Some(&Value::Bool(true)))
}

/// URL in `LOOP_MONITOR_URL` (the panel that launched this loop) if it is alive; otherwise `None`.
pub fn monitor_url_from_environment(env: &Env) -> Option<String> {
    let url = env.text(MONITOR_URL_VARIABLE).trim().to_string();
    (!url.is_empty() && local_url_answers_health(&url)).then_some(url)
}

/// URL of this work's panel that is already alive (live pid and health with the same work) or `None`.
pub fn running_monitor_url(work: &str) -> Option<String> {
    let info = read_monitor_info(work)?;
    (pid_alive(pid_of(info.get("pid"))) && monitor_answers_health(&info, work)).then(|| info["url"].as_str().unwrap_or("").to_string())
}

/// app.json of the live app (live pid + `GET <url>api/app/health` ok) or `None`.
pub fn read_live_app(env: &Env) -> Option<Value> {
    let registry = read_json_file(&app_registry_path(env))?;
    let url = registry.get("url").and_then(Value::as_str).filter(|u| !u.is_empty())?;
    if !pid_alive(pid_of(registry.get("pid"))) {
        return None;
    }
    request_local_json(url, "api/app/health", None, "", HEALTH_TIMEOUT_SECONDS).filter(|h| h.get("ok") == Some(&Value::Bool(true))).map(|_| registry)
}

/// `POST <url>api/app/projects/open {"path": repo}` -> ABSOLUTE url of the project panel, or `None`.
pub fn register_project_in_app(app: &Value, repo: &str) -> Option<String> {
    let url = app.get("url").and_then(Value::as_str)?;
    let token = app.get("token").and_then(Value::as_str).unwrap_or("");
    let answer = request_local_json(url, "api/app/projects/open", Some(&json!({"path": repo})), token, REGISTER_TIMEOUT_SECONDS)?;
    if answer.get("ok") != Some(&Value::Bool(true)) {
        return None;
    }
    let relative = answer.get("url").and_then(Value::as_str).filter(|u| !u.is_empty())?;
    let base = local_endpoint(url, "").unwrap_or_else(|| url.to_string());
    if relative.contains("://") {
        return Some(relative.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    let authority = rest.split('/').next().unwrap_or("");
    Some(if relative.starts_with('/') { format!("{scheme}://{authority}{relative}") } else { format!("{base}{relative}") })
}

/// Starts `harness serve --project <repo>` detached; output in internal/monitor.log. Returns `(pid, log offset)`.
pub fn launch_app(repo: &str, work: &str, program: &Path, open_browser: bool) -> std::io::Result<(i64, u64)> {
    let log_path = loop_path(work, "monitor.log");
    std::fs::create_dir_all(log_path.parent().expect("has parent"))?;
    let offset = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)?;
    let mut command = std::process::Command::new(program);
    command.args(["serve", "--project", repo]);
    if !open_browser {
        command.arg("--no-browser"); // the app opens the browser once, only when it starts
    }
    command.stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log).current_dir(program.parent().unwrap_or(Path::new(".")));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
    }
    let child = command.spawn()?;
    Ok((i64::from(child.id()), offset))
}

/// Last `MONITOR_URL=` line written to monitor.log after `offset`.
pub fn monitor_url_from_log(work: &str, offset: u64) -> Option<String> {
    let bytes = std::fs::read(loop_path(work, "monitor.log")).ok()?;
    let tail = bytes.get(offset as usize..)?;
    let text = String::from_utf8_lossy(tail);
    let urls: Vec<String> =
        pycompat::splitlines(&text).into_iter().map(str::trim).filter_map(|l| l.strip_prefix(MONITOR_URL_PREFIX).map(|u| u.trim().to_string())).collect();
    urls.last().filter(|u| !u.is_empty()).cloned()
}

/// URL the app already announced (`MONITOR_URL=` in the log or the work's MONITOR.json) that answers health; otherwise `None`.
pub fn answered_url(work: &str, offset: u64) -> Option<String> {
    if let Some(from_log) = monitor_url_from_log(work, offset) {
        if local_url_answers_health(&from_log) {
            return Some(from_log);
        }
    }
    let info = read_monitor_info(work)?;
    (same_path(info.get("work").and_then(Value::as_str).unwrap_or(""), work) && monitor_answers_health(&info, work))
        .then(|| info["url"].as_str().unwrap_or("").to_string())
}

pub fn wait_for_monitor(work: &str, pid: i64, offset: u64, wait_seconds: f64) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs_f64(wait_seconds);
    loop {
        if let Some(url) = answered_url(work, offset) {
            return Some(url);
        }
        if !pid_alive(pid) {
            // it exited: either it did not start, or a live app existed (race) and it only registered the project and printed the URL
            return answered_url(work, offset).or_else(|| running_monitor_url(work));
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_secs_f64(POLL_SECONDS));
    }
}

/// Returns the project panel URL (live `LOOP_MONITOR_URL`, the work's live MONITOR.json, a live app with the project registered, or a new
/// detached app) or `None`. Reusing never reopens the browser; `open_browser=false` starts with `--no-browser`. Never fails.
pub fn ensure_monitor(env: &Env, app_exe: &Path, repo: &str, work: &str, wait_seconds: f64, open_browser: bool) -> Option<String> {
    if let Some(url) = monitor_url_from_environment(env) {
        return Some(url); // the panel that launched the loop: do not start another one
    }
    if let Some(url) = running_monitor_url(work) {
        return Some(url);
    }
    if let Some(app) = read_live_app(env) {
        if let Some(url) = register_project_in_app(&app, &pycompat::nt_abspath(repo)) {
            return Some(url);
        }
        println!("[loop] warning: the app at {} did not register the project; trying to start another one", app.get("url").and_then(Value::as_str).unwrap_or(""));
    }
    let override_program = env.text(SERVER_OVERRIDE_VARIABLE);
    let program = if override_program.is_empty() { app_exe.to_path_buf() } else { PathBuf::from(override_program) };
    if !program.is_file() {
        println!("[loop] warning: panel unavailable ({} not found); the loop carries on without it", program.display());
        return None;
    }
    let _ = std::fs::remove_file(monitor_file(work)); // stale MONITOR.json (dead pid): the app writes its own when it opens the project
    let (pid, offset) = match launch_app(&pycompat::nt_abspath(repo), &pycompat::nt_abspath(work), &program, open_browser) {
        Ok(started) => started,
        Err(error) => {
            println!("[loop] warning: the panel did not start (OSError: {error}); the loop carries on without it");
            return None;
        }
    };
    if let Some(url) = wait_for_monitor(work, pid, offset, wait_seconds) {
        return Some(url);
    }
    if pid_alive(pid) {
        kill_tree(pid); // we started it and it did not become valid: leave no orphan process
    }
    println!("[loop] warning: the panel did not answer in {wait_seconds:.0}s (see {}); the loop carries on without it", loop_path(work, "monitor.log").display());
    None
}

fn is_flag(token: &str, full_name: &str) -> bool {
    let name = token.split('=').next().unwrap_or("");
    name.len() >= 4 && name.starts_with("--") && full_name.starts_with(name)
}

/// argv without `--answer`/`--option` (and their values): the relaunch command never repeats the old answer.
pub fn without_answer_flags(argv: &[String]) -> Vec<String> {
    let (mut kept, mut skip_next) = (Vec::new(), false);
    for token in argv {
        if skip_next {
            skip_next = false;
            continue;
        }
        if is_flag(token, "--answer") || is_flag(token, "--option") || is_flag(token, "--instruction") {
            skip_next = !token.contains('=');
            continue;
        }
        kept.push(token.clone());
    }
    kept
}

/// Writes LOOP-CMD.json: the exact command the panel uses to relaunch the loop (POST /api/answer and /api/resume). Returns the path.
pub fn write_loop_command(work: &str, argv: &[String], cwd: &str) -> Option<PathBuf> {
    let path = loop_path(work, "LOOP-CMD.json");
    let content = json!({"argv": without_answer_flags(argv), "cwd": cwd});
    let written = std::fs::create_dir_all(path.parent().expect("has parent")).and_then(|_| std::fs::write(&path, py_json_dumps_indent(&content, false, 2)));
    match written {
        Ok(()) => Some(path),
        Err(error) => {
            println!("[loop] warning: LOOP-CMD.json was not written ({error}); the panel cannot relaunch the loop");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A tiny local HTTP server answering every request with `body`.
    fn serve(body: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/p/abc/", listener.local_addr().unwrap().port());
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().take(8) {
                let Ok(mut stream) = stream else { break };
                let mut buffer = [0u8; 2048];
                let n = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..n]).to_string();
                let answer = if request.contains("/api/health") { body } else { "{\"ok\": false}" };
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", answer.len(), answer);
                if request.contains("stop") {
                    break;
                }
            }
        });
        (url, handle)
    }

    #[test]
    fn endpoints_only_for_local_hosts() {
        assert_eq!(local_endpoint("http://127.0.0.1:8000/p/x/", "api/health").as_deref(), Some("http://127.0.0.1:8000/p/x/api/health"));
        assert_eq!(local_endpoint("http://localhost:8000/p/x", "api/health").as_deref(), Some("http://localhost:8000/p/x/api/health"));
        assert_eq!(local_endpoint("http://localhost:8000", "/api/app/health").as_deref(), Some("http://localhost:8000/api/app/health"));
        assert_eq!(local_endpoint("http://example.com/p/x/", "api/health"), None);
        assert_eq!(local_endpoint("ftp://localhost/x/", "a"), None);
        assert_eq!(local_endpoint("not a url", "a"), None);
        assert!(monitor_disabled_by_environment(" OFF ") && !monitor_disabled_by_environment("on") && !monitor_disabled_by_environment(""));
    }

    #[test]
    fn health_is_asked_without_a_proxy() {
        let (url, _server) = serve("{\"ok\": true, \"work\": \"C:\\\\x\"}");
        assert!(local_url_answers_health(&url));
        assert!(monitor_answers_health(&json!({"url": url}), "C:\\x"));
        assert!(!monitor_answers_health(&json!({"url": url}), "C:\\other"));
        assert!(!local_url_answers_health("http://127.0.0.1:1/p/x/"));
        let env = Env::from_pairs([(MONITOR_URL_VARIABLE, url.clone())]);
        assert_eq!(monitor_url_from_environment(&env).as_deref(), Some(url.as_str()));
    }

    #[test]
    fn relaunch_command_drops_the_answer() {
        let argv: Vec<String> = ["harness", "loop", "C:\\r", "--answer", "yes", "--wave", "2", "--option=1", "--ans=x", "--e2e"].map(String::from).to_vec();
        assert_eq!(without_answer_flags(&argv), vec!["harness", "loop", "C:\\r", "--wave", "2", "--e2e"]);
        let with_instruction: Vec<String> = ["harness", "loop", "C:\\r", "--instruction", "fix the spec", "--wave", "2", "--instruction=again"].map(String::from).to_vec();
        assert_eq!(without_answer_flags(&with_instruction), vec!["harness", "loop", "C:\\r", "--wave", "2"]);
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let path = write_loop_command(&work, &argv, "C:\\cwd").unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(saved["cwd"], "C:\\cwd");
        assert_eq!(saved["argv"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn log_announces_the_url() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let log = loop_path(&work, "monitor.log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, "old\nMONITOR_URL=http://127.0.0.1:1/old/\n").unwrap();
        let offset = std::fs::metadata(&log).unwrap().len();
        assert_eq!(monitor_url_from_log(&work, offset), None);
        let mut file = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        writeln!(file, "boot\nMONITOR_URL=http://127.0.0.1:2/a/\n MONITOR_URL=http://127.0.0.1:2/b/ ").unwrap();
        assert_eq!(monitor_url_from_log(&work, offset).as_deref(), Some("http://127.0.0.1:2/b/"));
        assert!(running_monitor_url(&work).is_none() && read_monitor_info(&work).is_none());
    }
}
