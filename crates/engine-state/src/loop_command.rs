// SPDX-License-Identifier: MIT
//! Validation and rebuild of the saved relaunch command (`.specs/project/loop/json/LOOP-CMD.json`).
//!
//! Why: that file sits in a folder a headless Claude turn can write, and the user's next Answer / Resume / Instruction click relaunches the
//! loop from it. Running its `argv` as-is would let a prompt-injected agent run any program with the user's rights. So the file is never
//! executed: [`rebuild_loop_command`] takes ONLY known `harness loop` options (type checked) out of it and builds the command again as
//! `[this executable, "loop", the project repo, <known options>]`. Anything else is a [`LoopCommandInvalid`] and nothing starts.
//!
//! Dropped on purpose (never carried over from the file): `--answer`/`--option`/`--leftovers` (the app adds its own), `--claude` and
//! `--claude-fallback` (the app injects the binaries of its own config: a saved one would be an arbitrary executable), `--setup` (a shell
//! command; it only runs when a worktree is created, and a relaunch continues an existing one) and `--summary-dir` (an arbitrary folder).
//! [`ALLOWED_FLAGS`] mirrors the `loop_opts` parser (a test keeps the two in step).

use pycompat::{last_segment, nt_abspath, nt_normcase, nt_realpath, py_repr_str, Re};
use serde_json::{json, Value};
use std::sync::LazyLock;

pub const LOOP_COMMAND_INVALID_CODE: &str = "loop_command_invalid";
const MAX_WHOLE_NUMBER: i64 = 1_000_000;
const MAX_WAVE: i64 = 64;
const MAX_AMOUNT: f64 = 1_000_000.0;
const PERMISSION_MODES: [&str; 3] = ["auto", "acceptEdits", "dontAsk"];

static WHOLE_NUMBER: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\d{1,7}\z"));
static AMOUNT: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\d{1,7}(\.\d{1,6})?\z"));
static BRANCH_NAME: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z0-9][A-Za-z0-9._/-]{0,79}\z"));
static GIT_USER_NAME: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z0-9][A-Za-z0-9 ._@+-]{0,79}\z"));
/// Empty = no `--model` flag in the turns.
static MODEL_NAME: LazyLock<Re> = LazyLock::new(|| Re::new(r"^([A-Za-z0-9][A-Za-z0-9._:\[\]-]{0,79})?\z"));

/// The saved command is not something the app may relaunch (the message is shown to the user).
#[derive(Debug, Clone, PartialEq)]
pub struct LoopCommandInvalid(pub String);

impl std::fmt::Display for LoopCommandInvalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn invalid<T>(message: String) -> Result<T, LoopCommandInvalid> {
    Err(LoopCommandInvalid(message))
}

#[derive(Clone, Copy)]
enum Spec {
    Switch,
    Whole(i64, i64),
    Amount,
    Text(&'static LazyLock<Re>),
    Choice(&'static [&'static str]),
}

fn allowed(name: &str) -> Option<Spec> {
    Some(match name {
        "--e2e" | "--push" | "--no-push" | "--continue-on-blocked" | "--pause-on-blocked" | "--keep-going" | "--allow-silent" | "--no-auto-answer" | "--no-monitor" => Spec::Switch,
        "--wave" => Spec::Whole(1, MAX_WAVE),
        "--timeout" => Spec::Whole(1, MAX_WHOLE_NUMBER),
        "--max-stall" | "--limit-wait" | "--limit-retries" | "--gate-tiebreaks" | "--max-auto-answers" | "--auto-answers-per-feature" => Spec::Whole(0, MAX_WHOLE_NUMBER),
        "--budget" | "--max-cost" => Spec::Amount,
        "--branch" | "--base-branch" => Spec::Text(&BRANCH_NAME),
        "--git-user" => Spec::Text(&GIT_USER_NAME),
        "--orchestrator-model" => Spec::Text(&MODEL_NAME),
        "--permission-mode" => Spec::Choice(&PERMISSION_MODES),
        _ => return None,
    })
}

