// SPDX-License-Identifier: MIT
//! Import of test values into `.specs/project/.env.test` (the project's ONLY credential source at run time, CRD-10/CRD-11).
//!
//! Nothing fills the project file by itself any more: the user (panel button "Import from machine sources", or the planning chat) asks for
//! NAMES, and [`import_names`] copies each one's value from the first planning source that defines it (`credential_check::planning_sources`:
//! `env_sources` of config.json, default `~/.loop-harness/.env`) into the project file. A name the project file already defines with a value is
//! kept unless `replace` is set. The file is created when missing and `.gitignore` gets the line that keeps it out of git. Comments name the
//! ORIGIN of each value (the source file), never the value. Nothing here returns or logs a value: callers get names.

use crate::credential_check::{parse_env_file, planning_sources};
use cfg::Env;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

pub const ENV_TEST_IGNORE_LINE: &str = ".specs/project/.env.test";
const HEADER: &str = "# .specs/project/.env.test: THIS project's test values, the ONLY credential source of the loop. SECRET: ignored by git, never commit,\n\
# never paste into a document. Same names the project's TEST-ENVIRONMENT.md mentions as $NAME; values reach the agents only through\n\
# `testenv.py exec`. Nothing else is read at run time (not the environment, not ~/.loop-harness/.env).\n\
# Only DEV and localhost values. Lists use `;` (write \\; for a literal ;).\n";
const QUOTE_NEEDED_CHARACTERS: &str = "#&%{}?^ \t$`\"'\\";

/// `NAME=value`, single-quoted when the .env format needs it; `None` when the value cannot be written on one line.
pub fn format_line(name: &str, value: &str) -> Option<String> {
    if value.is_empty() || value.contains(['\r', '\n', '\0']) {
        return None;
    }
    if value.starts_with(['\'', '"']) || value.chars().any(|c| QUOTE_NEEDED_CHARACTERS.contains(c)) {
        if !value.contains('\'') {
            return Some(format!("{name}='{value}'"));
        }
        if !value.contains('"') {
            return Some(format!("{name}=\"{value}\""));
        }
        return None;
    }
    Some(format!("{name}={value}"))
}

fn read_text(path: &Path) -> String {
    cfg::env::read_text_sig(path).unwrap_or_default()
}

pub fn ensure_ignored(root: &str) -> bool {
    let path = Path::new(root).join(".gitignore");
    let text = read_text(&path);
    if text.lines().any(|l| l.trim() == ENV_TEST_IGNORE_LINE) {
        return false;
    }
    let prefix = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut handle) => handle.write_all(format!("{prefix}{ENV_TEST_IGNORE_LINE}\n").as_bytes()).is_ok(),
        Err(_) => false,
    }
}

/// `[{path, exists, names: [sorted names with a value]}]` per planning source (never a value).
pub fn source_names(env: &Env, sources: Option<&[String]>) -> Vec<Value> {
    let paths = sources.map(<[String]>::to_vec).unwrap_or_else(|| planning_sources(env));
    paths
        .iter()
        .map(|path| {
            let values = parse_env_file(Path::new(path));
            let names: Vec<&String> = values.iter().filter(|(_, v)| !v.is_empty()).map(|(k, _)| k).collect();
            json!({"path": path, "exists": Path::new(path).is_file(), "names": names})
        })
        .collect()
}

/// `(value, source path)` of the first source that defines `name` with a value, else `("", "")`.
pub fn find_value(name: &str, sources: &[String]) -> (String, String) {
    for path in sources {
        if let Some(value) = parse_env_file(Path::new(path)).get(name).filter(|v| !v.is_empty()) {
            return (value.clone(), path.clone());
        }
    }
    (String::new(), String::new())
}

/// `row` with its `NAME=value` replaced when NAME is one of `replacements` (an uncommented assignment only).
fn replace_line(row: &str, replacements: &[(String, String)]) -> String {
    let stripped = row.trim();
    if stripped.is_empty() || stripped.starts_with('#') || !stripped.contains('=') {
        return row.to_string();
    }
    let body = if stripped.starts_with("export ") { stripped[7..].trim_start() } else { stripped };
    let name = body.split_once('=').map(|p| p.0).unwrap_or("").trim();
    replacements.iter().find(|(n, _)| n == name).map(|(_, line)| line.clone()).unwrap_or_else(|| row.to_string())
}

