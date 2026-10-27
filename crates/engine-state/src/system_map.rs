// SPDX-License-Identifier: MIT
//! Contract merge for the root `docs/codebase/SYSTEM.md` of a monorepo (deterministic, no AI).
//!
//! Executors never edit SYSTEM.md. A feature that adds, changes or removes a route, DTO, auth scheme or env name writes
//! `.specs/features/<slug>/contract.md` with lines like
//!
//!   `- [add|change|remove] <route|dto|auth|env> <identity> — <one-line purpose> (<file>)`
//!
//! (identity = `METHOD /path`, DTO name, auth scheme name or env variable name; the separator is an em dash or ` - `).
//! [`merge_contract_deltas`] upserts them by kind + identity into the `<!-- CONTRACT:BEGIN -->...<!-- CONTRACT:END -->` section and
//! leaves every byte outside the markers untouched (line endings included).
//!
//! A ledger (`.specs/project/loop/internal/contract-merged.json`) remembers which contract.md content was merged for each feature,
//! so [`pending_slugs`] can list the ones not merged yet.

use crate::loop_state::{feature_dir, feature_folder_name};
use pycompat::json::py_json_dumps_indent;
use pycompat::{nt_abspath, splitlines, Re};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const CONTRACT_BEGIN: &str = "<!-- CONTRACT:BEGIN -->";
pub const CONTRACT_END: &str = "<!-- CONTRACT:END -->";
pub const NO_GRAMMAR_MARK: &str = "has no line in the contract grammar";
const KIND_ORDER: [&str; 4] = ["route", "dto", "auth", "env"];
const ROUTE_HEADING_PREFIX: &str = "#### Routes: ";
const HTTP_METHODS: &str = "GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS";

/// `docs/codebase/SYSTEM.md` with forward slashes (git pathspec form).
pub const SYSTEM_DOCUMENT: &str = "docs/codebase/SYSTEM.md";

static ENTRY_LINE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"^\s*[-*]\s*\[(add|change|remove)\]\s+(route|dto|auth|env)\s+(.+?)(?:\s+(?:—|-)\s+(.*?)\s*\(([^()]+)\))?\s*\z"));
static CANDIDATE_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*[-*]\s*\["));
static ROUTE_IDENTITY: LazyLock<Re> = LazyLock::new(|| Re::ci(&format!(r"^({HTTP_METHODS})\s+(/\S*)\z")));
static RENDERED_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^- (route|dto|auth|env) (.+?) — (.+)\z"));
static CONFLICT_SUFFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"\s*\(conflict with ([^)]+)\)\s*\z"));
static GENERATED_HEADING: LazyLock<Re> = LazyLock::new(|| Re::new(r"^#### (Routes: .+|DTOs|Auth|Env)\s*\z"));

fn kind_heading(kind: &str) -> &'static str {
    match kind {
        "dto" => "#### DTOs",
        "auth" => "#### Auth",
        _ => "#### Env",
    }
}

fn system_path(root: &str) -> PathBuf {
    Path::new(root).join("docs").join("codebase").join("SYSTEM.md")
}

fn ledger_path(root: &str) -> PathBuf {
    Path::new(root).join(".specs").join("project").join("loop").join("internal").join("contract-merged.json")
}

pub fn contract_path(root: &str, slug: &str) -> PathBuf {
    Path::new(&feature_dir(root, slug)).join("contract.md")
}

pub fn pending_line(slug: &str) -> String {
    format!("CONTRACT-PENDING: .specs/features/{}/contract.md", feature_folder_name(slug))
}

fn content_hash(raw: &[u8]) -> String {
    Sha256::digest(raw).iter().map(|b| format!("{b:02x}")).collect()
}

fn load_ledger(root: &str) -> Map<String, Value> {
    let parsed = std::fs::read(ledger_path(root)).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let mut data = match parsed {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    data.entry("merged").or_insert_with(|| json!({}));
    data.entry("origins").or_insert_with(|| json!({}));
    data
}

/// `sort_keys=True`.
fn sorted_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|k| (k.clone(), sorted_keys(&map[k]))).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted_keys).collect()),
        other => other.clone(),
    }
}

