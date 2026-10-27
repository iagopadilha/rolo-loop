// SPDX-License-Identifier: MIT
//! Repo catalog: objective facts about a repository, validation of the sheet the AI returns, and the cache per repo.
//!
//! The catalog file is `<app data folder>/repo-catalog.json` (`~/.loop-harness`, or `LOOP_DATA_DIR`) and is keyed by the normalized repo path; an
//! entry is reused while the repo's HEAD sha is the one it was built from.
//!
//! - [`scan`]: facts: name, remote, stack files, has_docs_codebase, head
//! - [`validate`]: `[{"id", "reason"}]`: schema, purpose <= 200 characters, every evidence path exists in the repo
//! - [`build_entry`]: `(entry, rejected)`: the sheet minus the rejected capabilities, plus the facts
//! - [`index`]: `[{"name", "purpose", "capability_ids"}]`: all the planning chat reads

use crate::loop_io::{atomic_write_text, now_iso, run_timeout_raw};
use cfg::Env;
use pycompat::json::py_json_dumps_indent;
use pycompat::{nt_abspath, nt_realpath, Re};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const CATALOG_FILE_NAME: &str = "repo-catalog.json";
pub const CATALOG_VERSION: i64 = 1;
pub const PURPOSE_LIMIT: usize = 200;
pub const DESCRIPTION_LIMIT: usize = 300;
pub const CAPABILITY_LIMIT: usize = 30;
static CAPABILITY_ID_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[a-z0-9]+(-[a-z0-9]+)*\z"));
const DOCS_CODEBASE_PARTS: [&str; 2] = ["docs", "codebase"];
const GIT_TIMEOUT_SECONDS: u64 = 20;
const STACK_MARKER_FILES: [&str; 19] = [
    "package.json",
    "tsconfig.json",
    "pyproject.toml",
    "requirements.txt",
    "setup.py",
    "go.mod",
    "Cargo.toml",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "composer.json",
    "Gemfile",
    "Dockerfile",
    "docker-compose.yml",
    "nest-cli.json",
    "vite.config.ts",
    "vite.config.js",
    "next.config.js",
    "angular.json",
];
const STACK_MARKER_SUFFIXES: [&str; 3] = [".sln", ".csproj", ".fsproj"];
const STACK_SCAN_DEPTH: usize = 2;
const STACK_SKIPPED_FOLDERS: [&str; 9] = [".git", "node_modules", "bin", "obj", "dist", "build", ".venv", "venv", "__pycache__"];
const STACK_MARKER_LIMIT: usize = 30;

/// `os.path.abspath(path)` with forward slashes and no trailing slash.
pub fn normalize_path(path: &str) -> String {
    let text = nt_abspath(path).replace('\\', "/");
    let trimmed = text.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        trimmed.to_string()
    }
}

pub fn catalog_file_path(env: &Env) -> PathBuf {
    env.user_data_dir().join(CATALOG_FILE_NAME)
}

