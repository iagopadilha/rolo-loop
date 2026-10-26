// SPDX-License-Identifier: MIT
//! Subprojects of a monorepo: detection (no AI), the stored list and the owner of a path.
//!
//! `detect` reads the folder layout only (rules DET-01..DET-05) and writes nothing. `show` prints the stored entry of
//! `.specs/project/SUBPROJECTS.json` (keyed by repo name, default = the root folder name). `for-paths` prints one line per touched
//! non-skipped subproject, and prints NOTHING for a single project or a list that is not confirmed.

use crate::classify_structure::is_skipped;
use pycompat::json::{py_json_dumps, py_json_dumps_indent};
use pycompat::{fnmatchcase, nt_abspath, nt_join, Re};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;

pub const SUBPROJECTS_RELATIVE_PATH: [&str; 3] = [".specs", "project", "SUBPROJECTS.json"];
pub const SYSTEM_DOCUMENT: &str = "docs/codebase/SYSTEM.md";
const PROJECT_FILE_PATTERNS: [&str; 10] = ["*.sln", "*.slnx", "*.csproj", "package.json", "pyproject.toml", "go.mod", "pom.xml", "Cargo.toml", "composer.json", "Gemfile"];
const CONTAINER_NAMES: [&str; 4] = ["apps", "packages", "services", "src"];
const PROJECT_FILE_DEPTH: i32 = 2;
const STACK_BY_FILE: [(&str, &str); 6] = [("pyproject.toml", "python"), ("go.mod", "go"), ("pom.xml", "java"), ("Cargo.toml", "rust"), ("composer.json", "php"), ("Gemfile", "ruby")];
const LOOP_WORKTREE_SUFFIX: &str = "-worktree-loop";
static CONFORMANCE_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?m)^Conformance:\s*(STANDARD|LEGACY)\b"));

pub fn normalize(path: &str) -> String {
    path.replace('\\', "/").trim().trim_matches('/').to_string()
}

fn skill_by_stack(stack: &str) -> &'static str {
    match stack {
        "dotnet" | "nestjs" => "backend-dev",
        "react" => "frontend-dev",
        _ => "",
    }
}

fn is_skipped_folder(name: &str) -> bool {
    is_skipped(name) || name == "docs" || name.starts_with('.')
}

