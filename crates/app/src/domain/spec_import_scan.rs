// SPDX-License-Identifier: MIT
//! Scan of a spec feature folder for the planning import: which `.md` files are read (recursive, depth 3), the size and count limits, what each
//! file holds (ids, sections) and the warnings the Preview shows. Pure file reading and regex; nothing is written and no AI is called. A folder
//! that is not a feature folder is an `InvalidRequest` (400); a folder over the limits comes back with `refused` filled (the Preview shows it,
//! the import raises it).

use crate::domain::errors::{AppError, Result};
use pycompat::{splitlines, Re};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const MAIN_SPEC_NAME: &str = "spec.md";
const MARKDOWN_EXTENSION: &str = ".md";
pub const MAX_FILE_BYTES: usize = 400 * 1024;
const MAX_FILES: usize = 10; // only the older top-level collect_from_folder
const SKIPPED_FOLDER_NAMES: [&str; 1] = ["node_modules"];
pub const STATUS_READ: &str = "read";
const STATUS_SKIPPED: &str = "skipped";
pub const CORE_SECTIONS: [&str; 5] = ["Goals", "Out of Scope", "Edge Cases", "Success Criteria", "Requirement Traceability"];
const OPEN_QUESTION_HEADINGS: [&str; 2] = ["open questions", "perguntas abertas"];

static EMPTY_ANSWER: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^(none|no|nenhuma?|n/?a|-)\z"));
static HEADING_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(#{1,6})\s+(.*?)\s*#*\s*\z"));
static REQUIREMENT_ID: LazyLock<Re> = LazyLock::new(|| Re::new(r"\b[A-Z]+-\d+\b"));
static REQUIREMENT_RANGE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\b([A-Z]+)-(\d+)\.\.(\d+)\b"));
static DECISION_ID: LazyLock<Re> = LazyLock::new(|| Re::new(r"\bD-\d+\b"));
static DECISION_FULL: LazyLock<Re> = LazyLock::new(|| Re::new(r"^D-\d+\z"));
static TASK_ID_PREFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"^T-\d+\z"));
static TASK_HEADING: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?m)^#{2,6}\s+(T\d+)\b"));
static LIST_ITEM: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*(?:[-*]|\d+[.)])\s+\S"));
static TABLE_SEPARATOR: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*\z"));
static CONFORMANCE_LINE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?m)^\W*Conform(?:ance|idade)\W*:\W*(STANDARD|LEGACY|PADRAO|LEGADO)\b"));
static COVERAGE_LINE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?m)^.*\bCoverage\b.*$"));
static UNMAPPED_COUNT: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(\d+)\s+unmapped"));
static ZERO_MAPPED: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?<![\w.])0\s+mapped"));
static ID_SPLIT: LazyLock<Re> = LazyLock::new(|| Re::new(r"(\d+)"));
static SENTENCE_END: LazyLock<Re> = LazyLock::new(|| Re::new(r"\.(?:\s|$)"));

/// The limits of a scan.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub depth: usize,
    pub files: usize,
    pub file_bytes: usize,
    pub total_bytes: usize,
}

pub const DEFAULT_LIMITS: Limits = Limits { depth: 3, files: 30, file_bytes: MAX_FILE_BYTES, total_bytes: 2 * 1024 * 1024 };

fn expand_user(path: &str) -> String {
    if path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
        let home = cfg::Env::process().home_dir();
        return format!("{}{}", home.display(), &path[1..]);
    }
    path.to_string()
}

// ------------------------------------------------ scan

