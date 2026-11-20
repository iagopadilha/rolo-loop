// SPDX-License-Identifier: MIT
//! `harness serve [--project <repo>] [--port N] [--idle-hours H] [--no-browser] [--no-desktop] [--data-dir D]`: starts the multi-project
//! panel. If an app is already alive (app.json), it only registers the project in it, prints MONITOR_URL=<project url> and exits 0.
//! With no live app and the desktop app installed (`desktop_exe` in config.json), it starts the desktop app (which runs the panel server
//! itself) and registers the project there; `--no-desktop`, `--no-browser`, `--port` or `LOOP_NO_DESKTOP=1` keep the headless server.

use app::composition::{build_application, start_application};
use app::config::Overrides;
use cfg::Env;
use clap::Args as ClapArgs;
use engine_state::monitor_proc;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DESKTOP_START_TIMEOUT: Duration = Duration::from_secs(20);
const DESKTOP_POLL: Duration = Duration::from_millis(250);

#[derive(ClapArgs)]
pub struct Args {
    /// project folder to open (registers it in the app)
    #[arg(long, default_value = "")]
    project: String,
    /// fixed port (default: first free one from 47800 to 47899)
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// hours without use until it shuts down on its own (0 = never)
    #[arg(long)]
    idle_hours: Option<f64>,
    /// does not open the browser when it starts (and never starts the desktop app)
    #[arg(long)]
    no_browser: bool,
    /// keeps the headless server even when the desktop app is installed
    #[arg(long)]
    no_desktop: bool,
    /// app data folder (default: ~/.loop-harness or LOOP_DATA_DIR)
    #[arg(long, default_value = "")]
    data_dir: String,
}

/// `--data-dir` applies to this process and to the children (engine, relaunched loop).
pub fn apply_data_dir(env: &mut Env, data_dir: &str) {
    if !data_dir.is_empty() {
        let absolute = pycompat::nt_abspath(data_dir);
        env.set(cfg::env::DATA_DIR_VARIABLE, &absolute);
        std::env::set_var(cfg::env::DATA_DIR_VARIABLE, &absolute);
    }
}

/// The desktop app to start instead of the headless server: the configured `desktop_exe` when it exists and nobody opted out.
fn desktop_to_launch(opted_out: bool, configured: Option<&str>, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let path = PathBuf::from(configured.filter(|p| !p.trim().is_empty())?);
    (!opted_out && exists(&path)).then_some(path)
}

/// Starts the desktop app detached and waits until its server answers (its app.json); `None` on failure or timeout (the caller falls
/// back to the headless server).
fn start_desktop_app(env: &Env, desktop: &Path, project: &str) -> Option<Value> {
    let mut command = std::process::Command::new(desktop);
    if !project.is_empty() {
        command.args(["--project", &pycompat::nt_abspath(project)]);
    }
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    if let Err(failure) = command.spawn() {
        eprintln!("the desktop app did not start ({failure}): using the headless server");
        return None;
    }
    let started = Instant::now();
    while started.elapsed() < DESKTOP_START_TIMEOUT {
        if let Some(live) = monitor_proc::read_live_app(env) {
            return Some(live);
        }
        std::thread::sleep(DESKTOP_POLL);
    }
    eprintln!("the desktop app did not answer in {}s: using the headless server", DESKTOP_START_TIMEOUT.as_secs());
    None
}

pub fn run(args: Args) -> i32 {
    let mut env = Env::process();
    apply_data_dir(&mut env, &args.data_dir);
    if !args.project.is_empty() && !std::path::Path::new(&args.project).is_dir() {
        eprintln!("project folder does not exist: {}", args.project);
        return 2;
    }
    let mut live = monitor_proc::read_live_app(&env);
    if live.is_none() {
        let configured = cfg::locator::read_user_config(&env.user_data_dir()).get("desktop_exe").and_then(Value::as_str).map(String::from);
        let opted_out = args.no_desktop || args.no_browser || args.port != 0 || monitor_proc::browser_open_disabled(&env.get("LOOP_NO_DESKTOP").unwrap_or_default());
        if let Some(desktop) = desktop_to_launch(opted_out, configured.as_deref(), |p| p.is_file()) {
            live = start_desktop_app(&env, &desktop, &args.project);
        }
    }
    if let Some(live) = live {
        let app_url = live["url"].as_str().unwrap_or("").to_string();
        let project_url =
            if args.project.is_empty() { Some(app_url.clone()) } else { monitor_proc::register_project_in_app(&live, &pycompat::nt_abspath(&args.project)) };
        let Some(project_url) = project_url else {
            eprintln!("the live app at {app_url} did not register the project {}", args.project);
            return 1;
        };
        println!("MONITOR_URL={project_url}");
        println!("APP_URL={app_url}");
        return 0;
    }
    let browser_disabled = monitor_proc::browser_open_disabled(&cfg::env::app_env_value(&env, "LOOP_MONITOR_OPEN"));
    let overrides = Overrides {
        user_data_dir: env.get(cfg::env::DATA_DIR_VARIABLE).unwrap_or_default(),
        fixed_port: args.port,
        idle_hours: args.idle_hours,
        open_browser: Some(!(args.no_browser || browser_disabled)),
        ..Overrides::default()
    };
    let assembled = build_application(&env, &overrides);
    let initial = if args.project.is_empty() { String::new() } else { pycompat::nt_abspath(&args.project) };
    match start_application(&assembled, &initial) {
        Ok(()) => 0,
        Err(failure) => {
            eprintln!("could not start the app: {failure}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_app_starts_only_when_installed_and_wanted() {
        let yes = |_: &Path| true;
        let no = |_: &Path| false;
        assert_eq!(desktop_to_launch(false, Some("C:/Apps/Agents Maestro.exe"), yes), Some(PathBuf::from("C:/Apps/Agents Maestro.exe")));
        assert_eq!(desktop_to_launch(true, Some("C:/Apps/Agents Maestro.exe"), yes), None);
        assert_eq!(desktop_to_launch(false, Some("C:/Apps/Agents Maestro.exe"), no), None);
        assert_eq!(desktop_to_launch(false, Some(" "), yes), None);
        assert_eq!(desktop_to_launch(false, None, yes), None);
    }
}
