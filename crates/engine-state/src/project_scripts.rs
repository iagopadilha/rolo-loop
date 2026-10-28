// SPDX-License-Identifier: MIT
//! The project's own scripts, sorted into safe and asks-once, with the reason a turn may wait on one (backlog I3).
//!
//! `npm run build`, `npm test`, `dotnet build`, `dotnet test`... may stop a headless turn on a prompt, run for a long time or need the network,
//! and the user could not tell why the turn paused. This module reads what the project declares and says it in words:
//!
//! * [`scan`]: the scripts of every `package.json` (root and one folder down) and the .NET solutions/projects. A script whose NAME is a plain
//!   check (build, lint, test, typecheck, check...) and whose body shows no publish, deploy, install or download is `safe`; every other
//!   script is `ask` (decided once by the user).
//! * [`hint`]: why a command may pause: `network`, `long` or `prompt`, in a sentence.
//! * [`remember`] / [`remembered`]: the user's answer, kept per project in `loop/json/SCRIPTS.json` (`{"allow": [...], "deny": [...]}`),
//!   commands as typed. The file only stores command text; the engine never runs it from there.

use crate::loop_io::{load_json, read, save_json};
use crate::loop_state::loop_path;
use serde_json::{json, Value};
use std::path::Path;

const STORE: &str = "SCRIPTS.json";
const SAFE_NAMES: [&str; 14] =
    ["build", "lint", "test", "typecheck", "type-check", "tsc", "check", "format:check", "lint:fix", "test:unit", "test:ci", "compile", "verify", "types"];
/// Words in a script body that make it more than a check.
const RISKY_WORDS: [&str; 10] = ["publish", "deploy", "release", "curl ", "wget ", "npm install", "npm i ", "rm -rf", "git push", "docker push"];
const SKIPPED_FOLDERS: [&str; 6] = ["node_modules", "bin", "obj", "dist", "target", "vendor"];

fn is_safe(name: &str, body: &str) -> bool {
    let lowered = body.to_ascii_lowercase();
    SAFE_NAMES.contains(&name) && !RISKY_WORDS.iter().any(|w| lowered.contains(w))
}

fn package_scripts(folder: &Path, relative: &str, out: &mut Vec<Value>) {
    let data = load_json(&folder.join("package.json"));
    let Some(scripts) = data.get("scripts").and_then(Value::as_object) else { return };
    let mut names: Vec<&String> = scripts.keys().collect();
    names.sort();
    for name in names {
        let body = scripts[name].as_str().unwrap_or("");
        let command = if name == "test" { "npm test".to_string() } else { format!("npm run {name}") };
        out.push(json!({"command": command, "dir": relative, "source": "package.json", "kind": if is_safe(name, body) { "safe" } else { "ask" }}));
    }
}

fn dotnet_here(folder: &Path) -> bool {
    std::fs::read_dir(folder)
        .map(|entries| entries.flatten().any(|e| matches!(e.path().extension().and_then(|x| x.to_str()), Some("sln") | Some("csproj"))))
        .unwrap_or(false)
}

/// `[{command, dir, source, kind}]`, `kind` = `safe` or `ask`. Root first, then each folder one level down, sorted by name.
pub fn scan(root: &str) -> Vec<Value> {
    let base = Path::new(root);
    let mut out = Vec::new();
    let mut folders: Vec<(String, std::path::PathBuf)> = vec![(String::new(), base.to_path_buf())];
    if let Ok(entries) = std::fs::read_dir(base) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.') && !SKIPPED_FOLDERS.contains(&n.as_str()))
            .collect();
        names.sort();
        folders.extend(names.into_iter().map(|n| (n.clone(), base.join(n))));
    }
    for (relative, folder) in folders {
        package_scripts(&folder, &relative, &mut out);
        if dotnet_here(&folder) {
            for command in ["dotnet build", "dotnet test"] {
                out.push(json!({"command": command, "dir": relative, "source": "dotnet", "kind": "safe"}));
            }
        }
    }
    out
}

/// Why a command may make a turn wait, in one sentence; `None` when nothing is known about it. `network`: it downloads (restore, install, an
/// `npx` package). `long`: tests and builds can run for minutes. `prompt`: it may ask a question nobody can answer in a headless turn.
pub fn hint(command: &str) -> Option<(&'static str, &'static str)> {
    let text = command.trim().to_ascii_lowercase();
    let has = |word: &str| text.split_whitespace().any(|w| w == word);
    if text.starts_with("npx ") && !text.contains("--yes") && !text.contains(" -y") {
        return Some(("prompt", "npx may ask to download a package and wait for an answer; add --yes or install the tool first."));
    }
    if has("init") || has("create") || has("login") || text.contains("--interactive") {
        return Some(("prompt", "this command may ask questions; a headless turn cannot answer them."));
    }
    if text.starts_with("dotnet build") || text.starts_with("dotnet test") || text.starts_with("dotnet run") {
        return Some(("network", "dotnet restores packages first when they are not on disk, so it needs the NuGet hosts; use --no-restore after a restore."));
    }
    if has("install") || has("ci") || has("restore") || has("add") {
        return Some(("network", "it downloads packages, so it needs the registry hosts of the project."));
    }
    if text.contains("test") || text.contains("e2e") || text.contains("build") {
        return Some(("long", "builds and tests can run for minutes; the turn waits until they end or the turn time limit."));
    }
    None
}