/// `{root, files:[{relative_path, size, sha256, status, reason, text}], counts:{files, bytes, skipped}, refused}`. `text` is only on read files.
/// `refused` is null or `{reason, counts}`: spec.md missing at the root, a read file empty or over the per-file limit, or totals over the limits.
pub fn scan_folder(path: &str, limits: &Limits) -> Result<Value> {
    let path = expand_user(path);
    let p = Path::new(&path);
    if p.is_file() {
        return Err(AppError::invalid_request("pick the feature folder, not a file: the folder that has spec.md"));
    }
    if !p.is_dir() {
        return Err(AppError::invalid_request(format!("folder not found: {path}")));
    }
    let mut files: Vec<Value> = Vec::new();
    walk_markdown_folder(p, "", 0, limits, &mut files)?;
    let read_files: Vec<&Value> = files.iter().filter(|e| e["status"] == STATUS_READ).collect();
    let bytes: u64 = read_files.iter().map(|e| e["size"].as_u64().unwrap_or(0)).sum();
    let counts = json!({"files": read_files.len(), "bytes": bytes, "skipped": files.len() - read_files.len()});
    let refused = find_refusal(&files, &read_files, &counts, limits);
    Ok(json!({"root": path, "files": files, "counts": counts, "refused": refused}))
}

fn walk_markdown_folder(root: &Path, relative_folder: &str, depth: usize, limits: &Limits, files: &mut Vec<Value>) -> Result<()> {
    let folder = if relative_folder.is_empty() { root.to_path_buf() } else { relative_folder.split('/').fold(root.to_path_buf(), |a, p| a.join(p)) };
    let mut names: Vec<String> = std::fs::read_dir(&folder)
        .map_err(|e| AppError::invalid_request(format!("could not read {}: {e}", folder.display())))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let relative = if relative_folder.is_empty() { name.clone() } else { format!("{relative_folder}/{name}") };
        let full: PathBuf = folder.join(&name);
        let meta = std::fs::symlink_metadata(&full);
        if meta.as_ref().is_ok_and(|m| m.file_type().is_symlink()) {
            files.push(skipped_entry(&relative, &full, "symbolic link"));
        } else if full.is_dir() {
            if name.starts_with('.') || SKIPPED_FOLDER_NAMES.contains(&name.as_str()) {
                continue;
            }
            if depth >= limits.depth {
                files.push(skipped_entry(&format!("{relative}/"), &full, &format!("deeper than {} levels", limits.depth)));
                continue;
            }
            walk_markdown_folder(root, &relative, depth + 1, limits, files)?;
        } else if !name.to_lowercase().ends_with(MARKDOWN_EXTENSION) {
            files.push(skipped_entry(&relative, &full, "not .md"));
        } else {
            files.push(read_entry(&relative, &full, limits));
        }
    }
    Ok(())
}

fn skipped_entry(relative: &str, full: &Path, reason: &str) -> Value {
    let is_plain_file = std::fs::symlink_metadata(full).is_ok_and(|m| m.is_file());
    let size = if is_plain_file { std::fs::metadata(full).map(|m| m.len()).unwrap_or(0) } else { 0 };
    json!({"relative_path": relative, "size": size, "sha256": "", "status": STATUS_SKIPPED, "reason": reason})
}

fn read_entry(relative: &str, full: &Path, limits: &Limits) -> Value {
    use std::io::Read;
    let mut raw = Vec::new();
    let read = std::fs::File::open(full).and_then(|f| f.take(limits.file_bytes as u64 + 1).read_to_end(&mut raw));
    if let Err(failure) = read {
        return skipped_entry(relative, full, &format!("could not be read: {failure}"));
    }
    let size = std::fs::metadata(full).map(|m| m.len()).unwrap_or(0);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let reason = file_content_problem(relative, raw.len(), &text, limits.file_bytes);
    if !reason.is_empty() {
        return json!({"relative_path": relative, "size": size, "sha256": "", "status": STATUS_SKIPPED, "reason": reason, "refusal": reason});
    }
    json!({"relative_path": relative, "size": size, "sha256": Sha256::digest(&raw).iter().map(|b| format!("{b:02x}")).collect::<String>(), "status": STATUS_READ, "reason": "", "text": text})
}

