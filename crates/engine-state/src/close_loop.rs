// SPDX-License-Identifier: MIT
//! Close loop: finish a loop safely. Runs OUTSIDE `claude -p` (the app calls it on the user's click), never pushes anything.
//!
//! `can_close` per repo: the loop is not running, the worktree exists, the push of `loop/*` is confirmed (delivery.json `push` starts with
//! `done` AND the remote branch holds the worktree HEAD), a PR exists (`PR created` / `PR already existed`), the tree is clean outside
//! `.specs/` and no copied file changed (worktree and source still match the hash recorded in meta.json). A compare link without PR is allowed
//! only with the explicit `confirm_compare_link`; nothing else is ever lifted.
//!
//! `close` order, fixed: (a) verified backup of each worktree, (b) the DURABLE `.specs/` files go back to the primary repo (ROADMAP with
//! statuses, FLOWS, STATE, features/*/spec|design|tasks, phases/, loop/VALIDATE.md, the summary; never loop/internal, never loop/json), (c)
//! `last-run.json`, (d) `git worktree remove` WITHOUT --force (only `--force-remove` forces it; non-primary first, the primary, which holds
//! `.specs/`, last and only if (b) and (c) worked). Local branches and commits stay. One repo failing never stops the others.

use crate::loop_io::{copy2, copy_tree, git, load_json, lock_holder_alive, run_timeout, save_json, status_rows};
use crate::loop_state::loop_path;
use crate::project_repos::{load_repos, primary_of};
use crate::{loop_summary, prod_database_script};
use cfg::Env;
use pycompat::{nt_abspath, nt_normcase, nt_realpath};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const LOOP_BRANCH_PREFIX: &str = "loop/";
pub const LOOP_WORKTREE_FOLDER_MARK: &str = "-worktree-loop";
/// Same as the panel's worktree service.
const FOLDERS_OUTSIDE_BACKUP: [&str; 4] = ["node_modules", "bin", "obj", ".git"];
const PR_CONFIRMED: [&str; 2] = ["PR created", "PR already existed"];
const DURABLE_PROJECT_FILES: [&str; 4] = ["project/ROADMAP.md", "project/FLOWS.md", "project/STATE.md", "project/loop/VALIDATE.md"];
const DURABLE_FEATURE_FILES: [&str; 3] = ["spec.md", "design.md", "tasks.md"];
const RUN_TOTAL_KEYS: [&str; 3] = ["cost_usd", "max_cost_usd", "turns"];
const REMOTE_TIMEOUT_SECONDS: u64 = 60;
const DEFAULT_MAX_COST_USD: f64 = 50.0;
const REASON_LIMIT: usize = 200;
/// The only place the engine writes into a secondary repo's worktree.
const ENGINE_SPECS_FOLDER: &str = ".specs\\project\\loop\\json";
/// The loop's run state in the PRIMARY worktree: written by the engine and the orchestrator, saved by the verified backup.
const ENGINE_RUN_STATE_FOLDER: &str = ".specs\\project\\loop";
/// Local files the engine itself writes into the worktree on every start and turn: engine-owned, they never block the removal.
const ENGINE_GENERATED_IGNORED: [&str; 4] = [".specs/project/.env.test", ".specs/project/NETWORK.json", ".specs/project/loop/internal/network-filter.js", ".specs/project/loop/internal/network-allow.json"];

fn reason_text(code: &str) -> String {
    match code {
        "loop_running" => "The loop is still running. Stop it or wait for it to finish.",
        "already_closed" => "Already closed: an earlier close removed this worktree (see last-run.json). Nothing left to do.",
        "worktree_missing" => "The worktree folder does not exist (already removed, or never created).",
        "push_unconfirmed" => "The loop branch is not confirmed on the remote (push not done, failed, or the remote does not hold the last commit). Deliver first.",
        "pr_missing" => "No PR was created for this repo. Deliver again, or confirm to close with only the compare link.",
        "pr_compare_link" => "Only a compare link exists; no PR was created. Confirm to close anyway.",
        "tree_dirty" => "The worktree has uncommitted changes; it was kept.",
        "copied_changed" => "A copied configuration file changed (in the worktree or in the original folder); the worktree was kept.",
        other => other,
    }
    .to_string()
}

fn reason(code: &str, text: &str) -> Value {
    json!({"code": code, "text": if text.is_empty() { reason_text(code) } else { text.to_string() }})
}

fn is_same_path(first: &str, second: &str) -> bool {
    nt_normcase(&nt_realpath(first)) == nt_normcase(&nt_realpath(second))
}

fn loop_is_running(primary_work: &str) -> bool {
    lock_holder_alive(&loop_path(primary_work, "LOOP.lock"))
}

/// `sha256:<hex>` of a file (the format meta.json records), `None` when it cannot be read.
fn file_hash(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| format!("sha256:{}", Sha256::digest(&b).iter().map(|x| format!("{x:02x}")).collect::<String>()))
}

fn last_run_path(primary_repo: &str) -> PathBuf {
    loop_path(primary_repo, "last-run.json")
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn timestamp() -> String {
    crate::loop_io::now_iso()
}

// ---------------------------------------------------------------- what the project looks like

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub repo_path: String,
    pub primary: bool,
    pub worktree: String,
    pub branch: String,
    pub base: String,
    pub copied: Map<String, Value>,
    pub copy: Vec<String>,
    pub closed_before: bool,
}

