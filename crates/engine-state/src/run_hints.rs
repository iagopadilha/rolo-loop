// SPDX-License-Identifier: MIT
//! `How to run` block of a project's CLAUDE.md: the commands to run the app locally, read from the project itself.
//!
//! Deterministic (no AI): the block between RUN:BEGIN and RUN:END is generated from `Properties/launchSettings.json` (.NET) and `package.json` /
//! `vite.config.*` (front ends), and rewritten on every call. Nothing outside the markers is touched. Standard: always run with HTTPS first (the
//! `https` launch profile for .NET); HTTP only when HTTPS does not answer.
//!
//! Also holds the codebase-map helpers (`docs/codebase`, its index and the `Codebase map` block).

use crate::context_block;
use pycompat::{nt_abspath, Re};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const BEGIN: &str = "<!-- RUN:BEGIN -->";
pub const END: &str = "<!-- RUN:END -->";
const SKIPPED_DIRS: [&str; 9] = ["node_modules", ".git", "bin", "obj", "dist", "build", ".specs", ".next", "__pycache__"];
const MAX_DEPTH: usize = 4;
const HTTPS_PROFILE: &str = "https";
static RUN_LINE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?m)^[ \t\r\f\v]*(?:[-*][ \t\r\f\v]*)?`?dotnet run\b.*$"));
static VITE_PORT_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"\bport\s*:\s*(\d+)"));
static VITE_HTTPS_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"\bhttps\s*:\s*(?!false)\S"));
const FRONT_SCRIPTS: [&str; 3] = ["dev", "start:dev", "start"];

pub const CODEBASE_BEGIN: &str = "<!-- CODEBASE:BEGIN -->";
pub const CODEBASE_END: &str = "<!-- CODEBASE:END -->";
const CODEBASE_PURPOSE: [(&str, &str); 8] = [
    ("STACK.md", "languages, frameworks, versions and main dependencies"),
    ("ARCHITECTURE.md", "layers, patterns and how data flows"),
    ("CONVENTIONS.md", "naming, code style and error handling in use"),
    ("STRUCTURE.md", "folder layout and where each kind of code lives"),
    ("TESTING.md", "how tests are written and run"),
    ("INTEGRATIONS.md", "external APIs, databases, queues and authentication"),
    ("CONCERNS.md", "tech debt, risks and fragile areas"),
    ("SYSTEM.md", "how the subprojects of this monorepo talk to each other (routes, contracts, flows)"),
];
/// The only document of the root `docs/codebase` of a confirmed monorepo (never "the map").
pub const SYSTEM_DOCUMENT_NAME: &str = "SYSTEM.md";
/// Where the map is written: the project root, versioned with the code, reused by the team.
pub const MAP_FOLDER: &str = "docs/codebase";
/// Older projects: still read when `MAP_FOLDER` has no document.
pub const LEGACY_MAP_FOLDER: &str = ".specs/codebase";
/// Written by the app (not by the AI): the index and the "mapped at" marker.
pub const MAP_INDEX_NAME: &str = "README.md";
static MAPPED_AT_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?m)^Mapped at commit (\S+) on (\S+)"));

fn purpose(name: &str) -> &'static str {
    CODEBASE_PURPOSE.iter().find(|(n, _)| *n == name).map(|(_, p)| *p).unwrap_or("mapping document")
}

fn read_text(path: &Path) -> String {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// `{name: [path]}` of the files called `names`, up to `MAX_DEPTH` folders down, skipping build/dependency folders (folders walked in sorted order).
fn walk_files(root: &Path, names: &[&str]) -> Vec<(String, Vec<PathBuf>)> {
    let mut found: Vec<(String, Vec<PathBuf>)> = names.iter().map(|n| (n.to_string(), vec![])).collect();
    fn visit(folder: &Path, depth: usize, found: &mut Vec<(String, Vec<PathBuf>)>) {
        let Ok(entries) = std::fs::read_dir(folder) else { return };
        let (mut subdirs, mut files): (Vec<String>, Vec<String>) = (vec![], vec![]);
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() {
                subdirs.push(name)
            } else {
                files.push(name)
            }
        }
        for name in &files {
            if let Some(slot) = found.iter_mut().find(|(n, _)| n == name) {
                slot.1.push(folder.join(name));
            }
        }
        if depth >= MAX_DEPTH {
            return;
        }
        subdirs.retain(|d| !SKIPPED_DIRS.contains(&d.as_str()));
        subdirs.sort();
        for name in subdirs {
            visit(&folder.join(name), depth + 1, found);
        }
    }
    visit(root, 0, &mut found);
    found
}

fn relative(root: &Path, path: &Path) -> String {
    let value = path.strip_prefix(root).map(|p| p.display().to_string().replace('\\', "/")).unwrap_or_else(|_| path.display().to_string());
    if value.is_empty() {
        ".".into()
    } else {
        value
    }
}

struct DotnetEntry {
    dir: String,
    has_https: bool,
    urls: String,
}

/// One entry per launchSettings.json that sits next to a .csproj; `*.Api` first.
fn dotnet_entries(root: &Path) -> Vec<DotnetEntry> {
    let mut entries = Vec::new();
    for path in walk_files(root, &["launchSettings.json"]).remove(0).1 {
        let Some(project_dir) = path.parent().and_then(Path::parent) else { continue };
        let has_project = std::fs::read_dir(project_dir).map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().ends_with(".csproj"))).unwrap_or(false);
        if !has_project {
            continue;
        }
        let settings = read_json(&path).unwrap_or(Value::Null);
        let https = settings.get("profiles").and_then(|p| p.get(HTTPS_PROFILE)).cloned().unwrap_or(Value::Null);
        let truthy = engine_truthy(Some(&https));
        let urls = https
            .get("applicationUrl")
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                o => o.to_string(),
            })
            .unwrap_or_default()
            .replace(';', " , ");
        entries.push(DotnetEntry { dir: relative(root, project_dir), has_https: truthy, urls });
    }
    entries.sort_by_key(|e| (!e.dir.ends_with(".Api"), e.dir.clone()));
    entries
}

fn engine_truthy(value: Option<&Value>) -> bool {
    crate::notify_truthy(value)
}

struct FrontEntry {
    dir: String,
    script: String,
    port: Option<String>,
    https: bool,
}

/// One entry per package.json with a dev script (port/https from the vite config).
fn front_entries(root: &Path) -> Vec<FrontEntry> {
    let mut entries = Vec::new();
    for path in walk_files(root, &["package.json"]).remove(0).1 {
        let scripts = read_json(&path).and_then(|p| p.get("scripts").cloned()).unwrap_or(Value::Null);
        let Some(script) = FRONT_SCRIPTS.iter().find(|name| scripts.get(**name).is_some()) else { continue };
        let Some(project_dir) = path.parent() else { continue };
        let mut names: Vec<String> =
            std::fs::read_dir(project_dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        names.sort();
        let config: String = names.iter().filter(|n| n.starts_with("vite.config.")).map(|n| read_text(&project_dir.join(n))).collect();
        let port = VITE_PORT_PATTERN.search(&config).map(|m| m[1].to_string());
        entries.push(FrontEntry { dir: relative(root, project_dir), script: script.to_string(), port, https: VITE_HTTPS_PATTERN.is_match(&config) });
    }
    entries
}

fn where_(dir: &str) -> String {
    if dir == "." {
        String::new()
    } else {
        format!(" (in `{dir}`)")
    }
}

/// The block text (with markers), or `None` when the folder has neither a .NET launchSettings nor a front end.
pub fn build_block(root: &str) -> Option<String> {
    let root_path = PathBuf::from(nt_abspath(root));
    let (back, front) = (dotnet_entries(&root_path), front_entries(&root_path));
    if back.is_empty() && front.is_empty() {
        return None;
    }
    let mut lines = vec![
        BEGIN.to_string(),
        "## How to run (generated by the harness app: rewritten on every update, do not edit inside the markers)".to_string(),
        "Always run with HTTPS first. Use HTTP only when the host or port does not answer over HTTPS.".to_string(),
        String::new(),
    ];
    for entry in &back {
        if entry.has_https {
            let urls = if entry.urls.is_empty() { String::new() } else { format!(" -> {}", entry.urls) };
            lines.push(format!("- .NET back end: `dotnet run --project {} --launch-profile {HTTPS_PROFILE}`{urls}", entry.dir));
        } else {
            lines.push(format!("- .NET back end `{}`: **no `{HTTPS_PROFILE}` profile in `Properties/launchSettings.json`**; add one with an `https://localhost:<port>` applicationUrl before running (HTTP-only is the fallback, not the standard).", entry.dir));
        }
    }
    if !back.is_empty() {
        lines.push("  - Untrusted dev certificate: `dotnet dev-certs https --trust` is the user's action; the agents only warn.".to_string());
    }
    for entry in &front {
        let scheme = if entry.https { "https (set in the vite config)" } else { "http on localhost unless the project serves https" };
        let port = entry.port.as_ref().map(|p| format!(", port {p}")).unwrap_or_default();
        lines.push(format!("- Front end{}: `npm run {}`{port}, {scheme}", where_(&entry.dir), entry.script));
    }
    lines.extend(["".to_string(), "If a command above disagrees with another line of this file, this block wins.".to_string(), END.to_string(), String::new()]);
    Some(lines.join("\n"))
}

