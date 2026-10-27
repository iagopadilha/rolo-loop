// SPDX-License-Identifier: MIT
//! Simple access release: ONE function, [`grant_access`], releases what an E2E run needs (a network entry, a higher cost limit) and writes every
//! grant AND every refusal into the permissions journal, `.specs/project/loop/json/PERMISSIONS-LOG.json` (what, why, who, when, backup).
//!
//! The journal is data for the panel (read-only in the state payload, see [`journal_view`]). It never holds a credential: the reason and the
//! value pass through the redaction before they are written, and a secret NAME is never asked for here. A grant changes only the one setting it
//! names; production hosts (`egress_never` of the config) are always refused, and nothing is granted without a reason.

use crate::loop_io::{atomic_write_text, load_json, now_iso, save_json};
use crate::loop_state::loop_path;
use crate::network_policy;
use crate::secret_redaction::{redact, REDACTION_MARK};
use egress_policy::{host_matches, parse_entry, read_project_entries};
use serde_json::{json, Value};
use std::path::Path;

pub const JOURNAL_NAME: &str = "PERMISSIONS-LOG.json";
pub const JOURNAL_VERSION: i64 = 1;
pub const KIND_HOST: &str = "host";
pub const KIND_COST_LIMIT: &str = "cost_limit";
pub const KIND_BASELINE: &str = "baseline";
pub const KINDS: [&str; 2] = [KIND_HOST, KIND_COST_LIMIT];
pub const OUTCOME_GRANTED: &str = "granted";
pub const OUTCOME_REFUSED: &str = "refused";
const BACKUP_FOLDER: &str = "access-backups";
const TEXT_LIMIT: usize = 300;

/// Where a grant applies and who asks for it.
#[derive(Clone, Debug)]
pub struct AccessContext<'a> {
    /// The project's repository (the master of `NETWORK.json`).
    pub repo: &'a str,
    /// The loop's work folder (the worktree, or the repo itself): holds `meta.json` and the journal.
    pub work: &'a str,
    pub data_dir: &'a Path,
    pub toolkit_root: &'a str,
    /// Free text: `user`, `panel`, `cli`...
    pub who: &'a str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrantResult {
    pub granted: bool,
    /// `false` when the setting already had the value (nothing was written besides the journal line).
    pub changed: bool,
    pub message: String,
    /// The journal line that was written (Null when the journal itself could not be written).
    pub entry: Value,
}

fn journal_path(work: &str) -> std::path::PathBuf {
    loop_path(work, JOURNAL_NAME)
}

fn clean(text: &str) -> String {
    redact(text.trim(), Some(TEXT_LIMIT), REDACTION_MARK)
}

/// All journal entries (oldest first); an unreadable or missing file is an empty journal.
pub fn read_journal(work: &str) -> Vec<Value> {
    load_json(&journal_path(work)).get("entries").and_then(Value::as_array).cloned().unwrap_or_default()
}

/// Appends one line to the journal and returns it. `Err` only when the file cannot be written.
pub fn record(work: &str, kind: &str, value: &str, reason: &str, who: &str, outcome: &str, detail: &str, backup: &str) -> Result<Value, String> {
    let mut entries = read_journal(work);
    let entry = json!({
        "id": entries.len() + 1, "at": now_iso(), "kind": kind, "value": clean(value), "reason": clean(reason),
        "who": if who.trim().is_empty() { "unknown".to_string() } else { clean(who) },
        "outcome": outcome, "detail": clean(detail), "backup": backup,
    });
    entries.push(entry.clone());
    save_json(&journal_path(work), &json!({"version": JOURNAL_VERSION, "entries": entries})).map_err(|e| format!("could not write {JOURNAL_NAME} ({:?})", e.kind()))?;
    Ok(entry)
}

/// What the state payload carries: the newest `limit` entries (newest first) and the counts. Read-only; never has a credential.
pub fn journal_view(work: &str, limit: usize) -> Value {
    let entries = read_journal(work);
    let count = |outcome: &str| entries.iter().filter(|e| e["outcome"] == outcome).count();
    let (granted, refused) = (count(OUTCOME_GRANTED), count(OUTCOME_REFUSED));
    let newest: Vec<Value> = entries.iter().rev().take(limit).cloned().collect();
    json!({"path": ".specs/project/loop/json/PERMISSIONS-LOG.json", "total": entries.len(), "granted": granted, "refused": refused, "entries": newest})
}

