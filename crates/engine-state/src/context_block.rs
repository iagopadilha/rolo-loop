// SPDX-License-Identifier: MIT
//! The generated `maestro:context` block of a project's `CLAUDE.md`.
//!
//! One block between `<!-- maestro:context -->` and `<!-- /maestro:context -->` holds only stable, non-restrictive facts about the project: its
//! Conformance (one sentence, never a line that starts with `Conformance:`), where the map is, where the system map is (a monorepo subproject),
//! how it is built, run and tested and the giant-file rule. Nothing about the loop, git or approvals.
//!
//! [`render`] gives the block text with `\n` line ends and no trailing newline; [`write`] keeps every byte outside the markers (line endings
//! included), writes the block with the file's own line ending, appends a missing block at the end, creates a missing `CLAUDE.md` with a
//! `# <name>` title, refuses to touch a file whose markers are damaged (two blocks, or a start without an end) and does not touch the file when
//! the rendered text is already there (no mtime change).

use pycompat::nt_abspath;
use serde_json::{json, Value};
use std::path::Path;

pub const START_MARKER: &str = "<!-- maestro:context -->";
pub const END_MARKER: &str = "<!-- /maestro:context -->";
pub const DAMAGED_ERROR: &str = "context block damaged";
pub const DEFAULT_GIANT_LINES: i64 = 1500;
pub const MAP_RELATIVE_FOLDER: &str = "docs/codebase";
const TESTING_DOCUMENT: &str = "TESTING.md";
/// The block of `run_hints::write_codebase_block`: a CLAUDE.md with this context block never keeps it (one block about the map per file).
pub const CODEBASE_START_MARKER: &str = "<!-- CODEBASE:BEGIN -->";
pub const CODEBASE_END_MARKER: &str = "<!-- CODEBASE:END -->";
const MAP_GUIDE_SENTENCE: &str = "They are a guide, not the source of truth: if a document disagrees with the code, the code wins. If a change really alters one of them, update the document in the same change.";
const DOCUMENT_PURPOSE: [(&str, &str); 7] = [
    ("STACK.md", "languages, frameworks, versions and main dependencies"),
    ("ARCHITECTURE.md", "layers, patterns and how data flows"),
    ("CONVENTIONS.md", "naming, code style and error handling in use"),
    ("STRUCTURE.md", "folder layout and where each kind of code lives"),
    ("TESTING.md", "how tests are written and run"),
    ("INTEGRATIONS.md", "external APIs, databases, queues and authentication"),
    ("CONCERNS.md", "tech debt, risks and fragile areas"),
];

fn purpose(name: &str) -> &'static str {
    DOCUMENT_PURPOSE.iter().find(|(n, _)| *n == name).map(|(_, p)| *p).unwrap_or("mapping document")
}

pub fn render_conformance_sentence(conformance: Option<&str>) -> String {
    match conformance {
        Some(value @ ("STANDARD" | "LEGACY")) => format!("This project's Conformance is {value}; the rules are in the Conformance block of this file."),
        _ => "This project's Conformance is not set yet; when it is set, the rules are in the Conformance block of this file.".to_string(),
    }
}

pub fn render_map_lines(map_documents: &[String]) -> Vec<String> {
    if map_documents.is_empty() {
        return vec![format!("The codebase map is not made yet (it will be in `{MAP_RELATIVE_FOLDER}/`).")];
    }
    let mut lines = vec![format!("The codebase map is in `{MAP_RELATIVE_FOLDER}/`. Read one document only when the task needs it, not at the start of every session:")];
    for name in map_documents {
        lines.push(format!("- `{MAP_RELATIVE_FOLDER}/{name}`: {}", purpose(name)));
    }
    lines.push(String::new());
    lines.push(MAP_GUIDE_SENTENCE.to_string());
    lines
}