fn subfolders(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .map(|r| r.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Glob in one folder (names compared case-insensitively on Windows, like `glob.glob`).
fn glob_in(folder: &Path, pattern: &str) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(folder)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    if cfg!(windows) {
                        fnmatchcase(&n.to_lowercase(), &pattern.to_lowercase())
                    } else {
                        fnmatchcase(n, pattern)
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

fn project_files_in(folder: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for pattern in PROJECT_FILE_PATTERNS {
        for name in glob_in(folder, pattern) {
            let full = folder.join(&name);
            // glob.glob matches files and folders alike
            found.push(full.display().to_string());
        }
    }
    found.sort();
    found
}

fn project_files_within(folder: &Path, depth: i32) -> Vec<String> {
    let mut found = project_files_in(folder);
    if depth > 0 {
        for name in subfolders(folder) {
            if !is_skipped_folder(&name) {
                found.extend(project_files_within(&folder.join(name), depth - 1));
            }
        }
    }
    found
}

fn read_package(path: &Path) -> Map<String, Value> {
    let text = std::fs::read(path).ok().and_then(|b| String::from_utf8(b).ok());
    match text.and_then(|t| serde_json::from_str::<Value>(t.strip_prefix('\u{feff}').unwrap_or(&t)).ok()) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

fn workspace_patterns(package: &Map<String, Value>) -> Vec<String> {
    let mut workspaces = package.get("workspaces").cloned();
    if let Some(Value::Object(o)) = &workspaces {
        workspaces = o.get("packages").cloned();
    }
    match workspaces {
        Some(Value::Array(items)) => items.iter().filter_map(|i| i.as_str().map(String::from)).collect(),
        _ => vec![],
    }
}

fn basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or("")
}

fn infer_stack(folder: &Path) -> String {
    let files = project_files_within(folder, PROJECT_FILE_DEPTH);
    let names: Vec<&str> = files.iter().map(|p| basename(p)).collect();
    if names.iter().any(|n| {
        let l = n.to_lowercase();
        l.ends_with(".sln") || l.ends_with(".slnx") || l.ends_with(".csproj")
    }) {
        return "dotnet".into();
    }
    let folder_text = folder.display().to_string();
    let mut packages: Vec<&String> = files.iter().filter(|p| basename(p) == "package.json").collect();
    packages.sort_by_key(|p| (pycompat::nt_dirname(p) != folder_text, p.len()));
    if let Some(first) = packages.first() {
        let package = read_package(Path::new(first));
        let mut deps = Map::new();
        for key in ["dependencies", "devDependencies", "peerDependencies"] {
            if let Some(Value::Object(o)) = package.get(key) {
                for (k, v) in o {
                    deps.insert(k.clone(), v.clone());
                }
            }
        }
        if deps.contains_key("@nestjs/core") {
            return "nestjs".into();
        }
        if deps.contains_key("react") {
            return "react".into();
        }
        return "node".into();
    }
    for (file, stack) in STACK_BY_FILE {
        if names.contains(&file) {
            return stack.into();
        }
    }
    "unknown".into()
}

fn has_code_files(folder: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(folder) else { return false };
    let mut dirs = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !is_skipped(&name) {
                dirs.push(path);
            }
        } else if crate::scale::is_code(&name) {
            return true;
        }
    }
    dirs.iter().any(|d| has_code_files(d))
}

fn make_project(root: &str, relative_path: &str, taken: &mut BTreeSet<String>) -> Value {
    let mut folder = root.to_string();
    for part in relative_path.split('/') {
        folder = nt_join(&folder, part);
    }
    let stack = infer_stack(Path::new(&folder));
    let name = relative_path.replace('/', "-");
    let (mut unique, mut counter) = (name.clone(), 2);
    while taken.contains(&unique) {
        unique = format!("{name}-{counter}");
        counter += 1;
    }
    taken.insert(unique.clone());
    let mut project = Map::new();
    project.insert("path".into(), json!(relative_path));
    project.insert("name".into(), json!(unique));
    project.insert("stack".into(), json!(stack));
    project.insert("skill".into(), json!(skill_by_stack(&stack)));
    project.insert("skip".into(), json!(false));
    project.insert("reason".into(), json!(""));
    project.insert("has_code".into(), json!(has_code_files(Path::new(&folder))));
    if Path::new(&folder).join(".git").exists() {
        project.insert("warning".into(), json!("has its own .git (a separate repository inside the folder)"));
    }
    Value::Object(project)
}

fn candidates_by_folders(root: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut containers = Vec::new();
    let rootp = Path::new(root);
    for name in subfolders(rootp) {
        let folder = rootp.join(&name);
        if CONTAINER_NAMES.contains(&name.as_str()) && project_files_in(&folder).is_empty() {
            if !name.starts_with('.') {
                containers.push(name);
            }
        } else if !is_skipped_folder(&name) && !project_files_within(&folder, PROJECT_FILE_DEPTH).is_empty() {
            found.push(name);
        }
    }
    if !found.is_empty() {
        return found;
    }
    for container in containers {
        for name in subfolders(&rootp.join(&container)) {
            if !is_skipped_folder(&name) && !project_files_within(&rootp.join(&container).join(&name), PROJECT_FILE_DEPTH).is_empty() {
                found.push(format!("{container}/{name}"));
            }
        }
    }
    found
}

fn has_magic(part: &str) -> bool {
    part.contains(['*', '?', '['])
}

/// `glob.glob(os.path.join(escape(root), *parts))` without recursion (`**` behaves as `*`).
fn glob_parts(base: &Path, parts: &[&str]) -> Vec<std::path::PathBuf> {
    let Some((first, rest)) = parts.split_first() else { return vec![base.to_path_buf()] };
    let mut out = Vec::new();
    if has_magic(first) {
        for name in glob_in(base, &first.replace("**", "*")) {
            if name.starts_with('.') && !first.starts_with('.') {
                continue;
            }
            let next = base.join(&name);
            if rest.is_empty() {
                out.push(next);
            } else if next.is_dir() {
                out.extend(glob_parts(&next, rest));
            }
        }
    } else {
        let next = base.join(first);
        if next.exists() {
            out.extend(if rest.is_empty() { vec![next] } else { glob_parts(&next, rest) });
        }
    }
    out
}

fn candidates_by_workspaces(root: &str, patterns: &[String]) -> Vec<String> {
    let mut included: Vec<String> = Vec::new();
    let mut excluded: BTreeSet<String> = BTreeSet::new();
    for pattern in patterns {
        let negated = pattern.starts_with('!');
        let cleaned = normalize(pattern.trim_start_matches('!'));
        let parts: Vec<&str> = cleaned.split('/').collect();
        let mut matches = glob_parts(Path::new(root), &parts);
        matches.sort();
        for m in matches {
            let relative = normalize(&m.strip_prefix(root).map(|p| p.display().to_string()).unwrap_or_default());
            if !m.is_dir() || relative.split('/').any(is_skipped_folder) {
                continue;
            }
            if negated {
                excluded.insert(relative);
            } else if !included.contains(&relative) {
                included.push(relative);
            }
        }
    }
    included.into_iter().filter(|p| !excluded.contains(p)).collect()
}

/// Script detection (DET-01..DET-05). Returns `{monorepo, projects, reason, warnings}`; never raises.
pub fn detect(root: &str) -> Value {
    let root = nt_abspath(root);
    let rootp = Path::new(&root);
    let root_files = project_files_in(rootp);
    let root_names: Vec<&str> = root_files.iter().map(|p| basename(p)).collect();
    let has_solution = root_names.iter().any(|n| {
        let l = n.to_lowercase();
        l.ends_with(".sln") || l.ends_with(".slnx")
    });
    let patterns = if root_names.contains(&"package.json") { workspace_patterns(&read_package(&rootp.join("package.json"))) } else { vec![] };
    let mut result = json!({"monorepo": false, "projects": [], "reason": "", "warnings": []});
    if !root_files.is_empty() && !(!patterns.is_empty() && !has_solution) {
        result["reason"] = json!("the root has a project file: a single project");
        return result;
    }
    let paths = if patterns.is_empty() { candidates_by_folders(&root) } else { candidates_by_workspaces(&root, &patterns) };
    if paths.len() < 2 {
        result["reason"] = json!("fewer than two project folders: not a monorepo");
        return result;
    }
    let mut taken = BTreeSet::new();
    let projects: Vec<Value> = paths.iter().map(|p| make_project(&root, p, &mut taken)).collect();
    let warnings: Vec<String> = projects
        .iter()
        .filter_map(|p| p.get("warning").map(|w| format!("{}: {}", p["path"].as_str().unwrap_or(""), w.as_str().unwrap_or(""))))
        .collect();
    result["monorepo"] = json!(true);
    result["reason"] = json!(if patterns.is_empty() { format!("{} project folders under the root", paths.len()) } else { "workspaces of the root package.json".to_string() });
    result["projects"] = Value::Array(projects);
    result["warnings"] = json!(warnings);
    result
}

pub fn default_repo_name(root: &str) -> String {
    let abs = nt_abspath(root);
    let name = abs.rsplit('\\').next().unwrap_or("").to_string();
    match name.strip_suffix(LOOP_WORKTREE_SUFFIX) {
        Some(n) => n.to_string(),
        None => name,
    }
}

/// `(repos object, error text)`. Never raises.
pub fn read_repos(root: &str) -> (Map<String, Value>, String) {
    let mut path = std::path::PathBuf::from(root);
    for part in SUBPROJECTS_RELATIVE_PATH {
        path.push(part);
    }
    if !path.exists() {
        return (Map::new(), String::new());
    }
    let parsed = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| String::from_utf8(b).map_err(|e| e.to_string())).and_then(|t| {
        serde_json::from_str::<Value>(t.strip_prefix('\u{feff}').unwrap_or(&t)).map_err(|e| e.to_string())
    });
    match parsed {
        Err(error) => (Map::new(), format!("unreadable SUBPROJECTS.json: {error}")),
        Ok(Value::Object(m)) => match m.get("repos") {
            Some(Value::Object(repos)) => (repos.clone(), String::new()),
            _ => (Map::new(), "SUBPROJECTS.json has no `repos` object".into()),
        },
        Ok(_) => (Map::new(), "SUBPROJECTS.json has no `repos` object".into()),
    }
}