/// The loop worktrees come from the primary's meta.json `repos` (what the loop really created); a single-repo project (no `repos` key) is one
/// entry built from the top-level fields, so it closes exactly like a multi-repo one with one repo.
pub fn project_entries(primary_repo: &str) -> (Vec<Entry>, Value) {
    let primary_repo = nt_abspath(primary_repo);
    let repos = load_repos(&primary_repo);
    let primary = primary_of(&repos).clone();
    let meta = load_json(&loop_path(&primary.loop_worktree_path(), "meta.json"));
    let mut recorded_repos = meta.get("repos").and_then(Value::as_object).cloned().unwrap_or_default();
    let last_run_repos = load_json(&last_run_path(&primary_repo)).get("repos").and_then(Value::as_object).cloned().unwrap_or_default();
    let meta_empty = meta.as_object().is_none_or(|m| m.is_empty());
    if meta_empty {
        recorded_repos = last_run_repos.clone(); // the primary worktree is already gone: the last close left the record of every repo
    }
    let mut entries = Vec::new();
    for repo in &repos {
        if !repo.primary && !(crate::notify_truthy(recorded_repos.get(&repo.name)) || crate::notify_truthy(last_run_repos.get(&repo.name)) || Path::new(&repo.loop_worktree_path()).is_dir()) {
            continue; // a `reference` repo, or an edit repo no feature changed, never got a worktree: nothing to close, merge or remove
        }
        let recorded: Value = if !recorded_repos.is_empty() {
            recorded_repos.get(&repo.name).cloned().unwrap_or(json!({}))
        } else if repo.primary {
            json!({"worktree": meta.get("worktree"), "branch": meta.get("branch"), "base": meta.get("base")})
        } else {
            json!({})
        };
        let copied = recorded.get("copied").and_then(Value::as_object).cloned().unwrap_or_default();
        let worktree = Some(s(&recorded, "worktree")).filter(|w| !w.is_empty()).unwrap_or_else(|| repo.loop_worktree_path());
        entries.push(Entry {
            name: repo.name.clone(),
            repo_path: repo.path.clone(),
            primary: repo.primary,
            worktree: nt_abspath(&worktree),
            branch: s(&recorded, "branch"),
            base: s(&recorded, "base"),
            copied,
            copy: repo.copy.clone(),
            closed_before: last_run_repos.get(&repo.name).is_some_and(|r| crate::notify_truthy(r.get("worktree_removed"))),
        });
    }
    (entries, meta)
}

/// True when `origin` has `branch` at the same commit as the worktree HEAD (asked to the remote, not to a local ref).
fn remote_holds_head(worktree: &str, branch: &str) -> bool {
    let (code, head, _) = git(worktree, &["rev-parse", "HEAD"]);
    if code != 0 || head.is_empty() {
        return false;
    }
    let (code, out, _) = run_timeout("git", &["-C", worktree, "ls-remote", "--heads", "origin", &format!("refs/heads/{branch}")], None, REMOTE_TIMEOUT_SECONDS);
    code == 0 && out.split_whitespace().next() == Some(head.as_str())
}

/// Uncommitted changes outside `.specs/` (the loop's own state is expected to be there).
fn tree_is_dirty(worktree: &str) -> bool {
    let (code, out, _) = git(worktree, &["status", "--porcelain", "--untracked-files=all", "--", ".", ":(exclude).specs"]);
    code != 0 || !out.trim().is_empty()
}

/// Names of the copied files whose worktree copy or whose source no longer matches the hash recorded at copy time.
fn copied_files_changed(entry: &Entry) -> Vec<String> {
    let mut names: Vec<&String> = entry.copied.keys().collect();
    names.sort();
    names
        .into_iter()
        .filter(|name| {
            let recorded = entry.copied[*name].as_str().map(String::from);
            file_hash(&Path::new(&entry.worktree).join(name)) != recorded || file_hash(&Path::new(&entry.repo_path).join(name)) != recorded
        })
        .cloned()
        .collect()
}

/// The `repos[]` item of the check payload.
pub fn check_repo(entry: &Entry, running: bool) -> Value {
    let delivery = load_json(&loop_path(&entry.worktree, "delivery.json"));
    let mut branch = if Path::new(&entry.worktree).is_dir() { git(&entry.worktree, &["branch", "--show-current"]).1 } else { String::new() };
    if branch.is_empty() {
        branch = entry.branch.clone();
    }
    let or_else = |v: Option<&Value>, default: &str| -> String { v.and_then(Value::as_str).filter(|x| !x.is_empty()).unwrap_or(default).to_string() };
    let base = Some(or_else(delivery.get("base"), &entry.base)).filter(|b| !b.is_empty());
    let pr_status = or_else(delivery.get("pr_status"), "not created");
    let push = or_else(delivery.get("push"), "not done");
    let pr = delivery.get("pr").cloned().unwrap_or(Value::Null);
    let mut reasons: Vec<Value> = Vec::new();
    if running {
        reasons.push(reason("loop_running", ""));
    }
    let mut item = json!({"name": entry.name, "can_close": false, "reasons": [], "needs_confirm": false, "branch": if branch.is_empty() { Value::Null } else { json!(branch) },
                          "base": base, "pr": pr, "pr_status": pr_status, "push": push});
    if !Path::new(&entry.worktree).is_dir() {
        reasons.push(if entry.closed_before { reason("already_closed", "") } else { reason("worktree_missing", "") });
        item["reasons"] = Value::Array(reasons);
        return item;
    }
    if !branch.starts_with(LOOP_BRANCH_PREFIX) {
        reasons.push(reason("push_unconfirmed", &format!("The worktree is not on a loop/* branch ({}); it is never closed from here.", if branch.is_empty() { "detached HEAD" } else { &branch })));
    } else if !(push.starts_with("done") && remote_holds_head(&entry.worktree, &branch)) {
        reasons.push(reason("push_unconfirmed", ""));
    }
    let compare_link = pr_status.starts_with("compare link") && crate::notify_truthy(Some(&pr));
    if !PR_CONFIRMED.contains(&pr_status.as_str()) {
        reasons.push(reason("pr_missing", &if compare_link { reason_text("pr_compare_link") } else { String::new() }));
    }
    if tree_is_dirty(&entry.worktree) {
        reasons.push(reason("tree_dirty", ""));
    }
    let changed = copied_files_changed(entry);
    if !changed.is_empty() {
        reasons.push(reason("copied_changed", &format!("{} ({})", reason_text("copied_changed"), changed.join(", "))));
    }
    let only_pr = !reasons.is_empty() && reasons.iter().all(|r| r["code"] == "pr_missing");
    item["needs_confirm"] = json!(only_pr && compare_link);
    item["can_close"] = json!(reasons.is_empty());
    item["reasons"] = Value::Array(reasons);
    item
}

