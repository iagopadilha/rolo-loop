// SPDX-License-Identifier: MIT
//! Command line of the unsupervised loop (`harness loop <repo> ...`) and the options object the engine carries through a run.
//!
//! [`parse`] behaves like the argparse parser of the reference: same flags, defaults, help and error texts (exit code 2 with the usage
//! block and `run_loop.py: error: ...`). The run also keeps the account/limit state the engine updates between turns, so one struct
//! holds both what the user asked for and what the run learned.

use pycompat::py_repr_str;
use serde_json::Value;
use time::PrimitiveDateTime;

pub const DEFAULT_ORCHESTRATOR_MODEL: &str = "opus";
/// Program name in the usage and error lines (kept as the reference prints it).
pub const PROG: &str = "run_loop.py";
const HELP: &str = include_str!("loop_usage.txt");

/// What the user asked for plus the state of the run (accounts, limit waits).
#[derive(Clone, Debug)]
pub struct LoopOpts {
    pub repo: String,
    pub wave: i64,
    pub e2e: bool,
    /// `--push` = Some(true), `--no-push` = Some(false), neither = None.
    pub push_flag: Option<bool>,
    pub budget: f64,
    pub timeout: i64,
    pub max_stall: i64,
    pub limit_wait: i64,
    pub limit_retries: i64,
    pub permission_mode: String,
    pub branch: String,
    pub base_branch: String,
    pub git_user: String,
    pub setup: String,
    pub claude: String,
    pub claude_fallback: Vec<String>,
    pub option: i64,
    pub answer: String,
    pub max_cost: f64,
    pub orchestrator_model: String,
    pub rewind: Option<i64>,
    pub yes: bool,
    pub leftovers: String,
    pub summary_dir: String,
    pub json: bool,
    pub check: bool,
    pub planning: bool,
    pub dry_run: bool,
    pub ensure_worktrees: bool,
    pub repos: String,
    pub continue_on_blocked: bool,
    pub pause_on_blocked: bool,
    pub allow_silent: bool,
    pub auto_answer: bool,
    pub no_monitor: bool,
    pub gate_tiebreaks: i64,
    pub max_auto_answers: i64,
    pub auto_answers_per_feature: i64,
    // ---- derived by the run
    /// `--push` was given explicitly.
    pub push_explicit: bool,
    /// Push at the end (the default).
    pub push: bool,
    pub repo_names: Option<Vec<String>>,
    pub primary_name: String,
    pub reference_dirs: Vec<String>,
    pub extra_worktrees: Vec<String>,
    pub only_repos: Option<Vec<String>>,
    pub repo_path: String,
    // ---- accounts and limits, updated between turns
    pub claude_index: usize,
    pub unusable_commands: Vec<Value>,
    pub unusable_notice: Option<Value>,
    pub claude_auth: Vec<(String, Value)>,
    pub limit_resets: Vec<PrimitiveDateTime>,
    pub limit_reset_phrases: Vec<(PrimitiveDateTime, String)>,
    pub limit_phrases: Vec<(String, String)>,
    pub main_reset: Option<PrimitiveDateTime>,
    pub picker_tried: Vec<String>,
    pub last_claude_used: Option<(String, String)>,
    pub weekly_alerts: Vec<(String, Value)>,
}

impl LoopOpts {
    pub fn new(repo: &str) -> Self {
        LoopOpts {
            repo: repo.into(),
            wave: 3,
            e2e: false,
            push_flag: None,
            budget: 5.0,
            timeout: 60,
            max_stall: 10,
            limit_wait: 30,
            limit_retries: 8,
            permission_mode: "auto".into(),
            branch: String::new(),
            base_branch: String::new(),
            git_user: String::new(),
            setup: String::new(),
            claude: String::new(),
            claude_fallback: vec![],
            option: 0,
            answer: String::new(),
            max_cost: 50.0,
            orchestrator_model: DEFAULT_ORCHESTRATOR_MODEL.into(),
            rewind: None,
            yes: false,
            leftovers: String::new(),
            summary_dir: String::new(),
            json: false,
            check: false,
            planning: false,
            dry_run: false,
            ensure_worktrees: false,
            repos: String::new(),
            continue_on_blocked: false,
            pause_on_blocked: false,
            allow_silent: false,
            auto_answer: true,
            no_monitor: false,
            gate_tiebreaks: 1,
            max_auto_answers: 12,
            auto_answers_per_feature: 3,
            push_explicit: false,
            push: true,
            repo_names: None,
            primary_name: String::new(),
            reference_dirs: vec![],
            extra_worktrees: vec![],
            only_repos: None,
            repo_path: String::new(),
            claude_index: 0,
            unusable_commands: vec![],
            unusable_notice: None,
            claude_auth: vec![],
            limit_resets: vec![],
            limit_reset_phrases: vec![],
            limit_phrases: vec![],
            main_reset: None,
            picker_tried: vec![],
            last_claude_used: None,
            weekly_alerts: vec![],
        }
    }
}