/// Copies `source` into `<work>/.specs/project/loop/internal/access-backups/` before it is changed; returns the path relative to `work`
/// (empty when there was nothing to copy).
pub fn backup_file(work: &str, source: &Path) -> Result<String, String> {
    if !source.is_file() {
        return Ok(String::new());
    }
    let stamp: String = now_iso().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let name = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let relative = format!(".specs/project/loop/internal/{BACKUP_FOLDER}/{name}.{stamp}.{}", read_journal(work).len() + 1);
    let target = relative.split('/').fold(std::path::PathBuf::from(work), |acc, part| acc.join(part));
    let bytes = std::fs::read(source).map_err(|e| format!("could not read {name} to back it up ({:?})", e.kind()))?;
    std::fs::create_dir_all(target.parent().expect("has parent")).map_err(|e| format!("could not create the backup folder ({:?})", e.kind()))?;
    std::fs::write(&target, bytes).map_err(|e| format!("could not write the backup of {name} ({:?})", e.kind()))?;
    Ok(relative)
}

fn overlaps(a: &str, b: &str) -> bool {
    host_matches(a, b) || host_matches(b, a)
}

fn refuse(ctx: &AccessContext, kind: &str, value: &str, reason: &str, why: String) -> GrantResult {
    let entry = record(ctx.work, kind, value, reason, ctx.who, OUTCOME_REFUSED, &why, "").unwrap_or(Value::Null);
    GrantResult { granted: false, changed: false, message: format!("refused: {why}"), entry }
}

fn grant(ctx: &AccessContext, kind: &str, value: &str, reason: &str, changed: bool, note: &str, backup: &str) -> GrantResult {
    match record(ctx.work, kind, value, reason, ctx.who, OUTCOME_GRANTED, note, backup) {
        Ok(entry) => GrantResult { granted: true, changed, message: note.to_string(), entry },
        Err(error) => GrantResult { granted: true, changed, message: format!("{note} (the journal was not updated: {error})"), entry: Value::Null },
    }
}

/// Releases one thing and journals it. `kind` is `host` (`value` = a network entry such as `dev.example.com GET,POST`) or `cost_limit` (`value` =
/// the new limit in US$). Anything else, an empty reason or an unsafe value is refused, and the refusal is journaled too.
pub fn grant_access(ctx: &AccessContext, kind: &str, value: &str, reason: &str) -> GrantResult {
    let (value, kind_text) = (value.trim(), kind.trim());
    if reason.trim().is_empty() {
        return refuse(ctx, kind_text, value, reason, "a reason is required: say why this access is needed".into());
    }
    match kind_text {
        KIND_HOST => grant_host(ctx, value, reason),
        KIND_COST_LIMIT => grant_cost_limit(ctx, value, reason),
        other => refuse(ctx, other, value, reason, format!("unknown kind '{}': use {}", clean(other), KINDS.join(" or "))),
    }
}

fn grant_host(ctx: &AccessContext, value: &str, reason: &str) -> GrantResult {
    let entry = match parse_entry(&json!(value), "project") {
        Ok(entry) => entry,
        Err(error) => return refuse(ctx, KIND_HOST, value, reason, format!("not a valid network entry: {error}")),
    };
    let effective = network_policy::load(ctx.repo, ctx.data_dir, None);
    if let Some(pattern) = effective.deny.iter().find(|p| overlaps(p, &entry.host)) {
        return refuse(ctx, KIND_HOST, value, reason, format!("{} is on the never-allow list (egress_never {pattern}): production is never released here", entry.host));
    }
    let (current, error) = read_project_entries(Path::new(ctx.repo));
    if !error.is_empty() {
        return refuse(ctx, KIND_HOST, value, reason, format!("{} is unusable ({error}); fix or delete it first", network_policy::NETWORK_LABEL));
    }
    let canonical = network_policy::format_entry(&entry);
    let known = |v: &Value| parse_entry(v, "project").map(|e| network_policy::format_entry(&e)).ok();
    if current.iter().filter_map(known).any(|c| c == canonical) {
        return grant(ctx, KIND_HOST, value, reason, false, &format!("{} was already listed; nothing changed", entry.text), "");
    }
    let backup = match backup_file(ctx.work, &Path::new(ctx.repo).join(".specs").join("project").join("NETWORK.json")) {
        Ok(path) => path,
        Err(error) => return refuse(ctx, KIND_HOST, value, reason, error),
    };
    let mut merged = current;
    merged.push(json!(entry.text));
    if let Err(error) = network_policy::write_project(ctx.repo, &merged) {
        return refuse(ctx, KIND_HOST, value, reason, error.to_string());
    }
    let note = apply_to_worktree(ctx, &format!("{} added to the network list", entry.text));
    grant(ctx, KIND_HOST, value, reason, true, &note, &backup)
}

/// The running loop reads its own copy: bring it up to date without a restart and without removing the worktree.
fn apply_to_worktree(ctx: &AccessContext, note: &str) -> String {
    if crate::setup_files::same_folder(ctx.repo, ctx.work) || !Path::new(ctx.work).is_dir() {
        return note.to_string();
    }
    let refreshed = crate::setup_files::refresh_loop_inputs(ctx.repo, ctx.work, &ctx.data_dir.display().to_string(), ctx.toolkit_root);
    if refreshed.notes.is_empty() {
        format!("{note}; copied into the worktree, no restart needed")
    } else {
        format!("{note}; copied into the worktree ({})", refreshed.notes.join("; "))
    }
}

