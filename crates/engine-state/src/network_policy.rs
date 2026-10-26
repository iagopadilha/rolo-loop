// SPDX-License-Identifier: MIT
//! The engine's layer over the shared network list (`egress-policy`): the project's `NETWORK.json`, the suggestions the planning chat
//! writes, the `.env.test` names, and the plain-JSON form of the effective list that the proxy and the preloads read.

use cfg::Env;
use egress_policy::{load_effective, parse_entry, read_project_entries, Effective, Entry, ANY_METHOD, HTTP_METHODS, TCP};
use pycompat::json::py_json_dumps_indent;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const FILE_VERSION: i64 = 1;
pub const NETWORK_LABEL: &str = ".specs/project/NETWORK.json";
pub const SUGGESTED_LABEL: &str = ".specs/project/NETWORK.suggested.json";
const IF_MISSING_PREFIX: &str = "if-missing=";

/// One entry of a list refused on save.
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkEntryError {
    pub index: usize,
    pub text: String,
    pub reason: String,
}

impl std::fmt::Display for NetworkEntryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "entry {} ({}): {}", self.index, self.text, self.reason)
    }
}

fn network_path(root: &str) -> PathBuf {
    Path::new(root).join(".specs").join("project").join("NETWORK.json")
}

fn suggested_path(root: &str) -> PathBuf {
    Path::new(root).join(".specs").join("project").join("NETWORK.suggested.json")
}

pub fn default_data_dir(env: &Env) -> PathBuf {
    env.user_data_dir()
}

/// NAMES defined with a non-empty value in `<project>/.specs/project/.env.test` (values are never returned).
pub fn read_env_names(project_root: &str) -> Vec<String> {
    let path = Path::new(project_root).join(".specs").join("project").join(".env.test");
    let Ok(bytes) = std::fs::read(path) else { return vec![] };
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut names = BTreeSet::new();
    for raw in text.lines() {
        let mut line = raw.trim();
        if line.is_empty() || line.starts_with('#') || !line.contains('=') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim_start();
        }
        let (name, value) = line.split_once('=').expect("has =");
        let mut value = value.trim();
        let chars: Vec<char> = value.chars().collect();
        if chars.len() >= 2 && chars[0] == chars[chars.len() - 1] && (chars[0] == '\'' || chars[0] == '"') {
            value = &value[1..value.len() - 1];
        }
        if !name.trim().is_empty() && !value.is_empty() {
            names.insert(name.trim().to_string());
        }
    }
    names.into_iter().collect()
}

/// Effective list of a project (`env_names` default to the names defined in the project's `.env.test`).
pub fn load(project_root: &str, data_dir: &Path, env_names: Option<&[String]>) -> Effective {
    let names = env_names.map(|n| n.to_vec()).unwrap_or_else(|| read_env_names(project_root));
    load_effective(Path::new(project_root), data_dir, &names)
}

/// Canonical text of a parsed entry (used to drop duplicates).
pub fn format_entry(entry: &Entry) -> String {
    let mut target = if entry.host.contains(':') { format!("[{}]", entry.host) } else { entry.host.clone() };
    if let Some(port) = entry.port {
        target.push_str(&format!(":{port}"));
    }
    target.push_str(&entry.path);
    let mut parts = vec![target];
    let default: BTreeSet<String> = ["GET", "HEAD"].iter().map(|s| s.to_string()).collect();
    if entry.methods != default {
        let single = entry.methods.iter().next().filter(|m| entry.methods.len() == 1 && (m.as_str() == ANY_METHOD || m.as_str() == TCP));
        parts.push(match single {
            Some(m) => m.clone(),
            None => HTTP_METHODS.iter().filter(|m| entry.methods.contains(**m)).cloned().collect::<Vec<_>>().join(","),
        });
    }
    if entry.cred {
        parts.push("cred".into());
    }
    if !entry.if_missing.is_empty() {
        parts.push(format!("{IF_MISSING_PREFIX}{}", entry.if_missing));
    }
    parts.join(" ")
}

/// JSON-friendly copy of an entry (`methods` as a sorted list).
pub fn entry_view(entry: &Entry) -> Value {
    json!({"host": entry.host, "port": entry.port, "path": entry.path, "methods": entry.methods.iter().collect::<Vec<_>>(), "cred": entry.cred,
           "if_missing": entry.if_missing, "origin": entry.origin, "text": entry.text})
}

/// The effective list as plain JSON data (for the proxy/preload list file and the API).
pub fn to_jsonable(effective: &Effective) -> Value {
    json!({"allow": effective.allow.iter().map(entry_view).collect::<Vec<_>>(), "deny": effective.deny, "notes": effective.notes,
           "broken": effective.broken, "env_names": effective.env_names})
}

