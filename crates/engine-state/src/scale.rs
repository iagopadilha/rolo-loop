// SPDX-License-Identifier: MIT
//! Sizes Phase 1 (brownfield): how many `brownfield-mapper` agents to dispatch and which documents each writes.
//!
//! `harness scale <repo-root> [--json]`. Tiers: small (up to ~150 code files) -> 1 agent writes the 7 documents; medium (up to ~1200) ->
//! 3 agents; large -> 7 agents, 1 document each. No Claude: it only reads the repo and writes nothing.

use crate::classify_structure::is_skipped;
use pycompat::json::py_json_dumps_indent;
use pycompat::{nt_abspath, nt_dirname};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const CODE_EXT: [&str; 22] = [
    ".cs", ".ts", ".tsx", ".js", ".jsx", ".py", ".java", ".go", ".rb", ".php", ".sql", ".vue", ".svelte", ".kt", ".rs", ".cpp", ".c", ".h", ".razor", ".cshtml", ".scss",
    ".css",
];
pub const INVENTORY_ROOT_NAME: &str = "_root";
pub const INVENTORY_RELATIVE_DIR: [&str; 5] = [".specs", "project", "loop", "internal", "inventory"];
pub const DOCS: [&str; 7] = ["STACK", "ARCHITECTURE", "CONVENTIONS", "STRUCTURE", "TESTING", "INTEGRATIONS", "CONCERNS"];

fn is_skip(name: &str) -> bool {
    is_skipped(name) || name == "packages"
}

pub fn is_code(name: &str) -> bool {
    let lower = name.to_lowercase();
    let ext = match lower.rfind('.') {
        Some(i) if i > 0 && !lower[i..].contains(['\\', '/']) => &lower[i..],
        _ => "",
    };
    CODE_EXT.contains(&ext) || ext == ".html"
}

fn git_files(root: &str) -> Option<Vec<String>> {
    let mut git = std::process::Command::new("git");
    git.args(["-C", root, "ls-files"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let out = crate::loop_io::hidden(&mut git).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() && !text.trim().is_empty() {
        Some(
            text.lines()
                .map(String::from)
                .collect::<Vec<_>>()
                .into_iter()
                .filter(|line| {
                    let segments: Vec<String> = line.replace('\\', "/").split('/').map(String::from).collect();
                    !segments[..segments.len().saturating_sub(1)].iter().any(|s| is_skip(s))
                })
                .collect(),
        )
    } else {
        None
    }
}

fn walk(root: &Path, current: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(current) else { return };
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !is_skip(&name) {
                dirs.push(path);
            }
        } else {
            out.push(path.strip_prefix(root).map(|p| p.display().to_string()).unwrap_or(name));
        }
    }
    for d in dirs {
        walk(root, &d, out);
    }
}

pub fn list_files(root: &str) -> Vec<String> {
    if let Some(files) = git_files(root) {
        return files;
    }
    let mut found = Vec::new();
    walk(Path::new(root), Path::new(root), &mut found);
    found
}

pub fn code_files(root: &str) -> Vec<String> {
    list_files(root).into_iter().filter(|f| is_code(f)).collect()
}

