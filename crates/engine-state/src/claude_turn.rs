// SPDX-License-Identifier: MIT
//! The `claude -p` call of one turn: argv (model, MCP, isolation, disallowed tools), environment with the push lock, run and parse.

use crate::app_ctx::AppCtx;
use crate::claude_accounts::current_claude;
use crate::loop_io::{git, run_process, ProcessResult};
use crate::loop_opts::{LoopOpts, DEFAULT_ORCHESTRATOR_MODEL};
use crate::{claude_binaries, cli_output, mcp_access, runtime_isolation, turn_environment};
use serde_json::Value;
use std::path::Path;

pub const DISABLED_PUSH_URL: &str = "push-disabled-by-loop://use-delivery";

/// Command line of `claude -p`: what the loop never needs is refused by the CLI itself, not only by the guard hook. No allow-list on
/// purpose: under `--permission-mode auto` it would only pre-approve, and a narrow one would stop a normal turn.
pub const LOOP_DISALLOWED_TOOLS: [&str; 6] = ["WebFetch", "WebSearch", "NotebookEdit", "Bash(ssh *)", "Bash(scp *)", "Bash(sftp *)"];

/// No MCP server in the headless turn (any MCP denial already stops the loop as risky).
pub const EMPTY_MCP_CONFIG: &str = "{\"mcpServers\":{}}";

/// `(mcp config, allowed tools, denied tools)` of the turn, computed by [`mcp_access::build_access`]: every known server the user's machine
/// defines AND the user allowed in Settings > MCP servers (config.json `mcp_servers`; none by default). No server = the empty strict config.
pub fn mcp_read_arguments(ctx: &AppCtx, project_root: &str) -> (String, Vec<String>, Vec<String>) {
    let data_dir = ctx.data_dir();
    let access = mcp_access::build_access(&ctx.env, Some(&data_dir), None, project_root);
    let config_path = access["config_path"].as_str().unwrap_or("");
    if config_path.is_empty() {
        return (EMPTY_MCP_CONFIG.to_string(), vec![], vec![]);
    }
    let list = |key: &str| -> Vec<String> { access[key].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default() };
    (config_path.to_string(), list("allowed_tools"), list("denied_tools"))
}

/// `--model <value>` of every orchestrator turn (normal, correction and resume); an empty option sends no flag.
pub fn model_arguments(opts: &LoopOpts) -> Vec<String> {
    let model = opts.orchestrator_model.trim();
    if model.is_empty() {
        vec![]
    } else {
        vec!["--model".into(), model.to_string()]
    }
}

/// Isolation flags of every turn (normal, correction, resume): `--setting-sources project,local`, `--add-dir=<bundle>` and the guard hook
/// (`--profile auto`) through inline `--settings`. Empty when `loop_isolation` is false in config.json: the argv is then the plain one.
pub fn isolation_arguments(ctx: &AppCtx, opts: &LoopOpts) -> Vec<String> {
    let config = runtime_isolation::read_config(&ctx.env);
    if !runtime_isolation::isolation_enabled(&config) {
        return vec![];
    }
    let prepared = runtime_isolation::prepare(&ctx.env, Some(&opts.repo), &config, &ctx.plugin_root_text(), &ctx.toolkit_text());
    let bundle = prepared["bundle"]["path"].as_str().unwrap_or("").to_string();
    let settings = runtime_isolation::guard_settings(&ctx.env, &ctx.guard_text(), "auto", None);
    runtime_isolation::isolation_argv(&ctx.env, &bundle, &settings)
}

/// True when the turn just run carried the user's caveman hooks (`loop_caveman` on, isolation on, a hook installed): recorded in the turn log.
pub fn is_caveman_on(ctx: &AppCtx) -> bool {
    !runtime_isolation::caveman_hooks_to_carry(&ctx.env, &runtime_isolation::read_config(&ctx.env), None).is_empty()
}

/// `LOOP_ISOLATED` and `LOOP_SKILLS_FILE` of the turn environment (empty when isolation is off).
pub fn isolation_environment(ctx: &AppCtx, cwd: &str) -> Vec<(String, String)> {
    let config = runtime_isolation::read_config(&ctx.env);
    if !runtime_isolation::isolation_enabled(&config) {
        return vec![];
    }
    let allowlist = runtime_isolation::skills_allowlist(Some(cwd), &config);
    runtime_isolation::isolation_env(&runtime_isolation::write_allowlist_file(&ctx.env, &ctx.data_dir(), &allowlist))
}