/// Read only. `{"ok", "can_close", "loop_finished", "repos": [...]}`; top-level can_close is true only when every repo is.
pub fn can_close(primary_repo: &str) -> Value {
    let (entries, meta) = project_entries(primary_repo);
    let primary_work = entries.iter().find(|e| e.primary).map(|e| e.worktree.clone()).unwrap_or_default();
    let running = loop_is_running(&primary_work);
    let repos: Vec<Value> = entries.iter().map(|e| check_repo(e, running)).collect();
    let all = repos.iter().all(|r| r["can_close"] == true);
    json!({"ok": true, "can_close": all, "loop_finished": crate::notify_truthy(meta.get("finished_at")) && !running, "repos": repos})
}

// ---------------------------------------------------------------- backup

fn count_backup_files(folder: &Path) -> usize {
    let mut total = 0;
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !FOLDERS_OUTSIDE_BACKUP.contains(&entry.file_name().to_string_lossy().as_ref()) {
                total += count_backup_files(&path);
            }
        } else {
            total += 1;
        }
    }
    total
}

/// `<output_dir>/loop-<name>-<stamp>/` with the summary and a copy of `.specs/`, checked by file count. Returns `(folder, error)`: error is
/// `None` when the backup is verified; nothing is deleted by this function either way.
pub fn back_up_worktree(env: &Env, worktree: &str, output_dir: &str) -> (String, Option<String>) {
    let base_name = pycompat::last_segment(worktree);
    let name = match base_name.find(LOOP_WORKTREE_FOLDER_MARK) {
        Some(i) if i > 0 => &base_name[..i],
        Some(_) => "project",
        None => base_name,
    };
    let stamp = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc()).format(time::macros::format_description!("[year]-[month]-[day]-[hour][minute]")).unwrap_or_default();
    let base = Path::new(&nt_abspath(output_dir)).join(format!("loop-{name}-{stamp}"));
    let (mut folder, mut suffix) = (base.clone(), 1);
    while folder.exists() {
        suffix += 1;
        folder = PathBuf::from(format!("{}-{suffix}", base.display()));
    }
    let shown = folder.display().to_string();
    let failed = |message: String| (shown.clone(), Some(message));
    if let Err(e) = std::fs::create_dir_all(&folder) {
        return failed(format!("backup failed: {}", e.to_string().chars().take(200).collect::<String>()));
    }
    let (code, out) = loop_summary::run_cli(&[worktree.to_string(), "--out".into(), shown.clone()], env);
    if code != 0 {
        let tail: String = { let c: Vec<char> = out.chars().collect(); c[c.len().saturating_sub(200)..].iter().collect() };
        return failed(format!("backup failed: loop_summary.py did not finish cleanly ({})", if tail.is_empty() { "no output".to_string() } else { tail }));
    }
    let has_md = std::fs::read_dir(&folder).map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().to_lowercase().ends_with(".md"))).unwrap_or(false);
    if !has_md {
        return failed(format!("backup failed: the summary was not generated in {shown}"));
    }
    let source = Path::new(worktree).join(".specs");
    if source.is_dir() {
        let destination = folder.join("specs");
        if let Err(e) = copy_tree(&source, &destination, &FOLDERS_OUTSIDE_BACKUP) {
            return failed(format!("backup failed: {}", e.to_string().chars().take(200).collect::<String>()));
        }
        let (a, b) = (count_backup_files(&source), count_backup_files(&destination));
        if a != b {
            return failed(format!("backup failed: the .specs copy does not match ({a} x {b} files)"));
        }
    }
    (shown, None)
}

// ---------------------------------------------------------------- durable .specs return

fn walk_files(folder: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() { walk_files(&path, out) } else { out.push(path) }
    }
}

/// Paths (relative to `.specs/`, forward slashes) of the plan that must outlive the worktree. Never loop/internal or loop/json.
pub fn durable_files(primary_work: &str) -> Vec<String> {
    let specs = Path::new(primary_work).join(".specs");
    let mut found: Vec<String> = DURABLE_PROJECT_FILES.iter().filter(|p| p.split('/').fold(specs.clone(), |a, x| a.join(x)).is_file()).map(|p| p.to_string()).collect();
    for name in DURABLE_FEATURE_FILES {
        let mut paths: Vec<String> = std::fs::read_dir(specs.join("features")).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.join(name).is_file()).map(|p| format!("features/{}/{name}", p.file_name().unwrap_or_default().to_string_lossy())).collect()).unwrap_or_default();
        paths.sort();
        found.extend(paths);
    }
    let mut phases: Vec<PathBuf> = Vec::new();
    walk_files(&specs.join("project").join("phases"), &mut phases);
    let mut relative: Vec<String> = phases.iter().filter_map(|p| p.strip_prefix(&specs).ok()).map(|p| p.display().to_string().replace('\\', "/")).collect();
    relative.sort();
    found.extend(relative);
    found
}

