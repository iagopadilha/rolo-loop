// SPDX-License-Identifier: MIT
//! Client of `claude -p` (headless Claude Code): builds the argv, runs it with stdin, reads the stream-json output, environment without secrets.
//!
//! Used by the read-only chat, the action router and the roadmap/mapping jobs. The loop itself stays in the engine (`run_loop`).

use crate::infrastructure::process_runner::{ProcessRunner, TIMEOUT_CODE, TIMEOUT_MESSAGE};
use cfg::Env;
use engine_state::claude_turn::DISABLED_PUSH_URL;
use engine_state::{claude_binaries, cli_output, runtime_isolation, secret_redaction, usage_windows};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

macro_rules! warn_line {
    ($($arg:tt)*) => { eprintln!("WARNING {}", format!($($arg)*)) };
}

pub const DEFAULT_BINARY_NAME: &str = "claude";
pub const EMPTY_MCP_CONFIG: &str = r#"{"mcpServers":{}}"#;
const ALWAYS_REMOVED_VARIABLES: [&str; 1] = ["LOOP_MONITOR_URL"];
pub const ERROR_MESSAGE_LIMIT: usize = 300;
/// Why `run_once` asks for the next command: the account ran out of usage...
pub const LIMIT_CAUSE: &str = "usage_limit";
/// ... or the account cannot be used at all (logged out, revoked token, no access).
pub const UNUSABLE_CAUSE: &str = "account_unusable";
const LIST_REMOTES_TIMEOUT_SECONDS: u64 = 30;

static SECRET_VARIABLE_NAME: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(token|secret|passw|credential|private_key|api_?key|auth)"));
static CLAUDE_VARIABLE_NAME: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(ANTHROPIC_|CLAUDE_)"));

thread_local! {
    static CALL_KIND: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Names what the `run_prompt` calls made while it lives are for ("chat", "roadmap", "map"...): the call log shows it.
pub struct CallKind {
    previous: String,
}

pub fn call_kind(kind: &str) -> CallKind {
    CallKind { previous: CALL_KIND.with(|k| k.replace(kind.to_string())) }
}

impl Drop for CallKind {
    fn drop(&mut self) {
        CALL_KIND.with(|k| *k.borrow_mut() = std::mem::take(&mut self.previous));
    }
}

/// Removes what looks like a credential before the text leaves through the API (a claude error may quote a command with a token).
pub fn redact_secrets(text: &str, limit: Option<usize>) -> String {
    secret_redaction::redact(text, limit, secret_redaction::REDACTION_MARK)
}

/// GIT_CONFIG_* that swap the pushurl of each remote for an invalid URL, adding to the existing GIT_CONFIG_COUNT.
pub fn build_push_disabled_variables(remote_names: &[String], base_env: &Env) -> Vec<(String, String)> {
    let mut count: u64 = base_env.get("GIT_CONFIG_COUNT").and_then(|c| c.trim().parse().ok()).unwrap_or(0);
    let mut variables = Vec::new();
    for name in remote_names {
        variables.push((format!("GIT_CONFIG_KEY_{count}"), format!("remote.{name}.pushurl")));
        variables.push((format!("GIT_CONFIG_VALUE_{count}"), DISABLED_PUSH_URL.to_string()));
        count += 1;
    }
    variables.push(("GIT_CONFIG_COUNT".into(), count.to_string()));
    variables
}

/// Options of [`ClaudeClient::build_command`].
#[derive(Clone, Debug)]
pub struct CommandOptions {
    pub model: String,
    pub budget_usd: Option<f64>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub no_mcp: bool,
    pub resume_session: String,
    pub fork_session: bool,
    pub prompt_argument: String,
    pub permission_mode: String,
    pub hook_profile: String,
    pub mcp_config_path: String,
}

impl Default for CommandOptions {
    fn default() -> Self {
        CommandOptions {
            model: String::new(),
            budget_usd: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            no_mcp: true,
            resume_session: String::new(),
            fork_session: false,
            prompt_argument: String::new(),
            permission_mode: String::new(),
            hook_profile: String::new(),
            mcp_config_path: String::new(),
        }
    }
}

/// Python `str(float)`-like text of a budget (`2.0`, `0.5`).
fn budget_text(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e16 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }
}

pub type CallRecorder = Arc<dyn Fn(&str, &str, &Value) + Send + Sync>;