fn atomic_write_json(path: &Path, content: &Value) -> std::io::Result<()> {
    crate::loop_io::atomic_write_text(path, &format!("{}\n", py_json_dumps_indent(content, false, 2)))
}

fn type_name(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "NoneType".into(),
        Value::Bool(_) => "bool".into(),
        Value::Number(n) if n.is_f64() => "float".into(),
        Value::Number(_) => "int".into(),
        Value::Array(_) => "list".into(),
        Value::Object(_) => "dict".into(),
    }
}

fn validated_texts(entries: &[Value]) -> Result<Vec<String>, NetworkEntryError> {
    entries
        .iter()
        .enumerate()
        .map(|(index, text)| parse_entry(text, "project").map(|e| e.text).map_err(|reason| NetworkEntryError { index, text: type_name(text), reason }))
        .collect()
}

/// Writes `<project>/.specs/project/NETWORK.json` atomically after validating EVERY entry (the first invalid one is returned and nothing
/// is written). Returns the stored texts.
pub fn write_project(project_root: &str, entries: &[Value]) -> Result<Vec<String>, NetworkEntryError> {
    let texts = validated_texts(entries)?;
    let _ = atomic_write_json(&network_path(project_root), &json!({"version": FILE_VERSION, "allow": texts}));
    Ok(texts)
}

// ---- suggestions (written by the planning chat, applied by the user's click) ------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Suggestions {
    pub exists: bool,
    pub allow: Vec<String>,
    pub notes: Vec<String>,
    pub broken: String,
}

/// Valid entry texts of NETWORK.suggested.json (invalid entries are left out with a note; `broken` is set when the file itself cannot be used).
pub fn read_suggestions(project_root: &str) -> Suggestions {
    let path = suggested_path(project_root);
    let mut result = Suggestions { exists: path.is_file(), ..Default::default() };
    let content = match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return result,
        Err(e) => {
            result.broken = format!("unreadable JSON ({})", if e.kind() == std::io::ErrorKind::PermissionDenied { "PermissionError" } else { "OSError" });
            result.notes.push(format!("{SUGGESTED_LABEL} is {}", result.broken));
            return result;
        }
        Ok(bytes) => match String::from_utf8(bytes).ok().and_then(|t| serde_json::from_str::<Value>(t.strip_prefix('\u{feff}').unwrap_or(&t)).ok()) {
            Some(v) => v,
            None => {
                result.broken = "unreadable JSON (JSONDecodeError)".into();
                result.notes.push(format!("{SUGGESTED_LABEL} is {}", result.broken));
                return result;
            }
        },
    };
    let list = match &content {
        Value::Object(m) => match m.get("allow") {
            None => Some(vec![]),
            Some(Value::Array(a)) => Some(a.clone()),
            _ => None,
        },
        _ => None,
    };
    let Some(list) = list else {
        result.broken = "unexpected shape (need an object with an `allow` list)".into();
        result.notes.push(format!("{SUGGESTED_LABEL} has an {}", result.broken));
        return result;
    };
    for (index, text) in list.iter().enumerate() {
        match parse_entry(text, "project") {
            Ok(e) => result.allow.push(e.text),
            Err(error) => result.notes.push(format!("suggested entry {index} ignored ({}): {error}", type_name(text))),
        }
    }
    result
}

/// Merges the valid suggested entries into NETWORK.json (no duplicates) and deletes the suggestion file. Returns `(added, allow)`.
/// `Err` when either file is unreadable (nothing is changed: an unreadable NETWORK.json is never overwritten).
pub fn apply_suggestions(project_root: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let suggestion = read_suggestions(project_root);
    if !suggestion.broken.is_empty() {
        return Err(format!("{SUGGESTED_LABEL} is {}", suggestion.broken));
    }
    let (current, error) = read_project_entries(Path::new(project_root));
    if !error.is_empty() {
        return Err(format!("{NETWORK_LABEL} is unusable ({error}); fix or delete it first"));
    }
    let mut known: BTreeSet<String> = current.iter().filter_map(|t| parse_entry(t, "project").ok()).map(|e| format_entry(&e)).collect();
    let (mut merged, mut added) = (current.clone(), Vec::<String>::new());
    for text in &suggestion.allow {
        let canonical = format_entry(&parse_entry(&json!(text), "project").map_err(|e| e.to_string())?);
        if known.insert(canonical) {
            merged.push(json!(text));
            added.push(text.clone());
        }
    }
    let allow = if added.is_empty() { current.iter().map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).collect() } else { write_project(project_root, &merged).map_err(|e| e.to_string())? };
    let _ = std::fs::remove_file(suggested_path(project_root));
    Ok((added, allow))
}

/// Deletes NETWORK.suggested.json. `true` when a file was removed.
pub fn dismiss_suggestions(project_root: &str) -> bool {
    std::fs::remove_file(suggested_path(project_root)).is_ok()
}

