// SPDX-License-Identifier: MIT
//! Agent-context files that exist in a repository folder but are not committed.
//!
//! The loop worktree is created when the loop starts, from the last commit, so a map (`docs/codebase/*.md`), a `CLAUDE.md` or the root
//! `docs/codebase/SYSTEM.md` that is only on disk is invisible to the agents. This module only READS: it never stages, copies, commits or
//! stashes anything.
//!
//! The scope is one `git status --porcelain -uall -z` limited with pathspecs to those files, so a huge untracked folder such as
//! `node_modules` is never walked and `.specs/` is never listed. Confirmed monorepo: `<path>/docs/codebase/*.md` and `<path>/CLAUDE.md`
//! of every non-skipped subproject plus `docs/codebase/SYSTEM.md` (the root `CLAUDE.md` is not listed). Anything else: the root
//! `docs/codebase/*.md` and the root `CLAUDE.md`.

use crate::loop_io::run_timeout_raw;
use crate::subprojects::{self, SYSTEM_DOCUMENT};
use serde_json::{json, Map, Value};

const GIT_TIMEOUT_SECONDS: u64 = 30;
pub const KIND_MAP: &str = "map";
pub const KIND_CLAUDE: &str = "claude";
pub const KIND_SYSTEM: &str = "system";
pub const CODEBASE_FOLDER: &str = "docs/codebase";
pub const CLAUDE_FILE: &str = "CLAUDE.md";
const LIST_LIMIT: usize = 12;
const WARNING_SENTENCE: &str = "These files are not committed. The loop worktree is created when the loop starts, from the last commit, so the agents will not see them. Commit them first.";

pub fn state_by_code(letter: char) -> &'static str {
    match letter {
        '?' => "untracked",
        'A' => "added",
        'M' => "modified",
        'R' => "renamed",
        'C' => "added",
        'T' => "modified",
        _ => "modified",
    }
}

/// The folders of a confirmed monorepo's non-skipped subprojects; `None` when the entry is not a confirmed monorepo.
pub fn confirmed_project_folders(entry: Option<&Map<String, Value>>) -> Option<Vec<String>> {
    let entry = entry?;
    if entry.get("monorepo") != Some(&Value::Bool(true)) || entry.get("confirmed") != Some(&Value::Bool(true)) {
        return None;
    }
    let mut folders: Vec<String> = Vec::new();
    for project in subprojects::entry_projects(Some(entry)) {
        let folder = subprojects::normalize(project.get("path").and_then(Value::as_str).unwrap_or(""));
        if !folder.is_empty() && project.get("skip") != Some(&Value::Bool(true)) && !folders.contains(&folder) {
            folders.push(folder);
        }
    }
    Some(folders)
}

/// Glob pathspecs of the agent-context files (the folder list comes from a confirmed monorepo entry, else the root).
pub fn build_pathspecs(entry: Option<&Map<String, Value>>) -> Vec<String> {
    let Some(folders) = confirmed_project_folders(entry) else {
        return vec![format!(":(glob){CODEBASE_FOLDER}/*.md"), format!(":(glob){CLAUDE_FILE}")];
    };
    let mut specs = Vec::new();
    for folder in folders {
        specs.push(format!(":(glob){folder}/{CODEBASE_FOLDER}/*.md"));
        specs.push(format!(":(glob){folder}/{CLAUDE_FILE}"));
    }
    specs.push(format!(":(glob){SYSTEM_DOCUMENT}"));
    specs
}

/// `map | claude | system` for a path found by the status, or `None` when it is not an agent-context file.
pub fn kind_of(path: &str, is_monorepo: bool) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or("");
    if path == SYSTEM_DOCUMENT && is_monorepo {
        return Some(KIND_SYSTEM);
    }
    if name == CLAUDE_FILE && !(is_monorepo && path == CLAUDE_FILE) {
        return Some(KIND_CLAUDE);
    }
    if name.ends_with(".md") && format!("/{path}").ends_with(&format!("/{CODEBASE_FOLDER}/{name}")) {
        return Some(KIND_MAP);
    }
    None
}

/// `git status --porcelain -z` text -> items. Records are `XY path`; a rename/copy is followed by one extra record (the origin).
pub fn parse_status(output: &str, is_monorepo: bool) -> Vec<Value> {
    let (mut items, records) = (Vec::<Value>::new(), output.split('\0').collect::<Vec<_>>());
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        index += 1;
        if record.chars().count() < 4 {
            continue;
        }
        let code: String = record.chars().take(2).collect();
        let path = subprojects::normalize(&record.chars().skip(3).collect::<String>());
        if code.contains('R') || code.contains('C') {
            index += 1; // the origin path
        }
        if code.contains('D') {
            continue; // a deleted file has nothing to carry
        }
        if let Some(kind) = kind_of(&path, is_monorepo) {
            let letter = if code == "??" { '?' } else { code.chars().find(|c| *c != ' ').unwrap_or('M') };
            items.push(json!({"path": path, "kind": kind, "state": state_by_code(letter)}));
        }
    }
    items.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    items
}