/// Every flag that [`allowed`] accepts, for the test that keeps it in step with the parser.
pub const ALLOWED_FLAGS: [&str; 24] = [
    "--e2e",
    "--push",
    "--no-push",
    "--continue-on-blocked",
    "--pause-on-blocked",
    "--keep-going",
    "--allow-silent",
    "--no-auto-answer",
    "--no-monitor",
    "--wave",
    "--timeout",
    "--max-stall",
    "--limit-wait",
    "--limit-retries",
    "--gate-tiebreaks",
    "--max-auto-answers",
    "--auto-answers-per-feature",
    "--budget",
    "--max-cost",
    "--branch",
    "--base-branch",
    "--git-user",
    "--orchestrator-model",
    "--permission-mode",
];
const DROPPED_FLAGS_WITH_VALUE: [&str; 8] = ["--answer", "--instruction", "--option", "--leftovers", "--claude", "--claude-fallback", "--setup", "--summary-dir"];

fn is_same_path(first: &str, second: &str) -> bool {
    nt_normcase(&nt_realpath(first)) == nt_normcase(&nt_realpath(second))
}

fn short(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// argv[0] is only a tamper signal (the command is rebuilt with `program`): this executable, or a program named `harness`.
fn check_program(token: &str, program: &str) -> Result<(), LoopCommandInvalid> {
    if token.is_empty() {
        return invalid("the saved command does not start with the harness program".into());
    }
    let base = last_segment(&token.replace('\\', "/")).to_lowercase();
    if is_same_path(token, program) || base == "harness" || base == "harness.exe" {
        return Ok(());
    }
    invalid(format!("the saved command starts with {}, not the harness program", py_repr_str(&short(token, 80))))
}

/// argv[1] must be the `loop` subcommand.
fn check_subcommand(token: &str) -> Result<(), LoopCommandInvalid> {
    if token == "loop" {
        return Ok(());
    }
    invalid(format!("the saved command runs {}, not loop", py_repr_str(&short(token, 80))))
}

/// The checked text of an option value.
fn convert_value(name: &str, spec: Spec, raw: &str) -> Result<String, LoopCommandInvalid> {
    let ok = match spec {
        Spec::Whole(lo, hi) => WHOLE_NUMBER.match_start(raw).is_some() && raw.parse::<i64>().is_ok_and(|n| lo <= n && n <= hi),
        Spec::Amount => AMOUNT.match_start(raw).is_some() && raw.parse::<f64>().is_ok_and(|n| n.is_finite() && n <= MAX_AMOUNT),
        Spec::Text(re) => re.match_start(raw).is_some() && !raw.contains(".."),
        Spec::Choice(choices) => choices.contains(&raw),
        Spec::Switch => false,
    };
    if ok {
        Ok(raw.to_string())
    } else {
        invalid(format!("the saved command has an invalid value for {name}: {}", py_repr_str(&short(raw, 80))))
    }
}

/// `(value, next index)` of an option: `--name=value` or `--name value`.
fn read_option_value(tokens: &[String], index: usize, inline: Option<&str>, name: &str) -> Result<(String, usize), LoopCommandInvalid> {
    if let Some(v) = inline {
        return Ok((v.to_string(), index + 1));
    }
    match tokens.get(index + 1) {
        Some(next) => Ok((next.clone(), index + 2)),
        None => invalid(format!("the saved command has {name} without a value")),
    }
}

/// `(positionals, kept argv options, dropped option names)`. An unknown option or a bad value is an error.
fn parse_saved_options(tokens: &[String]) -> Result<(Vec<String>, Vec<String>, Vec<String>), LoopCommandInvalid> {
    let (mut positionals, mut kept, mut dropped) = (Vec::new(), Vec::new(), Vec::new());
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        if !token.starts_with('-') || token == "-" {
            positionals.push(token.clone());
            index += 1;
            continue;
        }
        let (name, inline) = match token.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (token.as_str(), None),
        };
        if DROPPED_FLAGS_WITH_VALUE.contains(&name) {
            let (_, next) = read_option_value(tokens, index, inline, name)?;
            index = next;
            dropped.push(name.to_string());
            continue;
        }
        let Some(spec) = allowed(name) else {
            return invalid(format!("the saved command has an option the app does not relaunch with: {}", short(token, 80)));
        };
        if matches!(spec, Spec::Switch) {
            if inline.is_some() {
                return invalid(format!("the saved command gives a value to {name}"));
            }
            kept.push(name.to_string());
            index += 1;
            continue;
        }
        let (raw, next) = read_option_value(tokens, index, inline, name)?;
        index = next;
        kept.push(name.to_string());
        kept.push(convert_value(name, spec, &raw)?);
    }
    Ok((positionals, kept, dropped))
}

