// SPDX-License-Identifier: MIT
//! Composition root: builds the app (services), its router and server, wires the idle monitor, and runs it (`harness serve`).

use crate::api::controllers::build_router;
use crate::api::http_server::AppHttpServer;
use crate::config::Overrides;
use crate::infrastructure::activity_monitor::{ActivityMonitor, BusyProvider};
use crate::infrastructure::process_runner::SystemProcesses;
use crate::services::App;
use cfg::Env;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

/// Something is running that keeps the app on: a claude job, a loop, a chat reply or a relaunch.
struct AppBusy(Arc<App>);

impl BusyProvider for AppBusy {
    fn is_busy(&self) -> bool {
        let app = &self.0;
        app.jobs().is_busy() || app.loop_status().is_any_loop_running() || app.chat().is_busy() || app.loop_control().is_busy()
    }
}

pub struct Assembled {
    pub app: Arc<App>,
    pub server: Arc<AppHttpServer>,
    pub monitor: Arc<ActivityMonitor>,
}

impl Assembled {
    pub fn project_url(&self, project_id: &str) -> String {
        format!("{}p/{project_id}/", self.server.url())
    }
}

pub fn build_application(env: &Env, overrides: &Overrides) -> Assembled {
    let app = App::new(env, overrides, Arc::new(SystemProcesses));
    let server_slot: Arc<std::sync::OnceLock<Arc<AppHttpServer>>> = Arc::new(std::sync::OnceLock::new());
    let on_idle = {
        let slot = server_slot.clone();
        move || {
            if let Some(server) = slot.get() {
                server.request_shutdown();
            }
        }
    };
    let monitor = ActivityMonitor::new(app.config.idle_hours, on_idle);
    monitor.add_busy_provider(Arc::new(AppBusy(app.clone())));
    let _ = app.activity.set(monitor.clone());
    let router = build_router(&app);
    let server = AppHttpServer::new(app.clone(), router, &app.config.frontend_dir.display().to_string(), None);
    let _ = server_slot.set(server.clone());
    let shutdown_server = server.clone();
    let _ = app.shutdown.set(Box::new(move || shutdown_server.request_shutdown()));
    Assembled { app, server, monitor }
}

/// app.json: how the engine (monitor_proc) and the skill find the live app and register a project in it.
pub fn write_app_registry(assembled: &Assembled) -> std::io::Result<()> {
    let path = assembled.app.config.app_registry_path();
    let registry = json!({
        "pid": std::process::id(), "port": assembled.server.port(), "url": assembled.server.url(), "token": assembled.server.token,
        "started_at": engine_state::loop_io::now_iso(), "app_root_dir": assembled.app.config.app_root_dir.display().to_string(),
    });
    engine_state::loop_io::atomic_write_text(&path, &pycompat::json::py_json_dumps_indent(&registry, false, 2))
}

pub fn delete_app_registry(assembled: &Assembled) {
    let path = assembled.app.config.app_registry_path();
    let ours = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .is_some_and(|r| r["pid"].as_u64() == Some(u64::from(std::process::id())));
    if ours {
        let _ = std::fs::remove_file(path);
    }
}

fn open_in_browser(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).creation_flags(0x0800_0000).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

/// Opens the port, registers the app, opens the initial project (if any), prints APP_URL/MONITOR_URL and serves until stopped.
pub fn start_application(assembled: &Assembled, initial_project_path: &str) -> std::io::Result<()> {
    let config = &assembled.app.config;
    let app_url = assembled.server.open_port(config.port_range, config.fixed_port)?;
    assembled.app.set_app_url(&app_url);
    write_app_registry(assembled)?;
    let mut initial_url = app_url.clone();
    if !initial_project_path.is_empty() {
        let project =
            assembled.app.project_service().open_existing_project(initial_project_path).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.message))?;
        initial_url = assembled.project_url(&project.id);
    }
    use std::io::Write;
    println!("MONITOR_URL={initial_url}");
    println!("APP_URL={app_url}");
    let _ = std::io::stdout().flush();
    assembled.app.log.info("app_started", &[("url", json!(app_url)), ("pid", json!(std::process::id()))]);
    assembled.monitor.watch_idle_in_background();
    if config.open_browser {
        let url = initial_url.clone();
        std::thread::spawn(move || open_in_browser(&url));
    }
    assembled.server.serve_until_stopped();
    assembled.monitor.stop();
    assembled.app.project_service().release_projects_on_shutdown();
    delete_app_registry(assembled);
    assembled.app.log.info("app_stopped", &[("pid", json!(std::process::id()))]);
    Ok(())
}

/// The folder of the front end the server serves (used by `doctor`).
pub fn frontend_dir(assembled: &Assembled) -> &Path {
    &assembled.app.config.frontend_dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_serves_its_health_route_and_shuts_down_through_the_api() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let env = Env::from_pairs([("LOOP_DATA_DIR", data.to_str().unwrap()), ("LOOP_CLAUDE_HOME", data.to_str().unwrap()), ("LOOP_TOOLKIT_SIBLING", "off")]);
        let overrides = Overrides { open_browser: Some(false), idle_hours: Some(0.0), ..Overrides::default() };
        let assembled = build_application(&env, &overrides);
        let url = assembled.server.open_port((38200, 38300), 0).unwrap();
        assembled.app.set_app_url(&url);
        let runner = assembled.server.clone();
        let handle = std::thread::spawn(move || runner.serve_until_stopped());
        let answer = engine_state::monitor_proc::request_local_json(&url, "api/app/health", None, "", 5).unwrap();
        assert_eq!((answer["ok"].clone(), answer["version"].clone()), (json!(true), json!(env!("CARGO_PKG_VERSION"))));
        let token = assembled.server.token.clone();
        let bye = engine_state::monitor_proc::request_local_json(&url, "api/app/shutdown", Some(&json!({})), &token, 5).unwrap();
        assert_eq!(bye["ok"], true);
        handle.join().unwrap();
    }
}
