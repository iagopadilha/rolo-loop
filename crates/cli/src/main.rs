// SPDX-License-Identifier: MIT
//! `harness`: command line of claude-maestro.

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
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Paths(args) => paths::run(args),
        Command::State(args) => state_cmd::run(args),
    };
    std::process::exit(code);
}