fn last_commit_time(folder: &str) -> Option<i64> {
    let mut git = std::process::Command::new("git");
    git.args(["-C", folder, "log", "-1", "--format=%ct", "--", "."]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let out = crate::loop_io::hidden(&mut git).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !text.is_empty() {
        text.parse().ok()
    } else {
        None
    }
}

/// The inventory file of the folder (`internal/inventory/<name>.json` of the nearest parent with `.specs`), or `None`.
pub fn find_inventory_file(folder: &str) -> Option<PathBuf> {
    let folder = nt_abspath(folder);
    let mut current = folder.clone();
    loop {
        if Path::new(&current).join(".specs").is_dir() {
            let relative = relpath(&folder, &current).replace('\\', "/");
            let name = if relative == "." { INVENTORY_ROOT_NAME.to_string() } else { relative.replace('/', "-") };
            let mut path = PathBuf::from(&current);
            for part in INVENTORY_RELATIVE_DIR {
                path.push(part);
            }
            path.push(format!("{name}.json"));
            return if path.is_file() { Some(path) } else { None };
        }
        let parent = nt_dirname(&current);
        if parent == current {
            return None;
        }
        current = parent;
    }
}

fn relpath(path: &str, start: &str) -> String {
    let (p, s) = (nt_abspath(path), nt_abspath(start));
    if p.to_lowercase() == s.to_lowercase() {
        return ".".into();
    }
    match p.to_lowercase().strip_prefix(&format!("{}\\", s.to_lowercase())) {
        Some(rest) => p[p.len() - rest.len()..].to_string(),
        None => p,
    }
}

/// Code-file count of a fresh inventory (file newer than the folder's last commit), else `None`.
pub fn inventory_code_count(folder: &str) -> Option<i64> {
    let path = find_inventory_file(folder)?;
    let commit_time = last_commit_time(folder)?;
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64();
    if modified < commit_time as f64 {
        return None;
    }
    let data: Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    data.get("totals")?.get("files")?.as_i64()
}

/// Number of code files of the folder: from the free inventory when it is fresh, else measured here.
pub fn count_code_files(folder: &str) -> i64 {
    inventory_code_count(folder).unwrap_or_else(|| code_files(folder).len() as i64)
}

pub struct Measure {
    pub files: usize,
    pub code: i64,
    pub code_kb: i64,
    pub areas: Vec<(String, i64)>,
}

pub fn measure(root: &str) -> Measure {
    let files = list_files(root);
    let code: Vec<&String> = files.iter().filter(|f| is_code(f)).collect();
    let mut size: u64 = 0;
    for name in code.iter().take(5000) {
        if let Ok(meta) = std::fs::metadata(Path::new(root).join(name)) {
            size += meta.len();
        }
    }
    let mut tops: Vec<(String, i64)> = Vec::new();
    for name in &code {
        let parts: Vec<&str> = name.split(['\\', '/']).collect();
        let key = if parts.len() > 1 { parts[0].to_string() } else { ".".to_string() };
        match tops.iter_mut().find(|(k, _)| *k == key) {
            Some((_, n)) => *n += 1,
            None => tops.push((key, 1)),
        }
    }
    tops.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    tops.truncate(8);
    let counted = inventory_code_count(root);
    Measure { files: files.len(), code: counted.unwrap_or(code.len() as i64), code_kb: (size / 1024) as i64, areas: tops }
}

pub fn tier_of(m: &Measure) -> &'static str {
    if m.code <= 150 {
        "small"
    } else if m.code <= 1200 {
        "medium"
    } else {
        "large"
    }
}

fn groups(tier: &str) -> Vec<Vec<&'static str>> {
    match tier {
        "small" => vec![DOCS.to_vec()],
        "medium" => vec![vec!["STACK", "STRUCTURE", "CONVENTIONS"], vec!["ARCHITECTURE", "INTEGRATIONS"], vec!["TESTING", "CONCERNS"]],
        _ => DOCS.iter().map(|d| vec![*d]).collect(),
    }
}

pub const USAGE: &str = include_str!("scale_usage.txt");

pub fn run_cli(args: &[String]) -> (i32, String) {
    if args.is_empty() {
        return (2, format!("{USAGE}\n"));
    }
    let root = nt_abspath(&args[0]);
    let info = measure(&root);
    let tier = tier_of(&info);
    let plan_groups = groups(tier);
    if args.iter().any(|a| a == "--json") {
        let mut areas = Map::new();
        for (k, n) in &info.areas {
            areas.insert(k.clone(), json!(n));
        }
        let plan = json!({"tier": tier, "agents": plan_groups.len(), "groups": plan_groups,
                          "measured": {"files": info.files, "code": info.code, "code_kb": info.code_kb, "areas": Value::Object(areas)}});
        return (0, format!("{}\n", py_json_dumps_indent(&plan, false, 2)));
    }
    let mut out = format!("{tier}: {} code files ({} KB) -> {} agent(s)\n", info.code, info.code_kb, plan_groups.len());
    for (number, group) in plan_groups.iter().enumerate() {
        out.push_str(&format!("  agent {}: DOCS: {}\n", number + 1, group.join(",")));
    }
    (0, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_follow_the_code_count() {
        let m = |code| Measure { files: 0, code, code_kb: 0, areas: vec![] };
        assert_eq!(tier_of(&m(150)), "small");
        assert_eq!(tier_of(&m(151)), "medium");
        assert_eq!(tier_of(&m(1200)), "medium");
        assert_eq!(tier_of(&m(1201)), "large");
    }

    #[test]
    fn measures_a_plain_folder_skipping_vendor_dirs() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a.rs", "src/b.ts", "src/c.ts", "node_modules/x/y.js", "docs/readme.md", "web/i.html"] {
            let p = dir.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        let m = measure(&dir.path().display().to_string());
        assert_eq!(m.code, 4);
        assert_eq!(m.areas[0], ("src".to_string(), 2));
    }
}
