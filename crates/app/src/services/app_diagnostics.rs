// SPDX-License-Identifier: MIT
//! App diagnostics (`GET /api/app/info` and `harness doctor`) and the test notification (`POST /api/app/notify/test`).
//!
//! Never returns an environment variable value nor `.env` content: only channel names and whether they are configured.

use super::App;
use crate::infrastructure::claude_client::redact_secrets;
use crate::repositories::loop_read_formatting::text_or_empty;
use crate::repositories::small_stores::read_calls;
use cfg::locator::{Locator, TOOLKIT_MIN_VERSION};
use engine_state::{cli_output, mcp_access, notify, runtime_isolation, standards};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::LazyLock;

const GUARD_GIT_PUSH_INPUT: &str = r#"{"tool_name": "Bash", "tool_input": {"command": "git push"}}"#;
const GUARD_BLOCKED_CODE: i32 = 2;
const GUARD_TIMEOUT_SECONDS: u64 = 30;
const CLAUDE_VERSION_TIMEOUT_SECONDS: u64 = 30;
const VERSION_OUTPUT_LIMIT: usize = 120;
const TEST_NOTIFICATION_TIMEOUT_SECONDS: u64 = 120;
const DEFAULT_ENABLED_CHANNELS: [&str; 3] = ["console", "toast", "file"];
const CHANNEL_DETAIL_LIMIT: usize = 200;
const RECENT_TURNS_FOR_USAGE: usize = 5;
const TOOLKIT_INSTALL_LINES: [&str; 2] = ["/plugin marketplace add iagopadilha/claude-craft-kit", "/plugin install claude-craft-kit@claude-craft-kit"];
/// Skills the toolkit must carry (folder name with a SKILL.md; the content is not inspected): a missing required one fails `doctor`, an optional one
/// warns.
const REQUIRED_TOOLKIT_SKILLS: [&str; 6] = ["backend-dev", "frontend-dev", "tlc-spec-driven", "flow-validator", "playwright-cli", "github"];
const OPTIONAL_TOOLKIT_SKILLS: [&str; 2] = ["ui-component-tests", "ui-trace-analysis"];

static NOTIFY_RESULT_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(\S+)\s+((?:ok|FAILED|SKIPPED).*)\z"));

pub struct AppDiagnosticsService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn diagnostics(&self) -> AppDiagnosticsService<'_> {
        AppDiagnosticsService { app: self }
    }
}

