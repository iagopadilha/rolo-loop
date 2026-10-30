// SPDX-License-Identifier: MIT
//! `harness verify <root> --feature <slug> [--scope P] [--timeout S] [--needs-contract]`: the verification step as code.
//!
//! Runs the toolkit's `craft verify` (build, typecheck, lint, test as the project declares them) and prints the verifier's return
//! block (`FEATURE`, `VERDICT`, `RAN`, `FAIL`, `NOTES`) without a model in the loop. Exit code: 0 PASS, 1 FAIL, 3 UNVERIFIED (nothing
//! detected to run), 4 the toolkit is missing (dispatch the `verifier` agent instead), 2 bad arguments.

use crate::paths::{build_paths, repo_root};
use cfg::Env;
use clap::Args as ClapArgs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(ClapArgs)]
pub struct Args {
    /// Project root (the loop's worktree)
    root: PathBuf,
    /// The feature slug being verified
    #[arg(long)]
    feature: String,
    /// Folder the checks are scoped to (default: the root)
    #[arg(long)]
    scope: Option<PathBuf>,
    /// Seconds allowed per step
    #[arg(long, default_value_t = 300)]
    timeout: u32,
    /// A back-end feature of a monorepo: warn when `.specs/features/<slug>/contract.md` is missing
    #[arg(long)]
    needs_contract: bool,
}

/// The block the orchestrator reads: the `FEATURE` line first, then what `craft verify` printed, then the contract warning.
pub fn compose_output(feature: &str, craft_stdout: &str, contract_missing: bool) -> String {
    let mut out = format!("FEATURE: {feature}\n");
    let body = craft_stdout.trim_end();
    if body.is_empty() {
        out.push_str("VERDICT: UNVERIFIED\nRAN: -\nFAIL: none\nNOTES: craft verify printed nothing\n");
    } else {
        out.push_str(body);
        out.push('\n');
    }
    if contract_missing {
        out.push_str("WARNING: contract.md missing (a back-end feature of a monorepo needs .specs/features/<slug>/contract.md); send the executor back for it\n");
    }
    out
}

fn contract_missing(root: &Path, feature: &str) -> bool {
    !root.join(".specs").join("features").join(feature).join("contract.md").is_file()
}

pub fn run(args: Args) -> i32 {
    let env = Env::process();
    let toolkit = PathBuf::from(build_paths(&env, repo_root(&env)).toolkit);
    let craft = toolkit.join("bin").join("craft.exe");
    if toolkit.as_os_str().is_empty() || !craft.is_file() {
        print!("FEATURE: {}\nVERDICT: UNVERIFIED\nRAN: -\nFAIL: none\nNOTES: toolkit missing (run harness doctor); dispatch the verifier agent instead\n", args.feature);
        return 4;
    }
    let scope = args.scope.clone().unwrap_or_else(|| args.root.clone());
    let output = Command::new(&craft)
        .current_dir(&args.root)
        .args(["verify", "--scope"])
        .arg(&scope)
        .args(["--timeout", &args.timeout.to_string()])
        .output();
    let output = match output {
        Ok(o) => o,
        Err(error) => {
            eprintln!("harness verify: cannot run {}: {error}", craft.display());
            return 2;
        }
    };
    let code = output.status.code().unwrap_or(2);
    if !matches!(code, 0 | 1 | 3) {
        eprintln!("harness verify: craft verify exit {code}\n{}", String::from_utf8_lossy(&output.stderr));
        return 2;
    }
    let missing = args.needs_contract && contract_missing(&args.root, &args.feature);
    print!("{}", compose_output(&args.feature, &String::from_utf8_lossy(&output.stdout), missing));
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_starts_with_the_feature_line_and_keeps_the_verdict() {
        let out = compose_output("orders", "VERDICT: PASS\nRAN: dotnet build\nFAIL: none\n", false);
        assert!(out.starts_with("FEATURE: orders\nVERDICT: PASS\n"));
        assert!(!out.contains("WARNING"));
    }

    #[test]
    fn an_empty_craft_output_is_unverified_not_a_pass() {
        assert!(compose_output("a", "  \n", false).contains("VERDICT: UNVERIFIED"));
    }

    #[test]
    fn a_missing_contract_is_a_warning_line() {
        let out = compose_output("a", "VERDICT: PASS\n", true);
        assert!(out.contains("WARNING: contract.md missing"));
    }

    #[test]
    fn the_contract_check_looks_in_the_feature_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(contract_missing(dir.path(), "x"));
        let feature = dir.path().join(".specs").join("features").join("x");
        std::fs::create_dir_all(&feature).unwrap();
        std::fs::write(feature.join("contract.md"), "- [add] route GET /h — p (f)\n").unwrap();
        assert!(!contract_missing(dir.path(), "x"));
    }
}
