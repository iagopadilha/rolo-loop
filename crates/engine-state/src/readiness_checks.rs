// SPDX-License-Identifier: MIT
//! Readiness checks that need the subprojects list, the code inventory and the agent-context files.
//!
//! Called from `preflight` and `planning_readiness` (`--check` / `--check --planning`). Every function returns plain text lines and never
//! fails into the caller; a warning never becomes a problem.
//!
//!   [`subprojects_preflight`]  -> (problems, warns)   GATE-01..GATE-03
//!   [`giant_preflight`]        -> warns               GIANT-05
//!   [`system_map_warning`]     -> warns               SYS-08
//!   [`uncommitted_warnings`]   -> warns               WARN-01
//!   [`known_commands`]         -> {build, run, test, summary}
//!
//! Decisions:
//! - Only a CONFIRMED monorepo is checked by `subprojects_preflight`; a single project or an unconfirmed list gives nothing.
//! - A feature of `kind=both` needs no extra rule: each subproject its paths touch is required.
//! - A giant file counts as touched by a feature when a feature path IS the file or is a folder that holds it. It is cleared when a
//!   `refactor-*` feature whose paths name the file is in the feature's `deps=` chain (transitive), or when the feature is itself a
//!   `refactor-*` one, or when the feature has `note=giant-accepted`.
//! - Inventories live in the primary repo, so a path prefixed with another repo (`other:src`) is not matched against them.

use crate::loop_state::{split_deps, Features, Fields};
use crate::project_repos::RepoRef;
use crate::{setup_files, subprojects};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

static REPO_PATH_PREFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"^([a-z0-9._-]+):(?![/\\])(.+)\z"));
const GIANT_ACCEPTED_NOTE: &str = "giant-accepted";
const REFACTOR_PREFIX: &str = "refactor-";
const CODEBASE_FOLDER: &str = "docs/codebase";

// ---------------------------------------------------------------- small readers

fn read_text(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            text.strip_prefix('\u{feff}').map(String::from).unwrap_or(text)
        }
        Err(_) => String::new(),
    }
}

