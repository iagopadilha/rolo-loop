// SPDX-License-Identifier: MIT
//! End-of-flow delivery: push of the FEATURE BRANCH and a PR to the integration branch, with the link in the final report.
//!
//! `harness deliver <repo-or-worktree-root> [--base <integration branch>] [--branch NAME] [--draft] [--dry-run]`.
//!
//! The integration branch (PR target) is `--base`, else the ROADMAP `**Branch base:**` line, else env/.env LOOP_PR_BASE /
//! LOOP_INTEGRATION_BRANCH, else `developer`.
//!
//! Rules (no flag turns them off):
//!   * feature branch only: the integration branch, develop, main, master, release, production, dev are REFUSED;
//!   * only at the END: no open feature (pending/speccing/specced/approved/building) left in the ROADMAP;
//!   * no force, no merge, no branch deletion;
//!   * never in the headless loop (LOOP_HEADLESS=1: the script delivers);
//!   * a configured git identity (and LOOP_GIT_USER, if set in the app .env).
//!
//! PR: `gh pr create` (or reuses the branch's already open PR). Without `gh`, or with LOOP_PR_MODE=link, only the compare link is
//! returned. LOOP_PR_MODE=off neither creates nor computes a PR. Multi-repo projects: one `finalize` per repo worktree, one PR each.

use crate::loop_io::{git, load_json, now_iso, read, run_timeout, save_json, which};
use crate::loop_state::{loop_path, parse_roadmap, state_file, Features};
use crate::prod_database_script as prod;
use crate::{graph_view, notify_truthy};
use cfg::env::load_all;
use cfg::Env;
use pycompat::json::py_json_dumps_indent;
use pycompat::{nt_abspath, py_repr_str, Re};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::LazyLock;

const BASE_PROTECTED: [&str; 7] = ["main", "master", "develop", "developer", "dev", "release", "production"];
const OPEN: [&str; 5] = ["pending", "speccing", "specced", "approved", "building"];
pub const DEFAULT_BASE: &str = "developer";
const USAGE: &str = "usage: delivery.py [-h] [--branch BRANCH] [--base BASE] [--draft] [--dry-run]\n                   root\n";

static GITHUB_SLUG: LazyLock<Re> = LazyLock::new(|| Re::new(r"github\.com[:/]+([^/\s]+)/([^/\s]+?)(?:\.git)?/?\z"));

/// Integration branch (PR target): `chosen` first, then LOOP_PR_BASE / LOOP_INTEGRATION_BRANCH, default `developer`.
pub fn integration_branch(env: &mut Env, chosen: &str) -> String {
    if !chosen.trim().is_empty() {
        return chosen.trim().to_string();
    }
    load_all(env);
    for name in ["LOOP_PR_BASE", "LOOP_INTEGRATION_BRANCH"] {
        let value = env.text(name);
        if !value.trim().is_empty() {
            return value.trim().to_string();
        }
    }
    DEFAULT_BASE.into()
}

fn roadmap_text(root: &str) -> String {
    read(&Path::new(root).join(".specs").join("project").join("ROADMAP.md"))
}

/// The `**Branch base:**` of the ROADMAP under `root` (empty when absent or invalid).
pub fn roadmap_base_branch(root: &str) -> String {
    crate::roadmap_settings::read_roadmap_base_branch(&roadmap_text(root))
}

fn protected_names(env: &mut Env, chosen: &str) -> Vec<String> {
    let mut names: Vec<String> = BASE_PROTECTED.iter().map(|s| s.to_string()).collect();
    names.push(integration_branch(env, chosen).to_lowercase());
    names
}

/// Trimmed, without a `refs/heads/` prefix, lowercase.
pub fn short_branch_name(branch: &str) -> String {
    let name = branch.trim();
    (if name.to_lowercase().starts_with("refs/heads/") { &name["refs/heads/".len()..] } else { name }).to_lowercase()
}

/// True when `branch` is one plain short branch name that git accepts (`git check-ref-format --branch`).
pub fn valid_branch_name(root: &str, branch: &str) -> bool {
    if branch.is_empty() || branch != branch.trim() || branch.starts_with(['+', '-']) || branch.starts_with("refs/") || branch.contains(':') || branch.contains("@{") {
        return false;
    }
    let (code, out, _) = git(root, &["check-ref-format", "--branch", branch]);
    code == 0 && out == branch
}