/// Text between two markers (first pair, lazily) replaced by `block`, with the line ending after the end marker consumed.
fn replace_marked(text: &str, begin: &str, end: &str, block: &str) -> String {
    let Some(start) = text.find(begin) else { return text.to_string() };
    let Some(relative_end) = text[start + begin.len()..].find(end) else { return text.to_string() };
    let mut stop = start + begin.len() + relative_end + end.len();
    if text[stop..].starts_with('\n') {
        stop += 1;
    }
    format!("{}{block}{}", &text[..start], &text[stop..])
}

/// `dotnet run` lines written outside the generated block that do not use the https profile.
pub fn conflicting_lines(text: &str) -> Vec<String> {
    let outside = match text.find(BEGIN).and_then(|s| text[s + BEGIN.len()..].find(END).map(|e| (s, s + BEGIN.len() + e + END.len()))) {
        Some((s, e)) => format!("{}{}", &text[..s], &text[e..]),
        None => text.to_string(),
    };
    RUN_LINE_PATTERN
        .find_all(&outside)
        .into_iter()
        .map(|m| m.text().to_string())
        .filter(|line| !line.contains("--launch-profile"))
        .map(|l| l.trim().to_string())
        .collect()
}

/// Writes (or rewrites) the block in `<root>/CLAUDE.md`; returns `(changed, conflicting lines)`. No block = nothing written.
pub fn write_block(root: &str) -> (bool, Vec<String>) {
    let block = build_block(root);
    let path = Path::new(root).join("CLAUDE.md");
    let existing = path.exists().then(|| read_text(&path));
    let conflicts = conflicting_lines(existing.as_deref().unwrap_or(""));
    let Some(block) = block else { return (false, conflicts) };
    let text = existing.unwrap_or_else(|| format!("# {}\n\n", pycompat::last_segment(&nt_abspath(root))));
    let updated = if text.contains(BEGIN) && text[text.find(BEGIN).unwrap_or(0)..].contains(END) {
        replace_marked(&text, BEGIN, END, &block)
    } else {
        format!("{}\n\n{block}", text.trim_end_matches('\n'))
    };
    if updated == text {
        return (false, conflicts);
    }
    if std::fs::write(&path, updated.as_bytes()).is_err() {
        return (false, conflicts);
    }
    (true, conflicts)
}