/// The folder the process starts in: the project repo when the file has none, else the SAME folder as the repo or one of its loop worktrees
/// (a subfolder is not accepted: the process looks programs up from its folder).
fn resolve_working_folder(saved_cwd: Option<&Value>, project_repo: &str, allowed_folders: &[String]) -> Result<String, LoopCommandInvalid> {
    match saved_cwd {
        None | Some(Value::Null) => Ok(project_repo.to_string()),
        Some(Value::String(s)) if s.is_empty() => Ok(project_repo.to_string()),
        Some(Value::String(s)) if allowed_folders.iter().any(|f| is_same_path(s, f)) => Ok(nt_abspath(s)),
        Some(other) => {
            let shown = other.as_str().map(String::from).unwrap_or_else(|| other.to_string());
            invalid(format!("the saved command starts in {}, which is not this project's repository or loop worktree", py_repr_str(&short(&shown, 120))))
        }
    }
}

/// The command to relaunch, built from the saved `{"argv", "cwd"}` and the app's own values. Returns
/// `{"argv": [program, "loop", project_repo, *known options], "cwd": folder, "dropped": [option names]}`. An error for a foreign program or
/// subcommand, an unknown option, a bad value, another repository or a folder outside the project.
pub fn rebuild_loop_command(saved: &Value, project_repo: &str, allowed_folders: &[String], program: &str) -> Result<Value, LoopCommandInvalid> {
    let argv: Option<Vec<String>> =
        saved.get("argv").and_then(Value::as_array).filter(|a| a.len() >= 2).and_then(|a| a.iter().map(|i| i.as_str().map(String::from)).collect());
    let Some(argv) = argv else { return invalid("the saved command is not a list of text arguments".into()) };
    check_program(&argv[0], program)?;
    check_subcommand(&argv[1])?;
    let (positionals, kept, dropped) = parse_saved_options(&argv[2..])?;
    if positionals.len() != 1 || !is_same_path(&positionals[0], project_repo) {
        return invalid("the saved command is for another repository than this project's".into());
    }
    let folder = resolve_working_folder(saved.get("cwd"), project_repo, allowed_folders)?;
    let mut rebuilt: Vec<String> = vec![program.to_string(), "loop".into(), project_repo.to_string()];
    rebuilt.extend(kept);
    Ok(json!({"argv": rebuilt, "cwd": folder, "dropped": dropped}))
}