fn load_json(path: &Path) -> Map<String, Value> {
    let text = read_text(path);
    match serde_json::from_str::<Value>(if text.is_empty() { "null" } else { &text }) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

fn feature_path_list(fields: &Fields) -> Vec<String> {
    fields.text("paths").split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

/// True when `<folder>/docs/codebase/` holds at least one markdown document.
fn folder_has_map(folder: &str) -> bool {
    let dir = CODEBASE_FOLDER.split('/').fold(PathBuf::from(folder), |acc, p| acc.join(p));
    std::fs::read_dir(dir).map(|d| d.flatten().any(|e| { let n = e.file_name().to_string_lossy().to_lowercase(); !n.starts_with('.') && n.ends_with(".md") })).unwrap_or(false)
}

/// `{repo name: folder}` for the repos of the project; the primary (and the single-project case) is `root`.
fn repo_folders(root: &str, repos: &[RepoRef]) -> BTreeMap<String, String> {
    repos.iter().map(|item| (item.name.clone(), if item.primary { root.to_string() } else { item.path.clone() })).collect()
}

fn is_confirmed(entry: &Option<Map<String, Value>>) -> bool {
    entry.as_ref().is_some_and(|e| e.get("monorepo") == Some(&Value::Bool(true)) && e.get("confirmed") == Some(&Value::Bool(true)))
}

fn project_path(project: &Map<String, Value>) -> String {
    subprojects::normalize(project.get("path").and_then(Value::as_str).unwrap_or(""))
}

// ---------------------------------------------------------------- GATE-01..GATE-03

/// `(entry key, folder, relative path)` for a feature path. A `<repo>:` prefix names a repo of the project or of the stored list.
fn resolve_path(root: &str, raw_path: &str, repos: &[RepoRef]) -> (String, String, String) {
    let stored = subprojects::read_repos(root).0;
    let folders = repo_folders(root, repos);
    let primary = repos.iter().find(|i| i.primary);
    if let Some(m) = REPO_PATH_PREFIX.match_start(raw_path) {
        let name = m.group(1).unwrap_or("").to_string();
        if folders.contains_key(&name) || stored.contains_key(&name) {
            let relative = subprojects::normalize(m.group(2).unwrap_or(""));
            if primary.is_some_and(|p| p.name == name) {
                return (subprojects::default_repo_name(root), root.to_string(), relative);
            }
            let folder = folders.get(&name).cloned().unwrap_or_else(|| root.to_string());
            return (name, folder, relative);
        }
    }
    (subprojects::default_repo_name(root), root.to_string(), subprojects::normalize(raw_path))
}

/// What a non-skipped subproject lacks for its agents: `conformance` and/or `map`.
fn subproject_gaps(folder: &str, project: &Map<String, Value>) -> Vec<&'static str> {
    let base = project_path(project);
    let subfolder = base.split('/').fold(PathBuf::from(folder), |acc, p| acc.join(p)).display().to_string();
    let mut gaps = Vec::new();
    if subprojects::read_conformance(folder, &base) == "-" {
        gaps.push("conformance");
    }
    if project.get("has_code") != Some(&Value::Bool(false)) && !folder_has_map(&subfolder) {
        gaps.push("map");
    }
    gaps
}

fn problem_text(base: &str, gaps: &[&str]) -> String {
    let mut parts = Vec::new();
    if gaps.contains(&"conformance") {
        parts.push(format!("no `Conformance: STANDARD|LEGACY` line in {base}/CLAUDE.md (set it in the Subprojects table)"));
    }
    if gaps.contains(&"map") {
        parts.push(format!("no map in {base}/{CODEBASE_FOLDER} (run Map codebase for it)"));
    }
    format!("subproject `{base}` is touched by the roadmap but has {}", parts.join(" and "))
}

type Token = (String, String);

struct Touched {
    touched: BTreeMap<Token, (String, Map<String, Value>, Vec<String>)>,
    outside: Vec<(String, Vec<String>)>,
    skipped: BTreeMap<Token, (Map<String, Value>, Vec<String>)>,
    confirmed: Vec<(String, (String, Map<String, Value>))>,
}

/// The subprojects the features of a confirmed monorepo touch.
fn touched_subprojects(root: &str, features: &Features, repos: &[RepoRef]) -> Touched {
    let mut t = Touched { touched: BTreeMap::new(), outside: vec![], skipped: BTreeMap::new(), confirmed: vec![] };
    for (slug, fields) in features.iter() {
        for raw in feature_path_list(fields) {
            let (key, folder, relative) = resolve_path(root, &raw, repos);
            if !t.confirmed.iter().any(|(k, _)| *k == key) {
                let entry = subprojects::load(root, Some(&key)).0;
                if !is_confirmed(&entry) {
                    continue;
                }
                t.confirmed.push((key.clone(), (folder, entry.expect("confirmed"))));
            }
            let (conf_folder, conf_entry) = &t.confirmed.iter().find(|(k, _)| *k == key).expect("present").1;
            let (conf_folder, conf_entry) = (conf_folder.clone(), conf_entry.clone());
            let Some(project) = subprojects::owner_in(Some(&conf_entry), &relative) else {
                match t.outside.iter_mut().find(|(s, _)| s == slug) {
                    Some((_, paths)) => paths.push(raw),
                    None => t.outside.push((slug.clone(), vec![raw])),
                }
                continue;
            };
            let token = (key.clone(), project_path(&project));
            if project.get("skip") == Some(&Value::Bool(true)) {
                t.skipped.entry(token).or_insert_with(|| (project.clone(), vec![])).1.push(slug.clone());
            } else {
                t.touched.entry(token).or_insert_with(|| (conf_folder, project.clone(), vec![])).2.push(slug.clone());
            }
        }
    }
    t
}

fn sorted_unique(list: &[String]) -> Vec<String> {
    list.iter().cloned().collect::<BTreeSet<_>>().into_iter().collect()
}

/// GATE-01..GATE-03 -> `(problems, warns)`. `([], [])` for anything that is not a confirmed monorepo.
pub fn subprojects_preflight(root: &str, features: &Features, repos: &[RepoRef]) -> (Vec<String>, Vec<String>) {
    let (mut problems, mut warns) = (Vec::new(), Vec::new());
    let t = touched_subprojects(root, features, repos);
    for ((_, base), (folder, project, slugs)) in &t.touched {
        let gaps = subproject_gaps(folder, project);
        if !gaps.is_empty() {
            problems.push(format!("{} (features: {})", problem_text(base, &gaps), sorted_unique(slugs).join(", ")));
        }
    }
    for (slug, paths) in &t.outside {
        warns.push(format!("feature {slug}: paths outside any subproject ({}): the agents use the root rules only", paths.join(", ")));
    }
    for ((_, base), (_, slugs)) in &t.skipped {
        warns.push(format!("feature {}: subproject `{base}` skipped: no map, no Conformance for its agents", sorted_unique(slugs).join(", ")));
    }
    for (key, (folder, entry)) in &t.confirmed {
        for project in subprojects::entry_projects(Some(entry)) {
            let base = project_path(&project);
            if project.get("skip") == Some(&Value::Bool(true)) || t.touched.contains_key(&(key.clone(), base.clone())) {
                continue;
            }
            let gaps = subproject_gaps(folder, &project);
            if !gaps.is_empty() {
                let parts: Vec<&str> = gaps.iter().map(|g| if *g == "conformance" { "Conformance line" } else { "map" }).collect();
                warns.push(format!("subproject `{base}` is not touched by any feature yet but has no {}", parts.join(" and no ")));
            }
        }
    }
    (problems, warns)
}

/// RD-02: while the roadmap does not exist yet, a confirmed monorepo whose subprojects still miss the Conformance line or the map is an
/// open installer step: one problem for it (the first real missing step), so the "generate the roadmap" line is not the first thing said.
pub fn installer_problems(root: &str, repos: &[RepoRef]) -> Vec<String> {
    let mut problems = Vec::new();
    for item in repos {
        let entry = subprojects::load(root, Some(&item.name)).0;
        if !is_confirmed(&entry) || item.role == "reference" {
            continue;
        }
        for project in subprojects::entry_projects(entry.as_ref()) {
            let gaps = subproject_gaps(&item.path, &project);
            if project.get("skip") != Some(&Value::Bool(true)) && !gaps.is_empty() {
                let parts: Vec<&str> = gaps.iter().map(|g| if *g == "conformance" { "Conformance line" } else { "map" }).collect();
                problems.push(format!(
                    "setup installer not finished: {}subproject `{}` has no {}",
                    if repos.len() > 1 { format!("repo {}, ", item.name) } else { String::new() },
                    project_path(&project),
                    parts.join(" and no ")
                ));
            }
        }
    }
    problems
}

// ---------------------------------------------------------------- SYS-08

/// Two or more subprojects touched and no root `docs/codebase/SYSTEM.md` -> one warning.
pub fn system_map_warning(root: &str, features: &Features, repos: &[RepoRef]) -> Vec<String> {
    let t = touched_subprojects(root, features, repos);
    let system = subprojects::SYSTEM_DOCUMENT.split('/').fold(PathBuf::from(root), |acc, p| acc.join(p));
    if t.touched.len() < 2 || system.is_file() {
        return vec![];
    }
    let bases: BTreeSet<&String> = t.touched.keys().map(|(_, b)| b).collect();
    vec![format!(
        "no system map: the agents have no description of how the subprojects talk (features touch {}; Map the system in the Subprojects table)",
        bases.into_iter().cloned().collect::<Vec<_>>().join(", ")
    )]
}

// ---------------------------------------------------------------- GIANT-05

/// `{file path relative to root: lines}` from every inventory of the project.
fn giant_files_of(root: &str) -> Vec<(String, Value)> {
    let folder = [".specs", "project", "loop", "internal", "inventory"].iter().fold(PathBuf::from(root), |acc, p| acc.join(p));
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder).map(|d| d.flatten().map(|e| e.path()).filter(|p| { let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default(); !n.starts_with('.') && n.ends_with(".json") }).collect()).unwrap_or_default();
    files.sort();
    let mut giants: Vec<(String, Value)> = Vec::new();
    for path in files {
        let data = load_json(&path);
        let list = match data.get("giant") {
            Some(Value::Array(a)) => a.clone(),
            _ => vec![],
        };
        for item in list {
            let Some(obj) = item.as_object() else { continue };
            if !obj.get("path").is_some_and(|p| crate::notify_truthy(Some(p))) {
                continue;
            }
            let key = subprojects::normalize(obj["path"].as_str().unwrap_or(""));
            let lines = obj.get("lines").cloned().unwrap_or(Value::Null);
            match giants.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = lines,
                None => giants.push((key, lines)),
            }
        }
    }
    giants
}