#[derive(Clone)]
pub struct ClaudeClient {
    pub env: Env,
    pub process_runner: Arc<dyn ProcessRunner>,
    pub claude_binary: String,
    /// The guard binary (empty = the hook is not registered).
    pub guard_path: PathBuf,
    pub plugin_root: PathBuf,
    pub toolkit_root: PathBuf,
    pub fallback_binaries: Vec<String>,
    pub call_recorder: Option<CallRecorder>,
}

impl ClaudeClient {
    pub fn new(
        env: Env,
        process_runner: Arc<dyn ProcessRunner>,
        claude_binary: &str,
        guard_path: PathBuf,
        plugin_root: PathBuf,
        toolkit_root: PathBuf,
        fallback_binaries: Vec<String>,
    ) -> Self {
        ClaudeClient { env, process_runner, claude_binary: claude_binary.to_string(), guard_path, plugin_root, toolkit_root, fallback_binaries, call_recorder: None }
    }

    fn binary_name(&self) -> &str {
        if self.claude_binary.is_empty() {
            DEFAULT_BINARY_NAME
        } else {
            &self.claude_binary
        }
    }

    pub fn get_binary(&self) -> String {
        claude_binaries::executable(self.binary_name())
    }

    /// True if the configured binary (or `claude`) is found.
    pub fn is_available(&self) -> bool {
        !claude_binaries::resolve(self.binary_name()).is_empty()
    }

    /// The guard binary ('' when it does not exist).
    pub fn locate_guard(&self) -> String {
        if self.guard_path.is_file() {
            self.guard_path.display().to_string()
        } else {
            String::new()
        }
    }

    /// `--settings` JSON that registers the guard as a PreToolUse hook with `--profile <hook_profile>`; '' without a guard.
    pub fn build_hook_settings(&self, hook_profile: &str) -> String {
        let guard = self.locate_guard();
        if guard.is_empty() {
            warn_line!("guard not found: the `{hook_profile}` job runs without the PreToolUse hook (only the snapshot revert confines it)");
            return String::new();
        }
        pycompat::json::py_json_dumps(&json!({"hooks": runtime_isolation::hook_block(&runtime_isolation::guard_command(&guard, hook_profile))}), true)
    }

    pub fn plugin_root_text(&self) -> String {
        self.plugin_root.display().to_string()
    }

    /// Inline `--settings` of an isolated job: the guard with the job's profile (`auto` when the job has none) plus the user's `autoMode`.
    pub fn build_isolation_settings(&self, hook_profile: &str) -> String {
        let guard = self.locate_guard();
        let profile = if hook_profile.is_empty() { "auto" } else { hook_profile };
        if guard.is_empty() {
            warn_line!("guard not found: the `{profile}` job runs without the PreToolUse hook (only the snapshot revert confines it)");
        }
        runtime_isolation::guard_settings(&self.env, &guard, profile, None)
    }

    /// argv without a shell. Without `prompt_argument`, the prompt goes through stdin in `run_prompt`.
    ///
    /// Order: base, -p [prompt], --output-format stream-json --verbose, --model, --max-budget-usd, --permission-mode, --allowedTools...,
    /// --disallowedTools..., --settings <hook json>, --strict-mcp-config --mcp-config {empty}, --resume <session> [--fork-session].
    pub fn build_command(&self, options: &CommandOptions) -> Vec<String> {
        let mut argv = claude_binaries::base_argv(self.binary_name());
        argv.push("-p".into());
        if !options.prompt_argument.is_empty() {
            argv.push(options.prompt_argument.clone());
        }
        argv.extend(["--output-format", "stream-json", "--verbose"].map(String::from)); // the stream carries the rate_limit_event
        if !options.model.is_empty() {
            argv.extend(["--model".to_string(), options.model.clone()]);
        }
        if let Some(budget) = options.budget_usd.filter(|b| *b != 0.0) {
            argv.extend(["--max-budget-usd".to_string(), budget_text(budget)]);
        }
        if !options.permission_mode.is_empty() {
            argv.extend(["--permission-mode".to_string(), options.permission_mode.clone()]);
        }
        if !options.allowed_tools.is_empty() {
            argv.push("--allowedTools".into());
            argv.extend(options.allowed_tools.iter().cloned());
        }
        if !options.disallowed_tools.is_empty() {
            argv.push("--disallowedTools".into());
            argv.extend(options.disallowed_tools.iter().cloned());
        }
        if runtime_isolation::isolation_enabled(&runtime_isolation::read_config(&self.env)) {
            // the bundle itself is built in `run_prompt` (job thread); here only its fixed path is named, so a request never waits for a build
            let bundle = runtime_isolation::data_dir(&self.env).join(runtime_isolation::RUNTIME_DIR_NAME).join(runtime_isolation::BUNDLE_DIR_NAME);
            argv.extend(runtime_isolation::isolation_argv(&self.env, &bundle.display().to_string(), &self.build_isolation_settings(&options.hook_profile)));
        } else if !options.hook_profile.is_empty() {
            let settings = self.build_hook_settings(&options.hook_profile);
            if !settings.is_empty() {
                argv.extend(["--settings".to_string(), settings]);
            }
        }
        if options.no_mcp {
            argv.extend(["--strict-mcp-config".to_string(), "--mcp-config".to_string()]);
            argv.push(if options.mcp_config_path.is_empty() { EMPTY_MCP_CONFIG.to_string() } else { options.mcp_config_path.clone() });
        }
        if !options.resume_session.is_empty() {
            argv.extend(["--resume".to_string(), options.resume_session.clone()]);
            if options.fork_session {
                argv.push("--fork-session".into());
            }
        }
        argv
    }

