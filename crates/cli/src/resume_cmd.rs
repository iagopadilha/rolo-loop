// SPDX-License-Identifier: MIT
//! `harness resume <repo> [--instruction TEXT]`: resumes a stopped loop from outside the panel (backlog H5).
//!
//! It does what the panel's Resume does and nothing else: reads the command the loop saved (`LOOP-CMD.json`), checks and rebuilds it with
//! the same rules (`loop_command::rebuild_loop_command`: this program, the `loop` subcommand, known options with checked values, this
//! repository, a folder of this project), adds the instruction when there is one and the `claude` commands of the user's config, and runs
//! `harness loop`. No token and no web call: the checks are the file's, and a second loop on the same worktree is refused by the loop's own lock.

use clap::Args as ClapArgs;
use engine_state::{claude_binaries, loop_command, loop_io, loop_state};

#[derive(ClapArgs)]
pub struct Args {
    /// The project's repository folder
    pub repo: String,
    /// What the loop should do differently on resuming (the same as `--answer` for a stopped loop)
    #[arg(long, default_value = "")]
    pub instruction: String,
}

/// The arguments for `harness loop`, or the reason nothing can be resumed.
pub fn build_args(repo: &str, instruction: &str, env: &cfg::Env, program: &str) -> Result<Vec<String>, String> {
    let repo = pycompat::nt_abspath(repo);
    let work = loop_io::worktree_of(&repo);
    let path = loop_state::loop_path(&work, "LOOP-CMD.json");
    let saved = loop_io::load_json(&path);
    let usable = saved.get("argv").and_then(serde_json::Value::as_array).is_some_and(|a| !a.is_empty());
    if !usable {
        return Err(format!("nothing to resume: {} is missing or empty (start the loop from the panel once)", path.display()));
    }
    let config = cfg::locator::read_user_config(&env.user_data_dir());
    let claude = config.get("claude_binary").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
    let fallbacks = claude_binaries::read_fallbacks(config.get("claude_fallback_binaries").unwrap_or(&serde_json::Value::Null));
    loop_command::resume_args(&saved, &repo, &[repo.clone(), work], program, instruction, &claude, &fallbacks)
        .map_err(|refusal| format!("LOOP-CMD.json was not trusted, nothing was started ({refusal}). Start the loop again from the panel to record it again."))
}

pub fn run(args: Args) -> i32 {
    let env = cfg::Env::process();
    let program = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "harness".to_string());
    match build_args(&args.repo, &args.instruction, &env, &program) {
        Ok(loop_args) => engine_state::run_loop::run_cli(&loop_args),
        Err(message) => {
            eprintln!("harness resume: {message}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_saved_means_nothing_to_resume_and_the_message_says_where_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(&repo).unwrap();
        let env = cfg::Env::from_pairs([("LOOP_DATA_DIR", dir.path().join("data").display().to_string().as_str())]);
        let message = build_args(&repo.display().to_string(), "", &env, "harness.exe").unwrap_err();
        assert!(message.starts_with("nothing to resume:") && message.contains("LOOP-CMD.json") && message.contains("panel"), "{message}");
    }

    #[test]
    fn a_saved_command_is_rebuilt_with_the_panels_rules_and_a_foreign_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(&repo).unwrap();
        let repo_text = pycompat::nt_abspath(&repo.display().to_string());
        let work = loop_io::worktree_of(&repo_text);
        let env = cfg::Env::from_pairs([("LOOP_DATA_DIR", dir.path().join("data").display().to_string().as_str())]);
        let path = loop_state::loop_path(&work, "LOOP-CMD.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::json!({"argv": ["harness", "loop", repo_text, "--wave", "2", "--answer", "old"]}).to_string()).unwrap();
        let args = build_args(&repo_text, "do the other thing", &env, "harness.exe").unwrap();
        assert_eq!(args, vec![repo_text.clone(), "--wave".into(), "2".into(), "--instruction".into(), "do the other thing".into()]);
        std::fs::write(&path, serde_json::json!({"argv": ["powershell.exe", "loop", repo_text]}).to_string()).unwrap();
        assert!(build_args(&repo_text, "", &env, "harness.exe").unwrap_err().contains("was not trusted"));
    }
}
