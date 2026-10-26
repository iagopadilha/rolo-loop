// SPDX-License-Identifier: MIT
//! `harness paths [--json] [--data-dir D]`.

use cfg::locator::{plugin_root_of, read_user_config};
use cfg::{Env, Locator};
use clap::Args as ClapArgs;
use serde::Serialize;
use std::path::PathBuf;

#[derive(ClapArgs)]
pub struct Args {
    /// JSON output
    #[arg(long)]
    json: bool,
    #[arg(long, default_value = "")]
    data_dir: String,
}

#[derive(Serialize)]
pub struct Paths {
    pub maestro_plugin: String,
    pub orchestrator_skill: String,
    pub toolkit: String,
}

/// Repo root = the folder holding `plugin/`, found from the binary's location (or `LOOP_REPO_ROOT`).
pub fn repo_root(env: &Env) -> PathBuf {
    if let Some(v) = env.get("LOOP_REPO_ROOT").filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    for ancestor in exe.ancestors().skip(1) {
        if ancestor.join("plugin").is_dir() {
            return ancestor.to_path_buf();
        }
    }
    exe.parent().map(PathBuf::from).unwrap_or_default()
}

pub fn build_paths(env: &Env, repo_root: PathBuf) -> Paths {
    let config = read_user_config(&env.user_data_dir());
    let locator = Locator::new(env.clone(), repo_root);
    let configured = config
        .get("orchestrator_skill_dir")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let skill = locator.locate_orchestrator_skill_dir(configured);
    Paths {
        maestro_plugin: plugin_root_of(&skill).display().to_string(),
        orchestrator_skill: skill.display().to_string(),
        toolkit: locator.find_toolkit_root(&config).display().to_string(),
    }
}

pub fn run(args: Args) -> i32 {
    let mut env = Env::process();
    if !args.data_dir.is_empty() {
        env.set(cfg::env::DATA_DIR_VARIABLE, &args.data_dir);
    }
    let paths = build_paths(&env, repo_root(&env));
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&paths).expect("serializable")
        );
    } else {
        println!("maestro_plugin={}", paths.maestro_plugin);
        println!("orchestrator_skill={}", paths.orchestrator_skill);
        println!("toolkit={}", paths.toolkit);
    }
    0
}
