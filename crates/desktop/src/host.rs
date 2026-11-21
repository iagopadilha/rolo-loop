// SPDX-License-Identifier: MIT
//! The panel server behind the window: started in this process (`Owned`), or an already running `harness serve` the window attaches to
//! (`Attached`). Also records the desktop app in `config.json` so the CLI and the skill can start it.

use app::composition::{self, Assembled};
use app::config::Overrides;
use cfg::Env;
use engine_state::monitor_proc;
use serde_json::{json, Map, Value};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Keys this app keeps in `<data dir>/config.json`.
pub const DESKTOP_EXE_KEY: &str = "desktop_exe";
pub const DESKTOP_PORT_KEY: &str = "desktop_port";
const APP_ROOT_KEY: &str = "app_root_dir";
const SKILL_DIR_KEY: &str = "orchestrator_skill_dir";

pub enum Host {
    /// The server runs here, on its own thread.
    Owned { assembled: Arc<Assembled>, serving: Mutex<Option<JoinHandle<()>>>, url: String },
    /// Another process (`harness serve`) serves the panel; `app` is its `app.json`.
    Attached { app: Value, url: String },
}

#[derive(Debug)]
pub struct HostError(pub String);

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Host {
    /// Attaches to a live app, or builds and starts the server here (no browser, no idle exit: the window is the lifetime). Returns the
    /// host and the URL the window opens first (the project of `initial_project` when given, else the hub). `on_stopped` runs on the server
    /// thread when the server this app owns stops by itself (the panel's "Shut down app").
    pub fn start(env: &Env, initial_project: &str, on_stopped: impl FnOnce() + Send + 'static) -> Result<(Host, String), HostError> {
        if let Some(live) = monitor_proc::read_live_app(env) {
            let url = live.get("url").and_then(Value::as_str).unwrap_or_default().to_string();
            let host = Host::Attached { app: live, url: url.clone() };
            let first = if initial_project.is_empty() { url } else { host.open_project(initial_project).unwrap_or(url) };
            return Ok((host, first));
        }
        let config = cfg::locator::read_user_config(&env.user_data_dir());
        let remembered = config.get(DESKTOP_PORT_KEY).and_then(Value::as_u64).and_then(|p| u16::try_from(p).ok()).unwrap_or(0);
        let overrides = Overrides { fixed_port: preferred_port(remembered), idle_hours: Some(0.0), open_browser: Some(false), ..Overrides::default() };
        let assembled = Arc::new(composition::build_application(env, &overrides));
        let first = composition::open_and_register(&assembled, initial_project).map_err(|e| HostError(format!("the panel server could not start: {e}")))?;
        let url = assembled.server.url();
        let serving = {
            let assembled = assembled.clone();
            std::thread::Builder::new()
                .name("panel-server".into())
                .spawn(move || {
                    composition::serve(&assembled);
                    on_stopped();
                })
                .map_err(|e| HostError(e.to_string()))?
        };
        record_desktop_app(env, &assembled, assembled.server.port());
        Ok((Host::Owned { assembled, serving: Mutex::new(Some(serving)), url }, first))
    }

    /// Base URL of the panel (`http://127.0.0.1:<port>/`).
    pub fn url(&self) -> &str {
        match self {
            Host::Owned { url, .. } | Host::Attached { url, .. } => url,
        }
    }

    /// Opens (registers) a project folder and returns the URL of its panel.
    pub fn open_project(&self, path: &str) -> Option<String> {
        let absolute = pycompat::nt_abspath(path);
        match self {
            Host::Owned { assembled, .. } => assembled.app.project_service().open_existing_project(&absolute).ok().map(|p| assembled.project_url(&p.id)),
            Host::Attached { app, .. } => monitor_proc::register_project_in_app(app, &absolute),
        }
    }

    /// True while a loop, a claude job, a chat reply or a relaunch runs in the server this app owns.
    pub fn is_busy(&self) -> bool {
        match self {
            Host::Owned { assembled, .. } => assembled.is_busy(),
            Host::Attached { .. } => false,
        }
    }

    /// Stops the server this app owns and runs the same shutdown as `harness serve`; nothing for an attached server.
    pub fn shutdown(&self) {
        if let Host::Owned { assembled, serving, .. } = self {
            let handle = serving.lock().unwrap_or_else(|e| e.into_inner()).take();
            let Some(handle) = handle else { return };
            assembled.server.request_shutdown();
            let _ = handle.join();
            composition::finish(assembled);
        }
    }
}

