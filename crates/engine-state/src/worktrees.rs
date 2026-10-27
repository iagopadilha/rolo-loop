// SPDX-License-Identifier: MIT
//! Creating and re-attaching the loop worktrees: base branch choice, carried setup files, local file copies, the primary worktree and one
//! per extra repo.

use crate::loop_io::{copy2, copy_tree, git, load_json, merge_pending_delivery, read, save_meta, worktree_of};
use crate::loop_state::{self, loop_path, parse_roadmap, Features};
use crate::project_repos::{self, RepoRef};
use crate::setup_files::{self, RefreshResult};
use crate::{delivery, detect_setup, setup_approval, subprojects};
use cfg::Env;
use pycompat::{nt_realpath, Re};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Never copied into a worktree (they stay with --setup).
pub const HEAVY_FOLDERS: [&str; 4] = ["node_modules", "bin", "obj", "dist"];
/// Written by close_loop in the primary repo; names the branch a new start re-attaches to.
pub const LAST_RUN_NAME: &str = "last-run.json";
pub const MIN_GIT_WORKTREE_VERSION: (u32, u32) = (2, 5);
pub const SETUP_COMMIT_MESSAGE: &str = "chore(setup): carry codebase maps and agent context";
const SYNCED_INVENTORY: [&str; 6] = [".specs", "project", "loop", "internal", "inventory", ""];
const SYNCED_SUBPROJECTS: [&str; 3] = [".specs", "project", "SUBPROJECTS.json"];

static VERSION: LazyLock<Re> = LazyLock::new(|| Re::new(r"(\d+)\.(\d+)"));
static REPORTED_REFRESH_NOTES: LazyLock<Mutex<HashSet<(String, String)>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// What the worktree functions need from the options of the loop.
#[derive(Clone, Debug, Default)]
pub struct WtOpts {
    /// One-time setup command (`--setup`).
    pub setup: String,
    /// `--branch`.
    pub branch: String,
    pub dry_run: bool,
    /// `--base-branch` (the ROADMAP `Branch base`).
    pub base_branch: String,
    /// `--repos`: replaces the roadmap filter for the non-primary repos.
    pub only_repos: Option<Vec<String>>,
    /// Set by [`ensure_worktrees`]: the primary repo path.
    pub repo_path: String,
}

/// What the worktree functions need from the app around them.
pub struct Ctx<'a> {
    pub env: &'a mut Env,
    /// Plugin root of the dev toolkit (empty when it is not installed).
    pub toolkit_root: String,
}

impl Ctx<'_> {
    fn data_dir(&self) -> PathBuf {
        self.env.user_data_dir()
    }
}

/// `{slug: fields}` of the ROADMAP of `root` (empty when there is none or it is unreadable).
pub fn roadmap_features(root: &str) -> Features {
    let path = Path::new(root).join(".specs").join("project").join("ROADMAP.md");
    if path.exists() {
        parse_roadmap(&read(&path)).0
    } else {
        Features::default()
    }
}