fn grant_cost_limit(ctx: &AccessContext, value: &str, reason: &str) -> GrantResult {
    let Some(limit) = value.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0) else {
        return refuse(ctx, KIND_COST_LIMIT, value, reason, "the limit must be a number of US$ greater than 0".into());
    };
    let path = loop_path(ctx.work, "meta.json");
    let mut meta = load_json(&path);
    let number = |key: &str| meta.get(key).and_then(Value::as_f64).filter(|v| v.is_finite());
    let (spent, current) = (number("cost_usd").unwrap_or(0.0), number("max_cost_usd"));
    if limit <= spent {
        return refuse(ctx, KIND_COST_LIMIT, value, reason, format!("the new limit must be above the US$ {spent:.2} already spent"));
    }
    if current.is_some_and(|c| limit <= c) {
        return refuse(
            ctx,
            KIND_COST_LIMIT,
            value,
            reason,
            format!("the new limit must be above the one in force (US$ {:.2}); this releases more, never less", current.unwrap_or(0.0)),
        );
    }
    let backup = match backup_file(ctx.work, &path) {
        Ok(path) => path,
        Err(error) => return refuse(ctx, KIND_COST_LIMIT, value, reason, error),
    };
    if let Value::Object(map) = &mut meta {
        map.insert("max_cost_usd".into(), json!(limit));
    }
    if let Err(error) = save_json(&path, &meta) {
        return refuse(ctx, KIND_COST_LIMIT, value, reason, format!("could not write meta.json ({:?})", error.kind()));
    }
    grant(ctx, KIND_COST_LIMIT, value, reason, true, &format!("cost limit set to US$ {limit:.2}; a running loop reads it from the next turn"), &backup)
}