/// What the user answered before: `(allow, deny)` command lists.
pub fn remembered(work: &str) -> (Vec<String>, Vec<String>) {
    let data = load_json(&loop_path(work, STORE));
    let list = |key: &str| -> Vec<String> {
        data.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    (list("allow"), list("deny"))
}

/// Remembers the answer for a command (one line, no control characters). Returns false for an empty or multi-line command.
pub fn remember(work: &str, command: &str, allow: bool) -> bool {
    let command = command.trim();
    if command.is_empty() || command.len() > 200 || command.chars().any(char::is_control) {
        return false;
    }
    let (mut yes, mut no) = remembered(work);
    yes.retain(|c| c != command);
    no.retain(|c| c != command);
    if allow {
        yes.push(command.to_string());
    } else {
        no.push(command.to_string());
    }
    save_json(&loop_path(work, STORE), &json!({"allow": yes, "deny": no})).is_ok()
}

/// `{safe, ask, remembered_allow, remembered_deny}` for the Start and Test environment screens. `ask` rows carry `hint` (`{why, text}`) when
/// the command shows a known reason to wait, and drop out of `ask` when the user already answered them.
pub fn report(root: &str, work: &str) -> Value {
    let (allow, deny) = remembered(work);
    let (mut safe, mut ask) = (Vec::new(), Vec::new());
    for mut row in scan(root) {
        let command = row["command"].as_str().unwrap_or("").to_string();
        if let Some((why, text)) = hint(&command) {
            row["hint"] = json!({"why": why, "text": text});
        }
        if row["kind"] == "safe" || allow.contains(&command) {
            safe.push(row);
        } else if !deny.contains(&command) {
            ask.push(row);
        }
    }
    json!({"safe": safe, "ask": ask, "remembered_allow": allow, "remembered_deny": deny})
}

/// True when `read` finds the file (so a view can tell "nothing asked yet" from "nothing to ask").
pub fn answered_before(work: &str) -> bool {
    !read(&loop_path(work, STORE)).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn plain_checks_are_safe_and_the_rest_asks() {
        let dir = tempfile::tempdir().unwrap();
        put(
            dir.path(),
            "package.json",
            r#"{"scripts": {"build": "tsc -p .", "test": "vitest run", "deploy": "ship it", "lint": "eslint . && curl http://x", "seed": "node seed.js"}}"#,
        );
        put(dir.path(), "api/App.sln", "");
        put(dir.path(), "node_modules/dep/package.json", r#"{"scripts": {"test": "x"}}"#);
        let rows = scan(&dir.path().display().to_string());
        let kind = |command: &str| rows.iter().find(|r| r["command"] == command).map(|r| r["kind"].as_str().unwrap().to_string());
        assert_eq!((kind("npm run build").as_deref(), kind("npm test").as_deref()), (Some("safe"), Some("safe")));
        assert_eq!((kind("npm run deploy").as_deref(), kind("npm run lint").as_deref(), kind("npm run seed").as_deref()), (Some("ask"), Some("ask"), Some("ask")));
        assert_eq!(kind("dotnet test").as_deref(), Some("safe"));
        assert_eq!(rows.iter().filter(|r| r["dir"] == "api").count(), 2);
        assert_eq!(rows.len(), 7, "node_modules is not entered");
    }

    #[test]
    fn a_hint_says_why_a_command_may_wait() {
        assert_eq!(hint("npx some-tool run").unwrap().0, "prompt");
        assert_eq!(hint("npx --yes some-tool").map(|h| h.0), None);
        assert_eq!(hint("dotnet test").unwrap().0, "network");
        assert_eq!(hint("npm ci").unwrap().0, "network");
        assert_eq!(hint("npm run build").unwrap().0, "long");
        assert_eq!(hint("ls"), None);
    }

    #[test]
    fn an_answer_is_remembered_once_and_moves_the_command_out_of_ask() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        put(dir.path(), "package.json", r#"{"scripts": {"seed": "node seed.js", "reset": "node reset.js"}}"#);
        assert_eq!(report(&root, &root)["ask"].as_array().unwrap().len(), 2);
        assert!(!answered_before(&root));
        assert!(remember(&root, "npm run seed", true));
        assert!(remember(&root, "npm run reset", false));
        assert!(remember(&root, "npm run seed", true), "the same answer twice keeps one line");
        let report = report(&root, &root);
        assert_eq!(report["ask"].as_array().unwrap().len(), 0);
        assert_eq!(report["safe"][0]["command"], "npm run seed");
        assert_eq!((report["remembered_allow"].clone(), report["remembered_deny"].clone()), (json!(["npm run seed"]), json!(["npm run reset"])));
        assert!(answered_before(&root));
        assert!(!remember(&root, "", true) && !remember(&root, "a\nb", true));
    }
}
