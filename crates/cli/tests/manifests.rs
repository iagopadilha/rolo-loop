// SPDX-License-Identifier: MIT
//! One version per repository: the workspace, the plugin manifest and the marketplace entry must agree; the hook calls the guard binary; the
//! dependency on the kit is declared in a form the doctor can read.

use serde_json::Value;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn json(relative: &str) -> Value {
    let text = std::fs::read_to_string(root().join(relative)).unwrap_or_else(|e| panic!("{relative}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{relative}: {e}"))
}

#[test]
fn the_plugin_the_marketplace_and_the_workspace_share_one_version() {
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(json("plugin/.claude-plugin/plugin.json")["version"], version);
    assert_eq!(json(".claude-plugin/marketplace.json")["plugins"][0]["version"], version);
}

#[test]
fn the_marketplace_points_at_the_plugin_folder_by_name() {
    let marketplace = json(".claude-plugin/marketplace.json");
    assert_eq!(marketplace["plugins"][0]["source"], "./plugin");
    assert_eq!(marketplace["plugins"][0]["name"], json("plugin/.claude-plugin/plugin.json")["name"]);
}

#[test]
fn the_kit_dependency_is_declared_and_allowed_across_marketplaces() {
    let plugin = json("plugin/.claude-plugin/plugin.json");
    assert_eq!(plugin["dependencies"][0]["name"], "craft-dev-toolkit");
    assert_eq!(plugin["dependencies"][0]["version"], ">=0.3.0 <0.4.0");
    assert_eq!(json(".claude-plugin/marketplace.json")["allowCrossMarketplaceDependenciesOn"][0], "craft-dev-toolkit");
}

#[test]
fn the_hook_runs_the_guard_binary_and_the_skill_hook_agrees() {
    let hooks = json("plugin/hooks/hooks.json");
    let command = hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap();
    assert_eq!(command, "\"${CLAUDE_PLUGIN_ROOT}/bin/guard.exe\" --profile auto");
    let skill = std::fs::read_to_string(root().join("plugin/skills/orchestrator/SKILL.md")).unwrap();
    assert!(skill.contains("\"\\\"${CLAUDE_PLUGIN_ROOT}/bin/guard.exe\\\" --profile orchestrator\""));
}

#[test]
fn the_plugin_text_never_calls_a_python_script() {
    let mut stack = vec![root().join("plugin")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md" || e == "json") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for needle in ["harness.py", "run_loop.py", "loop_state.py", "guard.py", "guard-launcher"] {
                    assert!(!text.contains(needle), "{} mentions {needle}", path.display());
                }
            }
        }
    }
}