fn save_ledger(root: &str, ledger: &Map<String, Value>) {
    let path = ledger_path(root);
    let _ = std::fs::create_dir_all(path.parent().expect("has parent"));
    let temporary = PathBuf::from(format!("{}.tmp", path.display()));
    if std::fs::write(&temporary, py_json_dumps_indent(&sorted_keys(&Value::Object(ledger.clone())), false, 2)).is_ok() {
        let _ = crate::loop_io::replace_file(&temporary, &path);
    }
}

/// The canonical identity, or `None` when it is not valid for the kind.
fn normalize_identity(kind: &str, identity: &str) -> Option<String> {
    let identity = identity.trim();
    if kind == "route" {
        return ROUTE_IDENTITY.match_start(identity).map(|m| format!("{} {}", m.group(1).unwrap_or("").to_uppercase(), m.group(2).unwrap_or("")));
    }
    (!identity.is_empty() && !identity.contains(' ')).then(|| identity.to_string())
}

/// A delta is `(action, kind, identity, entry_text)`.
pub type Delta = (String, String, String, Option<String>);

/// `(deltas, malformed_lines)`.
pub fn parse_contract(text: &str, slug: &str) -> (Vec<Delta>, Vec<String>) {
    let (mut deltas, mut malformed) = (Vec::new(), Vec::new());
    for (index, line) in splitlines(text).into_iter().enumerate() {
        if CANDIDATE_LINE.match_start(line).is_none() {
            continue;
        }
        let bad = || format!("{slug}: line {} is not a valid contract line: {}", index + 1, line.trim());
        let Some(m) = ENTRY_LINE.match_start(line) else {
            malformed.push(bad());
            continue;
        };
        let (action, kind) = (m.group(1).unwrap_or("").to_string(), m.group(2).unwrap_or("").to_string());
        let identity = normalize_identity(&kind, m.group(3).unwrap_or(""));
        let (purpose, file) = (m.group(4), m.group(5));
        let has_both = purpose.is_some_and(|p| !p.is_empty()) && file.is_some_and(|f| !f.is_empty());
        let Some(identity) = identity.filter(|_| action == "remove" || has_both) else {
            malformed.push(bad());
            continue;
        };
        let entry_text = purpose.filter(|p| !p.is_empty()).map(|p| format!("{} ({})", p.trim(), file.unwrap_or("").trim()));
        deltas.push((action, kind, identity, entry_text));
    }
    (deltas, malformed)
}

/// `(before, section, after)` around the markers, or `None` when the markers are missing.
fn split_section(text: &str) -> Option<(&str, &str, &str)> {
    let begin = text.find(CONTRACT_BEGIN)?;
    let section_start = begin + CONTRACT_BEGIN.len();
    let end = section_start + text[section_start..].find(CONTRACT_END)?;
    Some((&text[..section_start], &text[section_start..end], &text[end..]))
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    text: String,
    conflict: Option<String>,
}

type Entries = BTreeMap<(String, String), Vec<Entry>>;

fn parse_section(section: &str) -> (Vec<String>, Entries) {
    let (mut preamble, mut entries) = (Vec::new(), Entries::new());
    for line in splitlines(section) {
        let stripped = line.trim();
        if stripped.is_empty() || GENERATED_HEADING.match_start(stripped).is_some() {
            continue;
        }
        let Some(rendered) = RENDERED_LINE.match_start(stripped) else {
            preamble.push(line.trim_end().to_string());
            continue;
        };
        let mut text = rendered.group(3).unwrap_or("").to_string();
        let conflict = CONFLICT_SUFFIX.search(&text);
        if let Some(c) = &conflict {
            text.truncate(c.start);
        }
        entries
            .entry((rendered.group(1).unwrap_or("").to_string(), rendered.group(2).unwrap_or("").to_string()))
            .or_default()
            .push(Entry { text, conflict: conflict.and_then(|c| c.group(1).map(String::from)) });
    }
    (preamble, entries)
}