/// The remembered port when it is free (so the page origin, and with it `localStorage`, stays the same), else 0 = first free of the range.
fn preferred_port(remembered: u16) -> u16 {
    if remembered != 0 && TcpListener::bind(("127.0.0.1", remembered)).is_ok() {
        remembered
    } else {
        0
    }
}

/// Writes `desktop_exe` and `desktop_port`, and fills `app_root_dir` / `orchestrator_skill_dir` when they are missing or point nowhere (an MSI
/// install never ran `harness install`). Other keys are kept; an unreadable file is left alone.
fn record_desktop_app(env: &Env, assembled: &Assembled, port: u16) {
    let path = env.user_data_dir().join("config.json");
    let Some(mut config) = read_config_object(&path) else {
        assembled.app.log.info("desktop_config_unreadable", &[("path", json!(path.display().to_string()))]);
        return;
    };
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let ctx = &assembled.app.config.ctx;
    let changed = merge_desktop_keys(&mut config, &exe, port, &assembled.app.config.app_root_dir, &ctx.skill_dir);
    if changed {
        let text = pycompat::json::py_json_dumps_indent(&Value::Object(config), false, 2);
        if let Err(failure) = engine_state::loop_io::atomic_write_text(&path, &text) {
            assembled.app.log.info("desktop_config_not_written", &[("error", json!(failure.to_string()))]);
        }
    }
}

/// `None` when the file exists but is not a JSON object (never overwrite what the user may fix by hand).
fn read_config_object(path: &Path) -> Option<Map<String, Value>> {
    let Ok(bytes) = std::fs::read(path) else { return Some(Map::new()) };
    let text = String::from_utf8(bytes).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if text.trim().is_empty() {
        return Some(Map::new());
    }
    serde_json::from_str::<Value>(text).ok()?.as_object().cloned()
}

/// Sets the desktop keys; returns true when something changed.
fn merge_desktop_keys(config: &mut Map<String, Value>, exe: &str, port: u16, app_root: &Path, skill_dir: &Path) -> bool {
    let before = config.clone();
    if !exe.is_empty() {
        config.insert(DESKTOP_EXE_KEY.into(), json!(exe));
    }
    if port != 0 {
        config.insert(DESKTOP_PORT_KEY.into(), json!(port));
    }
    let points_nowhere = |key: &str, config: &Map<String, Value>| config.get(key).and_then(Value::as_str).filter(|p| !p.is_empty() && Path::new(p).is_dir()).is_none();
    if points_nowhere(APP_ROOT_KEY, config) && app_root.is_dir() {
        config.insert(APP_ROOT_KEY.into(), json!(app_root.display().to_string()));
    }
    if points_nowhere(SKILL_DIR_KEY, config) && skill_dir.is_dir() {
        config.insert(SKILL_DIR_KEY.into(), json!(skill_dir.display().to_string()));
    }
    *config != before
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_keys_fill_only_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let app_root = dir.path().join("app");
        std::fs::create_dir_all(&app_root).unwrap();
        let mut config: Map<String, Value> = serde_json::from_value(json!({"idle_hours": 2, "app_root_dir": "C:/gone"})).unwrap();
        assert!(merge_desktop_keys(&mut config, "C:/Apps/Rolo Loop.exe", 47801, &app_root, Path::new("")));
        assert_eq!(config["desktop_exe"], "C:/Apps/Rolo Loop.exe");
        assert_eq!(config["desktop_port"], 47801);
        assert_eq!(config["app_root_dir"], app_root.display().to_string());
        assert_eq!(config["idle_hours"], 2);
        assert!(config.get("orchestrator_skill_dir").is_none());
        assert!(!merge_desktop_keys(&mut config, "C:/Apps/Rolo Loop.exe", 47801, &app_root, Path::new("")));
    }

    #[test]
    fn config_file_reading_keeps_a_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert_eq!(read_config_object(&path), Some(Map::new()));
        std::fs::write(&path, "\u{feff}{\"a\": 1}").unwrap();
        assert_eq!(read_config_object(&path).unwrap()["a"], 1);
        std::fs::write(&path, "{broken").unwrap();
        assert!(read_config_object(&path).is_none());
    }

    #[test]
    fn a_busy_remembered_port_is_not_reused() {
        let taken = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        assert_eq!(preferred_port(port), 0);
        assert_eq!(preferred_port(0), 0);
        drop(taken);
        assert_eq!(preferred_port(port), port);
    }
}
