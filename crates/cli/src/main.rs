// SPDX-License-Identifier: MIT
//! `harness`: command line of agent-maestro.

mod access_cmd;
mod demo_cmd;
mod doctor_cmd;
mod forward;
mod grant_cmd;
mod install_cmd;
mod paths;
mod resume_cmd;
mod serve_cmd;
mod state_cmd;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "harness", version, about = "agent-maestro: loop engineering for Claude Code")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Starts the multi-project panel (or registers the project in the live app)
    Serve(serve_cmd::Args),
    /// `loop <repo> --check`: the pre-flight check of a loop
    Check(forward::Forwarded),
    /// Diagnostics: skill, guard, claude, toolkit, notify channels (--access: what a loop session can reach)
    Doctor(doctor_cmd::Args),
    /// The granted E2E baseline: localhost plus the development hosts YOU list, production left denied (dry run unless --write)
    Baseline(grant_cmd::BaselineArgs),
    /// Releases one thing the E2E needs (a network entry or a higher cost limit) and journals it
    Grant(grant_cmd::GrantArgs),
    /// Resumes a stopped loop from outside the panel, with the same checks and settings as the panel's Resume
    Resume(resume_cmd::Args),
    /// Writes ~/.loop-harness/config.json so the skill finds the app
    Install(install_cmd::Args),
    /// The panel on a fake project with mocked, moving loop data (never calls claude)
    Demo(demo_cmd::Args),
    /// Stand-in for the engine commands inside `demo` (not for direct use)
    #[command(name = "demo-engine", hide = true)]
    DemoEngine(forward::Forwarded),
    /// Where the Maestro plugin, the orchestrator skill and the dev toolkit are
    Paths(paths::Args),
    /// Typed graph state of the loop (validate, next, graph, migrate, disjoint, loop)
    State(state_cmd::Args),
    /// Task waves of one feature (waves, set, mirror)
    Tasks(forward::Forwarded),
    /// Draws the loop progress from the ROADMAP
    Graph(forward::Forwarded),
    /// Classifies a project's structure as STANDARD or LEGACY
    Classify(forward::Forwarded),
    /// Sizes the brownfield mapping
    Scale(forward::Forwarded),
    /// Audits forgotten dependencies between ROADMAP features
    Audit(forward::Forwarded),
    /// Monorepo subprojects: detect, show, for-paths
    Subprojects(forward::Forwarded),
    /// Contract merge into the root docs/codebase/SYSTEM.md (merge, pending)
    #[command(name = "system-map")]
    SystemMap(forward::Forwarded),
    /// Shows or edits the Standards slots of a project's CLAUDE.md
    Standards(forward::Forwarded),
    /// Writes the loop's final summary in Markdown
    Summary(forward::Forwarded),
    /// Free inventory of the code of a folder
    Inventory(forward::Forwarded),
    /// Creates a project block rule and proves it in the real guard
    Rules(forward::Forwarded),
    /// Sends a test or custom notice through the configured channels
    Notify(forward::Forwarded),
    /// Pushes the feature branch and opens the PR at the end of the flow
    Deliver(forward::Forwarded),
    /// The unsupervised loop: runs claude in turns until it finishes or gets stuck
    Loop(forward::Forwarded),
    /// Finishes a loop safely: verified backup, durable plan back, worktree removal
    CloseLoop(forward::Forwarded),
}

fn main() {
    let exe_stem = std::env::current_exe().ok().and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned())).unwrap_or_default();
    if app::demo::stubs::is_claude_stub(&exe_stem) {
        let args: Vec<String> = std::env::args().skip(1).collect();
        std::process::exit(app::demo::stubs::claude_stub(&args));
    }
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Serve(args) => serve_cmd::run(args),
        Command::Check(a) => forward::check(a),
        Command::Demo(args) => demo_cmd::run(args),
        Command::DemoEngine(a) => app::demo::stubs::engine_stub(&a.args),
        Command::Doctor(args) => doctor_cmd::run(args),
        Command::Install(args) => install_cmd::run(args),
        Command::Resume(args) => resume_cmd::run(args),
        Command::Paths(args) => paths::run(args),
        Command::Baseline(args) => grant_cmd::baseline(&cfg::Env::process(), args),
        Command::Grant(args) => grant_cmd::grant(&cfg::Env::process(), args),
        Command::State(args) => state_cmd::run(args),
        Command::Tasks(a) => forward::tasks(a),
        Command::Graph(a) => forward::graph(a),
        Command::Classify(a) => forward::classify(a),
        Command::Scale(a) => forward::scale(a),
        Command::Audit(a) => forward::audit(a),
        Command::Subprojects(a) => forward::subprojects(a),
        Command::SystemMap(a) => forward::system_map(a),
        Command::Standards(a) => forward::standards(a),
        Command::Summary(a) => forward::summary(a),
        Command::Inventory(a) => forward::inventory(a),
        Command::Rules(a) => forward::rules(a),
        Command::Notify(a) => forward::notify(a),
        Command::Deliver(a) => forward::deliver(a),
        Command::Loop(a) => forward::run_loop(a),
        Command::CloseLoop(a) => forward::close_loop(a),
    };
    std::process::exit(code);
}
