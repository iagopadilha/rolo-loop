// SPDX-License-Identifier: MIT
//! `harness doctor [--json] [--project <repo>] [--data-dir D] [--toolkit-optional]`: diagnostics (the same as `GET /api/app/info`); FAILs when the
//! claude-craft-kit toolkit is not found; with `--project`, WARNs for each Standards slot whose skill is not installed. `--access` is the access
//! report (what a `claude -p` of the app can reach).

use crate::serve_cmd::apply_data_dir;
use app::config::Overrides;
use app::infrastructure::process_runner::SystemProcesses;
use app::services::claude_accounts::format_usage_line;
use app::services::App;
use cfg::Env;
use clap::Args as ClapArgs;
use engine_state::monitor_proc;
use serde_json::{json, Value};
use std::sync::Arc;

const TOOLKIT_INSTALL_LINES: [&str; 2] = ["/plugin marketplace add iagopadilha/claude-craft-kit", "/plugin install claude-craft-kit@claude-craft-kit"];
/// not found / "toolkit X found, Y needed" / "toolkit <path> lacks <file>".
const TOOLKIT_PROBLEM_PREFIXES: [&str; 2] = ["claude-craft-kit not found", "toolkit "];

#[derive(ClapArgs)]
pub struct Args {
    /// JSON output
    #[arg(long)]
    json: bool,
    #[arg(long, default_value = "")]
    data_dir: String,
    /// repo folder whose loop files tell the 5-hour/weekly usage (default: usage unknown)
    #[arg(long, default_value = "")]
    project: String,
    /// access report (MCP, skills, bundle, probe of the isolated claude, guard.log, network list, run-time layer self test)
    #[arg(long)]
    access: bool,
    /// with --access: do not start claude
    #[arg(long)]
    no_probe: bool,
    /// with --access: do not run the run-time egress layer self test
    #[arg(long)]
    no_selftest: bool,
    /// a missing or too old claude-craft-kit is a warning, not a problem (the installer uses it)
    #[arg(long)]
    toolkit_optional: bool,
}

/// `(problems, warnings)` for the toolkit skills that are missing: one line per skill, required = problem (fails the doctor), optional = warning.
fn toolkit_skill_messages(rows: &[Value]) -> (Vec<String>, Vec<String>) {
    let line = |kind: &str, row: &Value| format!("claude-craft-kit lacks the {kind} skill `{0}` (skills/{0}/SKILL.md not found)", row["skill"].as_str().unwrap_or(""));
    let missing = |row: &&Value| row["present"] != true;
    let problems = rows.iter().filter(|r| r["required"] == true).filter(missing).map(|r| line("required", r)).collect();
    let warnings = rows.iter().filter(|r| r["required"] != true).filter(missing).map(|r| line("optional", r)).collect();
    (problems, warnings)
}

fn build_diagnostics(env: &Env, project_folders: &[String]) -> Value {
    let app = App::new(env, &Overrides::default(), Arc::new(SystemProcesses));
    let mut diagnostics = app.diagnostics().build_diagnostics(project_folders);
    let (problems, warnings) = toolkit_skill_messages(diagnostics["toolkit_skills"].as_array().map(Vec::as_slice).unwrap_or(&[]));
    let extend = |key: &str, extra: Vec<String>, diagnostics: &mut Value| {
        let mut list = diagnostics[key].as_array().cloned().unwrap_or_default();
        list.extend(extra.into_iter().map(Value::from));
        diagnostics[key] = Value::Array(list);
    };
    extend("problems", problems, &mut diagnostics);
    extend("warnings", warnings, &mut diagnostics);
    diagnostics["live_app"] = match monitor_proc::read_live_app(env) {
        Some(live) => json!({"url": live["url"], "pid": live.get("pid")}),
        None => Value::Null,
    };
    diagnostics
}