    /// Environment of `claude -p`: without the app/panel token and without variables that look like secrets (only claude's own stay).
    pub fn build_env_without_secrets(&self) -> Vec<(String, String)> {
        std::env::vars()
            .filter(|(name, _)| CLAUDE_VARIABLE_NAME.is_match(name) || !(SECRET_VARIABLE_NAME.is_match(name) || ALWAYS_REMOVED_VARIABLES.contains(&name.as_str())))
            .collect()
    }

    /// Variables that make the push impossible for the `claude -p` process: an invalid `remote.<name>.pushurl` for EVERY remote of the folder.
    pub fn build_push_disabled_env(&self, folder: &str) -> Vec<(String, String)> {
        let argv: Vec<String> = ["git", "-C", folder, "remote"].iter().map(|s| s.to_string()).collect();
        let (code, output, _) = self.process_runner.run_and_wait(&argv, folder, LIST_REMOTES_TIMEOUT_SECONDS, None, &[], None);
        let names: Vec<String> = if code == 0 { output.split_whitespace().map(String::from).collect() } else { vec![] };
        build_push_disabled_variables(&names, &self.env)
    }

    /// Runs the argv (no shell, environment without secrets, stdin = `stdin_text`) and parses the stream-json output (its last `result` event).
    /// When the command hits an account usage limit, or its account cannot be used, the same argv runs again with each fallback binary in turn.
    ///
    /// Always returns an object (never fails): `ok`, `reply`, `cost_usd`, `session_id`, `error`, `raw`, `binary`, `account`.
    pub fn run_prompt(&self, argv: &[String], folder: &str, stdin_text: &str, timeout_seconds: u64, extra_env: &[(String, String)]) -> Value {
        let extra_env = self.isolation_variables(folder, extra_env);
        // the providers of Settings, read now (their order and secrets may have changed since the app started)
        let data_dir = self.env.user_data_dir();
        let fresh = cfg::locator::read_user_config(&data_dir);
        engine_state::providers::install(&fresh);
        let (primary, fallbacks) = engine_state::providers::chain(&fresh).unwrap_or_else(|| (self.claude_binary.clone(), self.fallback_binaries.clone()));
        let names = claude_binaries::ordered(&primary, &fallbacks);
        let mut result = Value::Object(Map::new());
        for (position, name) in names.iter().enumerate() {
            let current = self.binary_name().to_string(); // the command `argv` was built with
            let attempt = if *name == current { argv.to_vec() } else { claude_binaries::swap_binary(argv, &current, name) };
            let mut attempt_env = extra_env.clone();
            attempt_env.extend(engine_state::providers::environment_of(name, &data_dir));
            let (mut outcome, switch_cause) = self.run_once(&attempt, folder, stdin_text, timeout_seconds, &attempt_env);
            outcome["binary"] = json!(name);
            let account = outcome.as_object_mut().and_then(|o| o.remove("stderr_account")).unwrap_or(json!(""));
            outcome["account"] = account;
            result = outcome;
            if switch_cause.is_empty() || position == names.len() - 1 {
                break;
            }
            if switch_cause == UNUSABLE_CAUSE {
                warn_line!("`{name}` has an unusable account ({}): trying `{}`", result["error"].as_str().unwrap_or(""), names[position + 1]);
            } else {
                warn_line!("`{name}` hit the account usage limit: trying `{}`", names[position + 1]);
            }
        }
        if let Some(recorder) = &self.call_recorder {
            recorder(folder, &CALL_KIND.with(|k| k.borrow().clone()), &result);
        }
        result
    }