fn features_of(root: &str) -> Features {
    parse_roadmap(&roadmap_text(root)).0
}

/// Items of VALIDATE.md: what the orchestrator decided on its own and the user should check.
fn validate_items(root: &str) -> Vec<String> {
    read(&state_file(root, "VALIDATE.md")).lines().map(str::trim).filter(|l| l.starts_with("- ")).map(|l| l[2..].trim().to_string()).collect()
}

/// `git@github.com:org/repo.git | https://github.com/org/repo(.git)` -> `org/repo`.
pub fn github_slug(remote_url: &str) -> Option<String> {
    GITHUB_SLUG.search(remote_url).map(|m| format!("{}/{}", m.group(1).unwrap_or(""), m.group(2).unwrap_or("")))
}

pub fn compare_url(remote_url: &str, base: &str, branch: &str) -> Option<String> {
    github_slug(remote_url).map(|slug| format!("https://github.com/{slug}/compare/{base}...{branch}?expand=1"))
}

fn related_prs_block(related: &[(String, String)], own_name: &str) -> Vec<String> {
    let links: Vec<&(String, String)> = related.iter().filter(|(n, l)| !l.is_empty() && n != own_name).collect();
    if links.is_empty() {
        return vec![];
    }
    let mut out = vec![String::new(), "## Related PRs".to_string(), String::new()];
    out.extend(links.iter().map(|(n, l)| format!("- {n}: {l}")));
    out
}

pub fn pr_body(root: &str, branch: &str, base: &str, roadmap_root: &str, related: &[(String, String)], own_name: &str) -> String {
    let roadmap_root = if roadmap_root.is_empty() { root } else { roadmap_root };
    let mut out = vec![format!("Orchestrator delivery: branch `{branch}` to `{base}`."), String::new()];
    let feats = features_of(roadmap_root);
    let with_status = |wanted: &str| -> Vec<String> { feats.iter().filter(|(_, f)| f.status() == wanted).map(|(s, _)| s.clone()).collect() };
    let verified = with_status("verified");
    let blocked = with_status("blocked");
    out.push(format!("**Verified features ({}):** {}", verified.len(), if verified.is_empty() { "none".into() } else { verified.join(", ") }));
    if !blocked.is_empty() {
        out.extend([String::new(), format!("**Quarantined (NOT delivered, your call):** {}", blocked.join(", "))]);
    }
    let unverified: Vec<String> = feats.iter().filter(|(_, f)| f.text("note").contains("unverified")).map(|(s, _)| s.clone()).collect();
    if !unverified.is_empty() {
        out.extend([String::new(), format!("**Without automatic verification:** {}", unverified.join(", "))]);
    }
    out.extend([String::new(), "## Progress".into(), String::new(), "```mermaid".into(), graph_view::mermaid(&feats), "```".into()]);
    let items = validate_items(roadmap_root);
    if !items.is_empty() {
        out.extend([String::new(), "## For you to validate".into(), String::new()]);
        out.extend(items.iter().take(30).map(|i| format!("- {i}")));
    }
    let (_, log, _) = git(root, &["log", "--oneline", "--no-decorate", &format!("origin/{base}..HEAD")]);
    if !log.is_empty() {
        let lines: Vec<&str> = log.lines().take(30).collect();
        out.extend([String::new(), "## Commits".into(), String::new(), "```".into(), lines.join("\n"), "```".into()]);
    }
    out.extend(related_prs_block(related, own_name));
    out.extend([String::new(), "Generated from .specs/ (ROADMAP, VALIDATE). Nothing was merged: the merge decision is yours.".into()]);
    out.join("\n")
}

fn identity_problem(env: &Env, root: &str) -> String {
    let user = git(root, &["config", "user.name"]).1;
    let mail = git(root, &["config", "user.email"]).1;
    if user.is_empty() || mail.is_empty() {
        return "git user.name/user.email not configured".into();
    }
    let wanted = env.text("LOOP_GIT_USER");
    if !wanted.is_empty() && user != wanted {
        return format!("git user.name is {}, expected {} (LOOP_GIT_USER in the app .env)", py_repr_str(&user), py_repr_str(&wanted));
    }
    String::new()
}