/// The refusal reason of a file (too large, empty) or empty text.
pub fn file_content_problem(label: &str, raw_len: usize, text: &str, file_bytes: usize) -> String {
    if raw_len > file_bytes {
        return format!("{label} is larger than {} KB: split it or import a smaller file", file_bytes / 1024);
    }
    if text.trim().is_empty() {
        return format!("{label} is empty");
    }
    String::new()
}

fn find_refusal(files: &[Value], read_files: &[&Value], counts: &Value, limits: &Limits) -> Value {
    let mut reason = String::new();
    if !files.iter().any(|e| e["relative_path"] == MAIN_SPEC_NAME) {
        reason = "the folder has no spec.md: pick the folder of a spec feature (spec.md, design.md, tasks.md)".into();
    }
    for entry in files {
        if reason.is_empty() {
            reason = entry.get("refusal").and_then(Value::as_str).unwrap_or("").to_string();
        }
    }
    let total = counts["bytes"].as_u64().unwrap_or(0) as usize;
    if reason.is_empty() && read_files.len() > limits.files {
        reason = format!("the folder has {} .md files: the limit is {}", read_files.len(), limits.files);
    }
    if reason.is_empty() && total > limits.total_bytes {
        reason = format!("the .md files add up to {} KB: the limit is {} KB", total / 1024, limits.total_bytes / 1024);
    }
    if reason.is_empty() {
        Value::Null
    } else {
        json!({"reason": reason, "counts": counts})
    }
}

// ------------------------------------------------ single-file helpers (kept for the older callers)

/// `([(file name, text)], default import name = the folder name)`: the top-level `.md` files of a feature folder.
pub fn collect_from_folder(path: &str) -> Result<(Vec<(String, String)>, String)> {
    let path = expand_user(path);
    let p = Path::new(&path);
    if p.is_file() {
        return Err(AppError::invalid_request("pick the feature folder, not a file: the folder that has spec.md"));
    }
    if !p.is_dir() {
        return Err(AppError::invalid_request(format!("folder not found: {path}")));
    }
    let mut names: Vec<String> = std::fs::read_dir(p)
        .map_err(|e| AppError::invalid_request(e.to_string()))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.to_lowercase().ends_with(MARKDOWN_EXTENSION) && p.join(n).is_file())
        .collect();
    names.sort();
    if !names.iter().any(|n| n == MAIN_SPEC_NAME) {
        return Err(AppError::invalid_request("the folder has no spec.md: pick the folder of a spec feature (spec.md, design.md, tasks.md)"));
    }
    if names.len() > MAX_FILES {
        return Err(AppError::invalid_request(format!("the folder has {} .md files: the limit is {MAX_FILES}", names.len())));
    }
    let mut read = Vec::new();
    for name in names {
        let text = read_markdown(&p.join(&name))?;
        read.push((name, text));
    }
    Ok((read, pycompat::last_segment(&pycompat::nt_normpath(&path)).to_string()))
}

pub fn read_markdown(path: &Path) -> Result<String> {
    use std::io::Read;
    let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !label.to_lowercase().ends_with(MARKDOWN_EXTENSION) {
        return Err(AppError::invalid_request(format!("{label} is not a Markdown file (.md): only Markdown specs are imported")));
    }
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut raw))
        .map_err(|e| AppError::invalid_request(format!("could not read {label}: {e}")))?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let reason = file_content_problem(&label, raw.len(), &text, MAX_FILE_BYTES);
    if !reason.is_empty() {
        return Err(AppError::invalid_request(reason));
    }
    Ok(text)
}

// ------------------------------------------------ detect