/// `(entry, error)`; `entry` is `None` when absent or unreadable.
pub fn load(root: &str, repo_name: Option<&str>) -> (Option<Map<String, Value>>, String) {
    let (repos, error) = read_repos(root);
    let key = repo_name.filter(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| default_repo_name(root));
    let entry = match repos.get(&key) {
        Some(Value::Object(m)) => Some(m.clone()),
        _ => None,
    };
    (entry, error)
}

pub fn entry_projects(entry: Option<&Map<String, Value>>) -> Vec<Map<String, Value>> {
    let Some(Value::Array(items)) = entry.and_then(|e| e.get("projects")) else { return vec![] };
    items.iter().filter_map(Value::as_object).filter(|p| p.get("path").is_some_and(truthy)).cloned().collect()
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => py_json_dumps(other, false),
    }
}

pub fn monorepo_confirmed(root: &str, repo_name: Option<&str>) -> bool {
    load(root, repo_name).0.is_some_and(|e| e.get("monorepo") == Some(&Value::Bool(true)) && e.get("confirmed") == Some(&Value::Bool(true)))
}

/// `<repo>:path` -> (repo, path) when `<repo>` is a repo of the stored file; otherwise (default repo, path).
pub fn split_repo_prefix(root: &str, path: &str, repo_name: Option<&str>) -> (String, String) {
    let text = path.trim();
    if let Some((prefix, rest)) = text.split_once(':') {
        if prefix.chars().count() > 1 && !prefix.contains(['/', '\\']) && read_repos(root).0.contains_key(prefix) {
            return (prefix.to_string(), normalize(rest));
        }
    }
    (repo_name.filter(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| default_repo_name(root)), normalize(text))
}