/// H5. The arguments that `harness resume <repo>` gives to `harness loop` (everything after `loop`): the saved command checked and rebuilt exactly
/// as the panel's Resume does ([`rebuild_loop_command`]), then the instruction when there is one, then the configured `claude` command and its
/// backups (the saved file never carries them). The same settings as the panel, from the same file, with the same checks, so a watcher
/// outside the page can resume a stopped loop without any token.
pub fn resume_args(
    saved: &Value,
    project_repo: &str,
    allowed_folders: &[String],
    program: &str,
    instruction: &str,
    claude: &str,
    fallbacks: &[String],
) -> Result<Vec<String>, LoopCommandInvalid> {
    let rebuilt = rebuild_loop_command(saved, project_repo, allowed_folders, program)?;
    let mut args: Vec<String> = rebuilt["argv"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
    args.drain(..2.min(args.len())); // program and `loop`
    if !instruction.trim().is_empty() {
        args.extend(["--instruction".to_string(), instruction.trim().to_string()]);
    }
    if !claude.is_empty() {
        args.extend(["--claude".to_string(), claude.to_string()]);
    }
    for fallback in fallbacks {
        args.extend(["--claude-fallback".to_string(), fallback.clone()]);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_opts::{parse, Parsed};

    #[test]
    fn resume_uses_the_panels_checks_and_adds_the_instruction_and_the_commands() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().display().to_string();
        let saved = json!({"argv": ["harness", "loop", repo, "--wave", "2", "--answer", "old", "--claude", "evil.exe"], "cwd": null});
        let args = resume_args(&saved, &repo, &[repo.clone()], "harness.exe", " go on ", "claude", &["cc".to_string()]).unwrap();
        assert_eq!(args, vec![repo.as_str(), "--wave", "2", "--instruction", "go on", "--claude", "claude", "--claude-fallback", "cc"]);
        assert!(matches!(parse(&args), Parsed::Run(_)), "the loop parser accepts what resume builds");
        let bare = resume_args(&saved, &repo, &[], "harness.exe", "", "", &[]).unwrap();
        assert_eq!(bare, vec![repo.as_str(), "--wave", "2"], "the old answer and the saved claude never come back");
        let foreign = json!({"argv": ["powershell.exe", "loop", repo]});
        assert!(resume_args(&foreign, &repo, &[], "harness.exe", "", "", &[]).is_err());
    }

    fn saved(argv: &[&str], cwd: Value) -> Value {
        json!({"argv": argv, "cwd": cwd})
    }

    #[test]
    fn rebuilds_with_known_options_and_drops_the_dangerous_ones() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().display().to_string();
        let value = saved(
            &[
                "C:\\x\\harness.exe",
                "loop",
                &repo,
                "--wave",
                "2",
                "--e2e",
                "--answer",
                "do it",
                "--claude=evil.exe",
                "--setup",
                "calc",
                "--max-cost=12.5",
                "--permission-mode",
                "acceptEdits",
            ],
            Value::Null,
        );
        let rebuilt = rebuild_loop_command(&value, &repo, &[repo.clone()], "C:\\app\\harness.exe").unwrap();
        let argv: Vec<&str> = rebuilt["argv"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert_eq!(argv, vec!["C:\\app\\harness.exe", "loop", repo.as_str(), "--wave", "2", "--e2e", "--max-cost", "12.5", "--permission-mode", "acceptEdits"]);
        assert_eq!(rebuilt["dropped"], json!(["--answer", "--claude", "--setup"]));
        assert_eq!(rebuilt["cwd"], json!(repo));
    }

    #[test]
    fn refuses_anything_else() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().display().to_string();
        let other = tempfile::tempdir().unwrap();
        let run = |argv: &[&str], cwd: Value| rebuild_loop_command(&saved(argv, cwd), &repo, &[repo.clone()], "harness.exe").unwrap_err().0;
        assert_eq!(run(&["powershell.exe", "loop", &repo], Value::Null), "the saved command starts with 'powershell.exe', not the harness program");
        assert_eq!(run(&["harness", "state", &repo], Value::Null), "the saved command runs 'state', not loop");
        assert_eq!(run(&["harness", "loop", &repo, "--bogus"], Value::Null), "the saved command has an option the app does not relaunch with: --bogus");
        assert_eq!(run(&["harness", "loop", &repo, "--wave", "99"], Value::Null), "the saved command has an invalid value for --wave: '99'");
        assert_eq!(run(&["harness", "loop", &repo, "--branch", "a/../b"], Value::Null), "the saved command has an invalid value for --branch: 'a/../b'");
        assert_eq!(run(&["harness", "loop", &repo, "--permission-mode", "yolo"], Value::Null), "the saved command has an invalid value for --permission-mode: 'yolo'");
        assert_eq!(run(&["harness", "loop", &repo, "--e2e=1"], Value::Null), "the saved command gives a value to --e2e");
        assert_eq!(run(&["harness", "loop", &repo, "--wave"], Value::Null), "the saved command has --wave without a value");
        assert_eq!(run(&["harness", "loop", &other.path().display().to_string()], Value::Null), "the saved command is for another repository than this project's");
        assert!(run(&["harness", "loop", &repo], json!(other.path().display().to_string())).starts_with("the saved command starts in "));
        assert_eq!(run(&["harness"], Value::Null), "the saved command is not a list of text arguments");
        assert!(rebuild_loop_command(&json!({"argv": ["harness", "loop", repo.clone()]}), &repo, &[], "harness.exe").is_ok());
    }

    #[test]
    fn allowed_flags_are_all_accepted_by_the_loop_parser() {
        for flag in ALLOWED_FLAGS {
            let spec = allowed(flag).unwrap_or_else(|| panic!("{flag} not in allowed()"));
            let args: Vec<String> = match spec {
                Spec::Switch => vec!["r".into(), flag.into()],
                Spec::Whole(lo, _) => vec!["r".into(), flag.into(), lo.max(1).to_string()],
                Spec::Amount => vec!["r".into(), flag.into(), "1.5".into()],
                Spec::Text(_) => vec!["r".into(), flag.into(), "abc".into()],
                Spec::Choice(c) => vec!["r".into(), flag.into(), c[0].into()],
            };
            assert!(matches!(parse(&args), Parsed::Run(_)), "the loop parser rejects {flag}");
        }
    }
}