/// Copies the durable plan files, plus the summary the backup produced, into the primary repo's `.specs/`. Returns the relative list.
fn copy_durable_files(primary_work: &str, primary_repo: &str, backup_folder: &str) -> std::io::Result<Vec<String>> {
    let mut copied = Vec::new();
    for relative in durable_files(primary_work) {
        let source = relative.split('/').fold(Path::new(primary_work).join(".specs"), |a, x| a.join(x));
        let target = relative.split('/').fold(Path::new(primary_repo).join(".specs"), |a, x| a.join(x));
        std::fs::create_dir_all(target.parent().expect("has parent"))?;
        copy2(&source, &target)?;
        copied.push(relative);
    }
    if !backup_folder.is_empty() {
        let mut names: Vec<String> = std::fs::read_dir(backup_folder)?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        for name in names.iter().filter(|n| n.starts_with("loop-summary") && n.ends_with(".md")) {
            let target = Path::new(primary_repo).join(".specs").join("project").join("loop").join(name);
            std::fs::create_dir_all(target.parent().expect("has parent"))?;
            copy2(&Path::new(backup_folder).join(name), &target)?;
            copied.push(format!("project/loop/{name}"));
        }
    }
    Ok(copied)
}

// ---------------------------------------------------------------- last-run.json

/// `--max-cost` of the saved loop command (LOOP-CMD.json argv, `--max-cost N` or `--max-cost=N`); `None` when absent or unreadable.
fn command_max_cost(primary_work: &str) -> Option<f64> {
    let data = load_json(&loop_path(primary_work, "LOOP-CMD.json"));
    let argv = data.get("argv").and_then(Value::as_array)?;
    for (index, item) in argv.iter().enumerate() {
        let text = item.as_str().map(String::from).unwrap_or_else(|| item.to_string());
        let (name, _, joined) = pycompat::partition(&text, "=");
        if name == "--max-cost" {
            let raw = if joined.is_empty() { argv.get(index + 1).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default() } else { joined.to_string() };
            return raw.trim().parse::<f64>().ok();
        }
    }
    None
}

/// The cap in force: `meta.max_cost_usd` (raised in the panel) when above 0, else the loop command's `--max-cost`, else 50.
fn effective_max_cost(meta: &Value, primary_work: &str) -> f64 {
    let raised = match meta.get("max_cost_usd") {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(t)) => t.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    };
    if raised > 0.0 {
        return raised;
    }
    (if primary_work.is_empty() { None } else { command_max_cost(primary_work) }).unwrap_or(DEFAULT_MAX_COST_USD)
}

/// cost_usd, max_cost_usd (the EFFECTIVE cap) and turns of the run: from the primary worktree's meta.json, else what the last record kept.
fn run_totals(meta: &Value, prior: &Value, primary_work: &str) -> Vec<(String, Value)> {
    let has_meta = meta.as_object().is_some_and(|m| !m.is_empty());
    let mut totals: Vec<(String, Value)> = RUN_TOTAL_KEYS
        .iter()
        .map(|key| {
            let value = meta.get(*key).filter(|v| !v.is_null()).cloned().unwrap_or_else(|| prior.get(*key).cloned().unwrap_or(Value::Null));
            (key.to_string(), value)
        })
        .collect();
    if has_meta {
        if let Some(slot) = totals.iter_mut().find(|(k, _)| k == "max_cost_usd") {
            slot.1 = json!(effective_max_cost(meta, primary_work));
        }
    }
    totals
}

/// `<primary repo>/.specs/project/loop/json/last-run.json`. `removed` = names whose worktree is gone (or about to be). `finished` (wrap-up
/// Finish) marks the run and every repo entry `"finished": true`: the next start does NOT re-attach to that branch.
fn write_last_run(primary_repo: &str, entries: &[Entry], checks: &[(String, Value)], removed: &[String], durable: &[String], meta: &Value, primary_work: &str, finished: bool) {
    let prior = load_json(&last_run_path(primary_repo));
    let prior_repos = prior.get("repos").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut repos = Map::new();
    for entry in entries {
        let check = &checks.iter().find(|(n, _)| *n == entry.name).expect("check").1;
        if !Path::new(&entry.worktree).is_dir() && prior_repos.contains_key(&entry.name) {
            // closed before: keep what was recorded then
            let mut kept = prior_repos[&entry.name].clone();
            kept["worktree_removed"] = json!(true);
            repos.insert(entry.name.clone(), kept);
            continue;
        }
        let branch = Some(s(check, "branch")).filter(|b| !b.is_empty()).unwrap_or_else(|| entry.branch.clone());
        let head = if branch.is_empty() { String::new() } else { git(&entry.repo_path, &["rev-parse", "--short=10", &branch]).1 };
        let mut copied: Vec<String> = entry.copied.keys().cloned().collect();
        copied.sort();
        if copied.is_empty() {
            copied = entry.copy.clone();
        }
        repos.insert(
            entry.name.clone(),
            json!({"path": entry.repo_path, "branch": branch, "base": if entry.base.is_empty() { check["base"].clone() } else { json!(entry.base) }, "head": head, "pr": check["pr"],
                   "worktree": entry.worktree, "worktree_removed": removed.contains(&entry.name) || !Path::new(&entry.worktree).is_dir(), "copied": copied}),
        );
    }
    if finished {
        for item in repos.values_mut() {
            item["finished"] = json!(true);
        }
    }
    let durable_value = if durable.is_empty() { prior.get("durable_files").filter(|v| crate::notify_truthy(Some(*v))).cloned().unwrap_or(json!([])) } else { json!(durable) };
    let mut record = Map::new();
    record.insert("closed_at".into(), json!(timestamp()));
    record.insert("multi".into(), json!(entries.len() > 1));
    record.insert("repos".into(), Value::Object(repos));
    record.insert("durable_files".into(), durable_value);
    if finished {
        record.insert("finished".into(), json!(true));
    }
    for (k, v) in run_totals(meta, &prior, primary_work) {
        record.insert(k, v);
    }
    let _ = save_json(&last_run_path(primary_repo), &Value::Object(record));
}

