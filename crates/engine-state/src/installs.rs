// SPDX-License-Identifier: MIT
//! Routine installs run by the engine, per subproject, cached by lockfile hash.
//!
//! A routine install restores what a lockfile or manifest ALREADY declares (`npm ci`, `dotnet restore`, `pip install -r`, `cargo fetch`...,
//! see [`crate::detect_setup::TOOLCHAINS`]). It is not "a new dependency": the engine runs it itself, once per subproject, and runs it
//! again only when the lockfile changed or what it produced (`node_modules`) is gone. Adding a package is NOT routine and stays a decision.
//! The cache lives in the worktree's `loop/internal/installs.json` as `{"<dir>|<command>": "sha256:..."}`; nothing secret is stored or logged.

use crate::detect_setup::{install_plan, InstallStep};
use crate::loop_io::{hidden, load_json, save_json};
use crate::loop_state::loop_path;
use serde_json::{json, Map, Value};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

pub const CACHE_NAME: &str = "installs.json";
const INSTALL_TIMEOUT_SECONDS: u64 = 900;
const TAIL_CHARS: usize = 400;

/// What happened to one planned step.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Ran and succeeded.
    Ran,
    /// The lockfile hash matches the last successful run and the artifact is still there: nothing to do.
    UpToDate,
    /// The folder or its manifest is gone, or the step is not routine: left alone.
    Skipped(String),
    /// Exit code and the tail of the output.
    Failed(i32, String),
}

fn tail(text: &str) -> String {
    let chars: Vec<char> = text.trim().chars().collect();
    chars[chars.len().saturating_sub(TAIL_CHARS)..].iter().collect()
}

/// `(exit code, output tail)` of `command` run through the platform shell in `cwd`, with no console window and a time limit.
pub fn run_shell_hidden(command: &str, cwd: &Path, seconds: u64) -> (i32, String) {
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd");
        c.arg("/C").raw_arg(command);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    hidden(&mut cmd).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => return (1, error.to_string()),
    };
    let mut readers = Vec::new();
    for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)].into_iter().flatten()
    {
        readers.push(std::thread::spawn(move || {
            let (mut bytes, mut pipe) = (Vec::new(), pipe);
            let _ = pipe.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        }));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(1),
            Ok(None) if std::time::Instant::now() >= deadline => {
                crate::loop_io::kill_tree(i64::from(child.id()));
                let _ = child.kill();
                let _ = child.wait();
                return (1, format!("timed out after {seconds} seconds"));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(error) => return (1, error.to_string()),
        }
    };
    let output: Vec<String> = readers.into_iter().map(|r| r.join().unwrap_or_default()).collect();
    (code, tail(&output.join("\n")))
}

fn load_cache(work: &str) -> Map<String, Value> {
    load_json(&loop_path(work, CACHE_NAME)).as_object().cloned().unwrap_or_default()
}

/// Runs the routine steps of `plan` under `work` through `exec(step, folder) -> (exit code, output tail)`. A step whose lockfile hash is
/// the cached one (and whose artifact exists) is not run. A failure does not stop the others: each subproject stands alone. The cache is
/// saved after every success, so an interrupted start loses nothing.
pub fn run_plan(work: &str, plan: &[InstallStep], exec: &mut dyn FnMut(&InstallStep, &Path) -> (i32, String)) -> Vec<(InstallStep, Outcome)> {
    let mut cache = load_cache(work);
    let mut results = Vec::new();
    for step in plan {
        let folder = if step.dir.is_empty() { Path::new(work).to_path_buf() } else { Path::new(work).join(&step.dir) };
        if !step.routine {
            results.push((step.clone(), Outcome::Skipped("no lockfile declares these dependencies; installing them would change the project".into())));
            continue;
        }
        let hash = step.fingerprint_hash(Path::new(work));
        if !folder.is_dir() || hash.is_empty() {
            results.push((step.clone(), Outcome::Skipped("the folder or its manifest is not in the worktree".into())));
            continue;
        }
        let produced = step.artifact.is_empty() || folder.join(&step.artifact).exists();
        if produced && cache.get(&step.key()).and_then(Value::as_str) == Some(hash.as_str()) {
            results.push((step.clone(), Outcome::UpToDate));
            continue;
        }
        let (code, output) = exec(step, &folder);
        if code == 0 {
            cache.insert(step.key(), json!(hash));
            let _ = save_json(&loop_path(work, CACHE_NAME), &Value::Object(cache.clone()));
            results.push((step.clone(), Outcome::Ran));
        } else {
            cache.remove(&step.key());
            results.push((step.clone(), Outcome::Failed(code, output)));
        }
    }
    results
}

