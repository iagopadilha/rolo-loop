// SPDX-License-Identifier: MIT
//! `harness demo`: seeds the demo project, starts the app on an isolated data folder, opens the project's panel and runs the animator until Ctrl+C (or
//! the app's shutdown). Then removes the demo files unless `--keep`.
//!
//! The app is the real one (same composition as `serve`) with demo overrides: the transcripts folder points to the demo's fake Claude Code transcripts,
//! and `claude` plus the engine commands the panel can trigger are stubs (the harness executable itself) that only print a notice.

pub mod animator;
pub mod catalog;
pub mod player;
pub mod seeder;
pub mod stubs;
pub mod transcripts;
pub mod workspace;

use crate::composition::{build_application, start_application};
use crate::config::Overrides;
use crate::services::App;
use animator::{compute_story_seconds, DemoAnimator};
use catalog::build_turn_plans;
use cfg::Env;
use seeder::{DemoProjectSeeder, ENGINE_STUB_SUBCOMMAND};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use workspace::DemoWorkspace;

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
/// The demo app never shuts itself down for idleness (a recorder may leave it alone for a long time).
const DEMO_IDLE_HOURS: f64 = 0.0;
const ENGINE_COMMANDS: [&str; 3] = ["run_loop", "delivery", "loop_summary"];
const INTERRUPT_POLL_MILLISECONDS: u64 = 100;

#[derive(Clone, Debug)]
pub struct DemoOptions {
    pub port: u16,
    pub no_browser: bool,
    pub speed: f64,
    pub data_dir: String,
    pub loop_forever: bool,
    pub keep: bool,
}

/// A copy of the executable named `demo-claude.exe`: the `claude_binary` of the demo.
fn install_claude_stub(workspace: &DemoWorkspace, exe: &Path) -> std::io::Result<String> {
    let folder = workspace.base_dir.join("bin");
    std::fs::create_dir_all(&folder)?;
    let target = folder.join(format!("{}{}", stubs::CLAUDE_STUB_STEM, std::env::consts::EXE_SUFFIX));
    std::fs::copy(exe, &target)?;
    Ok(target.display().to_string())
}

pub fn run_demo(options: &DemoOptions) -> i32 {
    if !(options.speed.is_finite() && options.speed > 0.0) {
        eprintln!("--speed must be greater than 0");
        return 2;
    }
    let workspace = match DemoWorkspace::create(&options.data_dir) {
        Ok(workspace) => workspace,
        Err(failure) => {
            println!("could not run the demo: {failure}");
            return EXIT_FAILURE;
        }
    };
    let code = run_in_workspace(&workspace, options);
    if options.keep {
        println!("demo files kept in {}", workspace.base_dir.display());
    } else {
        workspace.remove();
    }
    code
}

fn run_in_workspace(workspace: &DemoWorkspace, options: &DemoOptions) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(failure) => {
            println!("could not run the demo: {failure}");
            return EXIT_FAILURE;
        }
    };
    match serve_demo(workspace, options, &exe) {
        Ok(()) => EXIT_OK,
        Err(failure) => {
            println!("could not run the demo: {failure}");
            EXIT_FAILURE
        }
    }
}

fn serve_demo(workspace: &DemoWorkspace, options: &DemoOptions, exe: &Path) -> Result<(), String> {
    let exe_text = exe.display().to_string();
    let mut seeder = DemoProjectSeeder::new(workspace, &exe_text);
    let claude_stub = install_claude_stub(workspace, exe).map_err(|e| e.to_string())?;
    let player = seeder.seed()?;
    let data_dir = workspace.app_data_dir.display().to_string();
    let mut env = Env::process();
    env.set(cfg::env::DATA_DIR_VARIABLE, &data_dir); // engine children and this process read the isolated folder
    std::env::set_var(cfg::env::DATA_DIR_VARIABLE, &data_dir);
    let overrides = Overrides {
        user_data_dir: data_dir,
        claude_binary: claude_stub,
        fixed_port: options.port,
        idle_hours: Some(DEMO_IDLE_HOURS),
        open_browser: Some(!options.no_browser),
        claude_transcripts_dir: workspace.transcripts_dir.display().to_string(),
    };
    let assembled = build_application(&env, &overrides);
    for name in ENGINE_COMMANDS {
        assembled.app.set_engine_override(name, vec![exe_text.clone(), ENGINE_STUB_SUBCOMMAND.to_string()]);
    }
    let log_app = assembled.app.clone();
    let log: Arc<dyn Fn(&str, &str) + Send + Sync> = Arc::new(move |event, detail| log_app.log.info(event, &[("detail", json!(detail))]));
    let mut animator = DemoAnimator::start(seeder, player, log, options.speed, options.loop_forever);
    println!("DEMO_DIR={}", workspace.base_dir.display());
    println!("DEMO_CYCLE_SECONDS={}", compute_story_seconds(&build_turn_plans(), options.speed).round() as i64);
    watch_interrupt(&assembled.app);
    let served = start_application(&assembled, &workspace.repo_dir.display().to_string());
    animator.stop_and_join();
    if !options.keep {
        forget_demo_project(&assembled.app, workspace);
        assembled.app.log.info("demo_files_removed", &[("path", json!(workspace.base_dir.display().to_string()))]);
    }
    served.map_err(|e| e.to_string())
}

/// Ctrl+C asks the server to stop, so the demo files are removed the normal way.
fn watch_interrupt(app: &Arc<App>) {
    winproc::install_ctrl_c_handler();
    let app = app.clone();
    let _ = std::thread::Builder::new().name("demo-interrupt".into()).spawn(move || loop {
        if winproc::interrupted() {
            if let Some(shutdown) = app.shutdown.get() {
                shutdown();
            }
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(INTERRUPT_POLL_MILLISECONDS));
    });
}

/// A chosen `--data-dir` keeps its project registry clean.
fn forget_demo_project(app: &App, workspace: &DemoWorkspace) {
    if workspace.is_temporary {
        return;
    }
    let service = app.project_service();
    let ours = pycompat::nt_normcase(&workspace.repo_dir.display().to_string());
    for project in service.list_projects() {
        if pycompat::nt_normcase(&project.repo_path) == ours {
            let _ = service.forget_project(&project.id);
        }
    }
}