    /// `extra_env` plus LOOP_ISOLATED / LOOP_SKILLS_FILE when isolation is on; builds the bundle (signature check, usually a no-op) here, in the
    /// thread that runs the job.
    fn isolation_variables(&self, folder: &str, extra_env: &[(String, String)]) -> Vec<(String, String)> {
        let config = runtime_isolation::read_config(&self.env);
        if !runtime_isolation::isolation_enabled(&config) {
            return extra_env.to_vec();
        }
        let root = runtime_isolation::project_root_of(folder);
        let prepared = runtime_isolation::prepare(&self.env, Some(&root), &config, &self.plugin_root_text(), &self.toolkit_root.display().to_string());
        if let Some(error) = prepared["bundle"]["error"].as_str().filter(|e| !e.is_empty()) {
            warn_line!("{error}");
        }
        let mut variables = extra_env.to_vec();
        variables.extend(runtime_isolation::isolation_env(std::path::Path::new(prepared["allowlist_file"].as_str().unwrap_or(""))));
        variables
    }

    /// `(result of run_prompt, why the next command should run)`: [`LIMIT_CAUSE`] for an account usage limit, [`UNUSABLE_CAUSE`] for an account that
    /// cannot be used, '' when the call worked or failed for another reason.
    pub fn run_once(&self, argv: &[String], folder: &str, stdin_text: &str, timeout_seconds: u64, extra_env: &[(String, String)]) -> (Value, &'static str) {
        let (argv, stdin_text) = claude_binaries::prompt_through_stdin(argv, stdin_text);
        let env = self.build_env_without_secrets();
        let (code, output, error) = self.process_runner.run_and_wait(&argv, folder, timeout_seconds, Some(&stdin_text), extra_env, Some(&env));
        let raw = Self::parse_json_output(&output);
        let raw_map = raw.as_object().cloned().unwrap_or_default();
        let text = |key: &str| raw_map.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        let mut result = json!({"ok": false, "reply": "", "cost_usd": Self::extract_cost(&raw), "session_id": text("session_id"), "error": "", "raw": raw, "stderr_account": claude_binaries::account_from_stderr(&error)});
        if code == TIMEOUT_CODE {
            result["error"] = json!(TIMEOUT_MESSAGE);
            return (result, "");
        }
        let is_error = engine_state::notify_truthy(raw_map.get("is_error"));
        if code != 0 || raw_map.is_empty() || is_error {
            let source = if error.is_empty() { &output } else { &error };
            let last_line = source.trim().lines().rfind(|l| !l.trim().is_empty()).map(String::from);
            let reason = if is_error { Some(text("result")).filter(|r| !r.is_empty()) } else { None };
            let message = reason.clone().or(last_line).unwrap_or_else(|| "claude finished with an error".into());
            result["error"] = json!(redact_secrets(&message, Some(ERROR_MESSAGE_LIMIT)));
            let failure_text = [reason.unwrap_or_default(), cli_output::strip_rate_limit_events(&output), error.clone()].join("\n"); // that event's name says "rate_limit"
            let status = raw_map.get("api_error_status").and_then(Value::as_i64);
            let rate_limit = raw_map.get("rate_limit").cloned().unwrap_or(Value::Null);
            if claude_binaries::is_usage_limit(&failure_text) || usage_windows::is_limited(&rate_limit, None) || status == Some(429) {
                return (result, LIMIT_CAUSE);
            }
            let unusable = claude_binaries::is_account_unusable(&failure_text) || matches!(status, Some(401 | 403));
            return (result, if unusable { UNUSABLE_CAUSE } else { "" });
        }
        result["ok"] = json!(true);
        result["reply"] = json!(text("result"));
        (result, "")
    }

    /// Normalised result event of the output (one object, an array of events, stream-json, noise around it); `{}` when nothing in it is a claude result.
    pub fn parse_json_output(output: &str) -> Value {
        let data = cli_output::parse(output);
        if engine_state::notify_truthy(data.get("ok")) {
            data
        } else {
            json!({})
        }
    }

