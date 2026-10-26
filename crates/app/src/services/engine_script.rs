// SPDX-License-Identifier: MIT
//! Runs an engine command (`loop`, `deliver`, `summary`...) of the app's own binary against a folder, without a shell, waiting for the end with a
//! timeout. The Python app ran `python <script> <dir> <args>`; here the engine is a subcommand of the same executable: `harness <name> <dir> <args>`.
//!
//! It is always the USER acting through the panel: `LOOP_HEADLESS` never reaches the command environment.

use super::App;
use crate::infrastructure::process_runner::TIMEOUT_CODE;
use std::collections::BTreeMap;

pub const HEADLESS_MODE_VARIABLE: &str = "LOOP_HEADLESS";

/// Name of the engine script in the old layout -> the subcommand of the binary.
const SUBCOMMANDS: [(&str, &str); 6] = [("run_loop", "loop"), ("delivery", "deliver"), ("loop_summary", "summary"), ("loop_state", "state"), ("close_loop", "close-loop"), ("notify", "notify")];

#[derive(Clone, Debug, PartialEq)]
pub struct EngineScriptResult {
    /// `None` when the command timed out.
    pub code: Option<i32>,
    pub output: String,
    pub error: String,
    pub timed_out: bool,
}

/// Command lines that replace an engine command (tests point them at a fake).
pub type EngineOverrides = BTreeMap<String, Vec<String>>;

impl App {
    /// The command that runs engine command `name` (the old script name: `run_loop`, `delivery`, `loop_summary`...): `[harness, <subcommand>]`, or the
    /// override of a test. Resolved on every call, so test overrides apply without rebuilding the app.
    pub fn engine_argv(&self, name: &str) -> Vec<String> {
        if let Some(command) = self.engine_overrides.lock().unwrap_or_else(|e| e.into_inner()).get(name) {
            return command.clone();
        }
        let subcommand = SUBCOMMANDS.iter().find(|(old, _)| *old == name).map(|(_, new)| *new).unwrap_or(name);
        vec![self.config.ctx.app_exe.display().to_string(), subcommand.to_string()]
    }

    /// The program the saved `LOOP-CMD.json` must start with (the app's own executable, or the override's first item).
    pub fn engine_program(&self) -> String {
        self.engine_argv("run_loop").first().cloned().unwrap_or_default()
    }

    pub fn set_engine_override(&self, name: &str, command: Vec<String>) {
        self.engine_overrides.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string(), command);
    }

    pub fn engine_scripts(&self) -> EngineScriptService<'_> {
        EngineScriptService { app: self }
    }
}

pub struct EngineScriptService<'a> {
    pub app: &'a App,
}

impl EngineScriptService<'_> {
    pub fn build_user_env(&self, extra_env: &[(String, String)]) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = std::env::vars().filter(|(name, _)| name != HEADLESS_MODE_VARIABLE).collect();
        for (key, value) in extra_env {
            match env.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
                Some(slot) => slot.1 = value.clone(),
                None => env.push((key.clone(), value.clone())),
            }
        }
        env
    }

    /// `harness <command> <target_dir> <args...>` with cwd = target_dir.
    pub fn run_engine_script_and_wait(&self, script_name: &str, target_dir: &str, args: &[String], timeout_seconds: u64, extra_env: &[(String, String)]) -> EngineScriptResult {
        let mut argv = self.app.engine_argv(script_name);
        argv.push(target_dir.to_string());
        argv.extend(args.iter().cloned());
        let env = self.build_user_env(extra_env);
        let (code, output, error) = self.app.processes.run_and_wait(&argv, target_dir, timeout_seconds, None, &[], Some(&env));
        let timed_out = code == TIMEOUT_CODE;
        EngineScriptResult { code: if timed_out { None } else { Some(code) }, output, error, timed_out }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_map_to_subcommands_and_can_be_overridden() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let exe = app.config.ctx.app_exe.display().to_string();
        assert_eq!(app.engine_argv("run_loop"), vec![exe.clone(), "loop".to_string()]);
        assert_eq!(app.engine_argv("delivery"), vec![exe.clone(), "deliver".to_string()]);
        assert_eq!(app.engine_argv("other"), vec![exe.clone(), "other".to_string()]);
        app.set_engine_override("run_loop", vec!["fake.exe".into(), "x".into()]);
        assert_eq!((app.engine_argv("run_loop"), app.engine_program()), (vec!["fake.exe".to_string(), "x".to_string()], "fake.exe".to_string()));
        let env = app.engine_scripts().build_user_env(&[("A_B".into(), "1".into())]);
        assert!(env.iter().any(|(k, v)| k == "A_B" && v == "1") && !env.iter().any(|(k, _)| k == HEADLESS_MODE_VARIABLE));
    }
}