fn path_covers(path: &str, giant: &str) -> bool {
    !path.is_empty() && (giant == path || giant.starts_with(&format!("{path}/")))
}

/// GIANT-01: the feature names the giant file in its paths, or a folder of its paths holds it AND the feature names the file.
fn feature_touches_giant(slug: &str, fields: &Fields, paths: &[String], giant: &str) -> bool {
    if paths.iter().any(|p| p == giant) {
        return true;
    }
    if !paths.iter().any(|p| path_covers(p, giant)) {
        return false;
    }
    let mut parts = vec![slug.to_string()];
    parts.extend(fields.0.iter().map(|(_, v)| v.clone()));
    let text = parts.join(" ").to_lowercase();
    text.contains(&giant.rsplit('/').next().unwrap_or("").to_lowercase())
}

/// The feature's paths of the primary repo (a path prefixed with another repo is dropped), normalized.
fn own_paths(root: &str, fields: &Fields, primary_names: &[String]) -> Vec<String> {
    let mut own: BTreeSet<String> = primary_names.iter().cloned().collect();
    own.insert(subprojects::default_repo_name(root));
    let mut paths = Vec::new();
    for raw in feature_path_list(fields) {
        match REPO_PATH_PREFIX.match_start(&raw) {
            Some(m) if !own.contains(m.group(1).unwrap_or("")) => {}
            Some(m) => paths.push(subprojects::normalize(m.group(2).unwrap_or(""))),
            None => paths.push(subprojects::normalize(&raw)),
        }
    }
    paths
}