/// Text of `network_policy --check`: the effective list, names only, never credential values.
pub fn describe(effective: &Effective, project_root: &str, data_dir: &Path) -> String {
    let mut lines = vec![format!("project: {project_root}"), format!("data dir: {}", data_dir.display()), "allow (origin, entry):".to_string()];
    for entry in &effective.allow {
        let inactive = if !entry.if_missing.is_empty() && effective.env_names.contains(&entry.if_missing) { format!(" [inactive: {} is defined in .env.test]", entry.if_missing) } else { String::new() };
        lines.push(format!("  {:<8} {}{}", entry.origin, entry.text, inactive));
    }
    lines.push("deny (egress_never, any port/path/method):".into());
    if effective.deny.is_empty() {
        lines.push("  (none)".into());
    }
    lines.extend(effective.deny.iter().map(|p| format!("  {p}")));
    if !effective.broken.is_empty() {
        lines.push(format!("BROKEN: {} (every non-local request fails closed)", effective.broken));
    }
    lines.extend(effective.notes.iter().map(|n| format!("note: {n}")));
    lines.push(format!(".env.test names (never values): {}", if effective.env_names.is_empty() { "(none)".to_string() } else { effective.env_names.join(", ") }));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        std::fs::create_dir_all(dir.path().join(".specs").join("project")).unwrap();
        (dir, root)
    }

    #[test]
    fn env_names_never_values() {
        let (dir, root) = project();
        std::fs::write(dir.path().join(".specs/project/.env.test"), "# c\nA=1\nexport B='x'\nC=\nD=\"\"\n E = 2 \nnoequals\n").unwrap();
        assert_eq!(read_env_names(&root), vec!["A", "B", "E"]);
    }

    #[test]
    fn write_validates_every_entry_first() {
        let (_d, root) = project();
        let err = write_project(&root, &[json!("api.example.com POST"), json!("http://bad")]).unwrap_err();
        assert_eq!((err.index, err.text.as_str()), (1, "http://bad"));
        assert!(!network_path(&root).exists());
        assert_eq!(write_project(&root, &[json!("api.example.com  POST   cred")]).unwrap(), vec!["api.example.com POST cred"]);
        let text = std::fs::read_to_string(network_path(&root)).unwrap();
        assert_eq!(text, "{\n  \"version\": 1,\n  \"allow\": [\n    \"api.example.com POST cred\"\n  ]\n}\n");
    }

    #[test]
    fn suggestions_merge_without_duplicates() {
        let (_d, root) = project();
        write_project(&root, &[json!("a.example.com")]).unwrap();
        std::fs::write(suggested_path(&root), r#"{"allow": ["a.example.com GET,HEAD", "b.example.com POST", 7]}"#).unwrap();
        let read = read_suggestions(&root);
        assert_eq!(read.allow, vec!["a.example.com GET,HEAD", "b.example.com POST"]);
        assert_eq!(read.notes, vec!["suggested entry 2 ignored (int): entry must be a string"]);
        let (added, allow) = apply_suggestions(&root).unwrap();
        assert_eq!((added, allow), (vec!["b.example.com POST".to_string()], vec!["a.example.com".to_string(), "b.example.com POST".to_string()]));
        assert!(!suggested_path(&root).exists());
        assert!(!dismiss_suggestions(&root));
        std::fs::write(suggested_path(&root), "{").unwrap();
        assert_eq!(apply_suggestions(&root).unwrap_err(), "NETWORK.suggested.json is unreadable JSON (JSONDecodeError)".replace("NETWORK.suggested.json", SUGGESTED_LABEL));
    }

    #[test]
    fn effective_list_to_json_and_describe() {
        let (dir, root) = project();
        std::fs::write(dir.path().join(".specs/project/NETWORK.json"), r#"{"version": 2, "allow": ["x.example.com", "bad entry!"]}"#).unwrap();
        let data = tempfile::tempdir().unwrap();
        let eff = load(&root, data.path(), None);
        assert!(eff.notes[0].starts_with(".specs/project/NETWORK.json has version 2 (expected 1), read anyway"));
        assert!(eff.notes[1].starts_with(".specs/project/NETWORK.json entry 1 ignored (bad entry!): "));
        let view = to_jsonable(&eff);
        assert_eq!(view["allow"][0]["origin"], "local");
        assert!(describe(&eff, &root, data.path()).contains("  project  x.example.com\n"));
        let broken = tempfile::tempdir().unwrap();
        std::fs::write(broken.path().join("config.json"), "[1]").unwrap();
        let eff = load(&root, broken.path(), None);
        assert_eq!(eff.broken, "config.json");
        assert_eq!(eff.notes[0], "config.json is unusable (not a JSON object): every non-local request fails closed");
    }
}
