// SPDX-License-Identifier: MIT
//! The simple per-project policy, kept in `PERMISSIONS.json` next to the user's other releases:
//!
//! ```json
//! { "commands": ["migrations", "dev_run", "npm run dev"],
//!   "e2e_dev_baseline": { "enabled": true, "dev_hosts": ["dev.example.com GET,POST"], "production_hosts": ["*.live.example.com"] } }
//! ```
//!
//! * `commands` (default empty = every standard command asks): the command classes or literal commands the loop may run on DEV without asking
//!   (see `guard_core::policy`). Production is never released: an entry that names it is refused here, and the guard refuses a production
//!   target before it looks at this list.
//! * `e2e_dev_baseline` (default off): the project opts in to the DEV E2E baseline. When it is on, the engine applies the baseline (`dev_baseline`)
//!   before each turn with the hosts listed here: localhost and the listed DEV hosts allowed, the listed production hosts denied. Nothing is guessed.
//!
//! Every other key of the file is kept as it is. A change is journaled in `PERMISSIONS-LOG.json` and the file it replaces is backed up.

use crate::access_grant::{self, AccessContext};
use crate::dev_baseline::{self, BaselineRequest};
use crate::loop_io::{atomic_write_text, load_json};
use crate::loop_state::loop_path;
use guard_core::policy::{self, ProjectCommands, CLASS_IDS, MIGRATIONS_ALIAS};
use pycompat::json::py_json_dumps_indent;
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const KIND_POLICY: &str = "policy";
pub const MAX_COMMANDS: usize = 40;
const ENTRY_LIMIT: usize = 80;
const MAX_HOSTS: usize = 40;

static PRODUCTION_WORD: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?<![a-z0-9])prod(?:uction)?(?![a-z0-9])"));

/// The user's file: the one that exists (current layout, then the old one), else the current place.
pub fn permissions_path(root: &str) -> PathBuf {
    let current = loop_path(root, "PERMISSIONS.json");
    if current.exists() {
        return current;
    }
    let legacy = Path::new(root).join(".specs").join("project").join("PERMISSIONS.json");
    if legacy.exists() {
        legacy
    } else {
        current
    }
}

fn read_map(root: &str) -> Map<String, Value> {
    match load_json(&permissions_path(root)) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// What `view` shows and the routes return.
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    pub commands: Vec<String>,
    pub e2e_enabled: bool,
    pub dev_hosts: Vec<String>,
    pub production_hosts: Vec<String>,
}

pub fn read(root: &str) -> Policy {
    let map = read_map(root);
    let parsed = ProjectCommands::from_json(&map);
    let strings = |key: &str| -> Vec<String> {
        map.get("e2e_dev_baseline")
            .and_then(|b| b.get(key))
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default()
    };
    Policy { commands: parsed.commands, e2e_enabled: parsed.e2e_dev_baseline, dev_hosts: strings("dev_hosts"), production_hosts: strings("production_hosts") }
}

/// The policy as JSON for the panel: what asks by default, what the project released, the baseline opt-in.
pub fn view(root: &str) -> Value {
    let policy = read(root);
    json!({
        "default": "ask",
        "classes": CLASS_IDS.iter().map(|id| json!({"id": id, "what": policy::plain(id)})).collect::<Vec<_>>(),
        "alias": {"id": MIGRATIONS_ALIAS, "what": "create and apply database migrations on DEV"},
        "commands": policy.commands,
        "production": "never",
        "e2e_dev_baseline": {"enabled": policy.e2e_enabled, "dev_hosts": policy.dev_hosts, "production_hosts": policy.production_hosts},
    })
}