/// Every feature slug `slug` depends on, directly or not (`deps=` slugs only).
fn dependency_chain(features: &Features, slug: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut pending: Vec<String> = features.get(slug).map(|f| split_deps(f).0).unwrap_or_default();
    while let Some(current) = pending.pop() {
        if seen.contains(&current) || !features.contains(&current) {
            continue;
        }
        pending.extend(split_deps(features.get(&current).expect("present")).0);
        seen.insert(current);
    }
    seen
}

/// GIANT-05 -> warns (one line listing the files and the features).
pub fn giant_preflight(root: &str, features: &Features, repos: &[RepoRef]) -> Vec<String> {
    let giants = giant_files_of(root);
    if giants.is_empty() || features.is_empty() {
        return vec![];
    }
    let primary_names: Vec<String> = repos.iter().filter(|i| i.primary).map(|i| i.name.clone()).collect();
    let paths_by_feature: BTreeMap<&String, Vec<String>> = features.iter().map(|(slug, fields)| (slug, own_paths(root, fields, &primary_names))).collect();
    let mut offenders: Vec<(String, Vec<String>)> = Vec::new(); // giant file -> [feature slugs]
    for (slug, fields) in features.iter() {
        if slug.starts_with(REFACTOR_PREFIX) || fields.text("note").contains(GIANT_ACCEPTED_NOTE) {
            continue; // a refactor feature IS the fix
        }
        let chain: Vec<String> = dependency_chain(features, slug).into_iter().filter(|i| i.starts_with(REFACTOR_PREFIX)).collect();
        for (giant, _) in &giants {
            if !feature_touches_giant(slug, fields, &paths_by_feature[slug], giant) {
                continue;
            }
            if chain.iter().any(|item| paths_by_feature.iter().find(|(k, _)| ***k == **item).is_some_and(|(_, ps)| ps.iter().any(|p| path_covers(p, giant)))) {
                continue;
            }
            match offenders.iter_mut().find(|(g, _)| g == giant) {
                Some(slot) => slot.1.push(slug.clone()),
                None => offenders.push((giant.clone(), vec![slug.clone()])),
            }
        }
    }
    if offenders.is_empty() {
        return vec![];
    }
    offenders.sort();
    let lines_of = |giant: &str| giants.iter().find(|(g, _)| g == giant).map(|(_, l)| l.clone()).unwrap_or(Value::Null);
    let files: Vec<String> = offenders
        .iter()
        .map(|(giant, _)| {
            let lines = lines_of(giant);
            if crate::notify_truthy(Some(&lines)) { format!("{giant} ({} lines)", lines.as_str().map(String::from).unwrap_or_else(|| lines.to_string())) } else { giant.clone() }
        })
        .collect();
    let slugs: BTreeSet<String> = offenders.iter().flat_map(|(_, s)| s.iter().cloned()).collect();
    vec![format!(
        "giant file(s) touched without a refactor: {}; features: {}. Add a `refactor-*` feature to their `deps=` or mark them `note=giant-accepted`",
        files.join(", "),
        slugs.into_iter().collect::<Vec<_>>().join(", ")
    )]
}