fn route_area(identity: &str) -> String {
    let path = identity.split_once(' ').map(|x| x.1).unwrap_or("").trim_matches('/');
    let first = path.split('/').next().unwrap_or("").split('?').next().unwrap_or("");
    if first.is_empty() {
        "root".into()
    } else {
        first.into()
    }
}

fn render_section(preamble: &[String], entries: &Entries, newline: &str) -> String {
    let mut lines: Vec<String> = preamble.to_vec();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut groups: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut routes_by_area: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (kind, identity) in entries.keys() {
        if kind == "route" {
            routes_by_area.entry(route_area(identity)).or_default().push(identity.clone());
        }
    }
    for (area, identities) in routes_by_area {
        groups.push((format!("{ROUTE_HEADING_PREFIX}{area}"), identities.into_iter().map(|i| ("route".to_string(), i)).collect()));
    }
    for kind in &KIND_ORDER[1..] {
        let keys: Vec<(String, String)> = entries.keys().filter(|k| k.0 == *kind).cloned().collect();
        if !keys.is_empty() {
            groups.push((kind_heading(kind).to_string(), keys));
        }
    }
    for (heading, keys) in groups {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(heading);
        for key in keys {
            for entry in &entries[&key] {
                let suffix = entry.conflict.as_ref().map(|c| format!(" (conflict with {c})")).unwrap_or_default();
                lines.push(format!("- {} {} — {}{suffix}", key.0, key.1, entry.text));
            }
        }
    }
    if lines.is_empty() {
        newline.to_string()
    } else {
        format!("{newline}{}{newline}", lines.join(newline))
    }
}

fn origin_of(origins: &Map<String, Value>, key_text: &str, text: &str) -> Option<String> {
    origins
        .get(key_text)?
        .as_array()?
        .iter()
        .find(|pair| pair.get(0).and_then(Value::as_str) == Some(text))
        .and_then(|pair| pair.get(1).and_then(Value::as_str).map(String::from))
}