    pub fn extract_cost(raw: &Value) -> Value {
        for key in ["total_cost_usd", "cost_usd"] {
            if let Some(number) = raw.get(key).filter(|v| v.is_number()) {
                return number.clone();
            }
        }
        Value::Null
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Runner that answers with a fixed output and remembers what it was asked.
    struct Scripted {
        replies: Mutex<Vec<(i32, String, String)>>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl ProcessRunner for Scripted {
        fn run_and_wait(&self, argv: &[String], _: &str, _: u64, _: Option<&str>, _: &[(String, String)], _: Option<&[(String, String)]>) -> (i32, String, String) {
            self.calls.lock().unwrap().push(argv.to_vec());
            let mut replies = self.replies.lock().unwrap();
            if replies.len() > 1 {
                replies.remove(0)
            } else {
                replies[0].clone()
            }
        }
        fn spawn_detached(&self, _: &[String], _: &str, _: &str, _: &[(String, String)]) -> std::io::Result<i64> {
            Ok(1)
        }
        fn is_alive(&self, _: i64) -> bool {
            false
        }
        fn kill_tree(&self, _: i64) {}
    }

    fn client(replies: Vec<(i32, &str, &str)>) -> (ClaudeClient, Arc<Scripted>) {
        let runner =
            Arc::new(Scripted { replies: Mutex::new(replies.into_iter().map(|(c, o, e)| (c, o.to_string(), e.to_string())).collect()), calls: Mutex::new(vec![]) });
        let dir = tempfile::tempdir().unwrap().keep();
        let env = Env::from_pairs([("LOOP_DATA_DIR", dir.to_str().unwrap()), ("LOOP_CLAUDE_HOME", dir.to_str().unwrap())]);
        (ClaudeClient::new(env, runner.clone(), "claude", PathBuf::new(), PathBuf::new(), PathBuf::new(), vec![]), runner)
    }

    #[test]
    fn command_has_the_documented_order() {
        let (c, _) = client(vec![(0, "", "")]);
        let data = runtime_isolation::data_dir(&c.env);
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("config.json"), r#"{"loop_isolation": false}"#).unwrap();
        let argv = c.build_command(&CommandOptions {
            model: "m".into(),
            budget_usd: Some(2.0),
            allowed_tools: vec!["Read".into()],
            permission_mode: "dontAsk".into(),
            resume_session: "s1".into(),
            fork_session: true,
            ..Default::default()
        });
        let tail: Vec<&str> = argv.iter().skip_while(|a| *a != "-p").map(String::as_str).collect();
        assert_eq!(
            tail,
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--model",
                "m",
                "--max-budget-usd",
                "2.0",
                "--permission-mode",
                "dontAsk",
                "--allowedTools",
                "Read",
                "--strict-mcp-config",
                "--mcp-config",
                EMPTY_MCP_CONFIG,
                "--resume",
                "s1",
                "--fork-session"
            ]
        );
    }

    #[test]
    fn run_prompt_reads_result_and_classifies_failures() {
        let ok = r#"{"type":"result","subtype":"success","is_error":false,"result":"hi","session_id":"abc","total_cost_usd":0.5}"#;
        let (c, _) = client(vec![(0, ok, "")]);
        let r = c.run_prompt(&["claude".into(), "-p".into()], "", "q", 5, &[]);
        assert_eq!(
            (r["ok"].clone(), r["reply"].clone(), r["session_id"].clone(), r["cost_usd"].clone(), r["binary"].clone()),
            (json!(true), json!("hi"), json!("abc"), json!(0.5), json!("claude"))
        );
        let (c, _) = client(vec![(1, "", "boom token=abc123456789\nlast line")]);
        let r = c.run_prompt(&["claude".into(), "-p".into()], "", "q", 5, &[]);
        assert_eq!((r["ok"].clone(), r["error"].clone()), (json!(false), json!("last line")));
        let (c, _) = client(vec![(TIMEOUT_CODE, "", "")]);
        assert_eq!(c.run_prompt(&["claude".into()], "", "", 1, &[])["error"], TIMEOUT_MESSAGE);
    }

    #[test]
    fn env_without_secrets_and_push_disabled() {
        let (c, _) = client(vec![(0, "origin\nupstream\n", "")]);
        std::env::set_var("MAESTRO_TEST_API_KEY", "x");
        std::env::set_var("ANTHROPIC_API_KEY", "keep");
        std::env::set_var("LOOP_MONITOR_URL", "http://x");
        let env = c.build_env_without_secrets();
        assert!(!env.iter().any(|(n, _)| n == "MAESTRO_TEST_API_KEY" || n == "LOOP_MONITOR_URL"));
        assert!(env.iter().any(|(n, _)| n == "ANTHROPIC_API_KEY"));
        let vars = c.build_push_disabled_env("");
        assert_eq!(vars.last().unwrap(), &("GIT_CONFIG_COUNT".to_string(), "2".to_string()));
        assert!(vars.contains(&("GIT_CONFIG_KEY_1".to_string(), "remote.upstream.pushurl".to_string())));
    }
}