fn clip(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

fn set(map: &mut Map<String, Value>, key: &str, value: Value) {
    map.insert(key.to_string(), value);
}

fn starts(map: &Map<String, Value>, key: &str, prefixes: &[&str]) -> bool {
    let text = map.get(key).and_then(Value::as_str).unwrap_or("");
    prefixes.iter().any(|p| text.starts_with(p))
}

fn temp_body(text: &str) -> Option<std::path::PathBuf> {
    let file = tempfile::Builder::new().suffix(".md").tempfile().ok()?;
    let (_, path) = file.keep().ok()?;
    std::fs::write(&path, text).ok()?;
    Some(path)
}

pub struct Options<'a> {
    pub branch: &'a str,
    pub base: &'a str,
    pub draft: bool,
    pub dry_run: bool,
    pub roadmap_root: &'a str,
    pub related: &'a [(String, String)],
    pub own_name: &'a str,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options { branch: "", base: "", draft: false, dry_run: false, roadmap_root: "", related: &[], own_name: "" }
    }
}

/// Push of the feature branch + PR. Returns `{"push", "pr", "pr_status"}`; never fails; never forces or merges.
pub fn finalize(env: &mut Env, root: &str, o: &Options) -> Map<String, Value> {
    let roadmap_root = if o.roadmap_root.is_empty() { root } else { o.roadmap_root };
    load_all(env);
    let mut result = Map::new();
    set(&mut result, "push", json!("not done"));
    set(&mut result, "pr", Value::Null);
    set(&mut result, "pr_status", json!("not created"));
    let branch = if o.branch.is_empty() { git(root, &["branch", "--show-current"]).1 } else { o.branch.to_string() };
    let chosen = if o.base.is_empty() { roadmap_base_branch(roadmap_root) } else { o.base.to_string() };
    let base = integration_branch(env, &chosen);
    let mode = { let m = env.text("LOOP_PR_MODE"); if m.is_empty() { "auto".to_string() } else { m.to_lowercase() } };
    if branch.is_empty() {
        set(&mut result, "push", json!("not done: detached HEAD (no branch)"));
    } else if !valid_branch_name(root, &branch) {
        set(&mut result, "push", json!(format!("refused: invalid branch name {branch}")));
    } else if protected_names(env, &chosen).contains(&short_branch_name(&branch)) {
        set(&mut result, "push", json!(format!("refused: {branch} is a protected branch; feature branch only")));
    } else if branch == base {
        set(&mut result, "push", json!(format!("refused: the feature branch cannot be the PR base ({base})")));
    } else {
        let mut open_features: Vec<String> = features_of(roadmap_root).iter().filter(|(_, f)| OPEN.contains(&f.status())).map(|(s, _)| s.clone()).collect();
        open_features.sort();
        let problem = identity_problem(env, root);
        let origin = git(root, &["remote", "get-url", "origin"]).1;
        let push = if !open_features.is_empty() {
            format!("not done: there are still open features ({}); push only at the end", open_features.iter().take(6).cloned().collect::<Vec<_>>().join(", "))
        } else if !problem.is_empty() {
            format!("refused: {problem}")
        } else if origin.is_empty() {
            "not done: no origin remote".into()
        } else if o.dry_run {
            format!("dry-run: git push -u origin {branch}")
        } else {
            let refspec = format!("refs/heads/{branch}:refs/heads/{branch}"); // explicit refspec: never a force, never another ref
            let (code, out, err) = run_git_push(root, &refspec);
            if code == 0 { format!("done: origin/{branch}") } else { format!("FAILED: {}", clip(if err.is_empty() { &out } else { &err }, 300)) }
        };
        set(&mut result, "push", json!(push));
        if (push.starts_with("done") || push.starts_with("dry-run")) && mode != "off" {
            let link = compare_url(&origin, &base, &branch);
            set(&mut result, "pr", link.map(Value::from).unwrap_or(Value::Null));
            set(&mut result, "pr_status", json!("compare link (PR not created)"));
            if mode != "link" && which("gh").is_some() && !o.dry_run {
                create_pr(env, &mut result, root, &branch, &base, o, roadmap_root);
            }
        }
    }
    let mut saved = result.clone();
    set(&mut saved, "branch", json!(branch));
    set(&mut saved, "base", json!(base));
    set(&mut saved, "at", json!(now_iso()));
    let target = loop_path(root, "delivery.json");
    let _ = save_json(&target, &Value::Object(saved));
    result
}