/// One entry of `commands`, cleaned: a class id, the `migrations` alias or a literal command the guard classifies. Production is refused.
pub fn clean_command(entry: &str) -> Result<String, String> {
    let text = entry.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err("an empty command cannot be released".into());
    }
    if text.chars().count() > ENTRY_LIMIT {
        return Err(format!("'{}...' is too long (at most {ENTRY_LIMIT} characters)", text.chars().take(30).collect::<String>()));
    }
    if PRODUCTION_WORD.is_match(&text) {
        return Err(format!("'{text}' names production: production is never released"));
    }
    let lowered = text.to_lowercase();
    if CLASS_IDS.contains(&lowered.as_str()) || lowered == MIGRATIONS_ALIAS {
        return Ok(lowered);
    }
    if policy::classify_statement(&text).is_none() {
        return Err(format!(
            "'{text}' is not a command the policy asks about; use one of {}, {MIGRATIONS_ALIAS}, or a command such as 'npm run dev' or 'dotnet ef database update'",
            CLASS_IDS.join(", ")
        ));
    }
    Ok(text)
}

fn clean_hosts(items: &[String], label: &str) -> Result<Vec<String>, String> {
    if items.len() > MAX_HOSTS {
        return Err(format!("at most {MAX_HOSTS} {label}"));
    }
    let mut out: Vec<String> = Vec::new();
    for item in items.iter().map(|i| i.trim()).filter(|i| !i.is_empty()) {
        if !out.iter().any(|o| o == item) {
            out.push(item.to_string());
        }
    }
    Ok(out)
}

fn save_map(root: &str, work: &str, map: &Map<String, Value>) -> Result<String, String> {
    let path = permissions_path(root);
    let backup = access_grant::backup_file(work, &path)?;
    let mut text = py_json_dumps_indent(&Value::Object(map.clone()), false, 2);
    text.push('\n');
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    atomic_write_text(&path, &text).map_err(|e| format!("could not write PERMISSIONS.json ({:?})", e.kind()))?;
    Ok(backup)
}

/// A change to the policy: `None` keeps the current value of that part.
#[derive(Clone, Debug, Default)]
pub struct Change {
    pub commands: Option<Vec<String>>,
    pub e2e_enabled: Option<bool>,
    pub dev_hosts: Option<Vec<String>>,
    pub production_hosts: Option<Vec<String>>,
}

/// Writes the change into the work folder's file and, when the repo is another folder with a file of its own, into that one too (the master a new
/// worktree starts from). Journals it. `Err` changes nothing.
pub fn save(ctx: &AccessContext, change: &Change, reason: &str) -> Result<Value, String> {
    let refuse = |why: String| -> Result<Value, String> {
        let _ = access_grant::record(ctx.work, KIND_POLICY, "project policy", reason, ctx.who, access_grant::OUTCOME_REFUSED, &why, "");
        Err(why)
    };
    if reason.trim().is_empty() {
        return refuse("a reason is required: say why the policy changes".into());
    }
    let mut map = read_map(ctx.work);
    if let Some(items) = &change.commands {
        if items.len() > MAX_COMMANDS {
            return refuse(format!("at most {MAX_COMMANDS} commands"));
        }
        let mut cleaned: Vec<String> = Vec::new();
        for item in items {
            match clean_command(item) {
                Ok(c) => {
                    if !cleaned.contains(&c) {
                        cleaned.push(c);
                    }
                }
                Err(e) => return refuse(e),
            }
        }
        map.insert("commands".into(), json!(cleaned));
    }
    if change.e2e_enabled.is_some() || change.dev_hosts.is_some() || change.production_hosts.is_some() {
        let current = read(ctx.work);
        let dev = match &change.dev_hosts {
            Some(v) => clean_hosts(v, "development hosts")?,
            None => current.dev_hosts,
        };
        let production = match &change.production_hosts {
            Some(v) => clean_hosts(v, "production hosts")?,
            None => current.production_hosts,
        };
        let enabled = change.e2e_enabled.unwrap_or(current.e2e_enabled);
        if enabled {
            // a dry check now, so an opt-in that can never be applied is refused when it is made
            let planned = dev_baseline::plan(ctx.repo, ctx.data_dir, &BaselineRequest { dev_hosts: dev.clone(), production_hosts: production.clone() });
            if !planned.problems.is_empty() {
                return refuse(planned.problems.join("; "));
            }
        }
        map.insert("e2e_dev_baseline".into(), json!({"enabled": enabled, "dev_hosts": dev, "production_hosts": production}));
    }
    let backup = match save_map(ctx.work, ctx.work, &map) {
        Ok(b) => b,
        Err(e) => return refuse(e),
    };
    if !crate::setup_files::same_folder(ctx.repo, ctx.work) && Path::new(ctx.repo).join(".specs").is_dir() {
        let _ = save_map(ctx.repo, ctx.work, &map);
    }
    let summary = format!("{} command(s) released, E2E DEV baseline {}", read(ctx.work).commands.len(), if read(ctx.work).e2e_enabled { "on" } else { "off" });
    let _ = access_grant::record(ctx.work, KIND_POLICY, "project policy", reason, ctx.who, access_grant::OUTCOME_GRANTED, &summary, &backup);
    Ok(view(ctx.work))
}

