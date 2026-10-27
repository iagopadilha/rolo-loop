// SPDX-License-Identifier: MIT
//! Running system processes: synchronous with a timeout, detached (outlives the app), live pid and kill tree.
//!
//! Never uses a shell: `argv` is always a list. It is the app's only place that starts processes (the engine has its own helpers in `loop_io`).

use engine_state::loop_io::{kill_tree, pid_alive, run_process_checked, ProcessResult};
use std::path::Path;

pub const TIMEOUT_CODE: i32 = -9;
pub const START_FAILURE_CODE: i32 = 127;
pub const TIMEOUT_MESSAGE: &str = "timed out";

/// What the services need from the operating system, so tests can replace it.
pub trait ProcessRunner: Send + Sync {
    /// Runs and waits. Returns `(code, output, error)` as UTF-8 text. A timeout kills the tree and returns [`TIMEOUT_CODE`].
    fn run_and_wait(
        &self,
        argv: &[String],
        folder: &str,
        timeout_seconds: u64,
        input_text: Option<&str>,
        extra_env: &[(String, String)],
        env: Option<&[(String, String)]>,
    ) -> (i32, String, String);
    /// Starts the process independently of this one (it outlives the app), output and error in `log_path`. Returns the pid.
    fn spawn_detached(&self, argv: &[String], folder: &str, log_path: &str, extra_env: &[(String, String)]) -> std::io::Result<i64>;
    /// True if the pid exists and is still running.
    fn is_alive(&self, pid: i64) -> bool;
    /// Kills the process and its children. Never fails.
    fn kill_tree(&self, pid: i64);
}

/// The real implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemProcesses;

/// The environment to run with: `None` when nothing changes (inherit); otherwise the base environment (or the process one) plus the extras.
pub fn build_env(env: Option<&[(String, String)]>, extra_env: &[(String, String)]) -> Option<Vec<(String, String)>> {
    if env.is_none() && extra_env.is_empty() {
        return None;
    }
    let mut result: Vec<(String, String)> = env.map(|e| e.to_vec()).unwrap_or_else(|| std::env::vars().collect());
    for (k, v) in extra_env {
        match result.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(k)) {
            Some(slot) => slot.1 = v.clone(),
            None => result.push((k.clone(), v.clone())),
        }
    }
    Some(result)
}

impl ProcessRunner for SystemProcesses {
    fn run_and_wait(
        &self,
        argv: &[String],
        folder: &str,
        timeout_seconds: u64,
        input_text: Option<&str>,
        extra_env: &[(String, String)],
        env: Option<&[(String, String)]>,
    ) -> (i32, String, String) {
        if argv.is_empty() {
            return (START_FAILURE_CODE, String::new(), "could not start ?: no command".into());
        }
        let full_env = build_env(env, extra_env).unwrap_or_else(|| std::env::vars().collect());
        let cwd = if !folder.is_empty() && Path::new(folder).is_dir() {
            folder.to_string()
        } else {
            std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()
        };
        let ProcessResult { code, stdout, stderr, timed_out } = match run_process_checked(argv, &cwd, &full_env, input_text.unwrap_or(""), timeout_seconds) {
            Ok(result) => result,
            Err(failure) => {
                let name = Path::new(&argv[0]).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "?".into());
                return (START_FAILURE_CODE, String::new(), format!("could not start {name}: {failure}"));
            }
        };
        if timed_out {
            return (TIMEOUT_CODE, stdout, format!("{stderr}\n{TIMEOUT_MESSAGE}").trim().to_string());
        }
        (code, stdout, stderr)
    }

    fn spawn_detached(&self, argv: &[String], folder: &str, log_path: &str, extra_env: &[(String, String)]) -> std::io::Result<i64> {
        if let Some(parent) = Path::new(log_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        let log = std::fs::OpenOptions::new().create(true).append(true).open(log_path)?;
        let mut command = std::process::Command::new(&argv[0]);
        command.args(&argv[1..]).stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
        if !folder.is_empty() && Path::new(folder).is_dir() {
            command.current_dir(folder);
        }
        if let Some(env) = build_env(None, extra_env) {
            command.env_clear().envs(env);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
        }
        Ok(i64::from(command.spawn()?.id()))
    }

    fn is_alive(&self, pid: i64) -> bool {
        pid_alive(pid)
    }

    fn kill_tree(&self, pid: i64) {
        if pid > 0 {
            kill_tree(pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn runs_reports_start_failures_and_timeouts() {
        let runner = SystemProcesses;
        let (code, out, err) = runner.run_and_wait(&argv(&["cmd", "/C", "echo hello"]), "", 20, None, &[], None);
        assert_eq!((code, out.trim(), err.as_str()), (0, "hello", ""));
        let (code, out, err) = runner.run_and_wait(&argv(&["definitely-not-a-program-xyz"]), "", 20, None, &[], None);
        assert_eq!((code, out.as_str()), (START_FAILURE_CODE, ""));
        assert!(err.starts_with("could not start definitely-not-a-program-xyz: "));
        let (code, _, err) = runner.run_and_wait(&argv(&["cmd", "/C", "ping -n 20 127.0.0.1 > nul"]), "", 1, None, &[], None);
        assert_eq!(code, TIMEOUT_CODE);
        assert!(err.ends_with(TIMEOUT_MESSAGE));
        let (_, out, _) = runner.run_and_wait(&argv(&["cmd", "/C", "echo %MAESTRO_T%"]), "", 20, None, &[("MAESTRO_T".into(), "42".into())], None);
        assert_eq!(out.trim(), "42");
    }

    #[test]
    fn detached_processes_write_to_the_log_and_can_be_killed() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("sub").join("p.log").display().to_string();
        let runner = SystemProcesses;
        let pid = runner.spawn_detached(&argv(&["cmd", "/C", "echo started && ping -n 30 127.0.0.1 > nul"]), "", &log, &[]).unwrap();
        assert!(pid > 0);
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert!(runner.is_alive(pid));
        assert!(std::fs::read_to_string(&log).unwrap().contains("started"));
        runner.kill_tree(pid);
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!runner.is_alive(pid));
        assert!(!runner.is_alive(0) && !runner.is_alive(-4));
    }

    #[test]
    fn env_is_built_only_when_asked() {
        assert!(build_env(None, &[]).is_none());
        let base = argv(&[]);
        assert_eq!(base.len(), 0);
        let built = build_env(Some(&[("A".into(), "1".into())]), &[("B".into(), "2".into()), ("a".into(), "3".into())]).unwrap();
        assert_eq!(built, vec![("A".to_string(), "3".to_string()), ("B".to_string(), "2".to_string())]);
    }
}
