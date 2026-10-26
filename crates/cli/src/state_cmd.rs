// SPDX-License-Identifier: MIT
//! `harness state <validate|migrate|disjoint|graph|loop|next> <folder> ...` (the typed graph state of the loop).

use clap::Args as ClapArgs;
use engine_state::loop_state::run_cli;
use engine_state::repos::{load_repos, primary_of};
use std::io::Write;

#[derive(ClapArgs)]
pub struct Args {
    /// Arguments forwarded as they are: `<command> <folder> [flags...]`
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    args: Vec<String>,
}

/// `(repo names, primary name)` of a project from its REPOS.json.
pub fn repo_context(work: &str) -> (Vec<String>, String) {
    let repos = load_repos(work);
    let primary = primary_of(&repos).name.clone();
    (repos.iter().map(|r| r.name.clone()).collect(), primary)
}

pub fn run(args: Args) -> i32 {
    let (code, out) = run_cli(&args.args, &repo_context);
    let _ = std::io::stdout().write_all(out.as_bytes());
    code
}