// ---------------------------------------------------------------- production database script

fn write_prod_database_script(env: &Env, primary_repo: &str, primary_work: &str, entries: &[Entry], checks: &[(String, Value)], output_dir: &str) -> Value {
    let script_entries: Vec<prod_database_script::Entry> = entries
        .iter()
        .filter(|e| Path::new(&e.worktree).is_dir())
        .map(|e| {
            let check = &checks.iter().find(|(n, _)| *n == e.name).expect("check").1;
            prod_database_script::Entry {
                name: e.name.clone(),
                root: e.worktree.clone(),
                branch: Some(s(check, "branch")).filter(|b| !b.is_empty()).unwrap_or_else(|| e.branch.clone()),
                base: if e.base.is_empty() { s(check, "base") } else { e.base.clone() },
                primary: e.primary,
                repo_path: e.repo_path.clone(),
            }
        })
        .collect();
    let roles = prod_database_script::read_roles(&[primary_repo]);
    let name = pycompat::last_segment(&nt_abspath(primary_repo)).to_string();
    prod_database_script::generate(env, &name, &script_entries, primary_work, output_dir, None, None, &roles, &[primary_repo])
}

// ---------------------------------------------------------------- close

/// `text` cut at a word boundary with an ellipsis (never mid-sentence); unchanged when it fits.
fn cut_reason(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let head: String = flat.chars().take(limit).collect();
    let cut = head.rsplit_once(' ').map(|x| x.0).unwrap_or(&head).trim_end_matches([' ', ',', ';', ':', '.', '-']).to_string();
    format!("{}...", if cut.is_empty() { head } else { cut })
}

/// True when `relative` is an engine-written copy or is git-ignored in the worktree AND the same path in the original repo holds identical
/// bytes (a local file copied in). Removing it loses nothing: the repo keeps its copy, the backup has it.
fn is_ignored_copy_of_repo_file(worktree: &str, repo: &str, relative: &str) -> bool {
    if ENGINE_GENERATED_IGNORED.contains(&relative.replace('\\', "/").as_str()) {
        return true;
    }
    if git(worktree, &["check-ignore", "-q", "--", relative]).0 != 0 || repo.is_empty() {
        return false;
    }
    match (std::fs::read(Path::new(worktree).join(relative)), std::fs::read(Path::new(repo).join(relative))) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Relative path of the first thing in the worktree that is NOT engine-owned `.specs/` state, or `None` when the only untracked content is
/// `.specs/` holding nothing but `*.json` under `.specs/project/loop/json/`. `saved_plan` = paths of durable plan files already copied back
/// to the repository and verified identical there: they are saved work, so modified or untracked they do not count as foreign.
fn foreign_content_in_specs(worktree: &str, saved_plan: &[String], repo: &str) -> Option<String> {
    let saved: Vec<String> = saved_plan.iter().map(|p| p.replace('\\', "/")).collect();
    let (code, rows, err) = status_rows(worktree, &["--untracked-files=all"]);
    if code != 0 {
        return Some(format!("(git status failed: {})", cut_reason(if err.is_empty() { "no detail" } else { &err }, 80)));
    }
    for row in rows {
        let path = row.path.replace('\\', "/");
        if saved.contains(&path) {
            continue;
        }
        if row.status != "??" || !path.starts_with(".specs/") {
            return Some(row.path);
        }
    }
    // Tracked files with no local change are content of the branch, not foreign: git removes them with the worktree.
    let tracked: Vec<String> = git(worktree, &["ls-files", "--", ".specs"]).1.lines().filter(|l| !l.trim().is_empty()).map(|l| l.replace('\\', "/")).collect();
    let mut files = Vec::new();
    walk_files(&Path::new(worktree).join(".specs"), &mut files);
    for full in files {
        let relative = full.strip_prefix(worktree).unwrap_or(&full).display().to_string();
        let unix = relative.replace('\\', "/");
        if saved.contains(&unix) || tracked.contains(&unix) || is_ignored_copy_of_repo_file(worktree, repo, &relative) {
            continue;
        }
        let normal = nt_normcase(&relative);
        if normal.starts_with(&format!("{}\\", nt_normcase(ENGINE_RUN_STATE_FOLDER))) {
            continue;
        }
        let dir = nt_normcase(relative.rsplit_once('\\').map(|x| x.0).unwrap_or(""));
        let name = full.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        if !(dir.starts_with(&nt_normcase(ENGINE_SPECS_FOLDER)) && name.ends_with(".json")) {
            return Some(unix);
        }
    }
    None
}

/// Called only AFTER the backup was verified. When the content of the worktree is exclusively engine-owned `.specs/` state plus already
/// saved plan files, deletes that state so the normal `git worktree remove` can run, WITHOUT --force. Returns the blocking path (nothing
/// deleted) or `None`.
fn clear_engine_specs(worktree: &str, saved_plan: &[String], repo: &str) -> Option<String> {
    let specs = Path::new(worktree).join(".specs");
    if !specs.is_dir() {
        return None;
    }
    let (code, out, _) = git(worktree, &["status", "--porcelain", "--untracked-files=all", "--", ".specs"]);
    if code == 0 && out.trim().is_empty() {
        return None; // gitignored (or tracked and clean): git removes it by itself
    }
    if let Some(blocker) = foreign_content_in_specs(worktree, saved_plan, repo) {
        return Some(blocker);
    }
    if !git(worktree, &["ls-files", "--", ".specs"]).1.trim().is_empty() {
        for command in [&["checkout", "--", ".specs"][..], &["clean", "-fdxq", "--", ".specs"][..]] {
            let (code, out, failure) = git(worktree, command);
            if code != 0 {
                return Some(format!("{} ({})", specs.display(), cut_reason(if !failure.is_empty() { &failure } else if !out.is_empty() { &out } else { "no detail" }, 80)));
            }
        }
        return None;
    }
    match std::fs::remove_dir_all(&specs) {
        Ok(()) => None,
        Err(failure) => Some(format!("{} ({})", specs.display(), cut_reason(&failure.to_string(), 80))),
    }
}

/// `git worktree remove` from the user's repo, no --force unless asked. Returns an error text, or `None` when the folder is gone.
fn remove_worktree(entry: &Entry, force: bool, saved_plan: &[String]) -> Option<String> {
    if is_same_path(&entry.worktree, &entry.repo_path) {
        return Some("refused: the worktree is the original repo folder".into()); // belt and braces: never the user's own folder
    }
    if !force {
        if let Some(blocker) = clear_engine_specs(&entry.worktree, saved_plan, &entry.repo_path) {
            return Some(format!("the worktree was kept: {blocker} holds content that is not engine-owned .specs/ state, so it was not deleted (backup done; remove it by hand or use force)"));
        }
    }
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&entry.worktree);
    let (_, out, err) = git(&entry.repo_path, &args);
    git(&entry.repo_path, &["worktree", "prune"]);
    if Path::new(&entry.worktree).is_dir() {
        return Some(format!("git worktree remove failed for {}, the folder is still there: {}", entry.worktree, cut_reason(if !err.is_empty() { &err } else if !out.is_empty() { &out } else { "no detail" }, REASON_LIMIT)));
    }
    None
}

