// SPDX-License-Identifier: MIT
//! Standards slots of a project: the `## Standards` section of a `CLAUDE.md`.
//!
//! A generic dev skill knows only the slot name; the skill that fills it is bound here, in the project:
//! one `slot: value` line per bound slot, nothing else until the next heading. An unknown slot, an empty value or a value that is not a
//! skill name is ignored and reported as a note. Writing keeps every other line of the file (and its newline style); an empty mapping
//! removes the section. In a monorepo the section lives in each subproject's `CLAUDE.md`, never in the root one.
//!
//! `harness standards <repo> [--subproject <path>] [--set slot=skill ...] [--clear slot ...] [--json]`.

use crate::subprojects::{entry_projects, load, normalize};
use pycompat::json::py_json_dumps_indent;
use pycompat::{nt_abspath, splitlines_keepends, Re};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const SLOTS: [&str; 5] = ["git", "backend", "frontend", "app-map", "observability"];
pub const SECTION_HEADING: &str = "## Standards";
const BOM: char = '\u{feff}';
static VALUE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z0-9_.-]+(:[A-Za-z0-9_.-]+)?\n?\z"));
static HEADING_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^#{1,6}\s"));
static SECTION_HEADING_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^##[ \t]+Standards[ \t]*\n?\z"));
static LINE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^([^:]+):(.*)\z"));
static FENCE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*(```|~~~)"));

/// `plugin:skill` -> `skill`, stripped and lowercased.
pub fn skill_name(value: &str) -> String {
    let name = value.trim().trim_start_matches('/');
    name.rsplit(':').next().unwrap_or("").trim().trim_start_matches('/').to_lowercase()
}

fn line_ending_of(text: &str) -> &'static str {
    match text.find('\n') {
        Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// `(start, end)` of the Standards section in a list of lines; headings inside a code fence do not count.
pub fn locate_section(lines: &[&str]) -> Option<(usize, usize)> {
    let mut start: Option<usize> = None;
    let mut fenced = false;
    for (index, line) in lines.iter().enumerate() {
        if FENCE_PATTERN.match_start(line).is_some() {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let text = line.trim_end_matches(['\r', '\n']);
        match start {
            None => {
                if SECTION_HEADING_PATTERN.match_start(text.trim_start_matches(BOM)).is_some() {
                    start = Some(index);
                }
            }
            Some(s) => {
                if HEADING_PATTERN.match_start(text).is_some() {
                    return Some((s, index));
                }
            }
        }
    }
    start.map(|s| (s, lines.len()))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    /// In file order, a repeated slot keeps its first position and its last value.
    pub bindings: Vec<(String, String)>,
    pub notes: Vec<String>,
}

impl Parsed {
    pub fn get(&self, slot: &str) -> Option<&str> {
        self.bindings.iter().find(|(s, _)| s == slot).map(|(_, v)| v.as_str())
    }
}

fn set_binding(list: &mut Vec<(String, String)>, slot: &str, value: &str) {
    if let Some(s) = list.iter_mut().find(|(k, _)| k == slot) {
        s.1 = value.to_string();
    } else {
        list.push((slot.to_string(), value.to_string()));
    }
}

pub fn parse(text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let lines = splitlines_keepends(text);
    let Some((start, end)) = locate_section(&lines) else { return parsed };
    for line in &lines[start + 1..end] {
        let content = line.trim();
        if content.is_empty() {
            continue;
        }
        let Some(found) = LINE_PATTERN.match_start(content) else {
            parsed.notes.push(format!("ignored line in the Standards section (expected `slot: skill`): {}", content.chars().take(60).collect::<String>()));
            continue;
        };
        let slot = found.group(1).unwrap_or("").trim().to_lowercase();
        let value = found.group(2).unwrap_or("").trim().to_string();
        if !SLOTS.contains(&slot.as_str()) {
            parsed.notes.push(format!("ignored unknown slot `{slot}` (slots: {})", SLOTS.join(", ")));
        } else if value.is_empty() {
            parsed.notes.push(format!("ignored slot `{slot}`: empty value"));
        } else if !VALUE_PATTERN.is_match(&value) {
            parsed.notes.push(format!("ignored slot `{slot}`: `{}` is not a skill name", value.chars().take(60).collect::<String>()));
        } else {
            set_binding(&mut parsed.bindings, &slot, &value);
        }
    }
    parsed
}

pub fn read(folder: &Path) -> Parsed {
    match std::fs::read(folder.join("CLAUDE.md")) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            parse(text.strip_prefix(BOM).unwrap_or(&text))
        }
        Err(_) => Parsed::default(),
    }
}

/// `[(slot, value)]` in SLOTS order for the known slots that have a non-empty value.
pub fn bound_items(bindings: &[(String, String)]) -> Vec<(String, String)> {
    SLOTS
        .iter()
        .filter_map(|slot| bindings.iter().find(|(s, _)| s == slot).map(|(_, v)| (slot.to_string(), v.trim().to_string())))
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

pub fn render(bindings: &[(String, String)], ending: &str) -> String {
    let items = bound_items(bindings);
    if items.is_empty() {
        return String::new();
    }
    let mut lines = vec![SECTION_HEADING.to_string()];
    lines.extend(items.iter().map(|(s, v)| format!("{s}: {v}")));
    format!("{}{ending}", lines.join(ending))
}

/// Errors (naming the slot) for an unknown slot or a value that is not a skill name; empty values pass (they clear a slot).
pub fn validate(bindings: &[(String, String)]) -> Result<(), String> {
    for (slot, value) in bindings {
        if !SLOTS.contains(&slot.as_str()) {
            return Err(format!("unknown slot: {slot}"));
        }
        let text = value.trim();
        if !text.is_empty() && !VALUE_PATTERN.is_match(text) {
            return Err(format!(
                "{slot}: `{}` is not a skill name (letters, digits, `_`, `.`, `-`, optionally `plugin:skill`)",
                text.chars().take(60).collect::<String>()
            ));
        }
    }
    Ok(())
}

/// `text` with its Standards section replaced, appended or removed; everything else untouched.
pub fn replace_section(text: &str, bindings: &[(String, String)]) -> String {
    let ending = line_ending_of(text);
    let section = render(bindings, ending);
    let lines = splitlines_keepends(text);
    let Some((start, end)) = locate_section(&lines) else {
        if section.is_empty() {
            return text.to_string();
        }
        if text.is_empty() {
            return section;
        }
        let head = if text.ends_with('\n') { text.to_string() } else { format!("{text}{ending}") };
        return format!("{head}{ending}{section}");
    };
    let mut block: Vec<&str> = lines[start..end].to_vec();
    let mut trailing: Vec<&str> = Vec::new();
    while block.last().is_some_and(|l| is_blank(l)) {
        trailing.insert(0, block.pop().expect("checked"));
    }
    if !section.is_empty() {
        if trailing.is_empty() && end < lines.len() {
            trailing = vec![ending];
        }
        return format!("{}{}{}{}", lines[..start].concat(), section, trailing.concat(), lines[end..].concat());
    }
    let mut before: Vec<&str> = lines[..start].to_vec();
    if end >= lines.len() && before.last().is_some_and(|l| is_blank(l)) {
        before.pop();
    }
    format!("{}{}", before.concat(), lines[end..].concat())
}

/// Replaces, appends or removes the Standards section of `<folder>/CLAUDE.md` (atomic write). Returns `read(folder)`.
pub fn write(folder: &Path, bindings: &[(String, String)]) -> Result<Parsed, String> {
    validate(bindings)?;
    let path = folder.join("CLAUDE.md");
    let text: Option<String> = match std::fs::read(&path) {
        Ok(b) => Some(String::from_utf8(b).map_err(|e| e.to_string())?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let updated = replace_section(text.as_deref().unwrap_or(""), bindings);
    if text.is_none() && updated.is_empty() {
        return Ok(read(folder));
    }
    if text.as_deref() != Some(updated.as_str()) {
        let temp = PathBuf::from(format!("{}.tmp-{}", path.display(), std::process::id()));
        std::fs::write(&temp, updated.as_bytes()).map_err(|e| e.to_string())?;
        if let Err(e) = std::fs::rename(&temp, &path) {
            let _ = std::fs::remove_file(&temp);
            return Err(e.to_string());
        }
    }
    Ok(read(folder))
}

/// `[(folder, bindings)]` for the root and each non-skipped SUBPROJECTS.json subproject.
pub fn project_bindings(project_root: &str) -> Vec<(String, Vec<(String, String)>)> {
    if project_root.is_empty() {
        return vec![];
    }
    let root = nt_abspath(project_root);
    let listed = entry_projects(load(&root, None).0.as_ref());
    let mut folders: Vec<String> = if listed.is_empty() { vec![".".into()] } else { vec![] };
    for project in &listed {
        let relative = normalize(project.get("path").and_then(Value::as_str).unwrap_or(""));
        if !relative.is_empty() && project.get("skip") != Some(&Value::Bool(true)) && !folders.contains(&relative) {
            folders.push(relative);
        }
    }
    folders
        .into_iter()
        .map(|relative| {
            let folder = if relative == "." { PathBuf::from(&root) } else { relative.split('/').fold(PathBuf::from(&root), |p, part| p.join(part)) };
            (relative, read(&folder).bindings)
        })
        .collect()
}

fn root_lists_subprojects(root: &str) -> bool {
    !entry_projects(load(&nt_abspath(root), None).0.as_ref()).is_empty()
}

fn emit(json_out: bool, folder: &str, result: &Parsed, projects: Option<&Vec<(String, Vec<(String, String)>)>>) -> String {
    if json_out {
        let mut bindings = Map::new();
        for (k, v) in &result.bindings {
            bindings.insert(k.clone(), json!(v));
        }
        let mut payload = Map::new();
        payload.insert("folder".into(), json!(folder));
        payload.insert("bindings".into(), Value::Object(bindings));
        payload.insert("notes".into(), json!(result.notes));
        payload.insert("slots".into(), json!(SLOTS));
        if let Some(p) = projects {
            let list: Vec<Value> = p
                .iter()
                .map(|(f, b)| {
                    let mut m = Map::new();
                    for (k, v) in b {
                        m.insert(k.clone(), json!(v));
                    }
                    json!({"folder": f, "bindings": Value::Object(m)})
                })
                .collect();
            payload.insert("projects".into(), Value::Array(list));
        }
        return format!("{}\n", py_json_dumps_indent(&Value::Object(payload), false, 2));
    }
    let mut out = format!("{folder}\n");
    for slot in SLOTS {
        out.push_str(&format!("  {:<13} {}\n", slot, result.get(slot).unwrap_or("-")));
    }
    for note in &result.notes {
        out.push_str(&format!("  note: {note}\n"));
    }
    for (folder, bindings) in projects.into_iter().flatten() {
        let bound = bound_items(bindings);
        let text = if bound.is_empty() { "(no slots bound)".to_string() } else { bound.iter().map(|(s, v)| format!("{s}={v}")).collect::<Vec<_>>().join(", ") };
        out.push_str(&format!("  {folder}: {text}\n"));
    }
    out
}

fn parse_assignments(items: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut assignments: Vec<(String, String)> = Vec::new();
    for item in items {
        let (slot, sep, value) = pycompat::partition(item, "=");
        let slot = slot.trim().to_lowercase();
        if !sep || !SLOTS.contains(&slot.as_str()) {
            return Err(format!("--set expects slot=skill with slot in {} (got `{item}`)", SLOTS.join(", ")));
        }
        set_binding(&mut assignments, &slot, value.trim());
    }
    Ok(assignments)
}

pub fn run_cli(args: &[String]) -> (i32, String, String) {
    let mut repo: Option<String> = None;
    let (mut subproject, mut json_out) = (String::new(), false);
    let (mut sets, mut clears): (Vec<String>, Vec<String>) = (vec![], vec![]);
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json_out = true;
                index += 1;
            }
            "--subproject" if index + 1 < args.len() => {
                subproject = args[index + 1].clone();
                index += 2;
            }
            flag @ ("--set" | "--clear") => {
                index += 1;
                while index < args.len() && !args[index].starts_with("--") {
                    if flag == "--set" {
                        sets.push(args[index].clone())
                    } else {
                        clears.push(args[index].clone())
                    }
                    index += 1;
                }
            }
            other => {
                if repo.is_none() {
                    repo = Some(other.to_string());
                }
                index += 1;
            }
        }
    }
    let Some(repo) = repo else {
        return (2, String::new(), "usage: harness.py standards [-h] [--subproject SUBPROJECT] [--set slot=skill [slot=skill ...]] [--clear slot [slot ...]] [--json] repo\nharness.py standards: error: the following arguments are required: repo\n".into());
    };
    let root = nt_abspath(&repo);
    if !Path::new(&root).is_dir() {
        return (2, String::new(), format!("repository folder does not exist: {repo}\n"));
    }
    let relative = normalize(&subproject);
    let folder = if relative.is_empty() { PathBuf::from(&root) } else { relative.split('/').fold(PathBuf::from(&root), |p, part| p.join(part)) };
    if !folder.is_dir() {
        return (2, String::new(), format!("subproject folder does not exist: {subproject}\n"));
    }
    let outcome = (|| -> Result<(Parsed, bool), String> {
        let assignments = parse_assignments(&sets)?;
        let cleared: Vec<String> = clears.iter().map(|s| s.trim().to_lowercase()).collect();
        let unknown: Vec<&String> = cleared.iter().filter(|s| !SLOTS.contains(&s.as_str())).collect();
        if !unknown.is_empty() {
            return Err(format!("--clear: unknown slot {} (slots: {})", unknown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "), SLOTS.join(", ")));
        }
        let editing = !assignments.is_empty() || !cleared.is_empty();
        if editing && relative.is_empty() && root_lists_subprojects(&root) {
            return Err("this repository lists subprojects: the section lives in each subproject's CLAUDE.md (use --subproject <path>)".into());
        }
        let mut current = read(&folder);
        if editing {
            let mut bindings = current.bindings.clone();
            for (k, v) in &assignments {
                set_binding(&mut bindings, k, v);
            }
            bindings.retain(|(k, _)| !cleared.contains(k));
            current = write(&folder, &bindings)?;
        }
        Ok((current, editing))
    })();
    match outcome {
        Err(message) => (2, String::new(), format!("{message}\n")),
        Ok((current, editing)) => {
            let projects = if relative.is_empty() && !editing { Some(project_bindings(&root)) } else { None };
            (0, emit(json_out, &folder.display().to_string(), &current, projects.as_ref()), String::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bindings_and_notes() {
        let p = parse("# T\n\n## Standards\ngit: acme-git\nbackend: acme:backend\nweird line\nnope: x\nfrontend:\nobservability: bad name!\n\n## Next\nx: y\n");
        assert_eq!(p.bindings, vec![("git".into(), "acme-git".into()), ("backend".into(), "acme:backend".into())]);
        assert_eq!(p.notes.len(), 4);
        assert!(p.notes[0].starts_with("ignored line in the Standards section"));
    }

    #[test]
    fn fenced_headings_do_not_count() {
        assert!(parse("```\n## Standards\ngit: a\n```\n").bindings.is_empty());
    }

    #[test]
    fn replace_keeps_the_rest_and_newline_style() {
        let text = "# T\r\n\r\n## Standards\r\ngit: old\r\n\r\n## Other\r\ntext\r\n";
        let out = replace_section(text, &[("git".into(), "new".into()), ("backend".into(), "b".into())]);
        assert_eq!(out, "# T\r\n\r\n## Standards\r\ngit: new\r\nbackend: b\r\n\r\n## Other\r\ntext\r\n");
        assert_eq!(replace_section(text, &[]), "# T\r\n\r\n## Other\r\ntext\r\n");
        assert_eq!(replace_section("# T\n", &[("git".into(), "g".into())]), "# T\n\n## Standards\ngit: g\n");
        assert_eq!(replace_section("", &[]), "");
    }

    #[test]
    fn write_validates_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write(dir.path(), &[("nope".into(), "x".into())]).is_err());
        assert!(write(dir.path(), &[("git".into(), "bad name".into())]).is_err());
        let r = write(dir.path(), &[("git".into(), "acme-git".into())]).unwrap();
        assert_eq!(r.get("git"), Some("acme-git"));
        let r = write(dir.path(), &[]).unwrap();
        assert!(r.bindings.is_empty());
        assert_eq!(std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap(), "");
        assert_eq!(skill_name("Plugin:/Skill"), "skill");
    }
}