/// Result of [`parse`]: the options, or the text to print and the exit code (`-h` = 0 on stdout, an error = 2 on stderr).
pub enum Parsed {
    Run(Box<LoopOpts>),
    Exit { code: i32, stdout: String, stderr: String },
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Switch,
    Int,
    Float,
    Text,
    Choice,
    Append,
}

const FLAGS: [(&str, Kind); 39] = [
    ("--help", Kind::Switch),
    ("--wave", Kind::Int),
    ("--e2e", Kind::Switch),
    ("--push", Kind::Switch),
    ("--no-push", Kind::Switch),
    ("--budget", Kind::Float),
    ("--timeout", Kind::Int),
    ("--max-stall", Kind::Int),
    ("--limit-wait", Kind::Int),
    ("--limit-retries", Kind::Int),
    ("--permission-mode", Kind::Text),
    ("--branch", Kind::Text),
    ("--base-branch", Kind::Text),
    ("--git-user", Kind::Text),
    ("--setup", Kind::Text),
    ("--claude", Kind::Text),
    ("--claude-fallback", Kind::Append),
    ("--option", Kind::Int),
    ("--answer", Kind::Text),
    ("--max-cost", Kind::Float),
    ("--orchestrator-model", Kind::Text),
    ("--rewind", Kind::Int),
    ("--yes", Kind::Switch),
    ("--leftovers", Kind::Choice),
    ("--summary-dir", Kind::Text),
    ("--json", Kind::Switch),
    ("--check", Kind::Switch),
    ("--planning", Kind::Switch),
    ("--dry-run", Kind::Switch),
    ("--ensure-worktrees", Kind::Switch),
    ("--repos", Kind::Text),
    ("--continue-on-blocked", Kind::Switch),
    ("--pause-on-blocked", Kind::Switch),
    ("--allow-silent", Kind::Switch),
    ("--no-auto-answer", Kind::Switch),
    ("--no-monitor", Kind::Switch),
    ("--gate-tiebreaks", Kind::Int),
    ("--max-auto-answers", Kind::Int),
    ("--auto-answers-per-feature", Kind::Int),
];

fn usage_block() -> &'static str {
    HELP.split("\n\n").next().unwrap_or(HELP)
}

fn fail(message: String) -> Parsed {
    Parsed::Exit { code: 2, stdout: String::new(), stderr: format!("{}\n{PROG}: error: {message}\n", usage_block()) }
}

fn looks_negative_number(text: &str) -> bool {
    let t = text.strip_prefix('-').unwrap_or(text);
    !t.is_empty() && t.parse::<f64>().is_ok() && text.starts_with('-')
}

/// Python `int(text)`: optional sign and digits, `_` between digits, surrounding spaces.
fn parse_int(text: &str) -> Option<i64> {
    let t = text.trim();
    if t.is_empty() || t.starts_with('_') || t.ends_with('_') || t.contains("__") {
        return None;
    }
    t.replace('_', "").parse::<i64>().ok()
}

/// Python `float(text)` for the plain forms an option takes.
fn parse_float(text: &str) -> Option<f64> {
    let t = text.trim();
    match t.to_lowercase().as_str() {
        "nan" | "+nan" | "-nan" => Some(f64::NAN),
        "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
        "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
        _ if t.is_empty() || t.contains("__") || t.starts_with('_') || t.ends_with('_') => None,
        _ => t.replace('_', "").parse::<f64>().ok().filter(|_| !t.to_lowercase().contains("inf")),
    }
}