// ---------------------------------------------------------------- WARN-01

/// RD-01: `[{"repo", "setup_files", "other_changes"}]`, one per edit repo with something uncommitted, none once its loop worktree exists.
pub fn uncommitted_blocks(repos: &[RepoRef], primary_repo: &str) -> Vec<Value> {
    setup_files::uncommitted_blocks(repos, primary_repo)
}

/// RD-01: ONE warning per repo about uncommitted changes (the setup files that will be carried, then the other changes that will not).
pub fn uncommitted_warnings(repos: &[RepoRef], primary_repo: &str) -> Vec<String> {
    uncommitted_blocks(repos, primary_repo).iter().map(setup_files::block_text).collect()
}

/// RDY-01: the structured twin of [`uncommitted_warnings`]: one `{kind, repo, files, text, fix}` per repo with uncommitted changes.
pub fn uncommitted_items(repos: &[RepoRef], primary_repo: &str) -> Vec<Value> {
    uncommitted_blocks(repos, primary_repo)
        .iter()
        .map(|block| {
            let mut files: Vec<String> = Vec::new();
            for key in ["setup_files", "other_changes"] {
                for f in block[key].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    if !files.iter().any(|x| x == f) {
                        files.push(f.to_string());
                    }
                }
            }
            json!({"kind": "uncommitted", "repo": block["repo"], "files": files, "text": setup_files::block_text(block), "fix": "Commit these files (the worktree is born from the last commit)"})
        })
        .collect()
}

// ---------------------------------------------------------------- build / run / test commands

const DOTNET_SKIP: [&str; 7] = ["bin", "obj", "node_modules", ".git", ".specs", ".vs", "packages"];

