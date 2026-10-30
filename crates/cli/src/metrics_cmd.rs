// SPDX-License-Identifier: MIT
//! `harness metrics [--json]`: how big the instructions an agent pays for on every spawn are (bytes and a rough token count,
//! bytes / 4). The measurement that comes before any claim of savings (lean-agent-pipeline, LAP-0). No model, no network.

use crate::paths::{build_paths, repo_root};
use cfg::Env;
use clap::Args as ClapArgs;
use serde_json::json;
use std::path::Path;

#[derive(ClapArgs)]
pub struct Args {
    /// JSON output
    #[arg(long)]
    json: bool,
}

/// `(name, bytes)` of every `*.md` directly in `folder`, sorted by name.
pub fn markdown_sizes(folder: &Path) -> Vec<(String, u64)> {
    let mut sizes: Vec<(String, u64)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "md"))
        .filter_map(|entry| Some((entry.path().file_stem()?.to_string_lossy().into_owned(), entry.metadata().ok()?.len())))
        .collect();
    sizes.sort();
    sizes
}

pub fn approx_tokens(bytes: u64) -> u64 {
    bytes / 4
}

pub fn run(args: Args) -> i32 {
    let env = Env::process();
    let paths = build_paths(&env, repo_root(&env));
    let plugin = Path::new(&paths.maestro_plugin);
    let agents = markdown_sizes(&plugin.join("agents"));
    let skill = Path::new(&paths.orchestrator_skill);
    let skill_bytes = std::fs::metadata(skill.join("SKILL.md")).map(|m| m.len()).unwrap_or(0);
    let references = markdown_sizes(&skill.join("references"));
    let references_bytes: u64 = references.iter().map(|(_, b)| b).sum();
    let agents_bytes: u64 = agents.iter().map(|(_, b)| b).sum();
    if args.json {
        let rows = |list: &[(String, u64)]| list.iter().map(|(n, b)| json!({"name": n, "bytes": b, "approx_tokens": approx_tokens(*b)})).collect::<Vec<_>>();
        let report = json!({
            "agents": rows(&agents), "agents_total_bytes": agents_bytes,
            "orchestrator_skill_bytes": skill_bytes, "references": rows(&references), "references_total_bytes": references_bytes,
        });
        println!("{}", serde_json::to_string_pretty(&report).expect("serializable"));
        return 0;
    }
    println!("agents (paid on every spawn of that agent; tokens ~ bytes/4):");
    for (name, bytes) in &agents {
        println!("  {name:<20} {bytes:>7} bytes  ~{:>5} tokens", approx_tokens(*bytes));
    }
    println!("  {:<20} {agents_bytes:>7} bytes  ~{:>5} tokens", "total", approx_tokens(agents_bytes));
    println!("orchestrator SKILL.md  {skill_bytes:>7} bytes  ~{:>5} tokens (once per turn)", approx_tokens(skill_bytes));
    println!("references (read one at a time, on demand): {references_bytes} bytes ~{} tokens in all", approx_tokens(references_bytes));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_markdown_files_are_counted_and_sorted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.md"), "12345678").unwrap();
        std::fs::write(dir.path().join("a.md"), "1234").unwrap();
        std::fs::write(dir.path().join("c.txt"), "zzzz").unwrap();
        assert_eq!(markdown_sizes(dir.path()), vec![("a".to_string(), 4), ("b".to_string(), 8)]);
        assert_eq!(approx_tokens(8), 2);
    }
}