/// Worktree-relative paths of the durable plan files that were copied back AND still match the copy in the primary repo (hash), so a
/// modified tracked plan file in the worktree is known to be saved.
fn verified_plan(worktree: &str, primary_repo: &str, durable: &[String]) -> Vec<String> {
    durable
        .iter()
        .filter(|relative| {
            let source = relative.split('/').fold(Path::new(worktree).join(".specs"), |a, x| a.join(x));
            let target = relative.split('/').fold(Path::new(primary_repo).join(".specs"), |a, x| a.join(x));
            source.is_file() && file_hash(&source).is_some() && file_hash(&source) == file_hash(&target)
        })
        .map(|relative| format!(".specs/{relative}"))
        .collect()
}

fn only_pr_missing(check: &Value) -> bool {
    let reasons = check["reasons"].as_array().cloned().unwrap_or_default();
    !reasons.is_empty() && reasons.iter().all(|r| r["code"] == "pr_missing")
}

/// `force_remove` lifts every check reason of a repo whose worktree exists, EXCEPT a worktree outside `loop/*` (never closed from here).
fn is_forceable(entry: &Entry, check: &Value, force_remove: bool) -> bool {
    force_remove && Path::new(&entry.worktree).is_dir() && check["branch"].as_str().unwrap_or("").starts_with(LOOP_BRANCH_PREFIX)
}

/// Options of [`close`].
#[derive(Clone, Debug, Default)]
pub struct CloseOptions {
    pub confirm_compare_link: bool,
    /// Repo names to close (`None` = all).
    pub only: Option<Vec<String>>,
    pub output_dir: String,
    pub force_remove: bool,
    pub allow_missing_pr: bool,
    pub finished: bool,
}