/// Writes a text file next to its backup; used by the baseline generator so every file it replaces is kept.
pub fn write_with_backup(work: &str, target: &Path, text: &str) -> Result<String, String> {
    let backup = backup_file(work, target)?;
    atomic_write_text(target, text).map_err(|e| format!("could not write {} ({:?})", target.display(), e.kind()))?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        repo: String,
        work: String,
        data: std::path::PathBuf,
    }

    fn fixture(network: Option<&str>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let work = dir.path().join("repo-worktree-loop");
        for folder in [&repo, &work] {
            std::fs::create_dir_all(folder.join(".specs/project")).unwrap();
        }
        if let Some(n) = network {
            std::fs::write(repo.join(".specs/project/NETWORK.json"), n).unwrap();
        }
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        Fixture { repo: repo.display().to_string(), work: work.display().to_string(), data, _dir: dir }
    }

    fn ctx(f: &Fixture) -> AccessContext<'_> {
        AccessContext { repo: &f.repo, work: &f.work, data_dir: &f.data, toolkit_root: "", who: "tester" }
    }

    #[test]
    fn a_host_is_released_backed_up_copied_and_journaled() {
        let f = fixture(Some(r#"{"version": 1, "allow": ["a.example.com"]}"#));
        let r = grant_access(&ctx(&f), "host", "dev.example.com GET,POST", "the apps read their settings at start");
        assert!(r.granted && r.changed, "{}", r.message);
        let network = std::fs::read_to_string(Path::new(&f.repo).join(".specs/project/NETWORK.json")).unwrap();
        assert!(network.contains("a.example.com") && network.contains("dev.example.com GET,POST"), "{network}");
        assert!(r.message.contains("copied into the worktree"), "{}", r.message);
        let copy = std::fs::read_to_string(Path::new(&f.work).join(".specs/project/NETWORK.json")).unwrap();
        assert!(copy.contains("dev.example.com"));
        let entry = &r.entry;
        assert_eq!((entry["kind"].as_str(), entry["outcome"].as_str(), entry["who"].as_str()), (Some("host"), Some("granted"), Some("tester")));
        assert!(entry["at"].as_str().unwrap().contains('T'));
        let backup = entry["backup"].as_str().unwrap();
        assert!(backup.starts_with(".specs/project/loop/internal/access-backups/NETWORK.json."), "{backup}");
        let saved = std::fs::read_to_string(Path::new(&f.work).join(backup)).unwrap();
        assert!(saved.contains("a.example.com") && !saved.contains("dev.example.com"));
        assert!(Path::new(&f.work).join(".specs/project/loop/json/PERMISSIONS-LOG.json").is_file());
    }

    #[test]
    fn a_listed_host_is_not_added_twice() {
        let f = fixture(Some(r#"{"allow": ["dev.example.com GET,HEAD"]}"#));
        let r = grant_access(&ctx(&f), "host", "dev.example.com", "again");
        assert!(r.granted && !r.changed, "{}", r.message);
        let (entries, _) = read_project_entries(Path::new(&f.repo));
        assert_eq!(entries.len(), 1);
        assert_eq!(read_journal(&f.work).len(), 1);
    }

    #[test]
    fn every_refusal_is_journaled_and_changes_nothing() {
        let f = fixture(Some(r#"{"allow": ["a.example.com"]}"#));
        std::fs::write(f.data.join("config.json"), r#"{"egress_never": ["*.prod.example.com"]}"#).unwrap();
        let before = std::fs::read_to_string(Path::new(&f.repo).join(".specs/project/NETWORK.json")).unwrap();
        let cases = [
            ("host", "api.prod.example.com", "needed", "never-allow list"),
            ("host", "*.prod.example.com POST", "needed", "never-allow list"),
            ("host", "dev.example.com", "", "a reason is required"),
            ("host", "http://dev.example.com", "needed", "not a valid network entry"),
            ("host", "not a host!", "needed", "not a valid network entry"),
            ("secret", "TOKEN", "needed", "unknown kind"),
            ("cost_limit", "abc", "needed", "greater than 0"),
        ];
        for (index, (kind, value, reason, expected)) in cases.iter().enumerate() {
            let r = grant_access(&ctx(&f), kind, value, reason);
            assert!(!r.granted && r.message.contains(expected), "{kind} {value}: {}", r.message);
            assert_eq!(r.entry["outcome"], "refused");
            assert_eq!(read_journal(&f.work).len(), index + 1);
        }
        assert_eq!(std::fs::read_to_string(Path::new(&f.repo).join(".specs/project/NETWORK.json")).unwrap(), before);
        assert!(!Path::new(&f.work).join(".specs/project/NETWORK.json").exists());
    }

    #[test]
    fn an_unusable_network_file_is_never_overwritten() {
        let f = fixture(Some("{ not json"));
        let r = grant_access(&ctx(&f), "host", "dev.example.com", "needed");
        assert!(!r.granted && r.message.contains("unusable"), "{}", r.message);
        assert_eq!(std::fs::read_to_string(Path::new(&f.repo).join(".specs/project/NETWORK.json")).unwrap(), "{ not json");
    }

    #[test]
    fn the_cost_limit_only_goes_up_and_keeps_a_backup() {
        let f = fixture(None);
        save_json(&loop_path(&f.work, "meta.json"), &json!({"cost_usd": 40.0, "max_cost_usd": 50.0, "turns": 9})).unwrap();
        assert!(!grant_access(&ctx(&f), "cost_limit", "35", "x").granted);
        assert!(!grant_access(&ctx(&f), "cost_limit", "50", "x").granted);
        let r = grant_access(&ctx(&f), "cost_limit", "65", "the E2E phase needs more");
        assert!(r.granted && r.changed, "{}", r.message);
        let meta = load_json(&loop_path(&f.work, "meta.json"));
        assert_eq!((meta["max_cost_usd"].as_f64(), meta["turns"].as_i64()), (Some(65.0), Some(9)));
        let backup = r.entry["backup"].as_str().unwrap();
        assert_eq!(load_json(&Path::new(&f.work).join(backup))["max_cost_usd"], json!(50.0));
        let view = journal_view(&f.work, 2);
        assert_eq!((view["total"].as_i64(), view["granted"].as_i64(), view["refused"].as_i64()), (Some(3), Some(1), Some(2)));
        assert_eq!(view["entries"].as_array().unwrap().len(), 2);
        assert_eq!(view["entries"][0]["id"], 3);
    }

    #[test]
    fn the_journal_never_keeps_a_credential() {
        let f = fixture(None);
        let r = grant_access(&ctx(&f), "host", "dev.example.com", "login failed with PASSWORD=hunter22 and Bearer abcdef123456");
        assert!(r.granted);
        let text = std::fs::read_to_string(Path::new(&f.work).join(".specs/project/loop/json/PERMISSIONS-LOG.json")).unwrap();
        assert!(!text.contains("hunter22") && !text.contains("abcdef123456"), "{text}");
        assert!(text.contains("[redacted]"));
    }

    #[test]
    fn a_missing_or_broken_journal_reads_as_empty() {
        let f = fixture(None);
        assert!(read_journal(&f.work).is_empty());
        std::fs::create_dir_all(Path::new(&f.work).join(".specs/project/loop/json")).unwrap();
        std::fs::write(Path::new(&f.work).join(".specs/project/loop/json/PERMISSIONS-LOG.json"), "{").unwrap();
        assert_eq!(journal_view(&f.work, 5)["total"], 0);
    }
}