/// Adds one entry to `commands` (what "approve always for this project" does). `Ok(false)` = it was already there.
pub fn add_command(ctx: &AccessContext, entry: &str, reason: &str) -> Result<bool, String> {
    let cleaned = clean_command(entry)?;
    let mut commands = read(ctx.work).commands;
    if commands.iter().any(|c| c.eq_ignore_ascii_case(&cleaned)) {
        return Ok(false);
    }
    commands.push(cleaned);
    save(ctx, &Change { commands: Some(commands), ..Default::default() }, reason).map(|_| true)
}

/// When the project opted in, applies the DEV baseline with the hosts of the opt-in. Idempotent and quiet: nothing is written (and nothing journaled)
/// when the network list and the test-environment document already have it. `None` = not opted in. The line returned is for the loop's log.
pub fn apply_e2e_baseline(ctx: &AccessContext) -> Option<Result<String, String>> {
    let policy = read(ctx.work);
    let policy = if policy.e2e_enabled { policy } else { read(ctx.repo) };
    if !policy.e2e_enabled {
        return None;
    }
    let planned = dev_baseline::plan(ctx.repo, ctx.data_dir, &BaselineRequest { dev_hosts: policy.dev_hosts, production_hosts: policy.production_hosts });
    if planned.problems.is_empty() && planned.added.is_empty() && planned.test_environment.is_none() {
        return Some(Ok("E2E DEV baseline already in place".into()));
    }
    Some(dev_baseline::write(ctx, &planned, "the project opted in to the E2E DEV baseline (PERMISSIONS.json e2e_dev_baseline)").map(|done| done.message))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        repo: String,
        data: PathBuf,
    }

    fn fixture(permissions: Option<&str>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        if let Some(text) = permissions {
            std::fs::write(repo.join(".specs/project/PERMISSIONS.json"), text).unwrap();
        }
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        Fixture { repo: repo.display().to_string(), data, _dir: dir }
    }

    fn ctx(f: &Fixture) -> AccessContext<'_> {
        AccessContext { repo: &f.repo, work: &f.repo, data_dir: &f.data, toolkit_root: "", who: "tester" }
    }

    #[test]
    fn the_default_policy_asks_and_production_is_never_released() {
        let f = fixture(None);
        let v = view(&f.repo);
        assert_eq!((v["default"].as_str(), v["production"].as_str(), v["commands"].as_array().unwrap().len()), (Some("ask"), Some("never"), 0));
        assert_eq!(v["e2e_dev_baseline"]["enabled"], false);
        for bad in ["", "ls", "rm -rf x", "dotnet ef database update --environment Production", "prod", "NODE_ENV=production npm run dev"] {
            assert!(clean_command(bad).is_err(), "{bad}");
        }
        assert_eq!(clean_command("  Migrations ").unwrap(), "migrations");
        assert_eq!(clean_command("npm   run dev").unwrap(), "npm run dev");
        assert!(clean_command(&format!("npm run dev {}", "x".repeat(90))).is_err());
    }

    #[test]
    fn saving_keeps_other_keys_backs_up_and_journals() {
        let f = fixture(Some(r#"{"allow": ["x"], "deny_bash_regex": ["rm"]}"#));
        let change = Change { commands: Some(vec!["migrations".into(), "npm run dev".into(), "migrations".into()]), ..Default::default() };
        assert!(save(&ctx(&f), &change, "  ").unwrap_err().contains("reason is required"));
        let out = save(&ctx(&f), &change, "the team always runs these on DEV").unwrap();
        assert_eq!(out["commands"], json!(["migrations", "npm run dev"]));
        let file: Value = serde_json::from_str(&std::fs::read_to_string(permissions_path(&f.repo)).unwrap()).unwrap();
        assert_eq!((file["allow"].clone(), file["deny_bash_regex"].clone()), (json!(["x"]), json!(["rm"])));
        let journal = access_grant::read_journal(&f.repo);
        assert_eq!((journal[0]["outcome"].as_str(), journal[1]["outcome"].as_str(), journal[1]["kind"].as_str()), (Some("refused"), Some("granted"), Some("policy")));
        assert!(!journal[1]["backup"].as_str().unwrap().is_empty());
        let refused =
            save(&ctx(&f), &Change { commands: Some(vec!["dotnet ef database update --environment production".into()]), ..Default::default() }, "x").unwrap_err();
        assert!(refused.contains("production is never released"), "{refused}");
        assert_eq!(read(&f.repo).commands, vec!["migrations", "npm run dev"], "a refused change changes nothing");
    }

    #[test]
    fn approving_always_adds_one_entry_once() {
        let f = fixture(None);
        assert!(add_command(&ctx(&f), "dev_run", "approved in the panel").unwrap());
        assert!(!add_command(&ctx(&f), "DEV_RUN", "again").unwrap());
        assert_eq!(read(&f.repo).commands, vec!["dev_run"]);
        assert!(add_command(&ctx(&f), "ls", "x").is_err());
    }

    #[test]
    fn the_baseline_is_applied_only_when_the_project_opted_in_and_never_for_production() {
        let off = fixture(None);
        assert!(apply_e2e_baseline(&ctx(&off)).is_none());
        let f = fixture(None);
        let on = Change {
            e2e_enabled: Some(true),
            dev_hosts: Some(vec!["dev.example.com GET,POST".into()]),
            production_hosts: Some(vec!["prod.example.com".into(), "*.live.example.com".into()]),
            ..Default::default()
        };
        save(&ctx(&f), &on, "E2E runs against DEV").unwrap();
        let done = apply_e2e_baseline(&ctx(&f)).unwrap().unwrap();
        assert!(done.contains("development entries added"), "{done}");
        let network = std::fs::read_to_string(Path::new(&f.repo).join(".specs/project/NETWORK.json")).unwrap();
        assert!(network.contains("dev.example.com") && network.contains("localhost") && !network.contains("prod.example.com"), "{network}");
        let again = apply_e2e_baseline(&ctx(&f)).unwrap().unwrap();
        assert_eq!(again, "E2E DEV baseline already in place");
        // a development host that reaches a production host is refused when the opt-in is made
        let g = fixture(None);
        let bad = Change {
            e2e_enabled: Some(true),
            dev_hosts: Some(vec!["svc.live.example.com".into()]),
            production_hosts: Some(vec!["*.live.example.com".into()]),
            ..Default::default()
        };
        let err = save(&ctx(&g), &bad, "x").unwrap_err();
        assert!(err.contains("production host"), "{err}");
        assert!(!read(&g.repo).e2e_enabled);
    }
}