/// Names of the map documents (`.md`, top level) in `folder`, without the app's own index. `[]` when the folder is missing.
pub fn list_map_documents(folder: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(folder) else { return vec![] };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.to_lowercase().ends_with(".md") && n != MAP_INDEX_NAME)
        .collect();
    names.sort();
    names
}

/// True when `root` is the root of a confirmed monorepo (its `docs/codebase` then holds only `SYSTEM.md`). Never fails.
pub fn is_confirmed_monorepo_root(root: &str) -> bool {
    !root.is_empty() && crate::subprojects::monorepo_confirmed(root, None)
}

/// Map documents of `<root>/<relative>` for the "is the project mapped" questions: in a confirmed monorepo the root `docs/codebase` is not a
/// project map (it holds only `SYSTEM.md` and the index), so nothing is listed there.
pub fn list_root_map_documents(root: &str, relative: &str) -> Vec<String> {
    if relative == MAP_FOLDER && is_confirmed_monorepo_root(root) {
        return vec![];
    }
    list_map_documents(&relative.split('/').fold(PathBuf::from(root), |a, p| a.join(p)))
}

/// Relative folder (`docs/codebase`, else the legacy `.specs/codebase`) that holds the map and the root it was found under: `(relative, root)`.
/// Nothing found = `(MAP_FOLDER, first root)`. `docs/codebase` is always read first. The root `docs/codebase` of a confirmed monorepo is skipped.
pub fn find_map_folder(roots: &[&str]) -> (String, String) {
    for relative in [MAP_FOLDER, LEGACY_MAP_FOLDER] {
        for root in roots {
            if !root.is_empty() && !list_root_map_documents(root, relative).is_empty() {
                return (relative.to_string(), root.to_string());
            }
        }
    }
    (MAP_FOLDER.to_string(), roots.first().map(|r| r.to_string()).unwrap_or_default())
}