pub fn owner_in(entry: Option<&Map<String, Value>>, path: &str) -> Option<Map<String, Value>> {
    let mut best: Option<Map<String, Value>> = None;
    for project in entry_projects(entry) {
        let base = normalize(&text_of(project.get("path")));
        if !base.is_empty() && (path == base || path.starts_with(&format!("{base}/"))) {
            let better = best.as_ref().is_none_or(|b| base.len() > normalize(&text_of(b.get("path"))).len());
            if better {
                best = Some(project);
            }
        }
    }
    best
}

pub fn owner_of(root: &str, path: &str, repo_name: Option<&str>) -> Option<Map<String, Value>> {
    let (repo, relative) = split_repo_prefix(root, path, repo_name);
    let (entry, _) = load(root, Some(&repo));
    entry.and_then(|e| owner_in(Some(&e), &relative))
}

fn read_conformance(root: &str, project_path: &str) -> String {
    let mut p = std::path::PathBuf::from(root);
    for part in normalize(project_path).split('/') {
        p.push(part);
    }
    p.push("CLAUDE.md");
    match std::fs::read(p) {
        Ok(b) => {
            let t = String::from_utf8_lossy(&b).into_owned();
            let t = t.strip_prefix('\u{feff}').unwrap_or(&t).to_string();
            CONFORMANCE_LINE.search(&t).map(|m| m.group(1).unwrap_or("-").to_string()).unwrap_or_else(|| "-".into())
        }
        Err(_) => "-".into(),
    }
}

