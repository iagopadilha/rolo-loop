// SPDX-License-Identifier: MIT
//! Demo stand-ins, run by the harness executable itself (no scripts):
//! - the `claude` CLI (the app's `claude_binary` in demo mode, a copy of the executable named `demo-claude.exe`): a fixed `--output-format json` reply
//!   saying nothing ran;
//! - the engine commands the panel can trigger (`run_loop`, `delivery`, `loop_summary`), as the hidden subcommand `demo-engine`.
//!
//! Neither calls the network, git or the real CLI.

use serde_json::json;
use std::io::Read;

pub const CLAUDE_STUB_STEM: &str = "demo-claude";
const DEMO_REPLY: &str =
    "Demo mode: no Claude call is made here. The data in this panel is mocked by `harness demo`; open a real repository with `harness serve` to talk to the loop.";
const NOTICE: &str = "demo mode: this loop is simulated by `harness demo`; nothing runs, pushes or spends here. Open a real repository to run a loop.";
const ANSWER_FILE_PARTS: [&str; 5] = [".specs", "project", "loop", "internal", "demo-answer.txt"];
const EXIT_OK: i32 = 0;
const EXIT_REFUSED: i32 = 1;

/// True when this process was started as the Claude stub (by the name of its executable).
pub fn is_claude_stub(exe_stem: &str) -> bool {
    exe_stem.eq_ignore_ascii_case(CLAUDE_STUB_STEM)
}

pub fn claude_stub(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--version") {
        println!("0.0.0 (agent-maestro demo stub)");
        return EXIT_OK;
    }
    let mut ignored = String::new();
    let _ = std::io::stdin().read_to_string(&mut ignored);
    let reply =
        json!({"type": "result", "subtype": "success", "is_error": false, "result": DEMO_REPLY, "total_cost_usd": 0, "session_id": "demo-session", "num_turns": 1});
    println!("{reply}");
    EXIT_OK
}

fn read_flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let at = args.iter().position(|a| a == name)?;
    args.get(at + 1).map(String::as_str)
}

/// `demo-engine <project folder> [flags...]`: `--check` prints a PROBLEM line, `--answer <text>` records the answer for the animator, anything else is refused.
pub fn engine_stub(argv: &[String]) -> i32 {
    let target = argv.first().map(String::as_str).unwrap_or("");
    let args = if argv.is_empty() { &[][..] } else { &argv[1..] };
    if args.iter().any(|a| a == "--check") {
        println!("[check] PROBLEM: {NOTICE}");
        return EXIT_REFUSED;
    }
    if let Some(answer) = read_flag(args, "--answer") {
        if !target.is_empty() && std::path::Path::new(target).is_dir() {
            let path = ANSWER_FILE_PARTS.iter().fold(std::path::PathBuf::from(target), |path, part| path.join(part));
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(&path, answer.as_bytes()).is_ok() {
                println!("demo mode: answer recorded; the demo loop continues on its own.");
                return EXIT_OK;
            }
        }
    }
    println!("{NOTICE}");
    EXIT_REFUSED
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_engine_stub_records_an_answer_and_refuses_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().display().to_string();
        assert_eq!(engine_stub(&strings(&[&target, "--check"])), 1);
        assert_eq!(engine_stub(&strings(&[&target, "--wave", "2"])), 1);
        assert_eq!(engine_stub(&strings(&[&target, "--answer", "On the cart step"])), 0);
        let written = std::fs::read_to_string(dir.path().join(".specs/project/loop/internal/demo-answer.txt")).unwrap();
        assert_eq!(written, "On the cart step");
    }

    #[test]
    fn the_stub_is_recognised_by_the_executable_name() {
        assert!(is_claude_stub("Demo-Claude"));
        assert!(!is_claude_stub("harness"));
    }
}