/// Closes the repos that may close; the others are kept with their reasons. Per-repo outcome in the body, never a failure.
pub fn close(env: &Env, primary_repo: &str, o: &CloseOptions) -> Value {
    let primary_repo = nt_abspath(primary_repo);
    let (entries, primary_meta) = project_entries(&primary_repo);
    let Some(primary_entry) = entries.iter().find(|e| e.primary).cloned() else {
        return json!({"ok": false, "code": "no_primary", "error": "the project has no primary repo"});
    };
    if loop_is_running(&primary_entry.worktree) {
        return json!({"ok": false, "code": "loop_running", "error": reason_text("loop_running")});
    }
    let output_dir = if o.output_dir.is_empty() { loop_summary::default_output_dir(env) } else { o.output_dir.clone() };
    let checks: Vec<(String, Value)> = entries.iter().map(|e| (e.name.clone(), check_repo(e, false))).collect();
    let check_of = |name: &str| checks.iter().find(|(n, _)| n == name).expect("check").1.clone();
    let mut outcome: Vec<(String, Value)> = entries.iter().map(|e| (e.name.clone(), json!({"status": "kept", "removed_worktree": false, "branch_kept": check_of(&e.name)["branch"], "reasons": []}))).collect();
    fn slot<'a>(outcome: &'a mut [(String, Value)], name: &str) -> &'a mut Value {
        &mut outcome.iter_mut().find(|(n, _)| n == name).expect("outcome").1
    }
    let mut closing: Vec<Entry> = Vec::new();
    for entry in &entries {
        let check = check_of(&entry.name);
        if o.only.as_ref().is_some_and(|only| !only.contains(&entry.name)) {
            slot(&mut outcome, &entry.name)["reasons"] = check["reasons"].clone();
        } else if check["can_close"] == true || (check["needs_confirm"] == true && o.confirm_compare_link) || (o.allow_missing_pr && only_pr_missing(&check)) || is_forceable(entry, &check, o.force_remove) {
            closing.push(entry.clone());
            let forced = o.force_remove && check["can_close"] != true && !(check["needs_confirm"] == true && o.confirm_compare_link) && !(o.allow_missing_pr && only_pr_missing(&check));
            slot(&mut outcome, &entry.name)["forced"] = json!(forced);
        } else {
            slot(&mut outcome, &entry.name)["reasons"] = check["reasons"].clone();
        }
    }
    let mut backups: Vec<(String, String)> = Vec::new();
    for entry in closing.clone() {
        // (a) verified backup; a repo whose backup fails is not touched
        let (folder, error) = back_up_worktree(env, &entry.worktree, &output_dir);
        backups.push((entry.name.clone(), folder.clone()));
        slot(&mut outcome, &entry.name)["backup"] = json!(folder);
        if let Some(error) = error {
            closing.retain(|c| c.name != entry.name);
            let s = slot(&mut outcome, &entry.name);
            s["status"] = json!("failed");
            s["reasons"] = json!([reason("error", &format!("{error} (nothing was deleted)"))]);
        }
    }
    let (mut durable, mut last_run, mut primary_safe): (Vec<String>, Option<String>, bool) = (vec![], None, false);
    if !closing.is_empty() {
        // (b) the plan goes home and (c) the run is recorded BEFORE any worktree is removed
        let backup_folder = backups.iter().find(|(n, _)| *n == primary_entry.name).map(|(_, f)| f.clone()).unwrap_or_default();
        let step = || -> std::io::Result<Vec<String>> {
            let copied = if Path::new(&primary_entry.worktree).is_dir() { copy_durable_files(&primary_entry.worktree, &primary_repo, &backup_folder)? } else { vec![] };
            Ok(copied)
        };
        match step() {
            Ok(copied) => {
                durable = copied;
                let names: Vec<String> = closing.iter().map(|e| e.name.clone()).collect();
                write_last_run(&primary_repo, &entries, &checks, &names, &durable, &primary_meta, &primary_entry.worktree, o.finished);
                last_run = Some(last_run_path(&primary_repo).display().to_string());
                primary_safe = true;
            }
            Err(failure) => {
                if closing.iter().any(|e| e.primary) {
                    closing.retain(|e| !e.primary);
                    let s = slot(&mut outcome, &primary_entry.name);
                    s["status"] = json!("failed");
                    s["reasons"] = json!([reason("error", &format!("the durable .specs/ files could not be copied back ({}); the primary worktree was kept", failure.to_string().chars().take(160).collect::<String>()))]);
                }
            }
        }
    }
    let prod_database = if closing.is_empty() { None } else { Some(write_prod_database_script(env, &primary_repo, &primary_entry.worktree, &entries, &checks, &output_dir)) };
    let mut order = closing.clone();
    order.sort_by_key(|e| e.primary); // (d) the primary, which holds `.specs/`, goes last
    for entry in &order {
        let saved = if entry.primary { verified_plan(&primary_entry.worktree, &primary_repo, &durable) } else { vec![] };
        let error = remove_worktree(entry, o.force_remove, &saved);
        let s = slot(&mut outcome, &entry.name);
        match error {
            Some(e) => {
                s["status"] = json!("failed");
                s["reasons"] = json!([reason("error", &e)]);
            }
            None => {
                s["status"] = json!("closed");
                s["removed_worktree"] = json!(true);
                s["reasons"] = json!([]);
            }
        }
    }
    let removed: Vec<String> = outcome.iter().filter(|(_, r)| r["removed_worktree"] == true).map(|(n, _)| n.clone()).collect();
    let closing_names: Vec<&String> = closing.iter().map(|e| &e.name).collect();
    if primary_safe && !(removed.len() == closing_names.len() && removed.iter().all(|n| closing_names.contains(&n))) {
        write_last_run(&primary_repo, &entries, &checks, &removed, &durable, &primary_meta, &primary_entry.worktree, o.finished); // a removal failed: the record must say so
    }
    let closed: Vec<&String> = outcome.iter().filter(|(_, r)| r["status"] == "closed").map(|(n, _)| n).collect();
    let kept: Vec<&String> = outcome.iter().filter(|(_, r)| r["status"] != "closed").map(|(n, _)| n).collect();
    let backup = backups.iter().find(|(n, _)| *n == primary_entry.name).or(backups.first()).map(|(_, f)| json!(f)).unwrap_or(Value::Null);
    let mut response = Map::new();
    response.insert("ok".into(), json!(true));
    response.insert("closed".into(), json!(closed));
    response.insert("kept".into(), json!(kept));
    response.insert("backup".into(), backup);
    response.insert("durable_copied".into(), json!(durable));
    response.insert("last_run".into(), last_run.map(Value::from).unwrap_or(Value::Null));
    response.insert("repos".into(), Value::Object(outcome.into_iter().collect()));
    if let Some(prod) = prod_database {
        if crate::notify_truthy(prod.get("path")) || crate::notify_truthy(prod.get("warnings")) {
            response.insert("warnings".into(), prod["warnings"].clone());
            response.insert("prod_database".into(), prod);
        }
    }
    Value::Object(response)
}

