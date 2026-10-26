// SPDX-License-Identifier: MIT
//! Subcommands that forward their arguments to an engine module as they are: `tasks`, `graph`, `classify`, `scale`.

use clap::Args as ClapArgs;
use std::io::Write;

#[derive(ClapArgs)]
pub struct Forwarded {
    /// Arguments forwarded as they are
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<String>,
}

fn emit(code: i32, stdout: &str, stderr: &str) -> i32 {
    let _ = std::io::stdout().write_all(stdout.as_bytes());
    let _ = std::io::stderr().write_all(stderr.as_bytes());
    code
}

pub fn tasks(f: Forwarded) -> i32 {
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let (code, out, err) = engine_state::task_waves::run_cli(&f.args, &cwd);
    emit(code, &out, &err)
}

pub fn graph(f: Forwarded) -> i32 {
    let (code, out) = engine_state::graph_view::run_cli(&f.args);
    emit(code, &out, "")
}

pub fn classify(f: Forwarded) -> i32 {
    let (code, out) = engine_state::classify_structure::run_cli(&f.args);
    emit(code, &out, "")
}

pub fn scale(f: Forwarded) -> i32 {
    let (code, out) = engine_state::scale::run_cli(&f.args);
    emit(code, &out, "")
}

pub fn audit(f: Forwarded) -> i32 {
    let (code, out) = engine_state::audit_deps::run_cli(&f.args);
    emit(code, &out, "")
}