/// `text` without the `CODEBASE` block(s) of run_hints (only between their markers, plus the line ending after the end marker and one surplus
/// blank line); everything else is byte-identical. A start without an end is left alone. Returns `(new text, removed)`.
pub fn remove_codebase_blocks(text: &str) -> (String, bool) {
    let mut text = text.to_string();
    let mut removed = false;
    loop {
        let Some(start) = text.find(CODEBASE_START_MARKER) else { return (text, removed) };
        let Some(relative_end) = text[start..].find(CODEBASE_END_MARKER) else { return (text, removed) };
        let mut end = start + relative_end + CODEBASE_END_MARKER.len();
        for line_ending in ["\r\n", "\n"] {
            if text[end..].starts_with(line_ending) {
                end += line_ending.len();
                break;
            }
        }
        let mut before = text[..start].to_string();
        let mut after = text[end..].to_string();
        for line_ending in ["\r\n", "\n"] {
            // the block was set apart by a blank line: do not leave two
            if before.ends_with(&line_ending.repeat(2)) && after.starts_with(line_ending) {
                after = after[line_ending.len()..].to_string();
                break;
            }
        }
        if after.is_empty() && (before.ends_with("\r\n\r\n") || before.ends_with("\n\n")) {
            // the block was the last thing in the file
            let cut = if before.ends_with("\r\n\r\n") { 2 } else { 1 };
            before.truncate(before.len() - cut);
        }
        text = before + &after;
        removed = true;
    }
}

pub fn render_command_lines(commands: Option<&Value>, map_documents: &[String]) -> Vec<String> {
    let text_of = |slot: &str| commands.and_then(|c| c.get(slot)).map(|v| match v { Value::String(s) => s.trim().to_string(), Value::Null => String::new(), other => pycompat::json::py_json_dumps(other, false) }).unwrap_or_default();
    let known: Vec<(&str, String)> = [("Build", "build"), ("Run", "run"), ("Test", "test")].iter().map(|(label, slot)| (*label, text_of(slot))).filter(|(_, text)| !text.is_empty()).collect();
    if known.is_empty() {
        if map_documents.iter().any(|d| d == TESTING_DOCUMENT) {
            return vec![format!("Build/test command not known: see `{MAP_RELATIVE_FOLDER}/{TESTING_DOCUMENT}`.")];
        }
        return vec![format!("Build/test command not known: see `{MAP_RELATIVE_FOLDER}/{TESTING_DOCUMENT}` when it exists.")];
    }
    known.into_iter().map(|(label, text)| format!("{label}: `{text}`")).collect()
}

/// The block (markers included). `folder` is only the project the block describes (kept for callers and future facts).
pub fn render(_folder: &str, facts: &Value) -> String {
    let map_documents: Vec<String> = facts.get("map_documents").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
    let giant_lines = match facts.get("giant_lines") {
        Some(Value::Number(n)) if n.as_f64().is_some_and(|f| f != 0.0) => n.as_f64().map(|f| f as i64).unwrap_or(DEFAULT_GIANT_LINES),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok().filter(|v| *v != 0).unwrap_or(DEFAULT_GIANT_LINES),
        _ => DEFAULT_GIANT_LINES,
    };
    let mut lines = vec![
        START_MARKER.to_string(),
        "## Project context (generated by the app; rewritten when the map, the Conformance or the project list changes)".to_string(),
        String::new(),
        render_conformance_sentence(facts.get("conformance").and_then(Value::as_str)),
        String::new(),
    ];
    lines.extend(render_map_lines(&map_documents));
    let pointer = facts.get("system_pointer").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if !pointer.is_empty() {
        lines.push(String::new());
        lines.push(format!("How this project talks to the others (routes, contracts, flows) is in `{pointer}`; read it only when the task crosses projects."));
    }
    lines.push(String::new());
    lines.extend(render_command_lines(facts.get("commands"), &map_documents));
    lines.push(String::new());
    lines.push(format!("Files above {giant_lines} lines: read them by signatures (Grep, then small ranges), never whole; never make a file cross {giant_lines} lines: split it instead."));
    lines.push(END_MARKER.to_string());
    lines.join("\n")
}

/// `Ok(Some((start, end after the end marker)))`, `Ok(None)` when absent, or `Err(())` when damaged.
pub fn find_block(text: &str) -> Result<Option<(usize, usize)>, ()> {
    let (starts, ends) = (text.matches(START_MARKER).count(), text.matches(END_MARKER).count());
    if starts != ends || starts > 1 {
        return Err(());
    }
    let Some(start) = text.find(START_MARKER) else { return Ok(None) };
    let end = text.find(END_MARKER).unwrap_or(0);
    if end < start {
        return Err(());
    }
    Ok(Some((start, end + END_MARKER.len())))
}