// ------------------------------------------------ file
/// `{"version", "repos"}`; an absent or unreadable file is an empty catalog.
pub fn load_catalog(env: &Env) -> Value {
    let content = std::fs::read(catalog_file_path(env)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    match content.as_ref().and_then(|c| c.get("repos")).filter(|r| r.is_object()) {
        Some(repos) => json!({"version": CATALOG_VERSION, "repos": repos}),
        None => json!({"version": CATALOG_VERSION, "repos": {}}),
    }
}

/// Atomic write (temp file + replace). The ONLY place the catalog file is written.
pub fn save_catalog(env: &Env, catalog: &Value) -> std::io::Result<()> {
    atomic_write_text(&catalog_file_path(env), &py_json_dumps_indent(catalog, false, 2))
}

pub fn get_entry(env: &Env, repo: &str) -> Option<Value> {
    load_catalog(env)["repos"].get(normalize_path(repo)).cloned()
}

pub fn store_entry(env: &Env, entry: &Value) -> std::io::Result<()> {
    let mut catalog = load_catalog(env);
    let key = normalize_path(entry["path"].as_str().unwrap_or(""));
    catalog["repos"][&key] = entry.clone();
    save_catalog(env, &catalog)
}

// ------------------------------------------------ facts
/// Output of `git -C repo ...` stripped, or '' on any failure.
pub fn run_git(repo: &str, arguments: &[&str]) -> String {
    let mut args = vec!["-C", repo];
    args.extend_from_slice(arguments);
    let (code, out, _) = run_timeout_raw("git", &args, None, GIT_TIMEOUT_SECONDS);
    if code == 0 {
        out.trim().to_string()
    } else {
        String::new()
    }
}

pub fn read_head(repo: &str) -> String {
    run_git(&nt_abspath(repo), &["rev-parse", "HEAD"])
}

pub fn find_stack_markers(repo: &str) -> Vec<String> {
    let root = PathBuf::from(repo);
    let mut found: Vec<String> = Vec::new();
    fn visit(current: &Path, root: &Path, depth: usize, found: &mut Vec<String>) {
        if found.len() >= STACK_MARKER_LIMIT {
            return;
        }
        let Ok(entries) = std::fs::read_dir(current) else { return };
        let (mut folders, mut files): (Vec<String>, Vec<String>) = (vec![], vec![]);
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() {
                folders.push(name)
            } else {
                files.push(name)
            }
        }
        for name in files {
            if STACK_MARKER_FILES.contains(&name.as_str()) || STACK_MARKER_SUFFIXES.iter().any(|s| name.ends_with(s)) {
                let relative = current.join(&name).strip_prefix(root).map(|p| p.display().to_string()).unwrap_or_default().replace('\\', "/");
                found.push(relative);
            }
        }
        if found.len() >= STACK_MARKER_LIMIT || depth >= STACK_SCAN_DEPTH {
            return;
        }
        folders.retain(|n| !STACK_SKIPPED_FOLDERS.contains(&n.as_str()));
        for name in folders {
            visit(&current.join(name), root, depth + 1, found);
        }
    }
    visit(&root, &root, 0, &mut found);
    found.sort();
    found.truncate(STACK_MARKER_LIMIT);
    found
}

/// Objective facts: `{"name", "path", "remote", "stack_files", "has_docs_codebase", "head"}` (head and remote are '' when unknown).
pub fn scan(repo: &str) -> Value {
    let repo = nt_abspath(repo);
    let docs_folder = DOCS_CODEBASE_PARTS.iter().fold(PathBuf::from(&repo), |a, p| a.join(p));
    let has_docs =
        docs_folder.is_dir() && std::fs::read_dir(&docs_folder).map(|d| d.flatten().any(|e| !e.file_name().to_string_lossy().starts_with('.'))).unwrap_or(false);
    json!({
        "name": pycompat::last_segment(repo.trim_end_matches(['\\', '/'])),
        "path": normalize_path(&repo),
        "remote": run_git(&repo, &["remote", "get-url", "origin"]),
        "stack_files": find_stack_markers(&repo),
        "has_docs_codebase": has_docs,
        "head": read_head(&repo),
    })
}

/// True while the stored entry was built from the repo's current HEAD (a rescan is not needed).
pub fn is_cached(env: &Env, repo: &str, head: Option<&str>) -> bool {
    let entry = get_entry(env, repo);
    let head = head.map(String::from).unwrap_or_else(|| read_head(repo));
    !head.is_empty() && entry.is_some_and(|e| e.get("head").and_then(Value::as_str) == Some(head.as_str()))
}

// ------------------------------------------------ validation
pub fn is_evidence_inside_repo(repo: &str, evidence: &Value) -> bool {
    let Some(text) = evidence.as_str().filter(|e| !e.trim().is_empty()) else { return false };
    if pycompat::nt_isabs(text) || text.chars().nth(1) == Some(':') && text.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let root = nt_realpath(repo);
    let target = nt_realpath(&Path::new(&root).join(text).display().to_string());
    let (norm_root, norm_target) = (pycompat::nt_normcase(&root), pycompat::nt_normcase(&target));
    let inside = norm_target == norm_root || norm_target.starts_with(&format!("{}\\", norm_root.trim_end_matches('\\')));
    inside && Path::new(&target).is_file()
}

fn error(id: &str, reason: String) -> Value {
    json!({"id": id, "reason": reason})
}

