// SPDX-License-Identifier: MIT
//! `harness serve [--project <repo>] [--port N] [--idle-hours H] [--no-browser] [--data-dir D]`: starts the multi-project panel. If an app is
//! already alive (app.json), it only registers the project in it, prints MONITOR_URL=<project url> and exits 0.

use app::composition::{build_application, start_application};
use app::config::Overrides;
use cfg::Env;
use clap::Args as ClapArgs;
use engine_state::monitor_proc;

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
    /// does not open the browser when it starts
    #[arg(long)]
    no_browser: bool,
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

pub fn run(args: Args) -> i32 {
    let mut env = Env::process();
    apply_data_dir(&mut env, &args.data_dir);
    if !args.project.is_empty() && !std::path::Path::new(&args.project).is_dir() {
        eprintln!("project folder does not exist: {}", args.project);
        return 2;
    }
    if let Some(live) = monitor_proc::read_live_app(&env) {
        let app_url = live["url"].as_str().unwrap_or("").to_string();
        let project_url = if args.project.is_empty() { Some(app_url.clone()) } else { monitor_proc::register_project_in_app(&live, &pycompat::nt_abspath(&args.project)) };
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