fn name_list(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

impl AppDiagnosticsService<'_> {
    /// `(found, blocks)` running the real guard with `git push` in the executor profile (expected: code 2).
    pub fn check_guard_blocks_git_push(&self) -> (bool, bool) {
        let guard = self.app.config.guard_path();
        if guard.as_os_str().is_empty() || !guard.is_file() {
            return (false, false);
        }
        let argv = vec![guard.display().to_string(), "--profile".into(), "executor".into()];
        let folder = guard.parent().map(|p| p.display().to_string()).unwrap_or_default();
        let (code, _, _) = self.app.processes.run_and_wait(&argv, &folder, GUARD_TIMEOUT_SECONDS, Some(GUARD_GIT_PUSH_INPUT), &[], None);
        (true, code == GUARD_BLOCKED_CODE)
    }

    /// `claude --version` -> `describe_version(...)` plus the redacted first output line. Never fails the diagnostics: an old or unreadable version is
    /// only a warning.
    pub fn read_claude_version(&self) -> Value {
        let mut output = String::new();
        if self.app.claude.is_available() {
            let argv = vec![self.app.claude.get_binary(), "--version".into()];
            let (code, stdout, stderr) = self.app.processes.run_and_wait(&argv, &self.app.config.app_root_dir.display().to_string(), CLAUDE_VERSION_TIMEOUT_SECONDS, None, &[], None);
            let combined = format!("{stdout}\n{stderr}");
            let first = pycompat::splitlines(&combined).into_iter().map(str::trim).find(|l| !l.is_empty());
            if let (Some(line), 0) = (first, code) {
                output = line.to_string();
            }
        }
        let mut info = cli_output::describe_version(&output);
        info["output"] = json!(redact_secrets(&output, Some(VERSION_OUTPUT_LIMIT)));
        info
    }

    /// `[{"name", "enabled", "required", "configured"}]` - only names and booleans. LOOP_NOTIFY_REQUIRED unset = none required.
    pub fn list_notify_channels(&self) -> Vec<Value> {
        let mut env = self.app.env.clone();
        cfg::env::load_all(&mut env);
        let value = |name: &str| name_list(&env.get(name).unwrap_or_default());
        let mut enabled = value("LOOP_NOTIFY_CHANNELS");
        if enabled.is_empty() {
            enabled = DEFAULT_ENABLED_CHANNELS.iter().map(|c| c.to_string()).collect();
        }
        let required = value("LOOP_NOTIFY_REQUIRED");
        notify::CHANNELS.iter().map(|name| json!({"name": name, "enabled": enabled.iter().any(|e| e == name), "required": required.iter().any(|r| r == name), "configured": notify::missing_config(&env, name).is_empty()})).collect()
    }

    /// Main and fallback `claude` commands: found or not, auth method and account (`auth status`, never a secret).
    pub fn describe_claude_commands(&self) -> Vec<Value> {
        self.app.accounts.get_rows(true) // cached a few minutes, shared with the loop panel
    }

    /// Each command row gets `usage`: the usage-view rows of that command (one per cc account), empty when none.
    fn attach_usage(command_rows: Vec<Value>, usage_view: &[Value]) -> Vec<Value> {
        command_rows
            .into_iter()
            .map(|row| {
                let name = super::claude_accounts::command_name(&text_or_empty(row.get("name")));
                let usage: Vec<&Value> = usage_view.iter().filter(|item| item["command"] == name.as_str()).collect();
                let mut out = row.as_object().cloned().unwrap_or_default();
                out.insert("usage".into(), json!(usage));
                Value::Object(out)
            })
            .collect()
    }

    /// 5-hour and weekly usage per command/account, from `meta.json`, the newest turns and the call log of the given project folders (the loop's work
    /// folder and the repo). No query of its own; without a project the configured commands show "usage unknown".
    pub fn build_claude_usage(&self, project_folders: &[String]) -> Vec<Value> {
        let files = &self.app.loop_files;
        let (mut meta, mut turns, mut calls): (Map<String, Value>, Vec<Value>, Vec<Value>) = (Map::new(), vec![], vec![]);
        for folder in project_folders {
            if meta.is_empty() {
                meta = files.read_meta(folder);
            }
            let numbers = files.list_turn_numbers(folder);
            for number in &numbers[numbers.len().saturating_sub(RECENT_TURNS_FOR_USAGE)..] {
                turns.push(Value::Object(files.read_turn(folder, *number)));
            }
            calls.extend(read_calls(folder, 1_000_000));
        }
        calls.sort_by(|a, b| text_or_empty(b.get("at")).cmp(&text_or_empty(a.get("at"))));
        self.app.accounts.build_usage_view(&Value::Object(meta), &turns, &calls, true)
    }

    /// Plugin root of the toolkit (empty when not found): `dev_toolkit_dir` of config.json, the plugin cache, the sibling clone.
    pub fn find_toolkit(&self) -> String {
        let config = cfg::locator::read_user_config(&self.app.config.user_data_dir);
        Locator::new(self.app.env.clone(), self.app.config.ctx.repo_root.clone()).find_toolkit_root(&config).display().to_string()
    }

    /// `[{"skill", "required", "present"}]` for the toolkit's required and optional skills, by folder name with a SKILL.md (empty without a toolkit).
    pub fn check_toolkit_skills(&self, toolkit_root: &str) -> Vec<Value> {
        if toolkit_root.is_empty() {
            return vec![];
        }
        let wanted = REQUIRED_TOOLKIT_SKILLS.iter().map(|n| (*n, true)).chain(OPTIONAL_TOOLKIT_SKILLS.iter().map(|n| (*n, false)));
        wanted.map(|(name, required)| json!({"skill": name, "required": required, "present": Path::new(toolkit_root).join("skills").join(name).join("SKILL.md").is_file()})).collect()
    }

    /// `[{"folder", "slot", "skill", "resolved"}]` for every Standards binding of the given projects (root and subprojects), resolved the way the
    /// runtime bundle does (user skills, toolkit, Maestro plugin, installed plugins).
    pub fn check_standards_bindings(&self, project_folders: &[String], toolkit_root: &str) -> Vec<Value> {
        let plugin_root = self.app.config.ctx.plugin_root_text();
        let others = runtime_isolation::installed_plugin_skill_dirs(&self.app.env);
        let user_dir = runtime_isolation::user_skills_dir(&self.app.env);
        let mut rows = Vec::new();
        for folder in project_folders {
            for (relative, bindings) in standards::project_bindings(folder) {
                for (slot, value) in bindings {
                    let name = standards::skill_name(&value);
                    let (found, _) = runtime_isolation::resolve_skill_sources(&plugin_root, &user_dir, std::slice::from_ref(&name), toolkit_root, &others);
                    let shown = if relative != "." { pycompat::nt_join(folder, &relative) } else { folder.clone() };
                    rows.push(json!({"folder": shown, "slot": slot, "skill": value, "resolved": found.contains_key(&name)}));
                }
            }
        }
        rows
    }

    /// Dictionary of `/api/app/info` (without `seconds_until_shutdown`, which belongs to the server process). `project_folders` = the folders whose
    /// loop files tell the usage of each command (none = "usage unknown").
    pub fn build_diagnostics(&self, project_folders: &[String]) -> Value {
        let config = &self.app.config;
        let (guard_found, guard_blocks) = self.check_guard_blocks_git_push();
        let usage_view = self.build_claude_usage(project_folders);
        let channels = self.list_notify_channels();
        let mut problems: Vec<String> = Vec::new();
        if config.ctx.skill_dir.as_os_str().is_empty() {
            problems.push("orchestrator skill not found (LOOP_SKILL_DIR, config.json orchestrator_skill_dir or the plugin next to the app)".into());
        }
        if !guard_found {
            problems.push("the plugin's bin/guard.exe was not found".into());
        } else if !guard_blocks {
            problems.push("guard.exe did not block `git push` (expected code 2): do not run the loop unsupervised".into());
        }
        let toolkit_root = self.find_toolkit();
        if toolkit_root.is_empty() {
            problems.push(format!("claude-craft-kit not found (the dev skills and the craft binary). In Claude Code run: {}", TOOLKIT_INSTALL_LINES.join(" ; ")));
        }
        let (toolkit_ok, toolkit_version) = Locator::toolkit_version_status(Path::new(&toolkit_root));
        if !toolkit_ok {
            problems.push(format!(
                "toolkit {} found, {TOOLKIT_MIN_VERSION} needed: /plugin update claude-craft-kit@claude-craft-kit",
                if toolkit_version.is_empty() { "(version unreadable, no bin/craft.exe)" } else { &toolkit_version }
            ));
        }
        let missing_files = Locator::toolkit_missing_files(Path::new(&toolkit_root));
        if !missing_files.is_empty() {
            problems.push(format!("toolkit {toolkit_root} lacks {} (the loop calls it): /plugin update claude-craft-kit@claude-craft-kit", missing_files.join(", ")));
        }
        let mut warnings: Vec<String> = Vec::new();
        let standards_rows = if project_folders.is_empty() { vec![] } else { self.check_standards_bindings(project_folders, &toolkit_root) };
        warnings.extend(standards_rows.iter().filter(|row| row["resolved"] != true).map(|row| {
            format!(
                "standards: slot `{}` of {} is bound to `{}`, which is not installed (looked in ~/.claude/skills, the toolkit, the Maestro plugin and the installed plugins)",
                row["slot"].as_str().unwrap_or(""),
                row["folder"].as_str().unwrap_or(""),
                row["skill"].as_str().unwrap_or("")
            )
        }));
        let claude_on_path = self.app.claude.is_available();
        let version = self.read_claude_version();
        if !claude_on_path {
            problems.push("`claude` command not found on the PATH".into());
        } else if engine_state::notify_truthy(version.get("warning")) {
            warnings.push(text_or_empty(version.get("warning")));
        }
        problems.extend(channels.iter().filter(|c| c["required"] == true && c["configured"] != true).map(|c| format!("required notify channel {} is not configured", c["name"].as_str().unwrap_or(""))));
        let mcp = self.describe_mcp_policy();
        warnings.extend(mcp["warnings"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from));
        let mut env = self.app.env.clone();
        let env_loaded = !cfg::env::load_all(&mut env).is_empty();
        json!({
            "mcp": mcp,
            "app_root_dir": config.app_root_dir.display().to_string(),
            "data_dir": config.user_data_dir.display().to_string(),
            "skill_dir": config.ctx.skill_dir.display().to_string(),
            "toolkit": {"path": toolkit_root, "found": !toolkit_root.is_empty()},
            "toolkit_version": {"found": toolkit_version, "min": TOOLKIT_MIN_VERSION, "ok": toolkit_ok},
            "toolkit_skills": self.check_toolkit_skills(&toolkit_root),
            "standards": standards_rows,
            "guard": {"path": config.guard_path().display().to_string(), "found": guard_found, "blocks_git_push": guard_blocks},
            "claude": {
                "on_path": claude_on_path, "binary_configured": !config.claude_binary.is_empty(), "version": version["version"], "version_output": version["output"],
                "min_tested_version": version["min_tested_version"], "older_than_tested": version["older_than_tested"],
            },
            "claude_commands": Self::attach_usage(self.describe_claude_commands(), &usage_view),
            "claude_usage": usage_view,
            "notify_channels": channels,
            "env_loaded": env_loaded,
            "problems": problems,
            "warnings": warnings,
        })
    }

    /// The effective MCP policy of config.json (allowed servers, kinds, production flags) and what is wrong with it. Never a credential.
    pub fn describe_mcp_policy(&self) -> Value {
        mcp_access::describe_policy(&self.app.env, None, "")
    }

    /// `notify --test [--channels a,b]`. Returns `{"ok", "results": {channel: short redacted text}, "error"}`.
    pub fn send_test_notification(&self, channels: &[Value]) -> Value {
        let channels: Vec<&str> = channels.iter().filter_map(Value::as_str).collect();
        let unknown: Vec<&str> = channels.iter().copied().filter(|c| !notify::CHANNELS.contains(c)).collect();
        if !unknown.is_empty() {
            return json!({"ok": false, "results": {}, "error": format!("unknown channel: {}", unknown.join(", "))});
        }
        let mut argv = self.app.engine_argv("notify");
        argv.push("--test".into());
        if !channels.is_empty() {
            argv.extend(["--channels".to_string(), channels.join(",")]);
        }
        let (code, output, error) = self.app.processes.run_and_wait(&argv, &self.app.config.app_root_dir.display().to_string(), TEST_NOTIFICATION_TIMEOUT_SECONDS, None, &[], None);
        let mut results = Map::new();
        for line in pycompat::splitlines(&output) {
            if let Some(found) = NOTIFY_RESULT_LINE.match_start(line.trim()) {
                if notify::CHANNELS.contains(&&found[1]) {
                    results.insert(found[1].to_string(), json!(redact_secrets(&found[2], Some(CHANNEL_DETAIL_LIMIT))));
                }
            }
        }
        let message = if code == 0 {
            String::new()
        } else {
            let last = error.trim().lines().last().filter(|_| !error.trim().is_empty()).unwrap_or("some channel failed");
            redact_secrets(last, Some(CHANNEL_DETAIL_LIMIT))
        };
        json!({"ok": code == 0, "results": results, "error": message})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_install_reports_what_is_missing_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let report = app.diagnostics().build_diagnostics(&[]);
        assert_eq!(report["toolkit"]["found"], false);
        assert_eq!(report["guard"]["found"], false);
        let problems: Vec<&str> = report["problems"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
        assert!(problems.iter().any(|p| p.starts_with("orchestrator skill not found")) && problems.iter().any(|p| p.starts_with("claude-craft-kit not found")), "{problems:?}");
        assert_eq!(report["notify_channels"].as_array().unwrap().len(), 5);
        assert!(report["notify_channels"].as_array().unwrap().iter().all(|c| c["required"] == false));
        assert_eq!(report["notify_channels"][0], json!({"name": "console", "enabled": true, "required": false, "configured": true}));
    }

    #[test]
    fn test_notification_refuses_unknown_channels() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        assert_eq!(app.diagnostics().send_test_notification(&[json!("pigeon"), json!(7)]), json!({"ok": false, "results": {}, "error": "unknown channel: pigeon"}));
    }

    #[test]
    fn toolkit_skills_are_checked_by_folder() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let root = dir.path().join("kit");
        std::fs::create_dir_all(root.join("skills/backend-dev")).unwrap();
        std::fs::write(root.join("skills/backend-dev/SKILL.md"), "x").unwrap();
        let rows = app.diagnostics().check_toolkit_skills(&root.display().to_string());
        assert_eq!((rows.len(), rows[0].clone()), (8, json!({"skill": "backend-dev", "required": true, "present": true})));
        assert_eq!(rows[7], json!({"skill": "ui-trace-analysis", "required": false, "present": false}));
        assert!(app.diagnostics().check_toolkit_skills("").is_empty());
    }
}
