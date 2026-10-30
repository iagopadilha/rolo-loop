// SPDX-License-Identifier: MIT
//! A dry check before a codebase map (M6): the guard's own rules (the `spec` profile the map agents run under) are applied to the paths the map
//! is going to write, and every refusal is reported in ONE message before the job spends anything. Nothing is written and no agent runs.

use cfg::Env;
use guard_core::paths::G;
use serde_json::json;
use std::path::Path;

/// The documents a map writes (the brownfield mapper's list), plus the folder itself.
pub const MAP_DOCUMENTS: [&str; 7] = ["STACK.md", "ARCHITECTURE.md", "CONVENTIONS.md", "STRUCTURE.md", "TESTING.md", "INTEGRATIONS.md", "CONCERNS.md"];
const MAP_FOLDER: &str = "docs/codebase";
const SHOWN: usize = 6;

fn refusal_line(stderr: &str) -> String {
    let first = stderr.lines().find(|l| l.starts_with("BLOCKED")).unwrap_or(stderr.lines().next().unwrap_or(""));
    first.chars().take(240).collect()
}

/// The refusals of the guard for every path the map writes under `map_root` (`<root>/docs/codebase/...`), with `cwd` as the folder the agent runs
/// in and `extra_env` as the job's extra environment (for example `LOOP_PRIMARY_SPECS`). Empty = the map can write what it needs.
pub fn check(cwd: &str, map_root: &str, extra_env: &[(String, String)]) -> Vec<String> {
    let mut env = Env::process();
    for (name, value) in extra_env {
        env.set(name, value);
    }
    let g = G { env, case_insensitive: cfg!(windows) };
    let folder = Path::new(map_root).join("docs").join("codebase");
    let mut payloads: Vec<(String, serde_json::Value)> = Vec::new();
    payloads.push((
        format!("{MAP_FOLDER}/"),
        json!({"tool_name": "Bash", "tool_input": {"command": format!("mkdir -p \"{}\"", folder.display().to_string().replace('\\', "/"))}, "cwd": cwd}),
    ));
    for name in MAP_DOCUMENTS {
        let path = folder.join(name).display().to_string();
        payloads.push((format!("{MAP_FOLDER}/{name}"), json!({"tool_name": "Write", "tool_input": {"file_path": path, "content": "x"}, "cwd": cwd})));
    }
    let mut refusals = Vec::new();
    for (label, payload) in payloads {
        let outcome = guard_core::run::run(&g, "spec", &payload.to_string());
        if outcome.code == 2 {
            refusals.push(format!("{label}: {}", refusal_line(&outcome.stderr)));
        }
    }
    refusals
}

/// The one message the user reads: every refused path, what to release and that nothing was spent.
pub fn message(refusals: &[String]) -> String {
    let shown: Vec<&String> = refusals.iter().take(SHOWN).collect();
    let more = refusals.len().saturating_sub(SHOWN);
    format!(
        "The guard would refuse {} of the files this map writes, so nothing was started and nothing was spent: {}{}. Fix or release those rules (PERMISSIONS.json) and map again.",
        refusals.len(),
        shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" | "),
        if more > 0 { format!(" | and {more} more") } else { String::new() }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under the target folder: the OS temp folder can be an 8.3 short path, which the guard expands on one side only.
    fn tempdir() -> tempfile::TempDir {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target");
        std::fs::create_dir_all(&base).unwrap();
        tempfile::tempdir_in(std::fs::canonicalize(base).unwrap().display().to_string().trim_start_matches("\\\\?\\")).unwrap()
    }

    fn repo() -> (tempfile::TempDir, String) {
        let dir = tempdir();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".specs/project")).unwrap();
        (dir, root.display().to_string())
    }

    #[test]
    fn a_plain_repo_can_be_mapped() {
        let (_dir, root) = repo();
        assert_eq!(check(&root, &root, &[]), Vec::<String>::new());
    }

    #[test]
    fn a_subproject_is_refused_until_its_folder_is_confirmed_and_all_refusals_come_in_one_message() {
        let (dir, root) = repo();
        let sub = format!("{root}/svc");
        std::fs::create_dir_all(&sub).unwrap();
        let refused = check(&root, &sub, &[]);
        assert_eq!(refused.len(), MAP_DOCUMENTS.len() + 1, "{refused:?}");
        assert!(refused[0].starts_with("docs/codebase/:") && refused.iter().all(|r| r.contains("BLOCKED")), "{refused:?}");
        let one = message(&refused);
        assert!(one.contains("nothing was spent") && one.contains("and 2 more"), "{one}");
        // the primary's confirmed list, exported to the job, opens exactly that folder
        let primary = dir.path().join("primary");
        std::fs::create_dir_all(primary.join(".specs/project")).unwrap();
        std::fs::write(primary.join(".specs/project/SUBPROJECTS.json"), r#"{"repos": {"proj": {"monorepo": true, "confirmed": true, "projects": [{"path": "svc"}]}}}"#)
            .unwrap();
        let env = [("LOOP_PRIMARY_SPECS".to_string(), primary.join(".specs").display().to_string())];
        assert_eq!(check(&root, &sub, &env), Vec::<String>::new());
    }
}