/// `harness close-loop <primary-repo> [--check] [--confirm-compare-link] [--repos a,b] [--output-dir DIR] [--allow-missing-pr]
/// [--force-remove] [--finished]`; returns `(exit code, stdout)`.
pub fn run_cli(args: &[String], env: &Env) -> (i32, String) {
    let mut o = CloseOptions::default();
    let (mut repo, mut check) = (String::new(), false);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--check" => check = true,
            "--close" => {}
            "--confirm-compare-link" => o.confirm_compare_link = true,
            "--allow-missing-pr" => o.allow_missing_pr = true,
            "--force-remove" => o.force_remove = true,
            "--finished" => o.finished = true,
            "--repos" if i + 1 < args.len() => {
                i += 1;
                let names: Vec<String> = args[i].split(',').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
                o.only = if names.is_empty() { None } else { Some(names) };
            }
            "--output-dir" | "--out" if i + 1 < args.len() => {
                i += 1;
                o.output_dir = args[i].clone();
            }
            a if !a.starts_with("--") && repo.is_empty() => repo = a.to_string(),
            _ => return (2, "usage: close_loop.py [-h] [--check] [--close] [--confirm-compare-link] [--repos REPOS]\n                     [--output-dir OUTPUT_DIR] [--allow-missing-pr]\n                     [--force-remove] [--finished]\n                     primary_repo\n".into()),
        }
        i += 1;
    }
    if repo.is_empty() {
        return (2, "usage: close_loop.py [-h] [--check] [--close] [--confirm-compare-link] [--repos REPOS]\n                     [--output-dir OUTPUT_DIR] [--allow-missing-pr]\n                     [--force-remove] [--finished]\n                     primary_repo\n".into());
    }
    let result = if check { can_close(&repo) } else { close(env, &repo, &o) };
    let ok = result["ok"] == true;
    (if ok { 0 } else { 1 }, format!("{}\n", pycompat::json::py_json_dumps_indent(&result, false, 2)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn env(dir: &Path) -> Env {
        Env::from_pairs([("LOOP_DATA_DIR", dir.join("data").display().to_string()), ("USERPROFILE", dir.display().to_string())])
    }

    /// A repo with a loop worktree on `loop/x`, delivered (push done + PR) as far as the files go.
    fn project(dir: &Path) -> (String, String) {
        let repo = dir.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-q", "-b", "developer"]);
        git_in(&repo, &["config", "user.name", "t"]);
        git_in(&repo, &["config", "user.email", "t@example.com"]);
        std::fs::write(repo.join("README.md"), "x").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-q", "-m", "init"]);
        let work = dir.join("app-worktree-loop");
        git_in(&repo, &["worktree", "add", "-q", "-b", "loop/x", &work.display().to_string(), "developer"]);
        let specs = work.join(".specs").join("project");
        std::fs::create_dir_all(specs.join("loop").join("json")).unwrap();
        std::fs::write(specs.join("ROADMAP.md"), "## Stage 1\n- [a] status=verified\n").unwrap();
        std::fs::write(specs.join("loop").join("json").join("meta.json"), r#"{"branch": "loop/x", "base": "developer", "turns": 2, "cost_usd": 1.5, "finished_at": "2026-01-01T00:00:00"}"#).unwrap();
        std::fs::write(specs.join("loop").join("json").join("delivery.json"), r#"{"push": "FAILED: no origin", "pr": null, "pr_status": "not created"}"#).unwrap();
        (repo.display().to_string(), work.display().to_string())
    }

    #[test]
    fn the_check_explains_what_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, _work) = project(dir.path());
        let checked = can_close(&repo);
        assert_eq!((checked["ok"].clone(), checked["can_close"].clone(), checked["loop_finished"].clone()), (json!(true), json!(false), json!(true)));
        let item = &checked["repos"][0];
        assert_eq!((item["name"].as_str(), item["branch"].as_str(), item["push"].as_str()), (Some("app"), Some("loop/x"), Some("FAILED: no origin")));
        let codes: Vec<&str> = item["reasons"].as_array().unwrap().iter().filter_map(|r| r["code"].as_str()).collect();
        assert_eq!(codes, vec!["push_unconfirmed", "pr_missing"]);
        assert_eq!(item["needs_confirm"], json!(false));
    }

    #[test]
    fn a_forced_close_backs_up_then_removes_and_records_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, work) = project(dir.path());
        let out = dir.path().join("out").display().to_string();
        let kept = close(&env(dir.path()), &repo, &CloseOptions { output_dir: out.clone(), ..Default::default() });
        assert_eq!((kept["closed"].clone(), kept["kept"].clone()), (json!([]), json!(["app"])));
        assert!(Path::new(&work).is_dir());
        let closed = close(&env(dir.path()), &repo, &CloseOptions { output_dir: out.clone(), force_remove: true, finished: true, ..Default::default() });
        assert_eq!((closed["ok"].clone(), closed["closed"].clone()), (json!(true), json!(["app"])), "{closed}");
        assert!(!Path::new(&work).exists());
        let backup = closed["backup"].as_str().unwrap();
        assert!(Path::new(backup).join("specs").join("project").join("ROADMAP.md").is_file());
        assert!(closed["durable_copied"].as_array().unwrap().iter().any(|p| p == "project/ROADMAP.md"));
        let last = load_json(&last_run_path(&repo));
        assert_eq!((last["multi"].clone(), last["finished"].clone(), last["turns"].clone(), last["cost_usd"].clone()), (json!(false), json!(true), json!(2), json!(1.5)));
        assert_eq!((last["repos"]["app"]["branch"].clone(), last["repos"]["app"]["worktree_removed"].clone(), last["repos"]["app"]["finished"].clone()), (json!("loop/x"), json!(true), json!(true)));
        assert_eq!(last["max_cost_usd"], json!(50.0));
        assert!(Path::new(&repo).join(".specs/project/ROADMAP.md").is_file());
        let again = can_close(&repo);
        assert_eq!(again["repos"][0]["reasons"][0]["code"], "already_closed");
        let (code, text) = run_cli(&[repo.clone(), "--check".into()], &env(dir.path()));
        assert_eq!(code, 0);
        assert!(text.starts_with("{\n  \"ok\": true"));
    }

    #[test]
    fn small_helpers() {
        assert_eq!(cut_reason("one two three four", 9), "one two...");
        assert_eq!(cut_reason("short  text", 50), "short text");
        assert_eq!(reason("pr_missing", "")["text"], reason_text("pr_missing"));
        assert!(only_pr_missing(&json!({"reasons": [{"code": "pr_missing"}]})) && !only_pr_missing(&json!({"reasons": []})));
        assert_eq!(effective_max_cost(&json!({"max_cost_usd": 7}), ""), 7.0);
        assert_eq!(effective_max_cost(&json!({}), ""), 50.0);
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path().display().to_string();
        let cmd = loop_path(&w, "LOOP-CMD.json");
        std::fs::create_dir_all(cmd.parent().unwrap()).unwrap();
        std::fs::write(&cmd, r#"{"argv": ["h", "loop", "r", "--max-cost", "12.5"]}"#).unwrap();
        assert_eq!(command_max_cost(&w), Some(12.5));
    }
}