/// argparse-like resolution of `--name` to a flag: exact, else a unique prefix.
fn resolve(name: &str) -> Result<&'static (&'static str, Kind), String> {
    if let Some(flag) = FLAGS.iter().find(|(n, _)| *n == name) {
        return Ok(flag);
    }
    let matches: Vec<&(&str, Kind)> = FLAGS.iter().filter(|(n, _)| n.starts_with(name)).collect();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(String::new()),
        _ => Err(format!("ambiguous option: {name} could match {}", matches.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", "))),
    }
}

/// Parses the arguments after `loop` (not including the program name).
pub fn parse(args: &[String]) -> Parsed {
    let mut o = LoopOpts::new("");
    let mut positionals: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    let mut only_positionals = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if only_positionals || !arg.starts_with('-') || arg == "-" || looks_negative_number(arg) {
            positionals.push(arg.clone());
            continue;
        }
        if arg == "--" {
            only_positionals = true;
            continue;
        }
        if arg == "-h" {
            return Parsed::Exit { code: 0, stdout: HELP.to_string(), stderr: String::new() };
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if arg.starts_with("--") => (n.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        if !name.starts_with("--") {
            unknown.push(arg.clone());
            continue;
        }
        let flag = match resolve(&name) {
            Ok(f) => f,
            Err(message) if message.is_empty() => {
                unknown.push(arg.clone());
                continue;
            }
            Err(message) => return fail(message),
        };
        let (flag_name, kind) = (flag.0, flag.1);
        if kind == Kind::Switch {
            if inline.is_some() {
                return fail(format!("argument {flag_name}: ignored explicit argument {}", py_repr_str(&inline.unwrap_or_default())));
            }
            match flag_name {
                "--help" => return Parsed::Exit { code: 0, stdout: HELP.to_string(), stderr: String::new() },
                "--e2e" => o.e2e = true,
                "--push" => o.push_flag = Some(true),
                "--no-push" => o.push_flag = Some(false),
                "--yes" => o.yes = true,
                "--json" => o.json = true,
                "--check" => o.check = true,
                "--planning" => o.planning = true,
                "--dry-run" => o.dry_run = true,
                "--ensure-worktrees" => o.ensure_worktrees = true,
                "--continue-on-blocked" => o.continue_on_blocked = true,
                "--pause-on-blocked" => o.pause_on_blocked = true,
                "--allow-silent" => o.allow_silent = true,
                "--no-auto-answer" => o.auto_answer = false,
                "--no-monitor" => o.no_monitor = true,
                _ => {}
            }
            continue;
        }
        let value = match inline {
            Some(v) => v,
            None => match args.get(i) {
                Some(next) if !next.starts_with('-') || next == "-" || looks_negative_number(next) => {
                    i += 1;
                    next.clone()
                }
                _ => return fail(format!("argument {flag_name}: expected one argument")),
            },
        };
        let int = |v: &str| parse_int(v).ok_or_else(|| format!("argument {flag_name}: invalid int value: {}", py_repr_str(v)));
        let float = |v: &str| parse_float(v).ok_or_else(|| format!("argument {flag_name}: invalid float value: {}", py_repr_str(v)));
        let result: Result<(), String> = (|| {
            match flag_name {
                "--wave" => o.wave = int(&value)?,
                "--budget" => o.budget = float(&value)?,
                "--timeout" => o.timeout = int(&value)?,
                "--max-stall" => o.max_stall = int(&value)?,
                "--limit-wait" => o.limit_wait = int(&value)?,
                "--limit-retries" => o.limit_retries = int(&value)?,
                "--permission-mode" => o.permission_mode = value.clone(),
                "--branch" => o.branch = value.clone(),
                "--base-branch" => o.base_branch = value.clone(),
                "--git-user" => o.git_user = value.clone(),
                "--setup" => o.setup = value.clone(),
                "--claude" => o.claude = value.clone(),
                "--claude-fallback" => o.claude_fallback.push(value.clone()),
                "--option" => o.option = int(&value)?,
                "--answer" => o.answer = value.clone(),
                "--max-cost" => o.max_cost = float(&value)?,
                "--orchestrator-model" => o.orchestrator_model = value.clone(),
                "--rewind" => o.rewind = Some(int(&value)?),
                "--leftovers" => {
                    if value != "commit" && value != "discard" {
                        return Err(format!("argument --leftovers: invalid choice: {} (choose from commit, discard)", py_repr_str(&value)));
                    }
                    o.leftovers = value.clone();
                }
                "--summary-dir" => o.summary_dir = value.clone(),
                "--repos" => o.repos = value.clone(),
                "--gate-tiebreaks" => o.gate_tiebreaks = int(&value)?,
                "--max-auto-answers" => o.max_auto_answers = int(&value)?,
                "--auto-answers-per-feature" => o.auto_answers_per_feature = int(&value)?,
                _ => {}
            }
            Ok(())
        })();
        if let Err(message) = result {
            return fail(message);
        }
    }
    if positionals.is_empty() {
        return fail("the following arguments are required: repo".into());
    }
    let mut extra: Vec<String> = positionals.split_off(1);
    extra.extend(unknown);
    if !extra.is_empty() {
        return fail(format!("unrecognized arguments: {}", extra.join(" ")));
    }
    o.repo = positionals.remove(0);
    Parsed::Run(Box::new(o))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Parsed {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn opts(args: &[&str]) -> LoopOpts {
        match run(args) {
            Parsed::Run(o) => *o,
            Parsed::Exit { stderr, .. } => panic!("exit: {stderr}"),
        }
    }

    fn error(args: &[&str]) -> String {
        match run(args) {
            Parsed::Exit { code: 2, stderr, .. } => stderr.lines().last().unwrap_or("").to_string(),
            _ => panic!("expected an error"),
        }
    }

    #[test]
    fn defaults_and_flags() {
        let o = opts(&["C:\\repo"]);
        assert_eq!((o.wave, o.budget, o.timeout, o.max_stall, o.limit_wait, o.limit_retries, o.max_cost), (3, 5.0, 60, 10, 30, 8, 50.0));
        assert_eq!((o.permission_mode.as_str(), o.orchestrator_model.as_str(), o.auto_answer, o.push_flag, o.rewind), ("auto", "opus", true, None, None));
        let o = opts(&["C:\\repo", "--wave", "5", "--no-push", "--claude-fallback", "cc", "--claude-fallback=cc2", "--max-cost", "0", "--rewind", "-1", "--no-auto-answer", "--leftovers", "commit"]);
        assert_eq!((o.wave, o.push_flag, o.claude_fallback.clone(), o.max_cost, o.rewind, o.auto_answer, o.leftovers.as_str()), (5, Some(false), vec!["cc".to_string(), "cc2".to_string()], 0.0, Some(-1), false, "commit"));
        let o = opts(&["--max-sta", "3", "C:\\repo", "--e2e"]);
        assert_eq!((o.max_stall, o.e2e, o.repo.as_str()), (3, true, "C:\\repo"));
    }

    #[test]
    fn errors_read_like_argparse() {
        assert_eq!(error(&["x", "--wave", "abc"]), "run_loop.py: error: argument --wave: invalid int value: 'abc'");
        assert_eq!(error(&["x", "--bogus"]), "run_loop.py: error: unrecognized arguments: --bogus");
        assert_eq!(error(&["x", "--leftovers", "zzz"]), "run_loop.py: error: argument --leftovers: invalid choice: 'zzz' (choose from commit, discard)");
        assert_eq!(error(&[]), "run_loop.py: error: the following arguments are required: repo");
        assert_eq!(error(&["x", "--wave"]), "run_loop.py: error: argument --wave: expected one argument");
        assert_eq!(error(&["x", "--answer", "--x"]), "run_loop.py: error: argument --answer: expected one argument");
        assert_eq!(error(&["x", "y"]), "run_loop.py: error: unrecognized arguments: y");
        assert!(error(&["x", "--no"]).contains("ambiguous option: --no could match"));
        match run(&["-h"]) {
            Parsed::Exit { code: 0, stdout, .. } => assert!(stdout.starts_with("usage: run_loop.py [-h] [--wave WAVE]")),
            _ => panic!("help"),
        }
    }
}