/// What the read files hold, by regex and headings only. `files`: `[{relative_path, text}]`.
pub fn detect_items(files: &[Value]) -> Value {
    let texts: Vec<(String, String)> =
        files.iter().map(|e| (e["relative_path"].as_str().unwrap_or("").to_string(), e.get("text").and_then(Value::as_str).unwrap_or("").to_string())).collect();
    let joined = texts.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
    let mut sections: Vec<(String, bool)> = CORE_SECTIONS.iter().map(|n| (n.to_string(), false)).collect();
    let (mut out_of_scope_rows, mut edge_cases, mut success_criteria) = (0usize, 0usize, 0usize);
    for (_, text) in &texts {
        let parsed = split_sections(text);
        for (name, present) in sections.iter_mut() {
            *present = *present || parsed.iter().any(|(title, _)| heading_names(title, name));
        }
        for (title, body) in &parsed {
            if heading_names(title, "Out of Scope") {
                out_of_scope_rows += count_table_rows(body);
            } else if heading_names(title, "Edge Cases") {
                edge_cases += count_list_items(body);
            } else if heading_names(title, "Success Criteria") {
                success_criteria += count_list_items(body);
            }
        }
    }
    let mut tasks: Vec<String> = Vec::new();
    for (relative_path, text) in &texts {
        if pycompat::last_segment(relative_path).to_lowercase() == "tasks.md" {
            tasks.extend(TASK_HEADING.find_all(text).iter().filter_map(|m| m.group(1).map(String::from)));
        }
    }
    let decisions: std::collections::BTreeSet<String> = DECISION_ID.find_all(&joined).iter().map(|m| m.group(0).unwrap_or("").to_string()).collect();
    let task_set: std::collections::BTreeSet<String> = tasks.into_iter().collect();
    let section_map: Map<String, Value> = sections.into_iter().map(|(n, p)| (n, json!(p))).collect();
    json!({"requirements": find_requirement_ids(&joined), "decisions": sort_ids(decisions.into_iter().collect()), "tasks": sort_ids(task_set.into_iter().collect()),
           "sections": section_map, "out_of_scope_rows": out_of_scope_rows, "edge_cases": edge_cases, "success_criteria": success_criteria})
}

pub fn find_requirement_ids(text: &str) -> Vec<String> {
    let mut found: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for m in REQUIREMENT_RANGE.find_all(text) {
        let (prefix, first_text, last_text) = (m.group(1).unwrap_or(""), m.group(2).unwrap_or(""), m.group(3).unwrap_or(""));
        let (Ok(first), Ok(last)) = (first_text.parse::<u64>(), last_text.parse::<u64>()) else { continue };
        if prefix != "D" && prefix != "T" && first <= last && last - first < 1000 {
            let width = first_text.len();
            for number in first..=last {
                found.insert(format!("{prefix}-{number:0>width$}"));
            }
        }
    }
    for m in REQUIREMENT_ID.find_all(text) {
        let identifier = m.group(0).unwrap_or("");
        if DECISION_FULL.match_start(identifier).is_none() && TASK_ID_PREFIX.match_start(identifier).is_none() {
            found.insert(identifier.to_string());
        }
    }
    sort_ids(found.into_iter().collect())
}

/// Natural order: digits compare as numbers, the rest as text.
pub fn sort_ids(mut identifiers: Vec<String>) -> Vec<String> {
    fn key(identifier: &str) -> Vec<(u8, u64, String)> {
        let mut parts = Vec::new();
        let mut last = 0;
        let push = |chunk: &str, parts: &mut Vec<(u8, u64, String)>| {
            if chunk.is_empty() {
                parts.push((1, 0, String::new()));
            } else if chunk.chars().all(|c| c.is_ascii_digit()) {
                parts.push((0, chunk.parse().unwrap_or(0), String::new()));
            } else {
                parts.push((1, 0, chunk.to_string()));
            }
        };
        for m in ID_SPLIT.find_all(identifier) {
            push(&identifier[last..m.start], &mut parts);
            push(&identifier[m.start..m.end], &mut parts);
            last = m.end;
        }
        push(&identifier[last..], &mut parts);
        parts
    }
    identifiers.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        // Python compares int with str as an error; the alternating split always puts text at even positions and digits at odd ones
        ka.cmp(&kb)
    });
    identifiers
}