/// Writes the block into `<folder>/CLAUDE.md`. Never fails: `{"written": false, "error": text}` on a problem.
pub fn write(folder: &str, facts: &Value, title: Option<&str>) -> Value {
    let result = |written: bool, error: &str| json!({"written": written, "error": error});
    let path = Path::new(folder).join("CLAUDE.md");
    let block = render(folder, facts);
    let (mut text, created, original);
    if path.exists() {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(failure) => return result(false, &format!("OSError: {}", failure.to_string().chars().take(200).collect::<String>())),
        };
        let Ok(read) = String::from_utf8(bytes) else { return result(false, "UnicodeDecodeError: invalid UTF-8") };
        original = read.clone();
        text = remove_codebase_blocks(&read).0;
        created = false;
    } else {
        if !Path::new(folder).is_dir() {
            return result(false, &format!("folder does not exist: {folder}"));
        }
        text = String::new();
        created = true;
        original = String::new();
    }
    let Ok(found) = find_block(&text) else { return result(false, DAMAGED_ERROR) };
    let line_ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let block = block.replace('\n', line_ending);
    let updated = if created {
        let name = title.map(String::from).unwrap_or_else(|| pycompat::last_segment(&nt_abspath(folder)).to_string());
        format!("# {name}{line_ending}{line_ending}{block}{line_ending}")
    } else if let Some((start, end)) = found {
        format!("{}{block}{}", &text[..start], &text[end..])
    } else {
        let separator = if text.is_empty() { "" } else if text.ends_with('\n') { line_ending } else { &line_ending.repeat(2) };
        text.push_str(separator);
        text.push_str(&block);
        text.push_str(line_ending);
        text
    };
    if updated == original {
        return result(false, "");
    }
    match std::fs::write(&path, updated.as_bytes()) {
        Ok(()) => result(true, ""),
        Err(failure) => result(false, &format!("OSError: {}", failure.to_string().chars().take(200).collect::<String>())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_is_written_once_and_keeps_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().display().to_string();
        let facts = json!({"conformance": "STANDARD", "map_documents": ["TESTING.md", "X.md"], "commands": {"test": "cargo test"}, "giant_lines": 900});
        assert_eq!(write(&folder, &facts, Some("Proj"))["written"], true);
        let first = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(first.starts_with("# Proj\n\n<!-- maestro:context -->\n") && first.contains("Conformance is STANDARD") && first.contains("Test: `cargo test`"));
        assert!(first.contains("`docs/codebase/X.md`: mapping document") && first.contains("Files above 900 lines"));
        assert_eq!(write(&folder, &facts, None)["written"], false);
        std::fs::write(dir.path().join("CLAUDE.md"), "team text\r\n<!-- CODEBASE:BEGIN -->\r\nold\r\n<!-- CODEBASE:END -->\r\n").unwrap();
        assert_eq!(write(&folder, &json!({}), None)["written"], true);
        let second = std::fs::read(dir.path().join("CLAUDE.md")).unwrap();
        let second = String::from_utf8(second).unwrap();
        assert!(second.starts_with("team text\r\n\r\n<!-- maestro:context -->\r\n") && !second.contains("CODEBASE") && second.ends_with("<!-- /maestro:context -->\r\n"));
        std::fs::write(dir.path().join("CLAUDE.md"), format!("{START_MARKER}\n")).unwrap();
        assert_eq!(write(&folder, &json!({}), None), json!({"written": false, "error": DAMAGED_ERROR}));
        assert!(write(&dir.path().join("none").display().to_string(), &json!({}), None)["error"].as_str().unwrap().starts_with("folder does not exist"));
    }

    #[test]
    fn codebase_blocks_are_cut_cleanly() {
        let text = "a\n\n<!-- CODEBASE:BEGIN -->\nx\n<!-- CODEBASE:END -->\n\nb\n";
        assert_eq!(remove_codebase_blocks(text), ("a\n\nb\n".to_string(), true));
        assert_eq!(remove_codebase_blocks("a\n\n<!-- CODEBASE:BEGIN -->\nx\n<!-- CODEBASE:END -->\n"), ("a\n".to_string(), true));
        assert_eq!(remove_codebase_blocks("<!-- CODEBASE:BEGIN -->\nx"), ("<!-- CODEBASE:BEGIN -->\nx".to_string(), false));
    }
}