/// Errors that make the whole sheet unusable (id ''): not an object, bad purpose, capabilities not a list or too many.
pub fn validate_sheet_level(entry: &Value) -> Vec<Value> {
    if !entry.is_object() {
        return vec![error("", "the sheet is not a JSON object".into())];
    }
    let mut errors = Vec::new();
    match entry.get("purpose").and_then(Value::as_str).filter(|p| !p.trim().is_empty()) {
        None => errors.push(error("", "purpose is missing".into())),
        Some(purpose) if purpose.chars().count() > PURPOSE_LIMIT => {
            errors.push(error("", format!("purpose has {} characters (limit {PURPOSE_LIMIT})", purpose.chars().count())))
        }
        Some(_) => {}
    }
    match entry.get("capabilities") {
        Some(Value::Array(list)) if list.len() > CAPABILITY_LIMIT => errors.push(error("", format!("more than {CAPABILITY_LIMIT} capabilities"))),
        Some(Value::Array(_)) => {}
        _ => errors.push(error("", "capabilities must be a list".into())),
    }
    errors
}

fn py_str(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "None".into(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

/// `[]` when the capability is fine; otherwise one `{"id", "reason"}` (id '' when it has no usable id).
pub fn validate_capability(capability: &Value, repo: &str, seen_ids: &mut Vec<String>) -> Vec<Value> {
    if !capability.is_object() {
        return vec![error("", "a capability is not an object".into())];
    }
    let Some(id) = capability.get("id").and_then(Value::as_str).filter(|i| CAPABILITY_ID_PATTERN.is_match(i)) else {
        return vec![error("", format!("capability id must be kebab-case: {}", py_str(capability.get("id"))))];
    };
    if seen_ids.iter().any(|s| s == id) {
        return vec![error(id, "duplicate capability id".into())];
    }
    seen_ids.push(id.to_string());
    let description_ok = capability.get("description").and_then(Value::as_str).is_some_and(|d| !d.trim().is_empty() && d.chars().count() <= DESCRIPTION_LIMIT);
    if !description_ok {
        return vec![error(id, format!("description is missing or longer than {DESCRIPTION_LIMIT} characters"))];
    }
    let evidence = capability.get("evidence").cloned().unwrap_or(Value::Null);
    if !is_evidence_inside_repo(repo, &evidence) {
        return vec![error(id, format!("evidence file not found: {}", if evidence.is_string() { py_str(Some(&evidence)) } else { "(none)".into() }))];
    }
    vec![]
}

/// `[{"id", "reason"}]`; empty = valid. A whole-sheet problem has id ''; a capability problem carries its id.
pub fn validate(entry: &Value, repo: &str) -> Vec<Value> {
    let mut errors = validate_sheet_level(entry);
    if !errors.is_empty() && !entry.get("capabilities").is_some_and(Value::is_array) {
        return errors;
    }
    let mut seen: Vec<String> = Vec::new();
    for capability in entry.get("capabilities").and_then(Value::as_array).into_iter().flatten() {
        errors.extend(validate_capability(capability, repo, &mut seen));
    }
    errors
}

/// `(entry, rejected)`. Capabilities that fail validation are dropped and listed in `rejected` (`{"id", "reason"}`); a sheet that is unusable as a
/// whole (bad purpose, not an object) gives `(None, errors)` and nothing should be stored.
pub fn build_entry(facts: &Value, sheet: &Value, repo: &str) -> (Option<Value>, Vec<Value>) {
    let sheet_errors = validate_sheet_level(sheet);
    if !sheet_errors.is_empty() {
        return (None, sheet_errors);
    }
    let (mut rejected, mut kept, mut seen) = (Vec::new(), Vec::new(), Vec::new());
    for capability in sheet["capabilities"].as_array().into_iter().flatten() {
        let problems = validate_capability(capability, repo, &mut seen);
        if problems.is_empty() {
            kept.push(json!({"id": capability["id"], "description": capability["description"].as_str().unwrap_or("").trim(), "evidence": capability["evidence"].as_str().unwrap_or("").replace('\\', "/")}));
        } else {
            rejected.extend(problems);
        }
    }
    let entry = json!({
        "name": facts["name"], "path": facts["path"], "head": facts["head"], "scanned_at": now_iso(),
        "weak": !facts["has_docs_codebase"].as_bool().unwrap_or(false), "purpose": sheet["purpose"].as_str().unwrap_or("").trim(), "capabilities": kept,
    });
    (Some(entry), rejected)
}

/// Entry of a repo with no docs/codebase: marked weak, no purpose, no capabilities (the AI is not asked).
pub fn build_weak_entry(facts: &Value) -> Value {
    json!({"name": facts["name"], "path": facts["path"], "head": facts["head"], "scanned_at": now_iso(), "weak": true, "purpose": "", "capabilities": []})
}

// ------------------------------------------------ index
/// `[{"name", "purpose", "capability_ids"}]` of every cataloged repo: no descriptions, no evidence.
pub fn index(env: &Env) -> Vec<Value> {
    let catalog = load_catalog(env);
    let repos: &Map<String, Value> = catalog["repos"].as_object().expect("repos is an object");
    repos
        .values()
        .filter(|e| e.is_object())
        .map(|entry| {
            let ids: Vec<Value> = entry
                .get("capabilities")
                .and_then(Value::as_array)
                .map(|c| c.iter().filter(|i| i.is_object()).map(|i| i.get("id").cloned().unwrap_or(Value::Null)).collect())
                .unwrap_or_default();
            json!({"name": entry.get("name").cloned().unwrap_or(json!("")), "purpose": entry.get("purpose").cloned().unwrap_or(json!("")), "capability_ids": ids})
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_for(dir: &Path) -> Env {
        Env::from_pairs([("LOOP_DATA_DIR", dir.join("data").to_str().unwrap())])
    }

    #[test]
    fn sheets_are_validated_against_the_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.rs"), "x").unwrap();
        std::fs::write(repo.join("package.json"), "{}").unwrap();
        let repo_text = repo.display().to_string();
        let good = json!({"id": "pay-api", "description": "takes payments", "evidence": "src\\a.rs"});
        let sheet = json!({"purpose": "Payments", "capabilities": [good, {"id": "Bad_Id"}, {"id": "pay-api", "description": "dup", "evidence": "src/a.rs"}, {"id": "ghost", "description": "x", "evidence": "nope.rs"}, {"id": "abs", "description": "x", "evidence": "C:\\x.rs"}, "str"]});
        let reasons: Vec<String> = validate(&sheet, &repo_text).iter().map(|e| format!("{}|{}", e["id"].as_str().unwrap(), e["reason"].as_str().unwrap())).collect();
        assert_eq!(
            reasons,
            [
                "|capability id must be kebab-case: Bad_Id",
                "pay-api|duplicate capability id",
                "ghost|evidence file not found: nope.rs",
                "abs|evidence file not found: C:\\x.rs",
                "|a capability is not an object"
            ]
        );
        let facts = scan(&repo_text);
        assert_eq!((facts["name"].clone(), facts["stack_files"].clone(), facts["has_docs_codebase"].clone()), (json!("repo"), json!(["package.json"]), json!(false)));
        let (entry, rejected) = build_entry(&facts, &sheet, &repo_text);
        let entry = entry.unwrap();
        assert_eq!((rejected.len(), entry["capabilities"].as_array().unwrap().len(), entry["weak"].clone()), (5, 1, json!(true)));
        assert_eq!(entry["capabilities"][0]["evidence"], "src/a.rs");
        assert!(build_entry(&facts, &json!({"purpose": " ", "capabilities": []}), &repo_text).0.is_none());
        assert_eq!(validate(&json!([]), &repo_text)[0]["reason"], "the sheet is not a JSON object");
        assert_eq!(validate_sheet_level(&json!({"purpose": "x".repeat(201), "capabilities": 3})).len(), 2);
    }

    #[test]
    fn the_catalog_file_is_keyed_by_path_and_cached_by_head() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        assert_eq!(load_catalog(&env), json!({"version": 1, "repos": {}}));
        let repo = dir.path().join("r");
        std::fs::create_dir_all(&repo).unwrap();
        let path = normalize_path(&repo.display().to_string());
        store_entry(&env, &json!({"name": "r", "path": path, "head": "abc", "purpose": "P", "capabilities": [{"id": "one"}, 5]})).unwrap();
        assert_eq!(get_entry(&env, &repo.display().to_string()).unwrap()["purpose"], "P");
        assert!(
            is_cached(&env, &repo.display().to_string(), Some("abc"))
                && !is_cached(&env, &repo.display().to_string(), Some("zzz"))
                && !is_cached(&env, &repo.display().to_string(), Some(""))
        );
        assert_eq!(index(&env), vec![json!({"name": "r", "purpose": "P", "capability_ids": ["one"]})]);
        std::fs::write(catalog_file_path(&env), "garbage").unwrap();
        assert!(index(&env).is_empty());
    }
}