fn print_diagnostics(diagnostics: &Value) {
    let mark = |value: bool| if value { "ok " } else { "NO " };
    let text = |value: &Value| value.as_str().unwrap_or("").to_string();
    let guard = &diagnostics["guard"];
    println!("app folder ....... {}", text(&diagnostics["app_root_dir"]));
    println!("data folder ...... {}", text(&diagnostics["data_dir"]));
    let skill = text(&diagnostics["skill_dir"]);
    println!("skill ............ {}", if skill.is_empty() { "(not found)" } else { &skill });
    let toolkit = &diagnostics["toolkit"];
    let toolkit_path = text(&toolkit["path"]);
    println!("toolkit .......... {}", if toolkit_path.is_empty() { "(not found)" } else { &toolkit_path });
    for row in diagnostics["toolkit_skills"].as_array().into_iter().flatten() {
        let note = if row["present"] == true { "" } else if row["required"] == true { " (required)" } else { " (optional)" };
        println!("{}", format!("[{}] toolkit skill {:<28}{note}", mark(row["present"] == true), text(&row["skill"])).trim_end());
    }
    let guard_path = text(&guard["path"]);
    println!("[{}] guard found ({})", mark(guard["found"] == true), if guard_path.is_empty() { "-" } else { &guard_path });
    println!("[{}] guard blocks `git push`", mark(guard["blocks_git_push"] == true));
    let claude = &diagnostics["claude"];
    println!("[{}] `claude` available", mark(claude["on_path"] == true));
    let version = text(&claude["version"]);
    println!("claude version ... {} (JSON contract tested with {})", if version.is_empty() { "unknown" } else { &version }, claude["min_tested_version"].as_str().unwrap_or("?"));
    for command in diagnostics["claude_commands"].as_array().into_iter().flatten() {
        let state = if command["found"] == true {
            let label = text(&command["label"]);
            if !label.is_empty() { label } else { Some(text(&command["auth"]["error"])).filter(|e| !e.is_empty()).unwrap_or_else(|| "account unknown".into()) }
        } else {
            "not found (skipped, never blocks)".to_string()
        };
        println!("[{}] {:<8} `{}`: {state}", mark(command["found"] == true), text(&command["role"]), text(&command["name"]));
    }
    for usage in diagnostics["claude_usage"].as_array().into_iter().flatten() {
        let age = usage["age_seconds"].as_i64().map(|s| format!(" (read {} min ago)", s / 60)).unwrap_or_default();
        println!("      usage {:<14} {}{age}", text(&usage["key"]), format_usage_line(usage));
    }
    println!("[{}] .env file found", mark(diagnostics["env_loaded"] == true));
    for channel in diagnostics["notify_channels"].as_array().into_iter().flatten() {
        let py = |v: &Value| if *v == true { "True" } else { "False" };
        println!("[{}] channel {:<9} enabled={} required={}", mark(channel["configured"] == true), text(&channel["name"]), py(&channel["enabled"]), py(&channel["required"]));
    }
    let live = &diagnostics["live_app"];
    println!("live app ......... {}", if live.is_null() { "none".to_string() } else { format!("{} (pid {})", text(&live["url"]), live["pid"].as_i64().map_or("None".to_string(), |p| p.to_string())) });
    for row in diagnostics["standards"].as_array().into_iter().flatten() {
        println!("[{}] standards {:<13} {} ({})", mark(row["resolved"] == true), text(&row["slot"]), text(&row["skill"]), text(&row["folder"]));
    }
    let mcp = &diagnostics["mcp"];
    let allowed: Vec<String> = mcp["servers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["allow"] == true)
        .map(|row| format!("{} ({}{}{})", text(&row["server"]), if row["kind"] == "sql" { "database" } else { "other" }, if row["production"] == true { ", production" } else { "" }, if row["read_only"] == true { ", read-only" } else { "" }))
        .collect();
    println!("mcp for loops .... {}", if allowed.is_empty() { "none allowed (a loop cannot use any MCP server)".to_string() } else { allowed.join(", ") });
    for pattern in mcp["extra"].as_array().into_iter().flatten() {
        println!("      extra tool pattern {}", text(pattern));
    }
    for warning in diagnostics["warnings"].as_array().into_iter().flatten() {
        println!("WARNING: {}", text(warning));
    }
    let problems = diagnostics["problems"].as_array().cloned().unwrap_or_default();
    for problem in &problems {
        println!("PROBLEM: {}", text(problem));
    }
    if toolkit["found"] != true {
        println!("  install it in Claude Code:");
        for line in TOOLKIT_INSTALL_LINES {
            println!("    {line}");
        }
    }
    println!("{}", if problems.is_empty() { "all good".to_string() } else { format!("{} problem(s)", problems.len()) });
}

pub fn run(args: Args) -> i32 {
    let mut env = Env::process();
    apply_data_dir(&mut env, &args.data_dir);
    if args.access {
        return crate::access_cmd::run(&env, &args.project, !args.no_probe, !args.no_selftest, args.json);
    }
    let folders: Vec<String> = if args.project.is_empty() { vec![] } else { vec![pycompat::nt_abspath(&args.project)] };
    let mut diagnostics = build_diagnostics(&env, &folders);
    if args.toolkit_optional {
        let all: Vec<Value> = diagnostics["problems"].as_array().cloned().unwrap_or_default();
        let (missing, rest): (Vec<Value>, Vec<Value>) = all.into_iter().partition(|p| p.as_str().is_some_and(|t| TOOLKIT_PROBLEM_PREFIXES.iter().any(|prefix| t.starts_with(prefix))));
        let mut warnings = diagnostics["warnings"].as_array().cloned().unwrap_or_default();
        warnings.extend(missing);
        diagnostics["problems"] = Value::Array(rest);
        diagnostics["warnings"] = Value::Array(warnings);
    }
    if args.json {
        println!("{}", pycompat::json::py_json_dumps_indent(&diagnostics, false, 2));
    } else {
        print_diagnostics(&diagnostics);
    }
    if diagnostics["problems"].as_array().is_some_and(|p| !p.is_empty()) { 1 } else { 0 }
}