fn apply_delta(entries: &mut Entries, origins: &mut Map<String, Value>, slug: &str, delta: &Delta, warnings: &mut Vec<String>) {
    let (action, kind, identity, text) = delta;
    let key = (kind.clone(), identity.clone());
    let key_text = format!("{kind} {identity}");
    if action == "remove" {
        entries.remove(&key);
        origins.shift_remove(&key_text);
        return;
    }
    let text = text.clone().unwrap_or_default();
    let existing = entries.get(&key).cloned().unwrap_or_default();
    if existing.iter().any(|e| e.text == text) {
        return;
    }
    let foreign: Vec<String> = existing
        .iter()
        .map(|e| origin_of(origins, &key_text, &e.text))
        .filter(|o| o.as_deref() != Some(slug))
        .map(|o| o.unwrap_or_else(|| "an earlier entry".into()))
        .collect();
    if existing.is_empty() || foreign.is_empty() {
        entries.insert(key, vec![Entry { text: text.clone(), conflict: None }]);
        origins.insert(key_text, json!([[text, slug]]));
        return;
    }
    let other = foreign[0].clone();
    warnings.push(format!("conflict on {key_text}: {slug} differs from {other}; both kept"));
    let mut all = existing;
    all.push(Entry { text: text.clone(), conflict: Some(other) });
    entries.insert(key, all);
    let slot = origins.entry(key_text).or_insert_with(|| json!([]));
    if let Some(list) = slot.as_array_mut() {
        list.push(json!([text, slug]));
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MergeResult {
    pub changed: bool,
    pub warnings: Vec<String>,
}

/// Merges the contract.md of each listed feature into SYSTEM.md.
pub fn merge_contract_deltas(root: &str, slugs: &[String]) -> MergeResult {
    let root = nt_abspath(root);
    let mut warnings: Vec<String> = Vec::new();
    let mut ledger = load_ledger(&root);
    let mut candidates: Vec<(String, Vec<u8>)> = Vec::new();
    for slug in slugs {
        let path = contract_path(&root, slug);
        if !path.is_file() {
            continue;
        }
        let Ok(raw) = std::fs::read(&path) else { continue };
        if ledger["merged"].get(slug).and_then(Value::as_str) == Some(content_hash(&raw).as_str()) {
            continue;
        }
        candidates.push((slug.clone(), raw));
    }
    if candidates.is_empty() {
        return MergeResult { changed: false, warnings };
    }
    let target = system_path(&root);
    if !target.is_file() {
        warnings.push(format!("SYSTEM.md is missing ({SYSTEM_DOCUMENT}): contract merge skipped, the deltas stay in contract.md"));
        return MergeResult { changed: false, warnings };
    }
    let original = String::from_utf8_lossy(&std::fs::read(&target).unwrap_or_default()).into_owned();
    let Some((before, section, after)) = split_section(&original) else {
        warnings.push(format!("SYSTEM.md has no {CONTRACT_BEGIN} ... {CONTRACT_END} section: contract merge skipped"));
        return MergeResult { changed: false, warnings };
    };
    let newline = if original.contains("\r\n") { "\r\n" } else { "\n" };
    let (preamble, mut entries) = parse_section(section);
    let mut origins = ledger.get("origins").and_then(Value::as_object).cloned().unwrap_or_default();
    for (slug, raw) in &candidates {
        let (deltas, malformed) = parse_contract(&String::from_utf8_lossy(raw), slug);
        let none_valid = deltas.is_empty() && malformed.is_empty();
        warnings.extend(malformed);
        if none_valid {
            warnings.push(format!("contract.md of {slug} {NO_GRAMMAR_MARK}: nothing merged"));
        }
        for delta in &deltas {
            apply_delta(&mut entries, &mut origins, slug, delta, &mut warnings);
        }
        if let Some(Value::Object(merged)) = ledger.get_mut("merged") {
            merged.insert(slug.clone(), json!(content_hash(raw)));
        }
    }
    ledger.insert("origins".into(), Value::Object(origins));
    let updated = format!("{before}{}{after}", render_section(&preamble, &entries, newline));
    let changed = updated != original;
    if changed {
        let _ = std::fs::write(&target, updated.as_bytes());
    }
    save_ledger(&root, &ledger);
    MergeResult { changed, warnings }
}

/// Slugs (in the given order) whose contract.md exists and was not merged in its current content.
pub fn pending_slugs(root: &str, slugs: &[String]) -> Vec<String> {
    let root = nt_abspath(root);
    let ledger = load_ledger(&root);
    slugs
        .iter()
        .filter(|slug| {
            let path = contract_path(&root, slug);
            path.is_file()
                && std::fs::read(&path).map(|raw| ledger["merged"].get(slug.as_str()).and_then(Value::as_str) != Some(content_hash(&raw).as_str())).unwrap_or(false)
        })
        .cloned()
        .collect()
}

const USAGE: &str = "Contract merge for the root `docs/codebase/SYSTEM.md` of a monorepo (deterministic, no AI).

Usage:
  harness system-map merge   <repo-root> --slugs a,b [--json]
  harness system-map pending <repo-root> --slugs a,b
";

fn parse_slugs(args: &[String]) -> Vec<String> {
    let Some(at) = args.iter().position(|a| a == "--slugs") else { return vec![] };
    args.get(at + 1).map(|v| v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()).unwrap_or_default()
}

/// `harness system-map merge|pending <root> --slugs a,b [--json]`: returns (exit code, stdout).
pub fn run_cli(args: &[String]) -> (i32, String) {
    if args.len() < 2 || !matches!(args[0].as_str(), "merge" | "pending") {
        return (
            2,
            format!(
                "{USAGE}
"
            ),
        );
    }
    let slugs = parse_slugs(args);
    if args[0] == "pending" {
        let lines: String = pending_slugs(&args[1], &slugs)
            .iter()
            .map(|slug| {
                format!(
                    "{}
",
                    pending_line(slug)
                )
            })
            .collect();
        return (0, lines);
    }
    let result = merge_contract_deltas(&args[1], &slugs);
    if args.iter().any(|a| a == "--json") {
        let body = json!({"changed": result.changed, "warnings": result.warnings});
        return (
            0,
            format!(
                "{}
",
                py_json_dumps_indent(&body, false, 2)
            ),
        );
    }
    let mut out = format!(
        "changed: {}
",
        if result.changed { "yes" } else { "no" }
    );
    for warning in &result.warnings {
        out.push_str(&format!(
            "WARNING: {warning}
"
        ));
    }
    (0, out)
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
    fn grammar() {
        let text = "- [add] route get /orders/{id} — Fetch one (Api/OrdersController.cs)\n- [remove] env OLD\n- [add] dto Order\n* [change] auth Bearer - JWT (Startup.cs)\nplain text\n";
        let (deltas, malformed) = parse_contract(text, "s");
        assert_eq!(deltas[0], ("add".to_string(), "route".to_string(), "GET /orders/{id}".to_string(), Some("Fetch one (Api/OrdersController.cs)".to_string())));
        assert_eq!(deltas[1].3, None);
        assert_eq!(deltas[2], ("change".to_string(), "auth".to_string(), "Bearer".to_string(), Some("JWT (Startup.cs)".to_string())));
        assert_eq!(malformed, vec!["s: line 3 is not a valid contract line: - [add] dto Order"]);
    }

    #[test]
    fn merge_keeps_bytes_outside_markers_and_reports_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let r = root.display().to_string();
        write(root, "docs/codebase/SYSTEM.md", "# System\r\n\r\n<!-- CONTRACT:BEGIN -->\r\nIntro line\r\n<!-- CONTRACT:END -->\r\nTail\r\n");
        write(root, ".specs/features/a/contract.md", "- [add] route GET /orders — List (O.cs)\n- [add] env DB_URL — database (appsettings.json)\n");
        let first = merge_contract_deltas(&r, &["a".into()]);
        assert!(first.changed && first.warnings.is_empty());
        let text = std::fs::read_to_string(root.join("docs/codebase/SYSTEM.md")).unwrap();
        assert_eq!(text, "# System\r\n\r\n<!-- CONTRACT:BEGIN -->\r\nIntro line\r\n\r\n#### Routes: orders\r\n- route GET /orders — List (O.cs)\r\n\r\n#### Env\r\n- env DB_URL — database (appsettings.json)\r\n<!-- CONTRACT:END -->\r\nTail\r\n");
        assert!(!merge_contract_deltas(&r, &["a".into()]).changed);
        write(root, ".specs/features/b/contract.md", "- [change] route GET /orders — Other (P.cs)\n");
        let second = merge_contract_deltas(&r, &["b".into()]);
        assert_eq!(second.warnings, vec!["conflict on route GET /orders: b differs from a; both kept"]);
        assert_eq!(pending_slugs(&r, &["a".into(), "b".into(), "zz".into()]), Vec::<String>::new());
        write(root, ".specs/features/a/contract.md", "- [remove] env DB_URL\n");
        assert_eq!(pending_slugs(&r, &["a".into(), "b".into()]), vec!["a"]);
        write(root, ".specs/features/c/contract.md", "just prose\n");
        assert_eq!(merge_contract_deltas(&r, &["c".into()]).warnings, vec!["contract.md of c has no line in the contract grammar: nothing merged"]);
    }

    #[test]
    fn missing_system_or_markers() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path().display().to_string();
        write(dir.path(), ".specs/features/a/contract.md", "- [remove] env X\n");
        assert!(merge_contract_deltas(&r, &["a".into()]).warnings[0].starts_with("SYSTEM.md is missing (docs/codebase/SYSTEM.md)"));
        write(dir.path(), "docs/codebase/SYSTEM.md", "no markers");
        assert!(merge_contract_deltas(&r, &["a".into()]).warnings[0].contains("section: contract merge skipped"));
    }
}