/// `[(heading title, body text of the section including its sub-sections)]` for every heading of the document.
pub fn split_sections(text: &str) -> Vec<(String, String)> {
    let lines = splitlines(text);
    let mut headings: Vec<(usize, usize, String)> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some(m) = HEADING_LINE.match_start(line) {
            headings.push((index, m.group(1).unwrap_or("").len(), m.group(2).unwrap_or("").to_string()));
        }
    }
    let mut sections = Vec::new();
    for (position, (index, level, title)) in headings.iter().enumerate() {
        let end = headings[position + 1..].iter().find(|(_, next_level, _)| next_level <= level).map(|(i, _, _)| *i).unwrap_or(lines.len());
        sections.push((title.clone(), lines[index + 1..end].join("\n")));
    }
    sections
}

fn heading_names(title: &str, name: &str) -> bool {
    title.to_lowercase().contains(&name.to_lowercase())
}

fn count_table_rows(body: &str) -> usize {
    let rows: Vec<&str> = splitlines(body).into_iter().filter(|l| l.trim().starts_with('|')).collect();
    if rows.iter().any(|l| TABLE_SEPARATOR.match_start(l).is_some()) {
        return rows.iter().filter(|l| TABLE_SEPARATOR.match_start(l).is_none()).count().saturating_sub(1);
    }
    rows.len()
}

fn count_list_items(body: &str) -> usize {
    splitlines(body).into_iter().filter(|l| LIST_ITEM.match_start(l).is_some()).count()
}

// ------------------------------------------------ warnings

/// The Preview warnings (strings).
pub fn build_warnings(scan: &Value, items: &Value, project_conformance: &str) -> Vec<String> {
    let texts: Vec<String> = scan
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| e["status"] == STATUS_READ)
        .map(|e| e.get("text").and_then(Value::as_str).unwrap_or("").to_string())
        .collect();
    let joined = texts.join("\n");
    let mut warnings: Vec<String> = CORE_SECTIONS
        .iter()
        .filter(|n| !items["sections"].get(**n).is_some_and(|v| engine_state::notify_truthy(Some(v))))
        .map(|n| format!("missing section: {n}"))
        .collect();
    if has_open_questions(&texts) {
        warnings.push("open questions are not empty: the AI will carry them to the ROADMAP".into());
    }
    let unmapped: u64 = COVERAGE_LINE
        .find_all(&joined)
        .iter()
        .flat_map(|l| UNMAPPED_COUNT.find_all(l.group(0).unwrap_or("")).into_iter().filter_map(|m| m.group(1).and_then(|n| n.parse::<u64>().ok())).collect::<Vec<_>>())
        .sum();
    if unmapped > 0 {
        warnings.push(format!("traceability reports {unmapped} unmapped requirements"));
    }
    let source = find_conformance(&joined);
    if !source.is_empty() && !project_conformance.is_empty() && source != project_conformance {
        warnings.push(format!("the spec says Conformance {source} but the project is {project_conformance}"));
    }
    if items["requirements"].as_array().is_none_or(|r| r.is_empty()) {
        warnings.push("no IDs: coverage by section only".into());
    }
    if COVERAGE_LINE.find_all(&joined).iter().any(|l| ZERO_MAPPED.search(l.group(0).unwrap_or("")).is_some()) {
        warnings.push("Coverage: 0 mapped".into());
    }
    warnings
}

fn has_open_questions(texts: &[String]) -> bool {
    for text in texts {
        for (title, body) in split_sections(text) {
            if OPEN_QUESTION_HEADINGS.iter().any(|h| heading_names(&title, h)) {
                let lines: Vec<&str> = splitlines(&body).into_iter().map(str::trim).filter(|l| !l.is_empty() && l.chars().any(|c| !"-*_ ".contains(c))).collect();
                let joined = lines.join(" ");
                let first_sentence = SENTENCE_END.split(&joined).into_iter().next().unwrap_or_default().trim().to_string();
                if !lines.is_empty() && EMPTY_ANSWER.match_start(&first_sentence).is_none() {
                    return true;
                }
            }
        }
    }
    false
}