/// The full argv of one turn.
pub fn claude_command(ctx: &AppCtx, opts: &LoopOpts, prompt: &str) -> Vec<String> {
    let mut argv = claude_binaries::base_argv(&current_claude(opts));
    // each non-primary worktree is granted as `--add-dir=<path>` (the `=` form: the flag is variadic and would swallow what follows); each
    // `reference` repo (read only, no worktree) is granted the same way, as its own folder
    let extra_dirs: Vec<String> = opts.extra_worktrees.iter().chain(opts.reference_dirs.iter()).map(|p| format!("--add-dir={p}")).collect();
    let (mcp_config, allowed_tools, denied_tools) = mcp_read_arguments(ctx, &opts.repo);
    for item in [
        "-p",
        prompt,
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        &opts.permission_mode,
        "--permission-prompts",
        "none",
        "--max-budget-usd",
        &py_float(opts.budget),
    ] {
        argv.push(item.to_string());
    }
    argv.extend(model_arguments(opts));
    argv.extend(extra_dirs);
    argv.extend(isolation_arguments(ctx, opts));
    argv.extend(["--strict-mcp-config".to_string(), "--mcp-config".to_string(), mcp_config]);
    if !allowed_tools.is_empty() {
        argv.push("--allowedTools".into());
        argv.extend(allowed_tools);
    }
    argv.push("--disallowedTools".into());
    let mut denied: Vec<String> = LOOP_DISALLOWED_TOOLS.iter().map(|s| s.to_string()).collect();
    denied.extend(denied_tools);
    argv.push(denied.join(",")); // last and as ONE argument: the option is variadic
    argv
}

/// `str(float)`: `5.0`, `2.5`, `10.0`.
pub fn py_float(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e16 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

/// Prompt of the single correction turn (same session, --resume): only write LOOP.json, never redo or start work.
pub fn loop_json_correction_prompt(turn: i64, branch: &str) -> String {
    format!(
        "/orchestrator continue --headless --turn {turn} --branch {branch} --finish-only. Your previous reply ended turn {turn} WITHOUT writing LOOP.json (e.g. you stopped to wait for background subagents). Do not start new work and do not wait for background tasks: if a subagent is still unfinished, record that in the reason and use status continue. Write STATE.md if needed and, as the last action, run: harness state loop <root> --status <continue|awaiting_user|done|error> --turn {turn} --reason \"...\" --next \"...\"."
    )
}

/// The process environment minus the secret-looking variables that came from the app `.env` (see `turn_environment`). The first turn of a
/// run prints their NAMES (never a value).
pub fn turn_environment_for(ctx: &AppCtx, reported: &mut bool) -> Vec<(String, String)> {
    let built = turn_environment::build(&ctx.env.vars(), &ctx.env.env_file_candidates());
    if !*reported {
        *reported = true;
        if !built.removed.is_empty() {
            println!("[loop] turn environment: {} secret-looking variable(s) from the app .env left out ({})", built.removed.len(), built.removed.join(", "));
        }
    }
    built.env
}

fn set_env(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    match env.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
        Some(slot) => slot.1 = value.to_string(),
        None => env.push((key.to_string(), value.to_string())),
    }
}

fn set_default(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    if !env.iter().any(|(k, _)| k.eq_ignore_ascii_case(key)) {
        env.push((key.to_string(), value.to_string()));
    }
}

/// Environment of `claude -p`: push is impossible even through a guard gap (`python -c`, subshell...). Disables the pushurl of EVERY remote
/// for that process only (GIT_CONFIG_*, nothing is written to the repo); the end delivery is done by this program, outside that environment.
pub fn push_lock_env(ctx: &AppCtx, cwd: &str, reported: &mut bool) -> Vec<(String, String)> {
    let mut env = turn_environment_for(ctx, reported);
    set_env(&mut env, "LOOP_HEADLESS", "1");
    for (k, v) in isolation_environment(ctx, cwd) {
        set_env(&mut env, &k, &v);
    }
    // wait for the background subagents (the verifier) instead of killing them 10 min after the last turn; the turn timeout still bounds it
    set_default(&mut env, "CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS", "0");
    set_default(&mut env, "LOOP_APP_HOME", &ctx.repo_root.display().to_string());
    let specs_dir = Path::new(&pycompat::nt_abspath(cwd)).join(".specs"); // cwd = the PRIMARY worktree (run_turn is called with `work`)
    if specs_dir.is_dir() {
        set_env(&mut env, "LOOP_SPECS_DIR", &specs_dir.display().to_string()); // the guard finds SCOPE.json even when its cwd is a second worktree
    }
    let remotes = git(cwd, &["remote"]).1;
    let mut count: i64 = env.iter().find(|(k, _)| k == "GIT_CONFIG_COUNT").and_then(|(_, v)| v.trim().parse().ok()).unwrap_or(0);
    for name in remotes.split_whitespace() {
        set_env(&mut env, &format!("GIT_CONFIG_KEY_{count}"), &format!("remote.{name}.pushurl"));
        set_env(&mut env, &format!("GIT_CONFIG_VALUE_{count}"), DISABLED_PUSH_URL);
        count += 1;
    }
    set_env(&mut env, "GIT_CONFIG_COUNT", &count.to_string());
    env
}

/// Runs one turn: `(exit code, stdout, stderr, timed out)`.
pub fn run_turn(ctx: &AppCtx, command: &[String], cwd: &str, timeout_min: i64, reported: &mut bool) -> (i32, String, String, bool) {
    // a .cmd wrapper (`cc`) would cut the prompt at a line break
    let (command, stdin_text) = claude_binaries::prompt_through_stdin(command, "");
    let env = push_lock_env(ctx, cwd, reported);
    let ProcessResult { code, stdout, stderr, timed_out } = run_process(&command, cwd, &env, &stdin_text, (timeout_min.max(0) as u64) * 60);
    (code, stdout, stderr, timed_out)
}