fn run_git_push(root: &str, refspec: &str) -> (i32, String, String) {
    run_timeout("git", &["-C", root, "push", "-u", "origin", refspec], None, 120)
}

fn create_pr(env: &Env, result: &mut Map<String, Value>, root: &str, branch: &str, base: &str, o: &Options, roadmap_root: &str) {
    let (code, existing, _) = run_timeout("gh", &["pr", "list", "--head", branch, "--base", base, "--state", "open", "--json", "url", "-q", ".[0].url"], Some(root), 60);
    if code == 0 && !existing.is_empty() {
        set(result, "pr", json!(existing));
        set(result, "pr_status", json!("PR already existed"));
        return;
    }
    let Some(body_file) = temp_body(&pr_body(root, branch, base, roadmap_root, o.related, o.own_name)) else { return };
    let body_path = body_file.display().to_string();
    let title = format!("[orchestrator] {branch}");
    let mut cmd: Vec<&str> = vec!["pr", "create", "--base", base, "--head", branch, "--title", &title, "--body-file", &body_path];
    let draft_env = env.text("LOOP_PR_DRAFT");
    if o.draft || draft_env == "1" || draft_env == "true" {
        cmd.push("--draft");
    }
    let (code, out, err) = run_timeout("gh", &cmd, Some(root), 90);
    let _ = std::fs::remove_file(&body_file);
    let url = out.lines().map(str::trim).find(|l| l.starts_with("http")).unwrap_or("").to_string();
    if code == 0 && !url.is_empty() {
        set(result, "pr", json!(url));
        set(result, "pr_status", json!("PR created"));
    } else {
        set(result, "pr_status", json!(format!("compare link; gh failed: {}", clip(if err.is_empty() { &out } else { &err }, 200))));
    }
}

/// Rewrites the body of a PR THIS run created so it lists the siblings created after it. Best effort.
fn edit_pr_body(root: &str, pr_url: &str, branch: &str, base: &str, roadmap_root: &str, related: &[(String, String)], own_name: &str) -> bool {
    if which("gh").is_none() {
        return false;
    }
    let Some(file) = temp_body(&pr_body(root, branch, base, roadmap_root, related, own_name)) else { return false };
    let path = file.display().to_string();
    let ok = run_timeout("gh", &["pr", "edit", pr_url, "--body-file", &path], Some(root), 90).0 == 0;
    let _ = std::fs::remove_file(&file);
    ok
}

/// The PR base `finalize` used, as written in that worktree's delivery.json.
fn read_base(env: &mut Env, root: &str, fallback: &str) -> String {
    let saved = load_json(&loop_path(root, "delivery.json"));
    match saved.get("base").and_then(Value::as_str) {
        Some(b) if !b.is_empty() => b.to_string(),
        _ => integration_branch(env, fallback),
    }
}

/// Writes the production database script (never a block) into `result["prod_database"]` and the primary's delivery.json.
pub fn attach_prod_database(env: &Env, primary_work: &str, meta: &Value, result: &mut Map<String, Value>, dry_run: bool, output_dir: &str, runner: Option<prod::Runner>) {
    if dry_run || !result.get("push").and_then(Value::as_str).unwrap_or("").starts_with("done") {
        return;
    }
    let outcome = prod::generate_for_meta(env, primary_work, meta, output_dir, runner, None, "");
    let warnings: Vec<Value> = outcome["warnings"].as_array().cloned().unwrap_or_default();
    if outcome["path"].as_str().is_some_and(|p| !p.is_empty()) || !warnings.is_empty() {
        set(result, "prod_database", outcome.clone());
        if !warnings.is_empty() {
            let mut all = result.get("warnings").and_then(Value::as_array).cloned().unwrap_or_default();
            all.extend(warnings);
            set(result, "warnings", Value::Array(all));
        }
        let target = loop_path(primary_work, "delivery.json");
        let parsed = std::fs::read(&target).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok());
        if let Some(Value::Object(mut saved)) = parsed {
            set(&mut saved, "prod_database", outcome.clone());
            if outcome["warnings"].as_array().is_some_and(|w| !w.is_empty()) {
                set(&mut saved, "warnings", result.get("warnings").cloned().unwrap_or(Value::Null));
            }
            let _ = save_json(&target, &Value::Object(saved));
        }
    }
}