/// Short sha written in the index (`Mapped at commit <sha> on <date>`), or `None`.
pub fn read_mapped_commit(folder: &Path) -> Option<String> {
    let found = MAPPED_AT_PATTERN.search(&read_text(&folder.join(MAP_INDEX_NAME)))?;
    (&found[1] != "unknown").then(|| found[1].to_string())
}

/// Text of `docs/codebase/README.md`: what each document is for and when the map was made.
pub fn build_map_index(names: &[String], commit: Option<&str>, today: &str) -> String {
    let mut lines = vec![
        "# Codebase map".to_string(),
        String::new(),
        "Short documents about what this code already is, generated by the Rolo Loop so the team and the AI can reuse them.".to_string(),
        "They are a guide, NOT the source of truth: when a document and the code disagree, the code wins.".to_string(),
        "Read one only when the task needs it. If a change really alters one of them, update it in the same change.".to_string(),
        String::new(),
    ];
    for name in names {
        lines.push(format!("- [{name}]({name}): {}", purpose(name)));
    }
    lines.extend([String::new(), format!("Mapped at commit {} on {today}", commit.filter(|c| !c.is_empty()).unwrap_or("unknown")), String::new()]);
    lines.join("\n")
}

/// Creates or updates `<root>/docs/codebase/README.md` (idempotent). True when the file changed. No documents = nothing.
pub fn write_map_index(root: &str, commit: Option<&str>, today: Option<&str>) -> bool {
    let folder = MAP_FOLDER.split('/').fold(PathBuf::from(root), |a, p| a.join(p));
    let names = list_map_documents(&folder);
    if names.is_empty() {
        return false;
    }
    let today = today.map(String::from).unwrap_or_else(|| crate::loop_io::now_iso()[..10].to_string());
    let text = build_map_index(&names, commit, &today);
    let path = folder.join(MAP_INDEX_NAME);
    if path.exists() && read_text(&path) == text {
        return false;
    }
    std::fs::write(&path, text.as_bytes()).is_ok()
}

/// The `Codebase map` block (with markers) pointing at the map (`docs/codebase`, or the legacy `.specs/codebase`), or `None`.
pub fn build_codebase_block(root: &str) -> Option<String> {
    let (relative, found_root) = find_map_folder(&[root]);
    let names = list_root_map_documents(&found_root, &relative);
    if names.is_empty() {
        return None;
    }
    let mut lines = vec![
        CODEBASE_BEGIN.to_string(),
        "## Codebase map (generated by the harness app: rewritten when the map is made again)".to_string(),
        format!("Short documents about what the code already is, in `{relative}/`. Read one ONLY WHEN the task needs it (stack, architecture, structure, conventions, tests, integrations, risks), not at the start of every session:"),
        String::new(),
    ];
    for name in &names {
        lines.push(format!("- `{relative}/{name}`: {}", purpose(name)));
    }
    lines.extend([
        String::new(),
        "They are a guide, not the source of truth: if a document disagrees with the code, the code wins. If a change really alters one of them, update the document in the same change.".to_string(),
        CODEBASE_END.to_string(),
        String::new(),
    ]);
    Some(lines.join("\n"))
}

/// Removes the `CODEBASE` block from the CLAUDE.md at `path`; true when the file changed.
pub fn remove_codebase_block_from_file(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    let Ok(text) = String::from_utf8(bytes) else { return false };
    let (updated, removed) = context_block::remove_codebase_blocks(&text);
    removed && std::fs::write(path, updated.as_bytes()).is_ok()
}