/// Normalised result of a turn (see [`cli_output::parse`]).
pub fn parse_result(text: &str) -> Value {
    cli_output::parse(text)
}

pub fn unrecognized_output(data: &Value) -> bool {
    data.get("ok") == Some(&Value::Bool(false)) && data.get("reason").and_then(Value::as_str) == Some(cli_output::UNRECOGNIZED)
}

/// The default orchestrator model.
pub fn default_model() -> &'static str {
    DEFAULT_ORCHESTRATOR_MODEL
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfg::Env;

    fn ctx(dir: &Path) -> AppCtx {
        let env = Env::from_pairs([
            ("LOOP_DATA_DIR", dir.join("data").display().to_string()),
            ("LOOP_CLAUDE_HOME", dir.join("home").display().to_string()),
            ("LOOP_CLAUDE_CONFIG_FILE", dir.join("none.json").display().to_string()),
            ("LOOP_REPO_ROOT", dir.display().to_string()),
            ("USERPROFILE", dir.display().to_string()),
            ("PATH", std::env::var("PATH").unwrap_or_default()),
        ]);
        AppCtx::discover_in(&env, dir.to_path_buf())
    }

    #[test]
    fn argv_is_isolated_and_closed() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        let mut opts = LoopOpts::new(&dir.path().display().to_string());
        opts.claude = "claude".into();
        opts.extra_worktrees = vec!["C:\\w2".into()];
        opts.reference_dirs = vec!["C:\\ref".into()];
        let argv = claude_command(&ctx, &opts, "/orchestrator continue --headless --turn 1");
        let pos = |flag: &str| argv.iter().position(|a| a == flag).unwrap_or_else(|| panic!("missing {flag}"));
        assert_eq!(argv[pos("-p") + 1], "/orchestrator continue --headless --turn 1");
        assert_eq!((argv[pos("--permission-mode") + 1].as_str(), argv[pos("--max-budget-usd") + 1].as_str(), argv[pos("--model") + 1].as_str()), ("auto", "5.0", "opus"));
        assert!(argv.contains(&"--add-dir=C:\\w2".to_string()) && argv.contains(&"--add-dir=C:\\ref".to_string()));
        assert_eq!(argv[pos("--mcp-config") + 1], EMPTY_MCP_CONFIG);
        assert!(argv.contains(&"--strict-mcp-config".to_string()) && !argv.contains(&"--allowedTools".to_string()));
        assert_eq!(argv.last().unwrap(), "WebFetch,WebSearch,NotebookEdit,Bash(ssh *),Bash(scp *),Bash(sftp *)");
        assert!(argv.contains(&"--setting-sources".to_string()));
        opts.orchestrator_model = "  ".into();
        assert!(!claude_command(&ctx, &opts, "x").contains(&"--model".to_string()));
        assert_eq!(py_float(2.5), "2.5");
        assert_eq!(py_float(10.0), "10.0");
        assert!(loop_json_correction_prompt(3, "loop/x").starts_with("/orchestrator continue --headless --turn 3 --branch loop/x --finish-only."));
    }

    #[test]
    fn env_blocks_push_and_marks_headless() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".specs")).unwrap();
        for args in [vec!["init", "-q"], vec!["remote", "add", "origin", "https://example.com/x.git"], vec!["remote", "add", "up", "https://example.com/y.git"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(&args).output().unwrap().status.success());
        }
        let ctx = ctx(dir.path());
        let mut reported = false;
        let env = push_lock_env(&ctx, &repo.display().to_string(), &mut reported);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("LOOP_HEADLESS").as_deref(), Some("1"));
        assert_eq!(get("GIT_CONFIG_COUNT").as_deref(), Some("2"));
        assert_eq!(get("GIT_CONFIG_KEY_0").as_deref(), Some("remote.origin.pushurl"));
        assert_eq!(get("GIT_CONFIG_VALUE_1").as_deref(), Some(DISABLED_PUSH_URL));
        assert!(get("LOOP_SPECS_DIR").unwrap().ends_with(".specs") && get("LOOP_ISOLATED").as_deref() == Some("1"));
        assert_eq!(get("CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS").as_deref(), Some("0"));
        assert!(reported);
    }

    #[test]
    fn runs_a_command_and_parses_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        let cwd = dir.path().display().to_string();
        std::fs::write(dir.path().join("out.json"), "{\"type\":\"result\",\"result\":\"hi\"}\n").unwrap();
        let cmd: Vec<String> = ["cmd", "/C", "type out.json"].map(String::from).to_vec();
        let mut reported = false;
        let (code, out, _err, timed_out) = run_turn(&ctx, &cmd, &cwd, 1, &mut reported);
        assert_eq!((code, timed_out), (0, false));
        let data = parse_result(&out);
        assert_eq!(data["result"], "hi");
        assert!(unrecognized_output(&parse_result("")) && !unrecognized_output(&data));
    }
}