fn glob_at_depth(folder: &Path, depth: usize, extension: &str) -> Vec<PathBuf> {
    fn level(dir: &Path, remaining: usize, extension: &str, out: &mut Vec<PathBuf>) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if remaining == 0 {
                if name.to_lowercase().ends_with(extension) {
                    out.push(entry.path());
                }
            } else if entry.path().is_dir() {
                level(&entry.path(), remaining - 1, extension, out);
            }
        }
    }
    let mut out = Vec::new();
    level(folder, depth, extension, &mut out);
    out
}

/// First .sln/.csproj up to `max_depth` folders below `folder` (ignores bin/obj/node_modules/.git/.specs). Empty if none.
pub fn find_dotnet(folder: &str, max_depth: usize) -> String {
    for depth in 0..=max_depth {
        for extension in [".sln", ".slnx", ".csproj"] {
            let mut hits: Vec<PathBuf> = glob_at_depth(Path::new(folder), depth, extension)
                .into_iter()
                .filter(|path| {
                    let relative = path.strip_prefix(folder).unwrap_or(path);
                    let parts: Vec<String> = relative.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
                    !parts[..parts.len().saturating_sub(1)].iter().any(|p| DOTNET_SKIP.contains(&p.as_str()))
                })
                .collect();
            hits.sort();
            if let Some(first) = hits.first() {
                return first.strip_prefix(folder).unwrap_or(first).display().to_string().replace('\\', "/");
            }
        }
    }
    String::new()
}

fn has_python_test(dir: &Path) -> bool {
    let Ok(read) = std::fs::read_dir(dir) else { return false };
    read.flatten().any(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            return false;
        }
        if e.path().is_dir() { has_python_test(&e.path()) } else { name.starts_with("test_") && name.ends_with(".py") }
    })
}