pub fn find_conformance(text: &str) -> String {
    match CONFORMANCE_LINE.search(text) {
        None => String::new(),
        Some(m) => match m.group(1).unwrap_or("").to_uppercase().as_str() {
            "PADRAO" => "STANDARD".into(),
            "LEGADO" => "LEGACY".into(),
            other => other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn scan_reads_markdown_and_flags_what_it_skips() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "spec.md", "# Spec\n## Goals\n- one\n## Out of Scope\n| A | B |\n|---|---|\n| x | y |\n## Edge Cases\n- e1\n- e2\n## Success Criteria\n1. ok\nREQ-01..03 and D-1 and T-2\n");
        write(dir.path(), "tasks.md", "## T1 first\n### T2 second\n");
        write(dir.path(), "notes.txt", "x");
        write(dir.path(), "node_modules/skip.md", "x");
        write(dir.path(), "a/b/c/d/deep.md", "x");
        write(dir.path(), "empty.md", "  \n");
        let scan = scan_folder(&dir.path().display().to_string(), &DEFAULT_LIMITS).unwrap();
        let by_path = |p: &str| scan["files"].as_array().unwrap().iter().find(|e| e["relative_path"] == p).cloned().unwrap();
        assert_eq!(by_path("notes.txt")["reason"], "not .md");
        assert_eq!(by_path("a/b/c/d/")["reason"], "deeper than 3 levels");
        assert_eq!(by_path("spec.md")["status"], "read");
        assert_eq!(scan["refused"]["reason"], "empty.md is empty");
        let items = detect_items(&scan["files"].as_array().unwrap().iter().filter(|e| e["status"] == "read").cloned().collect::<Vec<_>>());
        assert_eq!(items["requirements"], json!(["REQ-01", "REQ-02", "REQ-03"]));
        assert_eq!((items["decisions"].clone(), items["tasks"].clone()), (json!(["D-1"]), json!(["T1", "T2"])));
        assert_eq!((items["out_of_scope_rows"].clone(), items["edge_cases"].clone(), items["success_criteria"].clone()), (json!(1), json!(2), json!(1)));
        assert_eq!(items["sections"]["Goals"], json!(true));
        assert_eq!(items["sections"]["Requirement Traceability"], json!(false));
        assert!(scan_folder(&dir.path().join("spec.md").display().to_string(), &DEFAULT_LIMITS).is_err());
    }

    #[test]
    fn missing_spec_is_a_refusal_and_warnings_follow_the_content() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "design.md", "# Design\n## Open Questions\nWhich DB?\nCoverage: 0 mapped, 3 unmapped\nConformance: LEGADO\n");
        let scan = scan_folder(&dir.path().display().to_string(), &DEFAULT_LIMITS).unwrap();
        assert_eq!(scan["refused"]["reason"], "the folder has no spec.md: pick the folder of a spec feature (spec.md, design.md, tasks.md)");
        let read: Vec<Value> = scan["files"].as_array().unwrap().iter().filter(|e| e["status"] == "read").cloned().collect();
        let items = detect_items(&read);
        let warnings = build_warnings(&scan, &items, "STANDARD");
        assert!(warnings.contains(&"missing section: Goals".to_string()));
        assert!(warnings.contains(&"open questions are not empty: the AI will carry them to the ROADMAP".to_string()));
        assert!(warnings.contains(&"traceability reports 3 unmapped requirements".to_string()));
        assert!(warnings.contains(&"the spec says Conformance LEGACY but the project is STANDARD".to_string()));
        assert!(warnings.contains(&"no IDs: coverage by section only".to_string()) && warnings.contains(&"Coverage: 0 mapped".to_string()));
        assert_eq!(find_conformance("x"), "");
        assert_eq!(sort_ids(vec!["R-10".into(), "R-2".into(), "A-1".into()]), vec!["A-1", "R-2", "R-10"]);
        assert!(collect_from_folder(&dir.path().display().to_string()).is_err());
    }
}
