// SPDX-License-Identifier: MIT
//! `harness baseline` and `harness grant`: the granted E2E baseline and the simple access release. Both journal into
//! `.specs/project/loop/json/PERMISSIONS-LOG.json`; `baseline` shows a plan unless `--write` is given, so nothing changes silently.

use cfg::Env;
use clap::Args as ClapArgs;
use engine_state::access_grant::{grant_access, AccessContext};
use engine_state::app_ctx::AppCtx;
use engine_state::dev_baseline::{plan, write, BaselineRequest};
use engine_state::loop_io::worktree_of;
use std::path::Path;

#[derive(ClapArgs)]
pub struct BaselineArgs {
    /// The project's repository folder (default: the current folder)
    #[arg(default_value = "")]
    project: String,
    /// A development host as a network entry (`dev.example.com GET,POST`; default METHODS are GET,HEAD). Repeat for more. Hosts are never guessed.
    #[arg(long = "dev-host")]
    dev_hosts: Vec<String>,
    /// A production host or `*.domain` pattern that must stay denied. Repeat for more.
    #[arg(long = "production-host")]
    production_hosts: Vec<String>,
    /// Write the files. Without it the command only shows what it would do.
    #[arg(long)]
    write: bool,
    /// Why the baseline is needed (required with --write; kept in the permissions journal)
    #[arg(long, default_value = "")]
    reason: String,
}

#[derive(ClapArgs)]
pub struct GrantArgs {
    /// The project's repository folder (default: the current folder)
    #[arg(default_value = "")]
    project: String,
    /// `host` (a network entry) or `cost_limit` (US$)
    #[arg(long)]
    kind: String,
    #[arg(long)]
    value: String,
    /// Why the access is needed (required; kept in the permissions journal)
    #[arg(long)]
    reason: String,
    /// Who asks (kept in the journal)
    #[arg(long, default_value = "cli")]
    who: String,
}

struct Where {
    repo: String,
    work: String,
    data_dir: std::path::PathBuf,
    toolkit_root: String,
}

fn locate(env: &Env, project: &str) -> Where {
    let ctx = AppCtx::discover(env);
    let folder = if project.is_empty() { std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default() } else { project.to_string() };
    let repo = engine_state::runtime_isolation::project_root_of(&folder);
    let worktree = worktree_of(&repo);
    let work = if Path::new(&worktree).is_dir() { worktree } else { repo.clone() };
    Where { repo, work, data_dir: ctx.data_dir(), toolkit_root: ctx.toolkit_root.display().to_string() }
}

pub fn baseline(env: &Env, args: BaselineArgs) -> i32 {
    let at = locate(env, &args.project);
    let request = BaselineRequest { dev_hosts: args.dev_hosts, production_hosts: args.production_hosts };
    let planned = plan(&at.repo, &at.data_dir, &request);
    println!("project: {}", at.repo);
    println!("NETWORK.json would hold:");
    for entry in &planned.allow {
        println!("  allow {entry}{}", if planned.added.contains(entry) { "   (new)" } else { "" });
    }
    for host in &planned.production_hosts {
        println!("  denied (production, not listed): {host}");
    }
    println!("TEST-ENVIRONMENT.md: {}", if planned.test_environment.is_some() { "would be written as a skeleton" } else { "exists, kept as it is" });
    for problem in &planned.problems {
        println!("problem: {problem}");
    }
    if !args.write {
        println!("dry run: nothing was written. Run again with --write --reason \"...\" to apply it.");
        return i32::from(!planned.problems.is_empty());
    }
    let context = AccessContext { repo: &at.repo, work: &at.work, data_dir: &at.data_dir, toolkit_root: &at.toolkit_root, who: "cli" };
    match write(&context, &planned, &args.reason) {
        Ok(done) => {
            println!("{}", done.message);
            0
        }
        Err(error) => {
            println!("refused: {error}");
            1
        }
    }
}

pub fn grant(env: &Env, args: GrantArgs) -> i32 {
    let at = locate(env, &args.project);
    let context = AccessContext { repo: &at.repo, work: &at.work, data_dir: &at.data_dir, toolkit_root: &at.toolkit_root, who: &args.who };
    let result = grant_access(&context, &args.kind, &args.value, &args.reason);
    println!("{}", result.message);
    i32::from(!result.granted)
}