/// The uncommitted agent-context files of `repo` (see the module text). One git call; any failure gives `[]`.
pub fn uncommitted_context(repo: &str, entry: Option<&Map<String, Value>>) -> Vec<Value> {
    let loaded;
    let entry = match entry {
        Some(e) => Some(e),
        None => {
            loaded = subprojects::load(repo, None).0;
            loaded.as_ref()
        }
    };
    let specs = build_pathspecs(entry);
    let mut args: Vec<&str> = vec!["-C", repo, "status", "--porcelain", "-uall", "-z", "--"];
    args.extend(specs.iter().map(String::as_str));
    let (code, out, _) = run_timeout_raw("git", &args, None, GIT_TIMEOUT_SECONDS);
    if code != 0 {
        return vec![];
    }
    parse_status(&out, confirmed_project_folders(entry).is_some())
}

/// The WARNING: the fixed sentence plus up to 12 names then "and N more". Empty when there is nothing to say.
pub fn uncommitted_warning_text(items: &[Value]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = items.iter().map(|i| i["path"].as_str().unwrap_or("")).collect();
    let mut shown = names.iter().take(LIST_LIMIT).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > LIST_LIMIT {
        shown.push_str(&format!(" and {} more", names.len() - LIST_LIMIT));
    }
    format!("{WARNING_SENTENCE} {shown}")
}

/// Short text for the workspace chip, e.g. `maps: 3 files, CLAUDE.md: backend, SYSTEM.md`.
pub fn uncommitted_chip_summary(items: &[Value]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let kind = |i: &Value| i["kind"].as_str().unwrap_or("").to_string();
    let maps = items.iter().filter(|i| kind(i) == KIND_MAP).count();
    if maps > 0 {
        parts.push(format!("maps: {maps} file{}", if maps == 1 { "" } else { "s" }));
    }
    let owners: Vec<String> = items
        .iter()
        .filter(|i| kind(i) == KIND_CLAUDE)
        .map(|i| {
            let path = i["path"].as_str().unwrap_or("");
            path.rsplit_once('/').map(|(head, _)| head.to_string()).unwrap_or_else(|| "root".into())
        })
        .collect();
    if !owners.is_empty() {
        parts.push(if owners == ["root"] { CLAUDE_FILE.to_string() } else { format!("{CLAUDE_FILE}: {}", owners.join(", ")) });
    }
    if items.iter().any(|i| kind(i) == KIND_SYSTEM) {
        parts.push("SYSTEM.md".into());
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_pathspecs() {
        assert_eq!(kind_of("docs/codebase/STACK.md", false), Some("map"));
        assert_eq!(kind_of("api/docs/codebase/STACK.md", true), Some("map"));
        assert_eq!(kind_of("docs/codebase/SYSTEM.md", true), Some("system"));
        assert_eq!(kind_of("api/CLAUDE.md", true), Some("claude"));
        assert_eq!(kind_of("CLAUDE.md", true), None);
        assert_eq!(kind_of("CLAUDE.md", false), Some("claude"));
        assert_eq!(kind_of("README.md", false), None);
        assert_eq!(build_pathspecs(None), vec![":(glob)docs/codebase/*.md", ":(glob)CLAUDE.md"]);
        let entry: Map<String, Value> = serde_json::from_value(json!({"monorepo": true, "confirmed": true, "projects": [{"path": "api"}, {"path": "web", "skip": true}, {"path": "api/"}]})).unwrap();
        assert_eq!(build_pathspecs(Some(&entry)), vec![":(glob)api/docs/codebase/*.md", ":(glob)api/CLAUDE.md", ":(glob)docs/codebase/SYSTEM.md"]);
    }

    #[test]
    fn status_parsing_and_texts() {
        let out = "?? docs/codebase/B.md\0 M docs/codebase/A.md\0R  docs/codebase/N.md\0docs/codebase/O.md\0 D docs/codebase/Gone.md\0?? other.txt\0";
        let items = parse_status(out, false);
        let pairs: Vec<(&str, &str)> = items.iter().map(|i| (i["path"].as_str().unwrap(), i["state"].as_str().unwrap())).collect();
        assert_eq!(pairs, vec![("docs/codebase/A.md", "modified"), ("docs/codebase/B.md", "untracked"), ("docs/codebase/N.md", "renamed")]);
        assert!(uncommitted_warning_text(&items).ends_with("Commit them first. docs/codebase/A.md, docs/codebase/B.md, docs/codebase/N.md"));
        assert_eq!(uncommitted_warning_text(&[]), "");
        assert_eq!(uncommitted_chip_summary(&items), "maps: 3 files");
        let mixed = vec![json!({"path": "CLAUDE.md", "kind": "claude"}), json!({"path": "api/CLAUDE.md", "kind": "claude"}), json!({"path": "docs/codebase/SYSTEM.md", "kind": "system"})];
        assert_eq!(uncommitted_chip_summary(&mixed), "CLAUDE.md: root, api, SYSTEM.md");
    }
}