/// The SUBPROJECT lines (and SYSTEM) of the touched paths. Empty for a single project or an unconfirmed list.
pub fn for_paths_lines(root: &str, paths: &[String], repo_name: Option<&str>) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for path in paths {
        let (repo, relative) = split_repo_prefix(root, path, repo_name);
        if !monorepo_confirmed(root, Some(&repo)) {
            continue;
        }
        let (entry, _) = load(root, Some(&repo));
        let Some(project) = owner_in(entry.as_ref(), &relative) else { continue };
        if project.get("skip") == Some(&Value::Bool(true)) {
            continue;
        }
        let base = normalize(&text_of(project.get("path")));
        if seen.contains(&(repo.clone(), base.clone())) {
            continue;
        }
        seen.push((repo, base.clone()));
        let skill = project.get("skill").filter(|v| truthy(v)).map(|v| text_of(Some(v))).unwrap_or_else(|| "-".into());
        lines.push(format!("SUBPROJECT: {base} | CLAUDE: {base}/CLAUDE.md | MAP: {base}/docs/codebase | SKILL: {skill} | CONFORMANCE: {}", read_conformance(root, &base)));
    }
    let mut system = std::path::PathBuf::from(root);
    for part in SYSTEM_DOCUMENT.split('/') {
        system.push(part);
    }
    if !lines.is_empty() && system.is_file() {
        lines.push(format!("SYSTEM: {SYSTEM_DOCUMENT}"));
    }
    lines
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// `harness`-less module CLI: `subprojects.py detect|show|for-paths ...`; returns `(exit_code, stdout, stderr)`.
pub fn run_cli(args: &[String]) -> (i32, String, String) {
    let Some(command) = args.first().map(String::as_str) else {
        return (2, String::new(), "usage: subprojects.py [-h] {detect,show,for-paths} ...\nsubprojects.py: error: the following arguments are required: command\n".into());
    };
    let Some(root) = args.get(1).filter(|a| !a.starts_with("--")) else {
        return (2, String::new(), format!("usage: subprojects.py {command} [-h] root\nsubprojects.py {command}: error: the following arguments are required: root\n"));
    };
    let rest = &args[2..];
    match command {
        "detect" => {
            let result = detect(root);
            if rest.iter().any(|a| a == "--json") {
                return (0, format!("{}\n", py_json_dumps_indent(&result, false, 2)), String::new());
            }
            let mut out = format!("{}: {}\n", if result["monorepo"] == true { "monorepo" } else { "single project" }, result["reason"].as_str().unwrap_or(""));
            for p in result["projects"].as_array().into_iter().flatten() {
                let skill = p["skill"].as_str().filter(|s| !s.is_empty()).unwrap_or("-");
                out.push_str(&format!(
                    "  {} | name={} | stack={} | skill={} | code={}\n",
                    p["path"].as_str().unwrap_or(""),
                    p["name"].as_str().unwrap_or(""),
                    p["stack"].as_str().unwrap_or(""),
                    skill,
                    if p["has_code"] == true { "yes" } else { "no" }
                ));
            }
            for w in result["warnings"].as_array().into_iter().flatten() {
                out.push_str(&format!("  warning: {}\n", w.as_str().unwrap_or("")));
            }
            (0, out, String::new())
        }
        "show" => {
            let repo = flag_value(rest, "--repo");
            let (entry, error) = load(root, repo.as_deref());
            let mut out = String::new();
            if !error.is_empty() {
                out.push_str(&format!("error: {error}\n"));
            }
            if let Some(path) = flag_value(rest, "--path").filter(|p| !p.is_empty()) {
                match owner_of(root, &path, repo.as_deref()) {
                    Some(owner) => out.push_str(&format!("{}\n", py_json_dumps(&Value::Object(owner), false))),
                    None => out.push_str("no owner\n"),
                }
            } else {
                match entry.filter(|e| !e.is_empty()) {
                    Some(e) => out.push_str(&format!("{}\n", py_json_dumps_indent(&Value::Object(e), false, 2))),
                    None => out.push_str("no entry\n"),
                }
            }
            (0, out, String::new())
        }
        "for-paths" => {
            let paths: Vec<String> = flag_value(rest, "--paths").unwrap_or_default().split(',').filter(|p| !p.trim().is_empty()).map(String::from).collect();
            let repo = flag_value(rest, "--repo");
            let mut out = String::new();
            for line in for_paths_lines(root, &paths, repo.as_deref()) {
                out.push_str(&format!("{line}\n"));
            }
            (0, out, String::new())
        }
        other => (
            2,
            String::new(),
            format!("usage: subprojects.py [-h] {{detect,show,for-paths}} ...\nsubprojects.py: error: argument command: invalid choice: {} (choose from detect, show, for-paths)\n", pycompat::py_repr_str(other)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, text) in files {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        dir
    }

    #[test]
    fn root_project_file_means_single_project() {
        let d = tree(&[("package.json", "{}"), ("src/a.ts", "")]);
        let r = detect(&d.path().display().to_string());
        assert_eq!(r["monorepo"], false);
        assert_eq!(r["reason"], "the root has a project file: a single project");
    }

    #[test]
    fn folders_and_stacks_are_detected() {
        let d = tree(&[("api/Api.csproj", ""), ("api/A.cs", ""), ("web/package.json", "{\"dependencies\": {\"react\": \"1\"}}"), ("web/x.tsx", ""), ("docs/readme.md", "")]);
        let r = detect(&d.path().display().to_string());
        assert_eq!(r["monorepo"], true);
        assert_eq!(r["projects"][0]["stack"], "dotnet");
        assert_eq!(r["projects"][1]["skill"], "frontend-dev");
        assert_eq!(r["projects"][1]["has_code"], true);
    }

    #[test]
    fn workspaces_with_negation() {
        let d = tree(&[("package.json", "{\"workspaces\": [\"packages/*\", \"!packages/skip\"]}"), ("packages/a/package.json", "{}"), ("packages/skip/package.json", "{}"), ("packages/b/package.json", "{}")]);
        let r = detect(&d.path().display().to_string());
        let paths: Vec<&str> = r["projects"].as_array().unwrap().iter().map(|p| p["path"].as_str().unwrap()).collect();
        assert_eq!(paths, vec!["packages/a", "packages/b"]);
    }

    #[test]
    fn owner_uses_the_longest_prefix_and_for_paths_needs_confirmation() {
        let d = tree(&[(
            ".specs/project/SUBPROJECTS.json",
            &format!("{{\"repos\": {{\"{}\": {{\"monorepo\": true, \"confirmed\": true, \"projects\": [{{\"path\": \"apps\", \"skill\": \"x\"}}, {{\"path\": \"apps/web\", \"skill\": \"frontend-dev\"}}]}}}}}}", default_repo_name(&"PLACEHOLDER".to_string())),
        )]);
        let root = d.path().display().to_string();
        let name = default_repo_name(&root);
        std::fs::write(d.path().join(".specs/project/SUBPROJECTS.json"), format!("{{\"repos\": {{\"{name}\": {{\"monorepo\": true, \"confirmed\": true, \"projects\": [{{\"path\": \"apps\", \"skill\": \"x\"}}, {{\"path\": \"apps/web\", \"skill\": \"frontend-dev\"}}]}}}}}}")).unwrap();
        assert_eq!(owner_of(&root, "apps/web/src/a.ts", None).unwrap()["skill"], "frontend-dev");
        let lines = for_paths_lines(&root, &["apps/web/x".into(), "apps/web/y".into()], None);
        assert_eq!(lines, vec!["SUBPROJECT: apps/web | CLAUDE: apps/web/CLAUDE.md | MAP: apps/web/docs/codebase | SKILL: frontend-dev | CONFORMANCE: -"]);
        let none = tree(&[("x.txt", "")]);
        assert!(for_paths_lines(&none.path().display().to_string(), &["a".into()], None).is_empty());
    }
}