/// Delivery of the whole project. Without a `repos` map this IS the single-repo delivery; with it: one `finalize` per repo,
/// the primary first, then the sibling links are written into the PR bodies this run created.
pub fn finalize_project(env: &mut Env, primary_work: &str, meta: &Value, draft: bool, dry_run: bool, output_dir: &str, runner: Option<prod::Runner>, base: &str) -> Map<String, Value> {
    let base = if base.is_empty() { roadmap_base_branch(primary_work) } else { base.to_string() };
    let Some(repos) = meta.get("repos").and_then(Value::as_object).filter(|r| !r.is_empty()) else {
        let branch = meta.get("branch").and_then(Value::as_str).unwrap_or("");
        let mut single = finalize(env, primary_work, &Options { branch, base: &base, draft, dry_run, ..Default::default() });
        attach_prod_database(env, primary_work, meta, &mut single, dry_run, output_dir, runner);
        return single;
    };
    let mut names: Vec<&String> = repos.keys().collect();
    names.sort_by_key(|n| !notify_truthy(repos[*n].get("primary")));
    let primary_name = names[0].clone();
    let mut results: Vec<(String, Map<String, Value>)> = Vec::new();
    let mut roots: Vec<(String, String)> = Vec::new();
    for name in &names {
        let entry = &repos[*name];
        let field = |k: &str| entry.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let worktree = field("worktree");
        let root = nt_abspath(if **name == primary_name { primary_work } else { &worktree });
        roots.push(((*name).clone(), root.clone()));
        let item = if !Path::new(&root).is_dir() {
            let mut m = Map::new();
            set(&mut m, "push", json!("not done: worktree missing"));
            set(&mut m, "pr", Value::Null);
            set(&mut m, "pr_status", json!("not created"));
            set(&mut m, "branch", json!(field("branch")));
            set(&mut m, "base", json!(integration_branch(env, &base)));
            set(&mut m, "at", json!(now_iso()));
            m
        } else {
            let branch = field("branch");
            let mut m = finalize(env, &root, &Options { branch: &branch, base: &base, draft, dry_run, roadmap_root: primary_work, ..Default::default() });
            set(&mut m, "branch", json!(branch));
            set(&mut m, "base", json!(read_base(env, &root, &base)));
            set(&mut m, "at", json!(now_iso()));
            m
        };
        results.push(((*name).clone(), item));
    }
    let links: Vec<(String, String)> = results.iter().filter_map(|(n, r)| r.get("pr").and_then(Value::as_str).filter(|p| !p.is_empty()).map(|p| (n.clone(), p.to_string()))).collect();
    if links.len() > 1 {
        for (name, result) in &results {
            if result.get("pr_status").and_then(Value::as_str) == Some("PR created") {
                let root = &roots.iter().find(|(n, _)| n == name).expect("root").1;
                let g = |k: &str| result.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                edit_pr_body(root, &g("pr"), &g("branch"), &g("base"), primary_work, &links, name);
            }
        }
    }
    let mut aggregate = results.iter().find(|(n, _)| *n == primary_name).map(|(_, r)| r.clone()).expect("primary result");
    let mut by_repo = Map::new();
    for (name, result) in &results {
        by_repo.insert(name.clone(), Value::Object(result.clone()));
    }
    set(&mut aggregate, "repos", Value::Object(by_repo));
    let _ = save_json(&loop_path(primary_work, "delivery.json"), &Value::Object(aggregate.clone()));
    if results.iter().all(|(_, r)| starts(r, "push", &["done"])) {
        attach_prod_database(env, primary_work, meta, &mut aggregate, dry_run, output_dir, runner);
    }
    aggregate
}