/// [`run_plan`] over the plan detected in `work` itself (the worktree already holds the lockfiles) with the real shell.
pub fn run_detected(work: &str) -> Vec<(InstallStep, Outcome)> {
    let plan = install_plan(work);
    run_plan(work, &plan, &mut |step, folder| run_shell_hidden(&step.command, folder, INSTALL_TIMEOUT_SECONDS))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lay_out(root: &Path) {
        for (name, body) in [("web/package.json", "{}"), ("web/package-lock.json", "{\"a\":1}"), ("api/App.sln", "")] {
            let p = root.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
    }

    #[test]
    fn runs_each_subproject_once_then_uses_the_cache_until_the_lockfile_changes() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        lay_out(dir.path());
        let plan = install_plan(&work);
        let calls = std::cell::RefCell::new(Vec::<String>::new());
        let mut exec = |step: &InstallStep, folder: &Path| {
            calls.borrow_mut().push(format!("{}:{}", step.dir, step.command));
            if step.command == "npm ci" {
                std::fs::create_dir_all(folder.join("node_modules")).unwrap();
            }
            (0, String::new())
        };
        let first = run_plan(&work, &plan, &mut exec);
        assert!(first.iter().all(|(_, o)| *o == Outcome::Ran));
        let second = run_plan(&work, &plan, &mut exec);
        assert!(second.iter().all(|(_, o)| *o == Outcome::UpToDate), "{second:?}");
        assert_eq!(*calls.borrow(), vec!["api:dotnet restore", "web:npm ci"]);
        std::fs::write(dir.path().join("web/package-lock.json"), "{\"a\":2}").unwrap();
        let third = run_plan(&work, &plan, &mut exec);
        assert_eq!(third.iter().map(|(s, o)| (s.dir.as_str(), o.clone())).collect::<Vec<_>>(), vec![("api", Outcome::UpToDate), ("web", Outcome::Ran)]);
        std::fs::remove_dir_all(dir.path().join("web/node_modules")).unwrap();
        std::fs::write(dir.path().join("web/package-lock.json"), "{\"a\":2}").unwrap();
        let fourth = run_plan(&work, &plan, &mut exec);
        assert_eq!(fourth[1].1, Outcome::Ran, "a deleted node_modules is installed again");
    }

    #[test]
    fn a_failure_is_reported_not_cached_and_does_not_stop_the_other_subprojects() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        lay_out(dir.path());
        let plan = install_plan(&work);
        let mut exec = |step: &InstallStep, _: &Path| if step.dir == "api" { (3, "feed unreachable".to_string()) } else { (0, String::new()) };
        let results = run_plan(&work, &plan, &mut exec);
        assert_eq!(results[0].1, Outcome::Failed(3, "feed unreachable".into()));
        assert_eq!(results[1].1, Outcome::Ran);
        let again = run_plan(&work, &plan, &mut exec);
        assert!(matches!(again[0].1, Outcome::Failed(..)), "a failed install is retried");
    }

    #[test]
    fn a_manifest_without_lockfile_is_never_installed_by_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let mut ran = false;
        let results = run_plan(&work, &install_plan(&work), &mut |_, _| {
            ran = true;
            (0, String::new())
        });
        assert!(!ran && matches!(results[0].1, Outcome::Skipped(_)));
    }

    #[test]
    fn the_real_shell_runs_hidden_and_reports_the_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let (ok, _) = run_shell_hidden("echo ready", dir.path(), 30);
        assert_eq!(ok, 0);
        let (bad, _) = run_shell_hidden("exit 7", dir.path(), 30);
        assert_eq!(bad, 7);
    }
}