/// How a project folder is built, run and tested, as far as the folder itself says: `{build, run, test, summary}`.
pub fn known_commands(folder: &str) -> Map<String, Value> {
    let root = Path::new(folder);
    let mut commands: BTreeMap<&str, String> = [("build", String::new()), ("run", String::new()), ("test", String::new()), ("summary", String::new())].into();
    let package = load_json(&root.join("package.json"));
    let scripts = package.get("scripts").and_then(Value::as_object).cloned().unwrap_or_default();
    for (slot, names) in [("build", &["build"][..]), ("run", &["dev", "start"][..]), ("test", &["test"][..])] {
        if let Some(name) = names.iter().find(|n| scripts.contains_key(**n)) {
            commands.insert(slot, if slot == "test" { "npm test".into() } else { format!("npm run {name}") });
        }
    }
    let dotnet = !find_dotnet(folder, 3).is_empty();
    let fill = |commands: &mut BTreeMap<&str, String>, slot: &'static str, value: &str| {
        if commands[slot].is_empty() {
            commands.insert(slot, value.to_string());
        }
    };
    if dotnet {
        fill(&mut commands, "build", "dotnet build");
        fill(&mut commands, "test", "dotnet test");
        fill(&mut commands, "run", "dotnet run");
    }
    for (name, test_command) in [("pytest.ini", "pytest"), ("tox.ini", "tox"), ("go.mod", "go test ./..."), ("pom.xml", "mvn test"), ("Cargo.toml", "cargo test"), ("Makefile", "make test"), ("setup.cfg", "pytest")] {
        if root.join(name).is_file() {
            fill(&mut commands, "test", test_command);
        }
    }
    if read_text(&root.join("pyproject.toml")).contains("[tool.pytest") || has_python_test(&root.join("tests")) {
        fill(&mut commands, "test", "python -m unittest / pytest");
    }
    let testing = CODEBASE_FOLDER.split('/').fold(root.to_path_buf(), |acc, p| acc.join(p)).join("TESTING.md");
    let summary = if !read_text(&testing).trim().is_empty() {
        format!("{CODEBASE_FOLDER}/TESTING.md")
    } else if scripts.contains_key("test") || scripts.contains_key("build") {
        "npm test".into()
    } else if dotnet {
        "dotnet test".into()
    } else {
        commands["test"].clone()
    };
    commands.insert("summary", summary);
    ["build", "run", "test", "summary"].iter().map(|k| (k.to_string(), json!(commands[k]))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_state::parse_roadmap;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn monorepo(root: &Path) -> String {
        write(root, ".specs/project/SUBPROJECTS.json", r#"{"repos": {"main": {"monorepo": true, "confirmed": true, "projects": [{"path": "api"}, {"path": "web"}, {"path": "old", "skip": true}]}}}"#);
        write(root, "api/CLAUDE.md", "Conformance: STANDARD\n");
        write(root, "api/docs/codebase/STACK.md", "map");
        root.display().to_string()
    }

    #[test]
    fn gate_checks_on_a_confirmed_monorepo() {
        let parent = tempfile::tempdir().unwrap();
        let root_path = parent.path().join("main");
        std::fs::create_dir_all(&root_path).unwrap();
        let root = monorepo(&root_path);
        let features = parse_roadmap("## Stage 1\n- [a] status=pending paths=api/x,web/y\n- [b] status=pending paths=old/z,elsewhere/q\n").0;
        let (problems, warns) = subprojects_preflight(&root, &features, &[]);
        assert_eq!(problems, vec!["subproject `web` is touched by the roadmap but has no `Conformance: STANDARD|LEGACY` line in web/CLAUDE.md (set it in the Subprojects table) and no map in web/docs/codebase (run Map codebase for it) (features: a)"]);
        assert_eq!(warns, vec![
            "feature b: paths outside any subproject (elsewhere/q): the agents use the root rules only",
            "feature b: subproject `old` skipped: no map, no Conformance for its agents",
        ]);
        let single = parse_roadmap("## Stage 1\n- [a] status=pending paths=api/x\n").0;
        assert!(system_map_warning(&root, &single, &[]).is_empty());
        let two = parse_roadmap("## Stage 1\n- [a] status=pending paths=api/x,web/y\n").0;
        assert_eq!(system_map_warning(&root, &two, &[]), vec!["no system map: the agents have no description of how the subprojects talk (features touch api, web; Map the system in the Subprojects table)"]);
    }

    #[test]
    fn giant_files_need_a_refactor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        write(dir.path(), ".specs/project/loop/internal/inventory/a.json", r#"{"giant": [{"path": "src/big.cs", "lines": 5000}]}"#);
        let features = parse_roadmap("## Stage 1\n- [a] status=pending paths=src/big.cs\n- [refactor-big] status=pending paths=src/big.cs\n- [c] status=pending paths=src/big.cs deps=refactor-big\n- [d] status=pending paths=src/big.cs note=giant-accepted\n").0;
        assert_eq!(giant_preflight(&root, &features, &[]), vec!["giant file(s) touched without a refactor: src/big.cs (5000 lines); features: a. Add a `refactor-*` feature to their `deps=` or mark them `note=giant-accepted`"]);
        assert!(giant_preflight(&root, &Features::default(), &[]).is_empty());
    }

    #[test]
    fn commands_come_from_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        write(dir.path(), "package.json", r#"{"scripts": {"build": "x", "dev": "y", "test": "z"}}"#);
        write(dir.path(), "src/App.csproj", "");
        write(dir.path(), "go.mod", "");
        write(dir.path(), "docs/codebase/TESTING.md", "How to test");
        let c = known_commands(&root);
        assert_eq!((c["build"].as_str(), c["run"].as_str(), c["test"].as_str(), c["summary"].as_str()), (Some("npm run build"), Some("npm run dev"), Some("npm test"), Some("docs/codebase/TESTING.md")));
        assert_eq!(find_dotnet(&root, 3), "src/App.csproj");
        let empty = tempfile::tempdir().unwrap();
        let c = known_commands(&empty.path().display().to_string());
        assert_eq!(c["summary"], "");
    }
}
