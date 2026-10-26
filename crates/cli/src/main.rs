// SPDX-License-Identifier: MIT
//! `harness`: command line of claude-maestro.

mod paths;

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
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Paths(args) => paths::run(args),
    };
    std::process::exit(code);
}
