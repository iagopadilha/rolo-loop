// SPDX-License-Identifier: MIT
//! PreToolUse guard of the orchestrator (deterministic hook, does not depend on the model obeying).
//!
//! Registered in `hooks/hooks.json`: `guard --profile auto`. Reads the hook JSON on stdin; blocks with exit 2 and the
//! reason on stderr; any internal error is exit 0 (logged, and alerted at most once an hour).

use guard_core::paths::G;
use guard_core::run::run;
use std::io::{Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let profile = match args.iter().position(|a| a == "--profile") {
        Some(i) => args.get(i + 1).cloned().unwrap_or_default(),
        None => "executor".to_string(),
    };
    let mut raw = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    // A panic must never stall the loop: fail open, like every other internal error.
    let outcome = std::panic::catch_unwind(|| run(&G::from_process(), &profile, &text));
    match outcome {
        Ok(out) => {
            let _ = std::io::stdout().write_all(out.stdout.as_bytes());
            let _ = std::io::stderr().write_all(out.stderr.as_bytes());
            let _ = std::io::stdout().flush();
            std::process::exit(out.code);
        }
        Err(_) => std::process::exit(0),
    }
}