/// Copies the values of `names` from the planning sources into `<root>/.specs/project/.env.test`. Returns names only: `{"created", "added",
/// "replaced", "kept" (already defined, not replaced), "missing" (no source defines them), "unwritable" (a value that cannot go on one line)}`.
pub fn import_names(env: &Env, root: &str, names: &[String], replace: bool) -> Value {
    let path = Path::new(root).join(".specs").join("project").join(".env.test");
    let existed = path.is_file();
    let current = read_text(&path);
    let defined: BTreeSet<String> = parse_env_file(&path).into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, _)| k).collect();
    let sources = planning_sources(env);
    let (mut added, mut replaced, mut kept, mut missing, mut unwritable): (Vec<String>, Vec<String>, Vec<String>, Vec<String>, Vec<String>) = Default::default();
    let (mut lines, mut replacements): (Vec<String>, Vec<(String, String)>) = (vec![], vec![]);
    let mut seen: Vec<String> = Vec::new();
    for name in names.iter().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        if defined.contains(&name) && !replace {
            kept.push(name);
            continue;
        }
        let (value, origin) = find_value(&name, &sources);
        if value.is_empty() {
            missing.push(name);
            continue;
        }
        let Some(line) = format_line(&name, &value) else {
            unwritable.push(name);
            continue;
        };
        if defined.contains(&name) {
            replacements.push((name.clone(), line));
            replaced.push(name);
        } else {
            let origin_path = Path::new(&origin);
            let folder = origin_path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let file = origin_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            lines.push(format!("# {name}: imported from {folder}/{file}"));
            lines.push(line);
            added.push(name);
        }
    }
    let mut result = json!({"created": false, "added": added, "replaced": replaced, "kept": kept, "missing": missing, "unwritable": unwritable});
    if replacements.is_empty() && lines.is_empty() {
        return result;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut text = current;
    if !replacements.is_empty() {
        text = text.split('\n').map(|row| replace_line(row, &replacements)).collect::<Vec<_>>().join("\n");
    }
    if !lines.is_empty() {
        let separator = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
        let has_text = !text.trim().is_empty();
        let prefix = if has_text { "" } else { HEADER };
        text = format!("{text}{separator}{prefix}{}{}\n", if has_text { "\n" } else { "" }, lines.join("\n"));
    }
    if std::fs::write(&path, text).is_err() {
        return result;
    }
    ensure_ignored(root);
    result["created"] = json!(!existed);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_names_from_the_first_source_and_keeps_defined() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("machine").join(".env");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "A_KEY=one\nB_KEY=two words\nC_KEY=\n").unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("config.json"), format!("{{\"env_sources\": [{}]}}", pycompat::json::py_json_dumps(&json!(source.display().to_string()), true))).unwrap();
        let env = Env::from_pairs([("LOOP_DATA_DIR", data.to_str().unwrap()), ("LOOP_CLAUDE_HOME", data.to_str().unwrap())]);
        let root = dir.path().join("proj").display().to_string();
        std::fs::create_dir_all(&root).unwrap();
        let names: Vec<String> = ["A_KEY", "B_KEY", "C_KEY", "D_KEY"].map(String::from).to_vec();
        let result = import_names(&env, &root, &names, false);
        assert_eq!((result["created"].clone(), result["added"].clone(), result["missing"].clone()), (json!(true), json!(["A_KEY", "B_KEY"]), json!(["C_KEY", "D_KEY"])));
        let text = std::fs::read_to_string(Path::new(&root).join(".specs/project/.env.test")).unwrap();
        assert!(text.starts_with("# .specs/project/.env.test") && text.contains("A_KEY=one\n") && text.contains("B_KEY='two words'\n"));
        assert!(text.contains("# A_KEY: imported from machine/.env"));
        assert!(std::fs::read_to_string(Path::new(&root).join(".gitignore")).unwrap().contains(ENV_TEST_IGNORE_LINE));
        std::fs::write(&source, "A_KEY=changed\n").unwrap();
        let again = import_names(&env, &root, &names[..1], false);
        assert_eq!(again["kept"], json!(["A_KEY"]));
        let replaced = import_names(&env, &root, &names[..1], true);
        assert_eq!(replaced["replaced"], json!(["A_KEY"]));
        assert!(std::fs::read_to_string(Path::new(&root).join(".specs/project/.env.test")).unwrap().contains("A_KEY=changed"));
        assert_eq!(format_line("X", "a'b\"c"), None);
        assert_eq!(format_line("X", "a b").as_deref(), Some("X='a b'"));
    }
}
