// SPDX-License-Identifier: MIT
//! The AI decisions log: one grammar owner for `.specs/project/loop/VALIDATE.md`.
//!
//! Line grammar:
//!   `- [<slug>|general] turn=<N> level=<important|critical> <doubt> | chose: <choice> | revert: <how>`
//! Old lines without turn=/level= still parse (turn None, level important). A line that starts with `- [` but does not match is kept as
//! `unparsed` (never dropped). The file stays append-only: reviewed state lives elsewhere (app-owned).

use pycompat::{splitlines, Re};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::io::Write;
use std::path::PathBuf;
use std::sync::LazyLock;

pub const LEVELS: [&str; 2] = ["important", "critical"];
pub const DEFAULT_LEVEL: &str = "important";

static LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*-\s*\[([^\]]+)\]\s*(.*)\z"));
static TURN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^turn=(\d+|unknown)\s+"));
static LEVEL: LazyLock<Re> = LazyLock::new(|| Re::new(r"^level=(important|critical)\s+"));
static CHOSE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\s*\|\s*chose:\s*"));
static REVERT: LazyLock<Re> = LazyLock::new(|| Re::new(r"\s*\|\s*revert:\s*"));
static SPACES: LazyLock<Re> = LazyLock::new(|| Re::new(r"\s+"));

/// VALIDATE.md of a project or worktree root.
pub fn validate_path(root: &str) -> PathBuf {
    std::path::Path::new(root).join(".specs").join("project").join("loop").join("VALIDATE.md")
}

/// Stable id of a decision: sha1 of the raw line (whitespace at the ends ignored).
pub fn line_id(raw: &str) -> String {
    let digest = Sha1::digest(raw.trim().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn split_once(re: &Re, text: &str) -> Vec<String> {
    match re.search(text) {
        Some(m) => vec![text[..m.start].to_string(), text[m.end..].to_string()],
        None => vec![text.to_string()],
    }
}

/// One decision, or `None` when the line is not a decision line at all (headings, blank lines, prose).
pub fn parse_line(raw: &str, line_no: usize) -> Option<Value> {
    let m = LINE.match_start(raw)?;
    let mut body = m.group(2).unwrap_or("").trim().to_string();
    let mut turn = Value::Null;
    let mut level = DEFAULT_LEVEL.to_string();
    if let Some(t) = TURN.match_start(&body) {
        let value = t.group(1).unwrap_or("");
        if value.chars().all(|c| c.is_ascii_digit()) {
            turn = value.parse::<u64>().map(Value::from).unwrap_or(Value::Null);
        }
        body = body[t.end..].to_string();
    }
    if let Some(l) = LEVEL.match_start(&body) {
        level = l.group(1).unwrap_or("").to_string();
        body = body[l.end..].to_string();
    }
    let parts = split_once(&CHOSE, &body);
    if parts.len() != 2 {
        return Some(json!({"unparsed": true, "raw": raw.trim_end_matches('\n'), "line_no": line_no, "id": line_id(raw)}));
    }
    let rest = split_once(&REVERT, &parts[1]);
    Some(json!({
        "id": line_id(raw),
        "slug": m.group(1).unwrap_or("").trim(),
        "turn": turn,
        "level": level,
        "doubt": parts[0].trim(),
        "chose": rest[0].trim(),
        "revert": rest.get(1).map(|r| r.trim()).unwrap_or(""),
        "raw": raw.trim_end_matches('\n'),
        "line_no": line_no,
    }))
}

/// `(items, unparsed)`; items keep file order; `line_no` is 1-based.
pub fn parse(text: &str) -> (Vec<Value>, Vec<Value>) {
    let (mut items, mut unparsed) = (Vec::new(), Vec::new());
    for (index, raw) in splitlines(text).into_iter().enumerate() {
        match parse_line(raw, index + 1) {
            None => {}
            Some(p) if p.get("unparsed").is_some() => unparsed.push(p),
            Some(p) => items.push(p),
        }
    }
    (items, unparsed)
}

pub fn read_file(root: &str) -> String {
    std::fs::read(validate_path(root)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

fn clean(value: &str) -> String {
    SPACES.replace_lit(value, " ").replace('|', "/").trim().to_string()
}

pub fn format_line(slug: &str, turn: Option<i64>, level: &str, doubt: &str, chose: &str, revert: &str) -> String {
    let level = if LEVELS.contains(&level) { level } else { DEFAULT_LEVEL };
    let turn_text = turn.map(|t| t.to_string()).unwrap_or_else(|| "unknown".into());
    let slug = clean(slug);
    format!("- [{}] turn={turn_text} level={level} {} | chose: {} | revert: {}", if slug.is_empty() { "general" } else { &slug }, clean(doubt), clean(chose), clean(revert))
}

/// Appends one line (creates the folder and file when missing). Returns the line written.
pub fn append(root: &str, slug: &str, turn: Option<i64>, level: &str, doubt: &str, chose: &str, revert: &str) -> String {
    let line = format_line(slug, turn, level, doubt, chose, revert);
    let path = validate_path(root);
    let _ = std::fs::create_dir_all(path.parent().expect("has parent"));
    let current = read_file(root);
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{}{}", if current.is_empty() || current.ends_with('\n') { "" } else { "\n" }, line);
    }
    line
}

/// Number of lines in VALIDATE.md now: the mark saved in meta before a turn.
pub fn line_count(root: &str) -> usize {
    splitlines(&read_file(root)).len()
}

/// Critical decisions written after line `since_line` (1-based line numbers greater than the mark).
pub fn new_critical(root: &str, since_line: i64) -> Vec<Value> {
    parse(&read_file(root)).0.into_iter().filter(|i| i["level"] == "critical" && i["line_no"].as_i64().unwrap_or(0) > since_line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_round_trips() {
        let line = format_line("a|b", Some(3), "critical", "which  DB?", "pg | yes", "undo it");
        assert_eq!(line, "- [a/b] turn=3 level=critical which DB? | chose: pg / yes | revert: undo it");
        let p = parse_line(&line, 7).unwrap();
        assert_eq!((p["slug"].as_str(), p["turn"].as_i64(), p["level"].as_str(), p["chose"].as_str(), p["revert"].as_str()), (Some("a/b"), Some(3), Some("critical"), Some("pg / yes"), Some("undo it")));
        assert_eq!(parse_line("- [x] old style | chose: y", 1).unwrap()["level"], "important");
        assert_eq!(parse_line("- [x] no choice here", 2).unwrap()["unparsed"], true);
        assert!(parse_line("## heading", 3).is_none());
        assert_eq!(format_line("", None, "weird", "d", "c", ""), "- [general] turn=unknown level=important d | chose: c | revert: ");
    }

    #[test]
    fn append_and_critical_marks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        append(&root, "a", Some(1), "important", "d", "c", "r");
        let mark = line_count(&root) as i64;
        append(&root, "b", Some(2), "critical", "d2", "c2", "r2");
        let found = new_critical(&root, mark);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["slug"], "b");
        assert!(new_critical(&root, 0).len() == 1 && line_id("  x ") == line_id("x"));
        assert_eq!(line_id("abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }
}