/// The repos that get a worktree: the primary always, plus every NON-primary `edit` repo that a ROADMAP feature names in `paths=<repo>:`.
/// `only` (explicit `--repos`) replaces the roadmap filter for the non-primary ones. Reference repos never.
pub fn worktree_repos(repos: &[RepoRef], features: &Features, only: Option<&[String]>) -> Vec<RepoRef> {
    let primary = project_repos::primary_of(repos);
    let changed = project_repos::repos_changed_by_roadmap(&primary.name, features);
    project_repos::edit_repos(repos)
        .into_iter()
        .filter(|item| {
            item.primary
                || match only.filter(|o| !o.is_empty()) {
                    Some(only) => only.contains(&item.name),
                    None => changed.contains(&item.name),
                }
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------- worktree (script only)

/// One plain-text step line for the panel: `[loop] [<repo>] <kind>: <text>` (tag only in a multi-repo project, so a single-repo project
/// keeps today's output). Kinds: check | warn | worktree-add | reattach | copy | setup | ready | reuse | error. Flushed at once.
pub fn step(tag: &str, kind: &str, text: &str) {
    use std::io::Write;
    if tag.is_empty() {
        println!("[loop] {kind}: {text}");
    } else {
        println!("[loop] [{tag}] {kind}: {text}");
    }
    let _ = std::io::stdout().flush();
}

/// `setup_files::refresh_loop_inputs` plus its log lines. A note is printed once per worktree; the copied names only when `quiet` is false.
pub fn refresh_inputs(ctx: &Ctx, repo: &str, work: &str, tag: &str, quiet: bool) -> RefreshResult {
    let result = setup_files::refresh_loop_inputs(repo, work, &ctx.data_dir().display().to_string(), &ctx.toolkit_root);
    if !result.copied.is_empty() && !quiet {
        step(tag, "copy", &format!("refreshed from the repo into the worktree: {}", result.copied.join(", ")));
    }
    for note in &result.notes {
        let fresh = REPORTED_REFRESH_NOTES.lock().map(|mut set| set.insert((work.to_string(), note.clone()))).unwrap_or(true);
        if fresh {
            step(tag, "warn", note);
        }
    }
    result
}

pub fn branch_exists(repo: &str, branch: &str) -> bool {
    !branch.is_empty() && git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).0 == 0
}

/// `(base branch, warning)` of a NEW worktree: the integration branch (the ROADMAP `Branch base`, else LOOP_PR_BASE /
/// LOOP_INTEGRATION_BRANCH, default `developer`) when it exists locally, else the folder's current branch with a warning naming both.
/// `("", "")` when the folder has no current branch.
pub fn choose_base(env: &mut Env, repo: &str, base_branch: &str) -> (String, String) {
    let integration = delivery::integration_branch(env, base_branch);
    if branch_exists(repo, &integration) {
        return (integration, String::new());
    }
    let current = git(repo, &["branch", "--show-current"]).1;
    if current.is_empty() {
        return (String::new(), String::new());
    }
    let warning = format!("the integration branch `{integration}` does not exist locally: the worktree starts from the current branch `{current}`");
    (current, warning)
}

pub fn base_sha_of(repo: &str, base: &str) -> String {
    if branch_exists(repo, base) {
        git(repo, &["rev-parse", "--short", &format!("refs/heads/{base}")]).1
    } else {
        String::new()
    }
}

fn warn_line(tag: &str, text: &str) {
    if tag.is_empty() {
        use std::io::Write;
        println!("[loop] warn: {text}");
        let _ = std::io::stdout().flush();
    } else {
        step(tag, "warn", text);
    }
}

fn plain_line(tag: &str, kind: &str, text: &str) {
    if tag.is_empty() {
        use std::io::Write;
        println!("[loop] {text}");
        let _ = std::io::stdout().flush();
    } else {
        step(tag, kind, text);
    }
}

fn head(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars[..chars.len().min(n)].iter().collect()
}

/// The carried setup files become ONE commit on the loop branch right away (`git add` of those paths only, the loop's git identity), so
/// the worktree stays clean for a safe Finish. Gitignored files are not force-added: they stay engine state that Close tolerates.
pub fn commit_carried_setup(work: &str, carried: &[String], tag: &str) -> bool {
    let (mut tracked, mut ignored): (Vec<String>, Vec<String>) = (vec![], vec![]);
    for path in carried {
        if git(work, &["check-ignore", "-q", "--", path]).0 != 0 {
            tracked.push(path.clone())
        } else {
            ignored.push(path.clone())
        }
    }
    tracked.retain(|path| !git(work, &["status", "--porcelain", "--", path]).1.trim().is_empty()); // unchanged = nothing to commit
    if !ignored.is_empty() {
        warn_line(tag, &format!("carried setup files ignored by .gitignore (left uncommitted as engine state): {}", ignored.join(", ")));
    }
    if tracked.is_empty() {
        return false;
    }
    let mut add: Vec<&str> = vec!["add", "--"];
    add.extend(tracked.iter().map(String::as_str));
    let (mut code, _, mut err) = git(work, &add);
    if code == 0 {
        let mut commit: Vec<&str> = vec!["commit", "-q", "-m", SETUP_COMMIT_MESSAGE, "--"];
        commit.extend(tracked.iter().map(String::as_str));
        (code, _, err) = git(work, &commit);
    }
    if code != 0 {
        let why = if err.is_empty() { "git failed".to_string() } else { err };
        warn_line(tag, &format!("could not commit the carried setup files ({}); they stay uncommitted in the worktree", head(&why, 200)));
        return false;
    }
    plain_line(tag, "commit", &format!("committed {} carried setup file{}: {SETUP_COMMIT_MESSAGE}", tracked.len(), if tracked.len() == 1 { "" } else { "s" }));
    true
}

/// The uncommitted setup files of `repo` go into the new worktree (never over a file the branch has). Returns the paths copied.
pub fn carry_setup_into(repo: &str, work: &str, tag: &str, entry: Option<&Map<String, Value>>, include_specs: bool, also_commit: &[String]) -> Vec<String> {
    let listed: Vec<String> = setup_files::list_setup_files(repo, entry).iter().filter_map(|i| i["path"].as_str().map(String::from)).collect();
    let carried = setup_files::carry_setup_files(repo, work, entry, include_specs);
    if !carried.is_empty() {
        plain_line(tag, "copy", &format!("carried uncommitted setup files: {}", carried.join(", ")));
    }
    let mut to_commit = carried.clone();
    to_commit.extend(also_commit.iter().filter(|p| !carried.contains(p)).cloned());
    commit_carried_setup(work, &to_commit, tag);
    let others = setup_files::list_other_changes(repo, &listed);
    if !others.is_empty() {
        warn_line(
            tag,
            &format!("{} other changed file{} in your folder NOT carried: {}", others.len(), if others.len() == 1 { "" } else { "s" }, setup_files::shortened(&others)),
        );
    }
    carried
}

/// What the last Close loop recorded for repo `name` (`{"branch", "base", ...}`) in the primary repo's last-run.json, or `{}`.
pub fn last_run_entry(primary_repo: &str, name: &str) -> Map<String, Value> {
    load_json(&loop_path(primary_repo, LAST_RUN_NAME)).get("repos").and_then(|r| r.get(name)).and_then(Value::as_object).cloned().unwrap_or_default()
}

/// The branch the next start re-attaches to for repo `name`: the one the last Close loop recorded when it still exists and that run was
/// NOT finished (a wrap-up Finish marks `finished`: the next work starts on a new branch). Empty when there is nothing to re-attach.
pub fn reattach_branch_of(primary_repo: &str, name: &str, repo: Option<&str>) -> String {
    let recorded = last_run_entry(primary_repo, name);
    if crate::notify_truthy(recorded.get("finished")) {
        return String::new();
    }
    let branch = recorded.get("branch").and_then(Value::as_str).unwrap_or("");
    if branch_exists(repo.unwrap_or(primary_repo), branch) {
        branch.to_string()
    } else {
        String::new()
    }
}

pub fn file_sha256(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(format!("sha256:{}", Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

/// Copies the ticked git-ignored files (REPOS.json `copy`) from the user's repo into the worktree. Files only, relative to the repo, never
/// inside node_modules/bin/obj/dist. Never overwrites. Returns `{name: sha256 of the SOURCE}` (Close loop compares against it).
pub fn copy_local_files(repo: &str, work: &str, names: &[String], tag: &str) -> Map<String, Value> {
    let (mut hashes, mut skipped) = (Map::new(), Vec::<String>::new());
    let real_repo = nt_realpath(repo);
    for name in names {
        let relative = name.replace('\\', "/").trim().trim_matches('/').to_string();
        let join = |base: &str| relative.split('/').fold(PathBuf::from(base), |acc, p| acc.join(p));
        let (source, target) = (join(repo), join(work));
        let inside = nt_realpath(&source.display().to_string()).starts_with(&format!("{real_repo}\\"));
        let heavy = relative.split('/').any(|p| HEAVY_FOLDERS.contains(&p));
        if relative.is_empty() || !inside || heavy || !source.is_file() {
            skipped.push(relative);
            continue;
        }
        if !target.exists() {
            let _ = std::fs::create_dir_all(target.parent().expect("has parent"));
            let _ = copy2(&source, &target);
        }
        if let Ok(hash) = file_sha256(&source) {
            hashes.insert(relative, json!(hash));
        }
    }
    if !skipped.is_empty() {
        step(tag, "warn", &format!("not copied (missing, a folder or a generated folder): {}", skipped.join(", ")));
    }
    hashes
}

/// Reason when git is older than 2.5 (no `git worktree`), else empty.
pub fn git_version_problem(cwd: &str) -> String {
    let Some(m) = VERSION.search(&git(cwd, &["--version"]).1) else { return "could not read the git version".into() };
    let version: (u32, u32) = (m.group(1).and_then(|s| s.parse().ok()).unwrap_or(0), m.group(2).and_then(|s| s.parse().ok()).unwrap_or(0));
    if version >= MIN_GIT_WORKTREE_VERSION {
        String::new()
    } else {
        format!("git {}.{} is too old for worktrees (needs 2.5 or newer)", version.0, version.1)
    }
}

/// True while no loop ran in the worktree: no LOOP.json, no turn record, no lock and `turns` 0 in meta.json (a bare worktree).
pub fn worktree_never_ran(work: &str) -> bool {
    let loop_folder = loop_path(work, "meta.json").parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    if loop_folder.join("json").join("LOOP.json").exists() || loop_folder.join("internal").join("LOOP.lock").exists() {
        return false;
    }
    let turns = loop_folder.join("internal").join("turns");
    if std::fs::read_dir(turns).map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().ends_with(".json"))).unwrap_or(false) {
        return false;
    }
    let value = load_json(&loop_path(work, "meta.json")).get("turns").cloned();
    !matches!(value, Some(Value::Number(ref n)) if n.as_i64().is_some_and(|t| t > 0))
}

/// A reused worktree that never ran a loop gets `.specs`, `CLAUDE.md` and `.claude` copied again from the primary repo: the copy made at
/// creation can be stale. What the worktree itself holds under `.specs/project/loop` is put back over the fresh copy; only the roadmap
/// approval always follows the primary.
pub fn refresh_unused_worktree(repo: &str, work: &str, meta_path: &Path) {
    if !worktree_never_ran(work) {
        return;
    }
    let loop_folder = meta_path.parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    let Ok(keep_folder) = tempfile::Builder::new().prefix("loop-refresh-").tempdir() else { return };
    let kept = keep_folder.path().join("loop");
    if loop_folder.is_dir() {
        let _ = copy_tree(&loop_folder, &kept, &[]);
    }
    let mut refreshed: Vec<&str> = Vec::new();
    for name in [".specs", "CLAUDE.md", ".claude"] {
        let (source, target) = (Path::new(repo).join(name), Path::new(work).join(name));
        if !source.exists() {
            continue;
        }
        if target.is_dir() {
            let _ = std::fs::remove_dir_all(&target);
        } else if target.exists() {
            let _ = std::fs::remove_file(&target);
        }
        let _ = if source.is_dir() { copy_tree(&source, &target, &[]) } else { copy2(&source, &target) };
        refreshed.push(name);
    }
    loop_state::migrate_layout(work);
    if kept.is_dir() {
        let _ = copy_tree(&kept, &loop_folder, &["ROADMAP-APPROVED.json"]);
    }
    if !refreshed.is_empty() {
        println!("[loop] refreshed from the repo (the worktree never ran a loop): {}", refreshed.join(", "));
    }
}

fn inventory_relative(name: &str) -> String {
    let mut parts: Vec<&str> = SYNCED_INVENTORY[..5].to_vec();
    if !name.is_empty() {
        parts.push(name);
    }
    parts.join("\\")
}

/// Makes the git-ignored code inventory (`.specs/project/loop/internal/inventory/*.json`) and `SUBPROJECTS.json` visible from the worktree.
/// The inventory is written in the primary repo, usually AFTER the worktree was created or in a worktree that already ran: each inventory
/// file is copied when missing in the worktree or older than the repo's copy; `SUBPROJECTS.json` only when missing. Returns the relative
/// paths copied.
pub fn sync_untracked_map_files(repo: &str, work: &str) -> Vec<String> {
    let mut copied = Vec::new();
    let source_folder = SYNCED_INVENTORY[..5].iter().fold(PathBuf::from(repo), |acc, p| acc.join(p));
    if source_folder.is_dir() {
        let mut names: Vec<String> =
            std::fs::read_dir(&source_folder).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        names.sort();
        for name in names {
            let source = source_folder.join(&name);
            let target = SYNCED_INVENTORY[..5].iter().fold(PathBuf::from(work), |acc, p| acc.join(p)).join(&name);
            if !name.to_lowercase().ends_with(".json") || !source.is_file() {
                continue;
            }
            let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            if target.is_file() && modified(&target) >= modified(&source) {
                continue;
            }
            let _ = std::fs::create_dir_all(target.parent().expect("has parent"));
            if copy2(&source, &target).is_ok() {
                copied.push(inventory_relative(&name));
            }
        }
    }
    let join = |base: &str| SYNCED_SUBPROJECTS.iter().fold(PathBuf::from(base), |acc, p| acc.join(p));
    let (source, target) = (join(repo), join(work));
    if source.is_file() && !target.exists() {
        let _ = std::fs::create_dir_all(target.parent().expect("has parent"));
        if copy2(&source, &target).is_ok() {
            copied.push(SYNCED_SUBPROJECTS.join("\\"));
        }
    }
    copied
}

/// `(command, auto)`: the stored setup always wins, then the CLI one, else what the repo's manifests say (nothing runs to detect).
pub fn resolve_setup(item: &RepoRef, cli_setup: &str) -> (String, bool) {
    let stored = if item.setup.is_empty() { cli_setup } else { item.setup.as_str() };
    if !stored.is_empty() {
        return (stored.to_string(), false);
    }
    match detect_setup::detect_setup(&item.path) {
        Some(d) => (d, true),
        None => (String::new(), false),
    }
}

/// Stops BEFORE the worktree is created when the repo's stored setup (REPOS.json, agent-writable) is not the text the user saved in the
/// app: the step line `[repo] error: ...` and the exit text show why. Nothing runs.
pub fn refuse_unapproved_setup(ctx: &Ctx, item: &RepoRef, primary_repo: &str) -> Result<(), String> {
    let problem = setup_approval::describe_unapproved_setup(&ctx.data_dir(), primary_repo, item);
    if problem.is_empty() {
        return Ok(());
    }
    step(&item.name, "error", &problem);
    Err(format!("repo {}: {problem}", item.name))
}

/// Files to copy into the worktree: the stored list when configured (`[]` = none), else the suggested git-ignored local config.
pub fn resolve_copy(item: &RepoRef) -> Vec<String> {
    if item.copy_configured || !item.copy.is_empty() {
        return item.copy.clone();
    }
    if !Path::new(&item.path).is_dir() {
        return vec![];
    }
    let (code, output, _) = git(&item.path, &detect_setup::IGNORED_FILES_ARGUMENTS);
    detect_setup::suggest_copy_files(&item.path, if code == 0 { &output } else { "" }).iter().filter_map(|e| e["path"].as_str().map(String::from)).collect()
}

/// `subprocess.run(setup, cwd=work, shell=True).returncode`.
fn run_shell(command: &str, cwd: &str) -> i32 {
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd").arg("/C").raw_arg(command).current_dir(cwd).status()
    };
    #[cfg(not(windows))]
    let status = std::process::Command::new("sh").arg("-c").arg(command).current_dir(cwd).status();
    status.ok().and_then(|s| s.code()).unwrap_or(1)
}

fn now_stamp() -> String {
    time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(time::macros::format_description!("[year][month][day]-[hour][minute]"))
        .unwrap_or_default()
}

fn copy_into_worktree(repo: &str, work: &str) -> Vec<String> {
    let mut copied = Vec::new();
    for name in [".specs", "CLAUDE.md", ".claude"] {
        let (source, target) = (Path::new(repo).join(name), Path::new(work).join(name));
        if source.exists() && !target.exists() {
            let _ = if source.is_dir() { copy_tree(&source, &target, &[]) } else { copy2(&source, &target) };
            copied.push(name.to_string());
        }
    }
    copied
}

/// Creates the sibling worktree and the loop branch ONCE. Reuses it if it exists. Never recreates it every turn. When a previous Close loop
/// recorded a branch that still exists (last-run.json) and the folder is gone, the worktree is re-attached to that branch instead of
/// creating a new one (a new branch only with --branch). `tag` = the repo name in a multi-repo project; `copy_files` = the ticked
/// git-ignored files, their hashes are merged into `copy_hashes`; `setup` = the one-time command (`None` = the CLI --setup).
/// `Err(message)` is what the script prints before exiting with 1.
#[allow(clippy::too_many_arguments)]
pub fn ensure_worktree(
    ctx: &mut Ctx,
    repo: &str,
    opts: &WtOpts,
    tag: &str,
    copy_files: &[String],
    copy_hashes: Option<&mut Map<String, Value>>,
    setup: Option<&str>,
    setup_auto: bool,
) -> Result<(String, Value), String> {
    let setup = setup.unwrap_or(&opts.setup).to_string();
    let work = worktree_of(repo);
    let meta_path = loop_path(&work, "meta.json");
    if Path::new(&work).is_dir() {
        loop_state::migrate_layout(&work); // worktree created with an older layout
        let meta = load_json(&meta_path);
        let current = git(&work, &["branch", "--show-current"]).1;
        let recorded = meta.get("branch").and_then(Value::as_str).unwrap_or("").to_string();
        if recorded.is_empty() || current != recorded {
            if !tag.is_empty() {
                step(tag, "error", &format!("folder {work} already exists but is not this loop's worktree (current branch: {current})"));
            }
            return Err(format!("folder {work} already exists but is not this loop's worktree (current branch: {current}). Remove or rename it and run again."));
        }
        if !opts.dry_run {
            refresh_unused_worktree(repo, &work, &meta_path);
            let synced = sync_untracked_map_files(repo, &work);
            if !synced.is_empty() {
                println!("[loop] copied from the repo into the worktree: {}", synced.iter().map(|p| p.replace('\\', "/")).collect::<Vec<_>>().join(", "));
            }
            refresh_inputs(ctx, repo, &work, tag, false); // start or resume: the panel may have changed .env.test / NETWORK.json since the last run
        }
        let base = meta.get("base").and_then(Value::as_str).unwrap_or("").to_string();
        if !tag.is_empty() {
            step(tag, "reuse", &format!("worktree {work} on branch {recorded}"));
            step(tag, "ready", &format!("path={work} branch={recorded} base={base}"));
        } else {
            println!("[loop] reusing worktree {work} on branch {recorded}");
        }
        return Ok((work, meta));
    }
    if !tag.is_empty() {
        let problem = git_version_problem(repo);
        if !problem.is_empty() {
            step(tag, "error", &problem);
            return Err(format!("repo {tag}: {problem}"));
        }
    }
    let (code, current_branch, err) = git(repo, &["branch", "--show-current"]);
    if code != 0 || current_branch.is_empty() {
        if !tag.is_empty() {
            step(tag, "error", &format!("repo has no current branch (detached HEAD?): {err}"));
        }
        return Err(format!("repo has no current branch (detached HEAD?): {err}"));
    }
    let (mut base, base_warning) = choose_base(ctx.env, repo, &opts.base_branch);
    if !tag.is_empty() {
        step(tag, "check", &format!("git ok; current branch {current_branch}; the worktree starts from {base}"));
    }
    if !base_warning.is_empty() {
        warn_line(tag, &base_warning);
    }
    let recorded_name = if tag.is_empty() { project_repos::short_name_of(repo) } else { tag.to_string() };
    let recorded = last_run_entry(repo, &recorded_name);
    let reattach = if opts.branch.is_empty() { reattach_branch_of(repo, &recorded_name, None) } else { String::new() };
    let branch = if !reattach.is_empty() {
        reattach.clone()
    } else if !opts.branch.is_empty() {
        opts.branch.clone()
    } else {
        format!("loop/{}", now_stamp())
    };
    if !reattach.is_empty() {
        if let Some(b) = recorded.get("base").and_then(Value::as_str).filter(|b| !b.is_empty()) {
            base = b.to_string();
        }
    }
    if opts.dry_run {
        println!(
            "[dry-run] git worktree add {}{work} {}",
            if reattach.is_empty() { format!("-b {branch} ") } else { String::new() },
            if reattach.is_empty() { &base } else { &reattach }
        );
        return Ok((work.clone(), json!({"branch": branch, "base": base, "worktree": work, "copied": []})));
    }
    if !reattach.is_empty() {
        git(repo, &["worktree", "prune"]); // the removed folder may still be registered against the branch
        let (code, _, err) = git(repo, &["worktree", "add", &work, &branch]);
        if code != 0 {
            if !tag.is_empty() {
                step(tag, "error", &format!("failed to re-attach worktree: {err}"));
            }
            return Err(format!("failed to re-attach worktree to {branch}: {err}"));
        }
        step(tag, "reattach", &format!("{branch} (existing branch recorded by the last Close loop; no new branch)"));
    } else {
        if !tag.is_empty() {
            step(tag, "worktree-add", &format!("git worktree add -b {branch} {work} {base}"));
        }
        let (code, _, err) = git(repo, &["worktree", "add", "-b", &branch, &work, &base]);
        if code != 0 {
            if !tag.is_empty() {
                step(tag, "error", &format!("failed to create worktree: {err}"));
            }
            return Err(format!("failed to create worktree: {err}"));
        }
    }
    let copied = copy_into_worktree(repo, &work);
    // the root CLAUDE.md copied whole (not "carried") is a setup file too when it holds an app block: it joins the setup commit
    let copied_claude: Vec<String> =
        if copied.iter().any(|c| c == "CLAUDE.md") && setup_files::has_app_block(&work, "CLAUDE.md") { vec!["CLAUDE.md".into()] } else { vec![] };
    let entry = subprojects::load(repo, None).0;
    let carried = carry_setup_into(repo, &work, tag, entry.as_ref(), true, &copied_claude);
    loop_state::migrate_layout(&work); // .specs copied from a repo still in an older layout
    sync_untracked_map_files(repo, &work); // a `.specs` that already existed in the worktree (tracked) still gets the ignored inventory
    refresh_inputs(ctx, repo, &work, tag, false); // .env.test and NETWORK.json even when .specs/project is versioned and the carry leaves .env* out
    let hashes = if copy_files.is_empty() { Map::new() } else { copy_local_files(repo, &work, copy_files, tag) };
    if let Some(out) = copy_hashes {
        for (k, v) in &hashes {
            out.insert(k.clone(), v.clone());
        }
    }
    let mut meta = json!({
        "branch": branch, "base": base, "base_sha": base_sha_of(repo, &base), "worktree": work, "repo": repo, "copied": copied,
        "carried_setup_files": carried, "created_at": loop_io_now(), "turns": 0, "cost_usd": 0.0,
        "note": "worktree used ONLY by run_loop.py; the orchestrator commits here and never pushes",
    });
    merge_pending_delivery(repo, &mut meta);
    let _ = save_meta(&meta_path, &mut meta);
    if !tag.is_empty() {
        let mut names = copied.clone();
        names.extend(hashes.keys().cloned());
        step(tag, "copy", &if names.is_empty() { "nothing".to_string() } else { names.join(", ") });
    } else {
        println!(
            "[loop] worktree created: {work} (branch {branch}, base {base}; copied from the repo: {})",
            if copied.is_empty() { "nothing".to_string() } else { copied.join(", ") }
        );
    }
    if !setup.is_empty() {
        if !tag.is_empty() {
            step(tag, "setup", &format!("{setup}{}", if setup_auto { " (auto-detected)" } else { "" }));
        } else {
            println!("[loop] one-time setup: {setup}");
        }
        let code = run_shell(&setup, &work);
        if code != 0 {
            if !tag.is_empty() {
                step(tag, "error", &format!("setup failed (code {code})"));
            }
            return Err(format!(
                "{}setup failed (code {code}); fix it and run again (the worktree already exists)",
                if tag.is_empty() { String::new() } else { format!("repo {tag}: ") }
            ));
        }
    }
    if !tag.is_empty() {
        step(tag, "ready", &format!("path={work} branch={branch} base={base}"));
    }
    Ok((work, meta))
}

fn loop_io_now() -> String {
    crate::loop_io::now_iso()
}

/// Worktree of a NON-primary repo, on the same loop branch as the primary. `known` = what meta.repos recorded on a previous run. Born from
/// the user's folder; only the ticked `copy` files are copied (no .specs, CLAUDE.md or .claude: the loop runs in the primary). Returns the
/// meta.repos entry. Every step is a `[loop] [<repo>] <kind>: <text>` line.
pub fn ensure_extra_worktree(ctx: &mut Ctx, item: &RepoRef, opts: &WtOpts, branch: &str, known: &Map<String, Value>) -> Result<Value, String> {
    let (name, work) = (item.name.as_str(), item.loop_worktree_path());
    let known_base = known.get("base").and_then(Value::as_str).unwrap_or("").to_string();
    let mut entry = json!({"path": item.path, "worktree": work, "branch": branch, "base": known_base, "primary": false,
                           "copied": known.get("copied").filter(|c| c.is_object()).cloned().unwrap_or(json!({}))});
    if Path::new(&work).is_dir() {
        let current = git(&work, &["branch", "--show-current"]).1;
        if current != branch {
            step(name, "error", &format!("folder {work} already exists but is not this loop's worktree (current branch: {current}, expected {branch})"));
            return Err(format!(
                "repo {name}: folder {work} already exists but is not this loop's worktree (current branch: {current}). Remove or rename it and run again."
            ));
        }
        let base = if known_base.is_empty() { git(&item.path, &["branch", "--show-current"]).1 } else { known_base };
        entry["base"] = json!(base);
        step(name, "reuse", &format!("worktree {work} on branch {branch}"));
        step(name, "ready", &format!("path={work} branch={branch} base={base}"));
        return Ok(entry);
    }
    let problem = git_version_problem(&item.path);
    if !problem.is_empty() {
        step(name, "error", &problem);
        return Err(format!("repo {name}: {problem}"));
    }
    let (code, current_branch, err) = git(&item.path, &["branch", "--show-current"]);
    if code != 0 || current_branch.is_empty() {
        step(name, "error", &format!("repo has no current branch (detached HEAD?): {err}"));
        return Err(format!("repo {name} has no current branch (detached HEAD?): {err}"));
    }
    let (base, base_warning) = choose_base(ctx.env, &item.path, &opts.base_branch);
    entry["base"] = json!(base);
    step(name, "check", &format!("git ok; current branch {current_branch}; the worktree starts from {base}"));
    if !base_warning.is_empty() {
        step(name, "warn", &base_warning);
    }
    refuse_unapproved_setup(ctx, item, &opts.repo_path)?;
    let recorded = last_run_entry(&opts.repo_path, name);
    let reattach = branch_exists(&item.path, branch) && recorded.get("branch").and_then(Value::as_str) == Some(branch) && !crate::notify_truthy(recorded.get("finished"));
    let mut base = base;
    if reattach {
        if let Some(b) = recorded.get("base").and_then(Value::as_str).filter(|b| !b.is_empty()) {
            base = b.to_string();
        }
        entry["base"] = json!(base);
    }
    if opts.dry_run {
        println!(
            "[dry-run] [{name}] git worktree add {}{work} {}",
            if reattach { String::new() } else { format!("-b {branch} ") },
            if reattach { branch } else { &base }
        );
        return Ok(entry);
    }
    let (mut code, mut err);
    if reattach {
        git(&item.path, &["worktree", "prune"]);
        (code, _, err) = git(&item.path, &["worktree", "add", &work, branch]);
        if code == 0 {
            step(name, "reattach", &format!("{branch} (existing branch recorded by the last Close loop; no new branch)"));
        }
    } else {
        step(name, "worktree-add", &format!("git worktree add -b {branch} {work} {base}"));
        (code, _, err) = git(&item.path, &["worktree", "add", "-b", branch, &work, &base]);
    }
    if code != 0 {
        step(name, "error", &format!("failed to create worktree: {err}"));
        return Err(format!("repo {name}: failed to create worktree: {err}"));
    }
    let copied = copy_local_files(&item.path, &work, &resolve_copy(item), name);
    let sub_entry = subprojects::load(&opts.repo_path, Some(name)).0;
    let carried = carry_setup_into(&item.path, &work, name, sub_entry.as_ref(), false, &[]);
    entry["base_sha"] = json!(base_sha_of(&item.path, entry["base"].as_str().unwrap_or("")));
    entry["carried_setup_files"] = json!(carried);
    let copied_names: Vec<&String> = copied.keys().collect();
    step(name, "copy", &if copied_names.is_empty() { "nothing".to_string() } else { copied_names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ") });
    entry["copied"] = Value::Object(copied);
    let (setup, auto) = resolve_setup(item, "");
    if !setup.is_empty() {
        step(name, "setup", &format!("{setup}{}", if auto { " (auto-detected)" } else { "" }));
        code = run_shell(&setup, &work);
        err = String::new();
        if code != 0 {
            step(name, "error", &format!("setup failed (code {code})"));
            return Err(format!("repo {name}: setup failed (code {code}); fix it and run again (the worktree already exists)"));
        }
    } else if !opts.setup.is_empty() {
        step(name, "warn", &format!("--setup ({}) runs only in the primary repo; set this repo's own setup in the repository manager", opts.setup));
    }
    let _ = err;
    step(name, "ready", &format!("path={work} branch={branch} base={}", entry["base"].as_str().unwrap_or("")));
    Ok(entry)
}

/// One worktree for the primary plus one per `edit` repo that a ROADMAP feature changes (`paths=<repo>:`), all on the same loop branch;
/// returns `(primary worktree, meta)`. `reference` repos and edit repos nothing changes get no worktree. A project whose only worktree is
/// the primary's goes through [`ensure_worktree`] alone: same lines, same meta.json as before. Otherwise
/// `meta["repos"] = {name: {path, worktree, branch, base, primary, copied}}`; the top-level fields still describe the primary.
pub fn ensure_worktrees(ctx: &mut Ctx, repo: &str, opts: &mut WtOpts) -> Result<(String, Value), String> {
    let repos = project_repos::load_repos(repo);
    let work_now = worktree_of(repo);
    let features = roadmap_features(if Path::new(&work_now).is_dir() { &work_now } else { repo });
    let targets: Vec<String> = worktree_repos(&repos, &features, opts.only_repos.as_deref()).into_iter().map(|r| r.name).collect();
    if targets.len() == 1 {
        return ensure_worktree(ctx, repo, opts, "", &[], None, None, false);
    }
    let primary = project_repos::primary_of(&repos).clone();
    let mut hashes = Map::new();
    if !Path::new(&work_now).is_dir() {
        refuse_unapproved_setup(ctx, &primary, repo)?; // an existing worktree never runs setup again
    }
    let (primary_setup, primary_auto) = resolve_setup(&primary, &opts.setup);
    let (work, mut meta) = ensure_worktree(ctx, repo, opts, &primary.name, &resolve_copy(&primary), Some(&mut hashes), Some(&primary_setup), primary_auto)?;
    let previous = meta.get("repos").and_then(Value::as_object).cloned().unwrap_or_default();
    let primary_copied = if hashes.is_empty() {
        previous.get(&primary.name).and_then(|p| p.get("copied")).filter(|c| c.is_object()).cloned().unwrap_or(json!({}))
    } else {
        Value::Object(hashes)
    };
    let branch = meta["branch"].as_str().unwrap_or("").to_string();
    let mut entries: BTreeMap<String, Value> = BTreeMap::new();
    let mut order: Vec<String> = vec![primary.name.clone()];
    entries.insert(primary.name.clone(), json!({"path": repo, "worktree": work, "branch": branch, "base": meta.get("base").and_then(Value::as_str).unwrap_or(""), "primary": true, "copied": primary_copied}));
    opts.repo_path = repo.to_string();
    for item in &repos {
        if item.primary {
            continue;
        }
        order.push(item.name.clone());
        if !targets.contains(&item.name) {
            // a reference repo, an edit repo no feature changes, or one left out by --repos: keep what was recorded
            if let Some(old) = previous.get(&item.name).filter(|o| crate::notify_truthy(Some(*o))) {
                entries.insert(item.name.clone(), old.clone());
            }
            continue;
        }
        let known = previous.get(&item.name).and_then(Value::as_object).cloned().unwrap_or_default();
        let created = ensure_extra_worktree(ctx, item, opts, &branch, &known)?;
        entries.insert(item.name.clone(), created);
    }
    let mut repos_value = Map::new();
    for name in order {
        if let Some(v) = entries.remove(&name) {
            repos_value.insert(name, v);
        }
    }
    meta["repos"] = Value::Object(repos_value);
    if !opts.dry_run {
        let _ = save_meta(&loop_path(&work, "meta.json"), &mut meta);
    }
    Ok((work, meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn repo(parent: &Path, name: &str) -> String {
        let dir = parent.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        git_in(&dir, &["init", "-q", "-b", "developer"]);
        git_in(&dir, &["config", "user.name", "t"]);
        git_in(&dir, &["config", "user.email", "t@example.com"]);
        std::fs::write(dir.join("README.md"), "x").unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-q", "-m", "init"]);
        dir.display().to_string()
    }

    fn env() -> Env {
        Env::from_pairs([("LOOP_DATA_DIR", "C:\\no-such-dir")])
    }

    #[test]
    fn creates_once_then_reuses_and_carries_setup_files() {
        let parent = tempfile::tempdir().unwrap();
        let r = repo(parent.path(), "api");
        std::fs::create_dir_all(Path::new(&r).join(".specs").join("project")).unwrap();
        std::fs::write(Path::new(&r).join(".specs/project/ROADMAP.md"), "## Stage 1\n- [a] status=pending\n").unwrap();
        std::fs::create_dir_all(Path::new(&r).join("docs/codebase")).unwrap();
        std::fs::write(Path::new(&r).join("docs/codebase/STACK.md"), "map").unwrap();
        let mut e = env();
        let mut ctx = Ctx { env: &mut e, toolkit_root: String::new() };
        let mut opts = WtOpts { branch: "loop/test".into(), ..Default::default() };
        let (work, meta) = ensure_worktrees(&mut ctx, &r, &mut opts).unwrap();
        assert!(work.ends_with("api-worktree-loop") && Path::new(&work).join("README.md").is_file());
        assert_eq!((meta["branch"].as_str(), meta["base"].as_str(), meta["turns"].as_i64()), (Some("loop/test"), Some("developer"), Some(0)));
        assert!(Path::new(&work).join(".specs/project/ROADMAP.md").is_file());
        assert!(Path::new(&work).join("docs/codebase/STACK.md").is_file());
        let log = git(&work, &["log", "--oneline"]).1;
        assert!(log.contains("chore(setup): carry codebase maps and agent context"), "{log}");
        assert!(git(&work, &["status", "--porcelain"]).1.is_empty() || !git(&work, &["status", "--porcelain"]).1.contains("STACK.md"));
        let again = ensure_worktrees(&mut ctx, &r, &mut opts).unwrap();
        assert_eq!(again.1["branch"], "loop/test");
        let saved = load_json(&loop_path(&work, "meta.json"));
        assert_eq!(saved["branch"], "loop/test");
    }

    #[test]
    fn refuses_a_foreign_folder_and_dry_run_creates_nothing() {
        let parent = tempfile::tempdir().unwrap();
        let r = repo(parent.path(), "web");
        let mut e = env();
        let mut ctx = Ctx { env: &mut e, toolkit_root: String::new() };
        let dry = WtOpts { dry_run: true, branch: "loop/x".into(), ..Default::default() };
        let (work, meta) = ensure_worktree(&mut ctx, &r, &dry, "", &[], None, None, false).unwrap();
        assert!(!Path::new(&work).exists() && meta["branch"] == "loop/x");
        std::fs::create_dir_all(&work).unwrap();
        let err = ensure_worktree(&mut ctx, &r, &WtOpts::default(), "", &[], None, None, false).unwrap_err();
        assert!(err.starts_with(&format!("folder {work} already exists but is not this loop's worktree (current branch: ")));
    }

    #[test]
    fn local_files_skip_heavy_and_outside() {
        let parent = tempfile::tempdir().unwrap();
        let r = repo(parent.path(), "svc");
        std::fs::write(Path::new(&r).join(".env"), "A=1").unwrap();
        std::fs::create_dir_all(Path::new(&r).join("node_modules")).unwrap();
        std::fs::write(Path::new(&r).join("node_modules/.env"), "x").unwrap();
        let work = tempfile::tempdir().unwrap();
        let w = work.path().display().to_string();
        let hashes = copy_local_files(&r, &w, &[".env".into(), "node_modules/.env".into(), "missing".into(), "../x".into()], "");
        assert_eq!(hashes.keys().collect::<Vec<_>>(), vec![".env"]);
        assert!(hashes[".env"].as_str().unwrap().starts_with("sha256:") && work.path().join(".env").is_file());
    }

    #[test]
    fn base_choice_and_git_version() {
        let parent = tempfile::tempdir().unwrap();
        let r = repo(parent.path(), "b");
        let mut e = env();
        assert_eq!(choose_base(&mut e, &r, ""), ("developer".to_string(), String::new()));
        let (base, warning) = choose_base(&mut e, &r, "qa");
        assert_eq!(base, "developer");
        assert_eq!(warning, "the integration branch `qa` does not exist locally: the worktree starts from the current branch `developer`");
        assert_eq!(git_version_problem(&r), "");
        assert!(branch_exists(&r, "developer") && !branch_exists(&r, ""));
    }
}