fn done_or_dry(m: &Map<String, Value>) -> bool {
    starts(m, "push", &["done", "dry-run"])
}

/// `harness deliver ...`; returns `(exit_code, stdout, stderr)`.
pub fn run_cli(args: &[String], env: &mut Env) -> (i32, String, String) {
    let (mut root, mut branch, mut base, mut draft, mut dry_run) = (None, String::new(), String::new(), false, false);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--branch" | "--base" if i + 1 < args.len() => {
                if args[i] == "--branch" { branch = args[i + 1].clone() } else { base = args[i + 1].clone() }
                i += 1;
            }
            "--draft" => draft = true,
            "--dry-run" => dry_run = true,
            a if !a.starts_with("--") && root.is_none() => root = Some(a.to_string()),
            _ => return (2, String::new(), format!("{USAGE}delivery.py: error: unrecognized arguments: {}\n", args[i])),
        }
        i += 1;
    }
    let Some(root) = root else {
        return (2, String::new(), format!("{USAGE}delivery.py: error: the following arguments are required: root\n"));
    };
    if env.text("LOOP_HEADLESS") == "1" {
        let refused = json!({"push": "refused: in the headless loop the script delivers (run_loop.py)", "pr": null, "pr_status": "not created"});
        return (2, format!("{}\n", pycompat::json::py_json_dumps(&refused, false)), String::new());
    }
    let root = nt_abspath(&root);
    let meta = load_json(&loop_path(&root, "meta.json"));
    let print = |m: &Map<String, Value>| format!("{}\n", py_json_dumps_indent(&Value::Object(m.clone()), false, 2));
    if meta.get("repos").and_then(Value::as_object).is_some_and(|r| !r.is_empty()) && branch.is_empty() && base.is_empty() {
        let result = finalize_project(env, &root, &meta, draft, dry_run, "", None, "");
        let ok = result.get("repos").and_then(Value::as_object).is_some_and(|r| r.values().all(|v| v.as_object().is_some_and(done_or_dry)));
        return (if ok { 0 } else { 1 }, print(&result), String::new());
    }
    let mut result = finalize(env, &root, &Options { branch: &branch, base: &base, draft, dry_run, ..Default::default() });
    attach_prod_database(env, &root, &meta, &mut result, dry_run, "", None);
    (if done_or_dry(&result) { 0 } else { 1 }, print(&result), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        Env::from_pairs([("LOOP_DATA_DIR", "C:\\no-such-dir")])
    }

    #[test]
    fn names_and_links() {
        assert_eq!(short_branch_name(" Refs/Heads/Main "), "main");
        assert_eq!(github_slug("git@github.com:org/repo.git").as_deref(), Some("org/repo"));
        assert_eq!(github_slug("https://github.com/org/repo/").as_deref(), Some("org/repo"));
        assert_eq!(github_slug("https://example.com/org/repo"), None);
        assert_eq!(compare_url("https://github.com/o/r.git", "developer", "loop/x").as_deref(), Some("https://github.com/o/r/compare/developer...loop/x?expand=1"));
        assert_eq!(integration_branch(&mut env(), "  qa "), "qa");
    }

    #[test]
    fn protected_and_invalid_branches_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        let mut e = env();
        let main = finalize(&mut e, &root, &Options { branch: "main", ..Default::default() });
        assert_eq!(main["push"], "refused: main is a protected branch; feature branch only");
        let colon = finalize(&mut e, &root, &Options { branch: "loop/x:developer", ..Default::default() });
        assert_eq!(colon["push"], "refused: invalid branch name loop/x:developer");
        let detached = finalize(&mut e, &root, &Options::default());
        assert_eq!(detached["push"], "not done: detached HEAD (no branch)");
        assert!(loop_path(&root, "delivery.json").is_file());
    }

    #[test]
    fn related_block_skips_own_and_empty() {
        let links = vec![("a".to_string(), "u1".to_string()), ("b".to_string(), String::new()), ("c".to_string(), "u3".to_string())];
        assert_eq!(related_prs_block(&links, "a"), vec!["", "## Related PRs", "", "- c: u3"]);
        assert!(related_prs_block(&links[1..2], "").is_empty());
    }
}
