// SPDX-License-Identifier: MIT
//! `harness`: command line of claude-maestro.

mod forward;
mod paths;
mod state_cmd;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "harness",
    version,
    about = "claude-maestro: loop engineering for Claude Code"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Paths(args) => paths::run(args),
        Command::State(args) => state_cmd::run(args),
        Command::Tasks(a) => forward::tasks(a),
        Command::Graph(a) => forward::graph(a),
        Command::Classify(a) => forward::classify(a),
        Command::Scale(a) => forward::scale(a),
        Command::Audit(a) => forward::audit(a),
        Command::Standards(a) => forward::standards(a),
        Command::Summary(a) => forward::summary(a),
        Command::Inventory(a) => forward::inventory(a),
        Command::Rules(a) => forward::rules(a),
        Command::Notify(a) => forward::notify(a),
        Command::Deliver(a) => forward::deliver(a),
    };
    std::process::exit(code);
}