/// Writes (or rewrites) the Codebase map block in `<root>/CLAUDE.md`; true when the file changed. No documents = nothing. A CLAUDE.md that has
/// the `maestro:context` block already lists the map there (one block about the map per file): the `CODEBASE` block is then not written and an
/// existing one is removed.
pub fn write_codebase_block(root: &str) -> bool {
    let path = Path::new(root).join("CLAUDE.md");
    if path.exists() && read_text(&path).contains(context_block::START_MARKER) {
        return remove_codebase_block_from_file(&path);
    }
    let Some(block) = build_codebase_block(root) else { return false };
    let text = if path.exists() { read_text(&path) } else { format!("# {}\n\n", pycompat::last_segment(&nt_abspath(root))) };
    let updated = if text.contains(CODEBASE_BEGIN) && text[text.find(CODEBASE_BEGIN).unwrap_or(0)..].contains(CODEBASE_END) {
        replace_marked(&text, CODEBASE_BEGIN, CODEBASE_END, &block)
    } else {
        format!("{}\n\n{block}", text.trim_end_matches('\n'))
    };
    if updated == text {
        return false;
    }
    std::fs::write(&path, updated.as_bytes()).is_ok()
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
    fn run_block_comes_from_the_project_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(build_block(&root.display().to_string()).is_none());
        write(root, "src/Shop.Api/Shop.Api.csproj", "");
        write(root, "src/Shop.Api/Properties/launchSettings.json", r#"{"profiles": {"https": {"applicationUrl": "https://localhost:7001;http://localhost:5001"}}}"#);
        write(root, "src/Other/Other.csproj", "");
        write(root, "src/Other/Properties/launchSettings.json", r#"{"profiles": {"http": {}}}"#);
        write(root, "web/package.json", r#"{"scripts": {"start": "x", "dev": "vite"}}"#);
        write(root, "web/vite.config.ts", "export default { server: { port: 5173, https: true } }");
        write(root, "node_modules/p/package.json", r#"{"scripts": {"dev": "x"}}"#);
        let block = build_block(&root.display().to_string()).unwrap();
        assert!(block.starts_with("<!-- RUN:BEGIN -->\n## How to run") && block.ends_with("<!-- RUN:END -->\n"));
        assert!(block.contains("- .NET back end: `dotnet run --project src/Shop.Api --launch-profile https` -> https://localhost:7001 , http://localhost:5001"));
        assert!(block.contains("- .NET back end `src/Other`: **no `https` profile"));
        assert!(block.contains("- Front end (in `web`): `npm run dev`, port 5173, https (set in the vite config)"));
        assert!(!block.contains("node_modules"));
        std::fs::write(root.join("CLAUDE.md"), "# T\n\n- `dotnet run --project x`\n- dotnet run --launch-profile https\n").unwrap();
        let (changed, conflicts) = write_block(&root.display().to_string());
        assert_eq!((changed, conflicts), (true, vec!["- `dotnet run --project x`".to_string()]));
        assert_eq!(write_block(&root.display().to_string()).0, false);
        assert!(read_text(&root.join("CLAUDE.md")).contains("<!-- RUN:END -->"));
    }

    #[test]
    fn map_index_and_codebase_block() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let r = root.display().to_string();
        assert!(!write_map_index(&r, None, None) && !write_codebase_block(&r));
        write(root, "docs/codebase/STACK.md", "x");
        write(root, "docs/codebase/NOTES.md", "x");
        assert!(write_map_index(&r, Some("abc123"), Some("2026-01-01")));
        assert!(!write_map_index(&r, Some("abc123"), Some("2026-01-01")));
        let index = read_text(&root.join("docs/codebase/README.md"));
        assert!(index.contains("- [NOTES.md](NOTES.md): mapping document") && index.ends_with("Mapped at commit abc123 on 2026-01-01\n"));
        assert_eq!(read_mapped_commit(&root.join("docs/codebase")).as_deref(), Some("abc123"));
        assert!(write_codebase_block(&r));
        assert!(!write_codebase_block(&r));
        assert!(read_text(&root.join("CLAUDE.md")).contains("- `docs/codebase/STACK.md`: languages, frameworks"));
        std::fs::write(root.join("CLAUDE.md"), format!("{}\n{}", read_text(&root.join("CLAUDE.md")), context_block::START_MARKER)).unwrap();
        assert!(write_codebase_block(&r));
        assert!(!read_text(&root.join("CLAUDE.md")).contains(CODEBASE_BEGIN));
        assert_eq!(find_map_folder(&[&r]), (MAP_FOLDER.to_string(), r.clone()));
    }
}
