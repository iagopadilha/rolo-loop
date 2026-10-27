// SPDX-License-Identifier: MIT
//! The checks of the preflight (`--check` and before every run): environment, ROADMAP, approval, test environment, notice channels, push,
//! setup and the extra `--check --planning` readiness. Each returns its own `(problems, warnings)` pair.

use crate::app_ctx::AppCtx;
use crate::auto_answers::{gate_blocked_features, pick_auto_answer};
use crate::loop_io::{git, load_json, read, run_timeout, which, worktree_of};
use crate::loop_opts::LoopOpts;
use crate::loop_state::{check_fields, loop_path, parse_roadmap, state_file, Features, APPROVAL_NAME};
use crate::project_repos::{self, RepoRef, ROLE_REFERENCE};
use crate::worktree_state::roadmap_score;
use crate::worktrees::{branch_exists, choose_base, worktree_repos};
use crate::{audit_deps, claude_binaries, credential_check, mcp_access, notify, readiness_checks, rules, subprojects};
use cfg::env::{app_env_list, app_env_value, load_all};
use pycompat::{py_repr_str, Re};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const MAX_NAMED_FEATURES: usize = 6;
type Pair = (Vec<String>, Vec<String>);

static CONFORMANCE: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?m)^Conformance:\s*(STANDARD|LEGACY)"));
static GUARD_EXE: LazyLock<Re> = LazyLock::new(|| Re::new(r#"([^\s"]*guard\.exe)"#));
static REGRESSION: LazyLock<Re> = LazyLock::new(|| Re::new(r"\bregression=(yes|baseline)\b"));
static FILL_IN: LazyLock<Re> = LazyLock::new(|| Re::new("FILL_IN"));
static COMMAND_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"command:\s*(.+)"));
static REPO_PATH_PREFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"^([a-z0-9._-]+):(?![/\\])(.+)\z"));

fn env_of(ctx: &AppCtx) -> cfg::Env {
    let mut env = ctx.env.clone();
    load_all(&mut env);
    env
}

fn roadmap(root: &str) -> String {
    read(&Path::new(root).join(".specs").join("project").join("ROADMAP.md"))
}

fn push_new(target: &mut Vec<String>, items: Vec<String>) {
    for item in items {
        if !target.contains(&item) {
            target.push(item);
        }
    }
}

/// Mandatory notice channels: `LOOP_NOTIFY_REQUIRED` (comma-separated; empty or unset = none).
pub fn required_channels(ctx: &AppCtx) -> Vec<String> {
    app_env_list(&env_of(ctx), "LOOP_NOTIFY_REQUIRED")
}

/// Git identity for the push. Any account works; to require a specific one use `--git-user` or `LOOP_GIT_USER` (app .env).
pub fn identity_problem(ctx: &AppCtx, cwd: &str, opts: &LoopOpts) -> String {
    let user = git(cwd, &["config", "user.name"]).1;
    let mail = git(cwd, &["config", "user.email"]).1;
    if user.is_empty() || mail.is_empty() {
        return "git user.name/user.email not configured (git config --global user.name ...)".into();
    }
    let wanted = if opts.git_user.is_empty() { app_env_value(&ctx.env, "LOOP_GIT_USER") } else { opts.git_user.clone() };
    if !wanted.is_empty() && user != wanted {
        return format!("git user.name is {}, expected {} (--git-user or LOOP_GIT_USER in the app .env)", py_repr_str(&user), py_repr_str(&wanted));
    }
    String::new()
}

/// Expands `${CLAUDE_PLUGIN_ROOT}` / `$CLAUDE_PLUGIN_ROOT` to the resolved plugin root (and `${HOME}`-style variables).
pub fn expand_plugin_vars(ctx: &AppCtx, raw: &str, plugin_root: &str) -> String {
    let home = ctx.env.home_dir().display().to_string();
    let mut text = raw.to_string();
    for name in ["${CLAUDE_PLUGIN_ROOT}", "$CLAUDE_PLUGIN_ROOT"] {
        text = text.replace(name, plugin_root);
    }
    for name in ["${HOME}", "$HOME", "${USERPROFILE}"] {
        text = text.replace(name, &home);
    }
    match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => format!("{home}{rest}"),
        _ => text,
    }
}

/// One hook command that calls the guard binary: the path it names must exist on this machine.
pub fn check_guard_command(ctx: &AppCtx, command: &str, label: &str, plugin_root: &str, found: &mut Vec<String>) {
    let Some(m) = GUARD_EXE.search(command) else { return };
    let raw = m.group(1).unwrap_or("").to_string();
    let target = expand_plugin_vars(ctx, &raw, plugin_root);
    if raw.is_empty() || target.contains('$') {
        return; // another variable (e.g. CLAUDE_PROJECT_DIR): only Claude Code resolves it
    }
    if !Path::new(&target).exists() {
        found.push(format!("{label} points the guard at {raw}, which does not exist on this machine"));
    }
}

/// The guard must be reachable from the plugin's hooks: (a) `<plugin>/hooks/hooks.json` exists and its guard target exists
/// (`${CLAUDE_PLUGIN_ROOT}` = the plugin that holds the orchestrator skill), (b) a guard target in the skills/orchestrator/SKILL.md
/// frontmatter, when there is one, exists. Without a skill folder the guard check reports it.
pub fn hook_path_problems(ctx: &AppCtx) -> Vec<String> {
    if ctx.skill_dir.as_os_str().is_empty() {
        return vec![];
    }
    let plugin_root = ctx.plugin_root_text();
    let mut found: Vec<String> = Vec::new();
    let hooks_file = ctx.plugin_root.join("hooks").join("hooks.json");
    if !hooks_file.is_file() {
        found.push(format!("hooks/hooks.json not found in the plugin root ({plugin_root}): the agents run without the guard"));
    } else {
        let parsed: Result<Value, String> = std::fs::read(&hooks_file).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()));
        let data = match parsed {
            Ok(v) => v,
            Err(error) => {
                found.push(format!("hooks/hooks.json is not valid JSON: {error}"));
                json!({})
            }
        };
        let mut commands: Vec<String> = Vec::new();
        for entries in data.get("hooks").and_then(Value::as_object).into_iter().flat_map(|h| h.values()) {
            for entry in entries.as_array().into_iter().flatten() {
                for hook in entry.get("hooks").and_then(Value::as_array).into_iter().flatten() {
                    commands.push(hook.get("command").and_then(Value::as_str).unwrap_or("").to_string());
                }
            }
        }
        let guards: Vec<&String> = commands.iter().filter(|c| c.contains("guard.exe")).collect();
        if guards.is_empty() {
            found.push("hooks/hooks.json registers no command that calls guard.exe".into());
        }
        for command in guards {
            check_guard_command(ctx, command, "hooks/hooks.json", &plugin_root, &mut found);
        }
    }
    let text = read(&ctx.skill_dir.join("SKILL.md"));
    let frontmatter = if text.starts_with("---") && text.matches("---").count() >= 2 { text.split("---").nth(1).unwrap_or("").to_string() } else { String::new() };
    for m in COMMAND_LINE.find_all(&frontmatter) {
        let command = m.group(1).unwrap_or("");
        if command.contains("guard.exe") {
            check_guard_command(ctx, command.trim().trim_matches('\''), "skills/orchestrator/SKILL.md", &plugin_root, &mut found);
        }
    }
    found
}

/// claude version, permission modes and git: what changes from machine to machine.
pub fn environment_problems(opts: &LoopOpts, repo: &str) -> Pair {
    let (mut problems, mut warns) = (Vec::new(), Vec::new());
    let binary = claude_binaries::resolve(if opts.claude.is_empty() { "claude" } else { &opts.claude });
    if !binary.is_empty() && !binary.ends_with(".py") {
        let (_, out, err) = run_timeout(&binary, &["--help"], None, 60);
        let text = format!("{out}{err}");
        if !text.contains("--permission-prompts") {
            problems.push("this `claude` has no --permission-prompts (needs version >= 2.1.259); update Claude Code".into());
        }
        if !text.contains("--max-budget-usd") {
            problems.push("this `claude` has no --max-budget-usd; update Claude Code".into());
        }
        if opts.permission_mode == "auto" && !text.contains("\"auto\"") {
            problems.push("this `claude` does not list the `auto` mode; use --permission-mode acceptEdits".into());
        } else if opts.permission_mode == "auto" {
            warns.push("`auto` mode depends on the plan/account; if the first turn fails because of it, run with --permission-mode acceptEdits".into());
        }
    }
    if git(repo, &["worktree", "list"]).0 != 0 {
        problems.push("this git does not support `git worktree` (needs >= 2.5)".into());
    }
    (problems, warns)
}

// ---------------------------------------------------------------- preflight (--check and before every run)

/// A roadmap with regression=yes (or --e2e) validates routes by running the real app. Without the test environment file (headers, users and
/// the secret manager key of each one) the loop stops at the baseline. Find that out here, before spending a turn.
pub fn test_environment_problems(root: &str, opts: &LoopOpts) -> Vec<String> {
    if REGRESSION.search(&roadmap(root)).is_none() && !opts.e2e {
        return vec![];
    }
    let path = Path::new(root).join(".specs").join("project").join("TEST-ENVIRONMENT.md");
    if !path.exists() {
        return vec!["the ROADMAP has regression=yes (or --e2e), but .specs/project/TEST-ENVIRONMENT.md is missing (template: the app's templates/TEST_ENVIRONMENT.md): without headers, users and secret manager keys the loop stops at the baseline".into()];
    }
    let text = read(&path);
    let pending = FILL_IN.find_all(&text).len();
    if pending > 0 {
        return vec![format!(".specs/project/TEST-ENVIRONMENT.md still has {pending} FILL_IN field(s): complete it before running")];
    }
    credential_check::credential_problems(root, &text)
}

/// Test environment findings that do not stop the loop (sample data with no value).
pub fn test_environment_warnings(root: &str) -> Vec<String> {
    let path = Path::new(root).join(".specs").join("project").join("TEST-ENVIRONMENT.md");
    if !path.exists() {
        return vec![];
    }
    credential_check::credential_warnings(root, &read(&path))
}

/// Text of the stop notice when the test environment blocks the start: WHO asked for the tests, WHAT is missing and WHERE the values go (the
/// project's `.specs/project/.env.test`, its only credential source). Empty when the problem is not that one.
pub fn test_environment_stop_reason(root: &str, problems: &[String]) -> String {
    if problems.is_empty() {
        return String::new();
    }
    let features = parse_roadmap(&roadmap(root)).0;
    let asked: Vec<String> = features.iter().filter(|(_, f)| matches!(f.get("regression"), Some("yes") | Some("baseline"))).map(|(s, _)| s.clone()).collect();
    let who = if asked.is_empty() {
        "the loop was started with --e2e".to_string()
    } else {
        format!("regression=yes on: {}", asked.iter().take(MAX_NAMED_FEATURES).cloned().collect::<Vec<_>>().join(", "))
    };
    format!(
        "The loop did NOT start (nothing was spent): the real-app tests were requested ({who}) but the test environment cannot run.\nMissing: {}\nWhere: names go in .specs/project/TEST-ENVIRONMENT.md (names only); values go in .specs/project/.env.test of the project, which is the ONLY credential source the loop reads (no environment variable, no ~/.loop-harness/.env, no shared file). To copy values from a machine file, use \"Import from machine sources\" in the Test environment tab (sources: env_sources in the app's config.json). Fix it and start the loop again, or remove regression=yes from the ROADMAP.",
        problems.join("; ")
    )
}

/// All verified/blocked = nothing to do, EXCEPT when the user is answering a question (--option/--answer): the answer may reopen a
/// quarantined feature (e.g. fix the spec and redo the gate).
pub fn nothing_left_to_do(done: i64, total: i64, opts: &LoopOpts, state: Option<&Value>, tiebreak_possible: bool) -> bool {
    let will_auto_answer =
        state.is_some_and(|s| s.get("status").and_then(Value::as_str) == Some("awaiting_user")) && opts.auto_answer && state.and_then(pick_auto_answer).is_some();
    total != 0 && done == total && !(opts.option != 0 || !opts.answer.is_empty()) && !will_auto_answer && !tiebreak_possible
}

/// The roadmap must have been approved by the user (ROADMAP-APPROVED.json). When answering a question (--answer/--option) the signature is
/// not compared (the loop may legitimately change rows mid-run), but the approval file must exist: an answer never starts a loop on a
/// roadmap nobody approved.
pub fn approval_problems(root: &str, answering: bool) -> Vec<String> {
    if answering {
        if loop_path(root, APPROVAL_NAME).is_file() {
            return vec![];
        }
        return vec!["roadmap never approved: an answer does not replace the approval (Approve roadmap button in the panel)".into()];
    }
    let approval = crate::loop_state::roadmap_approval(root);
    if approval["approved"] == true {
        return vec![];
    }
    vec![format!("roadmap not approved: {} (Approve roadmap button in the panel)", approval["reason"].as_str().unwrap_or(""))]
}

/// Repo names for the grammar check; `None` for a single-repo project (no prefix validation).
pub fn repo_names_of(repos: &[RepoRef]) -> Option<Vec<String>> {
    (repos.len() > 1).then(|| repos.iter().map(|r| r.name.clone()).collect())
}

/// Preflight of the NON-primary repos of a multi-repo project (the primary is checked by preflight itself). Every message names the repo.
/// `features` limits the worktree checks to the edit repos a feature changes; `None` = every edit repo. A `reference` repo only needs its
/// folder. Empty for a single-repo project. Creates nothing.
pub fn extra_repos_preflight(repos: &[RepoRef], opts: &LoopOpts, features: Option<&Features>) -> Pair {
    let (mut problems, mut warns) = (Vec::new(), Vec::new());
    let targets: Option<Vec<String>> = features.map(|f| worktree_repos(repos, f, None).into_iter().map(|r| r.name).collect());
    for item in repos.iter().filter(|r| !r.primary) {
        let tag = format!("repo {}: ", item.name);
        if item.role == ROLE_REFERENCE {
            if !Path::new(&item.path).is_dir() {
                warns.push(format!("{tag}reference folder {} is missing (moved or deleted); the loop will not be able to read it", item.path));
            } else if !readiness_checks_folder_has_map(&item.path)
                && std::fs::read_dir(&item.path).map(|d| d.flatten().any(|e| !e.file_name().to_string_lossy().starts_with('.'))).unwrap_or(false)
            {
                warns.push(format!("reference repo {} has no codebase map: agents will read the raw code; map it from Workspace", item.name));
            }
            continue;
        }
        if targets.as_ref().is_some_and(|t| !t.contains(&item.name)) {
            continue;
        }
        if !Path::new(&item.path).is_dir() {
            problems.push(format!("{tag}folder {} is missing (moved or deleted); remove it from the project or restore it", item.path));
            continue;
        }
        if git(&item.path, &["rev-parse", "--is-inside-work-tree"]).1 != "true" {
            problems.push(format!("{tag}{} is not a git repository", item.path));
            continue;
        }
        if CONFORMANCE.search(&read(&Path::new(&item.path).join("CLAUDE.md"))).is_none() {
            problems.push(format!("{tag}its CLAUDE.md has no `Conformance: STANDARD|LEGACY` line (harness classify {} --write STANDARD|LEGACY)", item.path));
        }
        if !Path::new(&item.loop_worktree_path()).is_dir() && git(&item.path, &["branch", "--show-current"]).1.is_empty() {
            problems.push(format!("{tag}no current branch (detached HEAD)"));
        }
        if opts.push && git(&item.path, &["remote", "get-url", "origin"]).1.is_empty() {
            warns.push(format!("{tag}no `origin` remote: nowhere to push its feature branch"));
        }
    }
    (problems, warns)
}

fn readiness_checks_folder_has_map(folder: &str) -> bool {
    let dir = ["docs", "codebase"].iter().fold(PathBuf::from(folder), |a, p| a.join(p));
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().to_lowercase();
                !n.starts_with('.') && n.ends_with(".md")
            })
        })
        .unwrap_or(false)
}

pub fn check_fix(kind: &str) -> &'static str {
    match kind {
        "missing-roadmap" => "Generate and approve the roadmap in Planning",
        "uncommitted" => "Commit these files (the worktree is born from the last commit)",
        "dotnet-restore" => "Add `dotnet restore` as the Setup line in the ROADMAP header (planning)",
        "node-modules" => "Add `npm ci` as the Setup line",
        "permission-mode" => "If the first turn fails, use permission mode acceptEdits",
        _ => "",
    }
}

/// The kind of a check line, from its text (the text itself is never changed).
pub fn check_item_kind(text: &str) -> &'static str {
    let lowered = text.to_lowercase();
    if lowered.starts_with("missing .specs/project/roadmap.md") {
        "missing-roadmap"
    } else if lowered.starts_with("uncommitted changes in ") {
        "uncommitted"
    } else if lowered.starts_with(".net detected") {
        "dotnet-restore"
    } else if lowered.starts_with("node project") {
        "node-modules"
    } else if lowered.contains("`auto` mode") || lowered.contains("auto mode depends") {
        "permission-mode"
    } else {
        "other"
    }
}

/// `[{kind, repo, files, text, fix}]` for the `--json` output of the check. The uncommitted text lines are replaced by the structured
/// one-per-repo items; every other line keeps its text and gets a kind and a fix sentence.
pub fn check_items(problems: &[String], warns: &[String], repos: &[RepoRef], repo: &str) -> Vec<Value> {
    let mut items: Vec<Value> = Vec::new();
    for text in problems.iter().chain(warns.iter()) {
        let kind = check_item_kind(text);
        if kind == "uncommitted" {
            continue;
        }
        items.push(
            json!({"kind": kind, "level": if problems.contains(text) { "problem" } else { "warning" }, "repo": null, "files": [], "text": text, "fix": check_fix(kind)}),
        );
    }
    let mut all: Vec<Value> = readiness_checks::uncommitted_items(repos, repo)
        .into_iter()
        .map(|mut item| {
            if item.get("level").is_none() {
                item["level"] = json!("warning");
            }
            item
        })
        .collect();
    all.extend(items);
    all
}

pub fn claude_command_problems(opts: &LoopOpts) -> Pair {
    let (mut problems, mut warns) = (Vec::new(), Vec::new());
    let name = if opts.claude.is_empty() { "claude" } else { &opts.claude };
    if claude_binaries::resolve(name).is_empty() {
        problems.push(format!("`{name}` command not found on PATH"));
    }
    for fallback in &opts.claude_fallback {
        if claude_binaries::resolve(fallback).is_empty() {
            warns.push(format!("fallback command `{fallback}` not found on PATH: the loop will not switch to it on a usage limit"));
        }
    }
    (problems, warns)
}

/// Detached HEAD. The uncommitted changes are ONE block per repo (`readiness_checks::uncommitted_warnings`), not repeated here.
pub fn repo_branch_problems(repo: &str) -> Pair {
    let problems = if git(repo, &["branch", "--show-current"]).1.is_empty() { vec!["repo has no current branch (detached HEAD)".to_string()] } else { vec![] };
    (problems, vec![])
}

/// A repo whose worktree is not created yet and has no local integration branch starts from its current branch: say which.
pub fn base_branch_warnings(env: &mut cfg::Env, repos: &[RepoRef], base_branch: &str) -> Vec<String> {
    let mut warns = Vec::new();
    for item in project_repos::edit_repos(repos) {
        if Path::new(&item.loop_worktree_path()).is_dir() || !Path::new(&item.path).is_dir() || git(&item.path, &["rev-parse", "--is-inside-work-tree"]).1 != "true" {
            continue;
        }
        let (base, warning) = choose_base(env, &item.path, base_branch);
        if !base.is_empty() && !warning.is_empty() {
            warns.push(format!("{}{warning}", if repos.len() > 1 { format!("repo {}: ", item.name) } else { String::new() }));
        }
    }
    warns
}

/// ROADMAP.md exists, follows the grammar and still has something to do. `setup_open`: a real earlier step (the installer, a missing
/// Conformance, no claude) is still open, so "generate the roadmap" is not said yet: the first problem is that step.
pub fn roadmap_problems(root: &str, repos: &[RepoRef], opts: &LoopOpts, setup_open: bool) -> Pair {
    let mut problems = Vec::new();
    let path = Path::new(root).join(".specs").join("project").join("ROADMAP.md");
    if !path.exists() {
        if !setup_open {
            problems.push("missing .specs/project/ROADMAP.md: generate and APPROVE the roadmap first (panel, or a normal /orchestrator conversation)".to_string());
        }
        return (problems, vec![]);
    }
    let (features, errors) = parse_roadmap(&read(&path));
    let names = repo_names_of(repos);
    for item in errors.iter().chain(check_fields(&features, names.as_deref()).iter()) {
        problems.push(format!("ROADMAP outside the grammar: {item}"));
    }
    let (_, total, done) = roadmap_score(root);
    let specs = Path::new(root).join(".specs").join("project").display().to_string();
    let state = load_json(&state_file(root, "LOOP.json"));
    let tiebreak = !gate_blocked_features(&specs).is_empty() && opts.auto_answer && opts.gate_tiebreaks > 0;
    if total == 0 {
        problems.push("ROADMAP.md has no feature line (`- [slug] status=...`)".into());
    } else if nothing_left_to_do(done, total, opts, Some(&state), tiebreak) {
        problems.push(format!("all {total} features are already verified/blocked: nothing to do"));
    } else {
        let note = if done == total { " (answering the loop's question: the answer may reopen a quarantined feature)" } else { "" };
        println!("[check] ROADMAP: {total} features, {done} closed, {} to do{note}", total - done);
    }
    (problems, vec![])
}

pub fn approval_problems_when_roadmap_exists(root: &str, opts: &LoopOpts) -> Vec<String> {
    if !Path::new(root).join(".specs").join("project").join("ROADMAP.md").exists() {
        return vec![];
    }
    approval_problems(root, opts.option != 0 || !opts.answer.is_empty())
}

/// The root CLAUDE.md must carry a `Conformance: STANDARD|LEGACY` line (a confirmed monorepo has it per subproject instead).
pub fn root_conformance_problems(root: &str) -> Vec<String> {
    if subprojects::monorepo_confirmed(root, None) || CONFORMANCE.search(&read(&Path::new(root).join("CLAUDE.md"))).is_some() {
        return vec![];
    }
    vec!["CLAUDE.md has no `Conformance: STANDARD|LEGACY` line (harness classify <repo> --write STANDARD|LEGACY, or the Conformance button in the panel)".into()]
}

pub fn rules_problems(root: &str) -> Pair {
    let (code, out, _) = rules::run_cli(&["test".to_string(), root.to_string(), "--quiet".to_string()]);
    if code == 0 {
        return (vec![], vec![]);
    }
    let lines: Vec<&str> = out.lines().collect();
    let tail = lines[lines.len().saturating_sub(4)..].join(" ");
    (vec![format!("a project block rule stopped working (harness rules test): {}", tail.chars().take(300).collect::<String>())], vec![])
}

pub fn dependency_audit_warnings(root: &str) -> Vec<String> {
    let audit = audit_deps::audit(root);
    let loose: Vec<&audit_deps::Suggestion> = audit.suggestions.iter().filter(|s| s.cycle.is_empty()).collect();
    match loose.first() {
        Some(first) => vec![format!(
            "{} forgotten dependency(ies) between features (e.g. {} -> {}). Run `harness audit --write` and decide; the orchestrator applies `deps=` before each wave.",
            loose.len(),
            first.feature,
            first.depends_on
        )],
        None => vec![],
    }
}

/// The guard binary must exist and block `git push` under the executor profile.
pub fn guard_problems(ctx: &AppCtx) -> Pair {
    if ctx.guard.as_os_str().is_empty() || !ctx.guard.is_file() {
        return (
            vec![format!("the orchestrator plugin's guard was not found (looked in {}): set LOOP_SKILL_DIR or run `harness install`", ctx.describe_guard_search_paths())],
            vec![],
        );
    }
    let result = crate::loop_io::run_process(
        &[ctx.guard_text(), "--profile".into(), "executor".into()],
        &std::env::temp_dir().display().to_string(),
        &ctx.env.vars(),
        "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"git push\"}}",
        60,
    );
    if result.code != 2 {
        return (vec![format!("the guard did not block `git push` (code {}, expected 2): do not run unsupervised", result.code)], vec![]);
    }
    (vec![], vec![])
}

pub fn cost_cap_warnings(opts: &LoopOpts) -> Vec<String> {
    if opts.max_cost != 0.0 {
        vec![]
    } else {
        vec!["no total cost cap (--max-cost 0): the loop only stops on progress/error, not on spend".into()]
    }
}

/// Mandatory notice channels present and configured; unknown or half-configured channels only warn.
pub fn notice_channel_problems(ctx: &AppCtx, opts: &LoopOpts) -> Pair {
    let env = env_of(ctx);
    let (mut problems, mut warns) = (Vec::new(), Vec::new());
    let configured = app_env_list(&env, "LOOP_NOTIFY_CHANNELS");
    let channels: Vec<String> = if configured.is_empty() { vec!["console".into(), "toast".into(), "file".into()] } else { configured };
    if opts.allow_silent {
        warns.push("--allow-silent: mandatory notice turned off (for testing only)".into());
    } else {
        for name in app_env_list(&env, "LOOP_NOTIFY_REQUIRED") {
            if !channels.contains(&name) {
                problems.push(format!("mandatory notice channel missing from LOOP_NOTIFY_CHANNELS: {name} (LOOP_NOTIFY_REQUIRED)"));
            } else {
                let missing = notify::missing_config(&env, &name);
                if !missing.is_empty() {
                    problems.push(format!("mandatory notice channel {name} is missing {} in the app .env", missing.join(", ")));
                }
            }
        }
    }
    for name in &channels {
        let missing = notify::missing_config(&env, name);
        if !notify::CHANNELS.contains(&name.as_str()) {
            warns.push(format!("unknown notice channel in LOOP_NOTIFY_CHANNELS: {name}"));
        } else if !missing.is_empty() {
            warns.push(format!("channel {name} is on but is missing {} in the app .env: it will be skipped", missing.join(", ")));
        }
    }
    (problems, warns)
}

/// `--push`: identity and `origin`; a problem when the push was asked explicitly, else only a warning.
pub fn push_problems(ctx: &AppCtx, repo: &str, opts: &LoopOpts) -> Pair {
    let (mut problems, mut warns) = (Vec::new(), Vec::new());
    if opts.push {
        let problem = identity_problem(ctx, repo, opts);
        let no_origin = git(repo, &["remote", "get-url", "origin"]).1.is_empty();
        let mut texts: Vec<String> = Vec::new();
        if !problem.is_empty() {
            texts.push(problem);
        }
        if no_origin {
            texts.push("no `origin` remote: nowhere to push the feature branch".into());
        }
        for text in texts {
            if opts.push_explicit {
                problems.push(format!("--push: {text}"))
            } else {
                warns.push(format!("the final push/PR will not happen: {text}"))
            }
        }
        if which("gh").is_none() {
            warns.push("`gh` not found: the end of the loop pushes the branch but only returns the compare LINK (the PR is not created)".into());
        }
    }
    (problems, warns)
}

/// Node / .NET repos: what a fresh worktree is born without.
pub fn build_setup_warnings(repo: &str, work: &str, opts: &LoopOpts) -> Vec<String> {
    let mut warns = Vec::new();
    let has_package = Path::new(repo).join("package.json").is_file();
    if has_package && opts.setup.is_empty() && !Path::new(work).is_dir() {
        warns.push("Node project: the worktree is born without node_modules. Use --setup \"npm ci\" (otherwise verify returns UNVERIFIED).".into());
    }
    let dotnet = find_dotnet(repo, 3);
    if !dotnet.is_empty() && !Path::new(work).is_dir() {
        warns.push(format!(".NET detected ({dotnet}): the worktree is born without bin/obj; `dotnet build` does an implicit restore (needs network or the NuGet cache). The executor does not run `dotnet restore` nor `dotnet add package` (guard); to restore beforehand, use --setup with `dotnet restore`."));
    }
    if (!dotnet.is_empty() || has_package) && !Path::new(repo).join(".gitignore").is_file() {
        warns.push("repo has no .gitignore: bin/obj/node_modules generated by the build dirty the worktree and the loop stops on `dirty tree`.".into());
    }
    warns
}

/// First .sln/.csproj up to `max_depth` folders below the root (ignores bin/obj/node_modules/.git/.specs). Empty if none.
pub fn find_dotnet(repo: &str, max_depth: usize) -> String {
    readiness_checks::find_dotnet(repo, max_depth)
}

// ---------------------------------------------------------------- readiness (--check --planning)

/// Every feature path must belong to an `edit` repo: a path in a `reference` or unlisted repo cannot be changed by the loop.
pub fn feature_path_repo_problems(repos: &[RepoRef], features: &Features) -> Vec<String> {
    let primary = project_repos::primary_of(repos);
    let mut problems = Vec::new();
    for (slug, fields) in features.iter() {
        let mut seen: Vec<String> = Vec::new();
        for entry in fields.text("paths").split(',').map(str::trim) {
            let m = if entry.is_empty() { None } else { REPO_PATH_PREFIX.match_start(entry) };
            let name = m.as_ref().and_then(|m| m.group(1).map(String::from)).unwrap_or_else(|| primary.name.clone());
            if entry.is_empty() || seen.contains(&name) {
                continue;
            }
            seen.push(name.clone());
            match repos.iter().find(|r| r.name == name) {
                None => problems.push(format!(
                    "feature {slug}: path `{entry}` is in repo `{name}`, which is not listed in the project: add it as an edit repo or change the feature"
                )),
                Some(target) if target.role == ROLE_REFERENCE => {
                    problems.push(format!("feature {slug}: path `{entry}` is in the reference repo `{name}` (read only): promote it to edit or change the feature"))
                }
                _ => {}
            }
        }
    }
    problems
}

/// Can every edit repo a feature changes get its worktree? (folder is a git repo, current branch, a free branch name, no foreign folder).
pub fn worktree_readiness(repos: &[RepoRef], features: &Features, opts: &LoopOpts) -> Pair {
    let (mut problems, warns) = (Vec::new(), Vec::new());
    for item in worktree_repos(repos, features, None) {
        let tag = format!("repo {}: ", item.name);
        if !Path::new(&item.path).is_dir() {
            problems.push(format!("{tag}folder {} is missing: no worktree can be created", item.path));
            continue;
        }
        if git(&item.path, &["rev-parse", "--is-inside-work-tree"]).1 != "true" {
            problems.push(format!("{tag}{} is not a git repository: no worktree can be created", item.path));
            continue;
        }
        let work = item.loop_worktree_path();
        if Path::new(&work).is_dir() {
            let meta = load_json(&loop_path(&work, "meta.json"));
            let recorded = meta.get("branch").and_then(Value::as_str).unwrap_or("");
            if recorded.is_empty() || git(&work, &["branch", "--show-current"]).1 != recorded {
                problems.push(format!("{tag}folder {work} already exists but is not this loop's worktree: remove or rename it"));
            }
            continue;
        }
        if git(&item.path, &["branch", "--show-current"]).1.is_empty() {
            problems.push(format!("{tag}no current branch (detached HEAD): the worktree needs a branch to start from"));
        }
        if !opts.branch.is_empty() && branch_exists(&item.path, &opts.branch) {
            problems.push(format!("{tag}branch `{}` already exists: pick another --branch name", opts.branch));
        }
    }
    (problems, warns)
}

/// Configured MCP servers (`mcp_servers` of config.json, allowed or not) that any of the texts names (`db-dev`, `mcp__db-dev__query`).
pub fn mentioned_mcp_servers(ctx: &AppCtx, texts: &[&str]) -> Vec<String> {
    let text = texts.join("\n").to_lowercase();
    let policy = mcp_access::read_policy(&ctx.env, None);
    policy
        .servers
        .iter()
        .map(|(name, _)| name.clone())
        .filter(|name| {
            Re::try_build(&format!("(?<![a-z0-9-]){}(?![a-z0-9-])", pycompat::escape(&name.to_lowercase())), false).is_some_and(|re| re.search(&text).is_some())
        })
        .collect()
}

pub fn claude_config_readable(ctx: &AppCtx) -> bool {
    let path = crate::mcp_discovery::claude_config_path(&ctx.env);
    std::fs::read(path)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|t| serde_json::from_str::<Value>(t.strip_prefix('\u{feff}').unwrap_or(&t)).ok())
        .is_some_and(|v| v.is_object())
}

pub fn mcp_readiness(ctx: &AppCtx, root: &str, roadmap_text: &str) -> Pair {
    let (mut problems, mut warns) = (Vec::new(), Vec::new());
    let environment_text = read(&Path::new(root).join(".specs").join("project").join("TEST-ENVIRONMENT.md"));
    let needed = mentioned_mcp_servers(ctx, &[&environment_text, roadmap_text]);
    let configured: Vec<String> = mcp_access::available_servers(&ctx.env, None, "").iter().filter_map(|r| r["server"].as_str().map(String::from)).collect();
    let missing: Vec<&String> = needed.iter().filter(|n| !configured.contains(n)).collect();
    let config_path = crate::mcp_discovery::claude_config_path(&ctx.env).display().to_string();
    if !missing.is_empty() {
        problems.push(format!(
            "MCP server(s) named by TEST-ENVIRONMENT.md or a feature are not allowed for loops or not defined on this machine ({config_path}): {} (tick them in Settings > MCP servers; define a missing one with `claude mcp add --scope user`)",
            missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ));
    } else if needed.is_empty() {
        let names = mcp_access::server_names(&mcp_access::read_policy(&ctx.env, None));
        if !names.is_empty() && !claude_config_readable(ctx) {
            warns.push(format!("{config_path} could not be read: the MCP servers ({}) will not be available to the loop", names.join(", ")));
        }
    }
    (problems, warns)
}

/// How an edit repo is built/tested, as far as the repo itself says: docs/codebase/TESTING.md or a project file. Empty when unknown. A
/// confirmed monorepo also counts the non-skipped subprojects.
pub fn known_test_command(path: &str) -> String {
    let known = readiness_checks::known_commands(path)["summary"].as_str().unwrap_or("").to_string();
    if !known.is_empty() {
        return known;
    }
    let entry = subprojects::load(path, None).0;
    if let Some(entry) = entry.filter(|e| e.get("confirmed") == Some(&Value::Bool(true))) {
        for project in subprojects::entry_projects(Some(&entry)) {
            if project.get("skip") != Some(&Value::Bool(true)) {
                let folder = subprojects::normalize(project.get("path").and_then(Value::as_str).unwrap_or("")).split('/').fold(PathBuf::from(path), |a, p| a.join(p));
                let known = readiness_checks::known_commands(&folder.display().to_string())["summary"].as_str().unwrap_or("").to_string();
                if !known.is_empty() {
                    return known;
                }
            }
        }
    }
    String::new()
}

/// Names TEST-ENVIRONMENT.md mentions must be defined in the project's `.env.test` (its only credential source).
pub fn credential_readiness(root: &str) -> Pair {
    let path = Path::new(root).join(".specs").join("project").join("TEST-ENVIRONMENT.md");
    if !path.exists() {
        return (vec![], vec![]);
    }
    let text = read(&path);
    (credential_check::credential_problems(root, &text), credential_check::credential_warnings(root, &text))
}

/// The extra checks of `--check --planning` (the rest of the preflight, guard included, already ran).
pub fn planning_readiness(ctx: &AppCtx, root: &str, repos: &[RepoRef], features: &Features, opts: &LoopOpts) -> Pair {
    let (mut problems, mut warns) = (feature_path_repo_problems(repos, features), Vec::new());
    let (new_problems, new_warns) = worktree_readiness(repos, features, opts);
    problems.extend(new_problems);
    warns.extend(new_warns);
    warns.extend(readiness_checks::giant_preflight(root, features, repos));
    warns.extend(readiness_checks::system_map_warning(root, features, repos));
    let (new_problems, new_warns) = mcp_readiness(ctx, root, &roadmap(root));
    problems.extend(new_problems);
    warns.extend(new_warns);
    let (new_problems, new_warns) = credential_readiness(root);
    push_new(&mut problems, new_problems);
    push_new(&mut warns, new_warns);
    for item in worktree_repos(repos, features, None) {
        if Path::new(&item.path).is_dir() && known_test_command(&item.path).is_empty() {
            warns.push(format!(
                "repo {}: no build/test command known (no docs/codebase/TESTING.md, no package.json scripts or project file): the verifier cannot prove its features",
                item.name
            ));
        }
    }
    (problems, warns)
}

/// Checks everything the loop needs and returns `(problems, warnings)`. Creates nothing. Each step returns its own pair, in the order they
/// are reported.
pub fn preflight(ctx: &AppCtx, repo: &str, opts: &LoopOpts) -> Pair {
    let work = worktree_of(repo);
    let root = if Path::new(&work).is_dir() { work.clone() } else { repo.to_string() };
    let repos = project_repos::load_repos(repo);
    let (mut problems, mut warns) = claude_command_problems(opts);
    if git(repo, &["rev-parse", "--is-inside-work-tree"]).1 != "true" {
        return (vec![format!("{repo} is not a git repository")], warns);
    }
    let features = crate::worktrees::roadmap_features(&root);
    let has_roadmap = Path::new(&root).join(".specs").join("project").join("ROADMAP.md").exists();
    let mut env = env_of(ctx);
    let apply = |problems: &mut Vec<String>, warns: &mut Vec<String>, (step_problems, step_warns): Pair| {
        push_new(problems, step_problems); // two checks may report the same finding: once, in order
        push_new(warns, step_warns);
    };
    apply(&mut problems, &mut warns, repo_branch_problems(repo));
    apply(&mut problems, &mut warns, extra_repos_preflight(&repos, opts, Some(&features)));
    apply(&mut problems, &mut warns, (root_conformance_problems(&root), vec![]));
    apply(&mut problems, &mut warns, (if has_roadmap { vec![] } else { readiness_checks::installer_problems(&root, &repos) }, vec![]));
    let setup_open = !problems.is_empty(); // a real earlier step is still open: "generate the roadmap" is not said yet
    apply(&mut problems, &mut warns, roadmap_problems(&root, &repos, opts, setup_open));
    apply(&mut problems, &mut warns, (if opts.planning { vec![] } else { approval_problems_when_roadmap_exists(&root, opts) }, vec![])); // --planning: the approval is what this check prepares
    apply(&mut problems, &mut warns, readiness_checks::subprojects_preflight(&root, &features, &repos));
    apply(&mut problems, &mut warns, (vec![], readiness_checks::uncommitted_warnings(&repos, repo)));
    apply(&mut problems, &mut warns, (vec![], base_branch_warnings(&mut env, &repos, &opts.base_branch)));
    apply(&mut problems, &mut warns, (test_environment_problems(&root, opts), test_environment_warnings(&root)));
    apply(&mut problems, &mut warns, (hook_path_problems(ctx), vec![]));
    apply(&mut problems, &mut warns, rules_problems(&root));
    apply(&mut problems, &mut warns, (vec![], dependency_audit_warnings(&root)));
    apply(&mut problems, &mut warns, environment_problems(opts, repo));
    apply(&mut problems, &mut warns, guard_problems(ctx));
    if opts.planning {
        apply(&mut problems, &mut warns, planning_readiness(ctx, &root, &repos, &features, opts));
    }
    apply(&mut problems, &mut warns, (vec![], cost_cap_warnings(opts)));
    apply(&mut problems, &mut warns, notice_channel_problems(ctx, opts));
    apply(&mut problems, &mut warns, push_problems(ctx, repo, opts));
    apply(&mut problems, &mut warns, (vec![], build_setup_warnings(repo, &work, opts)));
    (problems, warns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfg::Env;

    fn ctx(dir: &Path) -> AppCtx {
        let env = Env::from_pairs([
            ("LOOP_DATA_DIR", dir.join("data").display().to_string()),
            ("LOOP_CLAUDE_HOME", dir.join("home").display().to_string()),
            ("USERPROFILE", dir.display().to_string()),
            ("LOOP_REPO_ROOT", dir.display().to_string()),
            ("PATH", std::env::var("PATH").unwrap_or_default()),
        ]);
        AppCtx::discover_in(&env, dir.to_path_buf())
    }

    fn repo(dir: &Path) -> String {
        let r = dir.join("app");
        std::fs::create_dir_all(&r).unwrap();
        for args in [vec!["init", "-q", "-b", "developer"], vec!["config", "user.name", "t"], vec!["config", "user.email", "t@example.com"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&r).args(&args).output().unwrap().status.success());
        }
        std::fs::write(r.join("README.md"), "x").unwrap();
        std::fs::write(r.join("CLAUDE.md"), "Conformance: STANDARD\n").unwrap();
        for args in [vec!["add", "."], vec!["commit", "-q", "-m", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&r).args(&args).output().unwrap().status.success());
        }
        r.display().to_string()
    }

    #[test]
    fn kinds_and_fixes() {
        assert_eq!(check_item_kind("Missing .specs/project/ROADMAP.md: generate"), "missing-roadmap");
        assert_eq!(check_item_kind("uncommitted changes in api: 1 setup file"), "uncommitted");
        assert_eq!(check_item_kind(".NET detected (x.sln): the worktree"), "dotnet-restore");
        assert_eq!(check_item_kind("Node project: the worktree"), "node-modules");
        assert_eq!(check_item_kind("`auto` mode depends on the plan"), "permission-mode");
        assert_eq!(check_item_kind("anything else"), "other");
        assert_eq!(check_fix("node-modules"), "Add `npm ci` as the Setup line");
        let items = check_items(&["a problem".to_string()], &["Node project: x".to_string()], &[], "C:\\nope");
        assert_eq!((items[0]["level"].clone(), items[1]["kind"].clone(), items[1]["level"].clone()), (json!("problem"), json!("node-modules"), json!("warning")));
    }

    #[test]
    fn a_fresh_repo_without_roadmap_fails_with_the_roadmap_problem() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        let r = repo(dir.path());
        let mut opts = LoopOpts::new(&r);
        opts.claude = "definitely-not-a-claude-binary".into();
        let (problems, warns) = preflight(&ctx, &r, &opts);
        assert!(problems.contains(&"`definitely-not-a-claude-binary` command not found on PATH".to_string()), "{problems:?}");
        assert!(!problems.iter().any(|p| p.starts_with("missing .specs/project/ROADMAP.md")), "setup is open, so the roadmap line is not said yet: {problems:?}");
        assert!(problems.iter().any(|p| p.contains("guard was not found")));
        assert!(warns.iter().any(|w| w.contains("no `origin` remote") || w.contains("the final push/PR will not happen")));
        opts.claude = "cmd".into();
        let (problems, _) = preflight(&ctx, &r, &opts);
        assert!(problems.iter().any(|p| p.starts_with("missing .specs/project/ROADMAP.md")), "{problems:?}");
    }

    #[test]
    fn roadmap_and_approval_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        let opts = LoopOpts::new(&root);
        std::fs::create_dir_all(dir.path().join(".specs/project")).unwrap();
        std::fs::write(dir.path().join(".specs/project/ROADMAP.md"), "## Stage 1\n- [a] status=verified paths=src/a\n- [b] status=blocked paths=src/b\n").unwrap();
        let (problems, _) = roadmap_problems(&root, &[], &opts, false);
        assert_eq!(problems, vec!["all 2 features are already verified/blocked: nothing to do"]);
        let mut answering = LoopOpts::new(&root);
        answering.answer = "yes".into();
        assert!(roadmap_problems(&root, &[], &answering, false).0.is_empty());
        assert_eq!(approval_problems(&root, true), vec!["roadmap never approved: an answer does not replace the approval (Approve roadmap button in the panel)"]);
        assert!(approval_problems(&root, false)[0].starts_with("roadmap not approved: "));
        assert_eq!(root_conformance_problems(&root).len(), 1);
        std::fs::write(dir.path().join("CLAUDE.md"), "Conformance: LEGACY\n").unwrap();
        assert!(root_conformance_problems(&root).is_empty());
        assert_eq!(cost_cap_warnings(&LoopOpts { max_cost: 0.0, ..LoopOpts::new("x") }).len(), 1);
        assert!(cost_cap_warnings(&opts).is_empty());
    }

    #[test]
    fn test_environment_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        std::fs::create_dir_all(dir.path().join(".specs/project")).unwrap();
        let opts = LoopOpts::new(&root);
        assert!(test_environment_problems(&root, &opts).is_empty());
        std::fs::write(dir.path().join(".specs/project/ROADMAP.md"), "## Stage 1\n- [a] status=pending regression=yes\n").unwrap();
        let missing = test_environment_problems(&root, &opts);
        assert!(missing[0].starts_with("the ROADMAP has regression=yes (or --e2e), but .specs/project/TEST-ENVIRONMENT.md is missing"));
        std::fs::write(dir.path().join(".specs/project/TEST-ENVIRONMENT.md"), "FILL_IN and FILL_IN\n").unwrap();
        assert_eq!(test_environment_problems(&root, &opts), vec![".specs/project/TEST-ENVIRONMENT.md still has 2 FILL_IN field(s): complete it before running"]);
        let reason = test_environment_stop_reason(&root, &missing);
        assert!(reason.starts_with("The loop did NOT start (nothing was spent): the real-app tests were requested (regression=yes on: a)"));
        assert_eq!(test_environment_stop_reason(&root, &[]), "");
    }

    #[test]
    fn hooks_must_point_at_a_real_guard() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = dir.path().join("plugin");
        std::fs::create_dir_all(plugin.join("skills/orchestrator")).unwrap();
        std::fs::create_dir_all(plugin.join("hooks")).unwrap();
        std::fs::write(plugin.join("skills/orchestrator/SKILL.md"), "---\nname: orchestrator\n---\nbody").unwrap();
        let env = Env::from_pairs([
            ("LOOP_SKILL_DIR", plugin.join("skills/orchestrator").display().to_string()),
            ("LOOP_DATA_DIR", dir.path().join("data").display().to_string()),
            ("USERPROFILE", dir.path().display().to_string()),
        ]);
        let ctx = AppCtx::discover_in(&env, dir.path().to_path_buf());
        let root = ctx.plugin_root_text();
        assert_eq!(hook_path_problems(&ctx), vec![format!("hooks/hooks.json not found in the plugin root ({root}): the agents run without the guard")]);
        std::fs::write(
            plugin.join("hooks/hooks.json"),
            r#"{"hooks": {"PreToolUse": [{"hooks": [{"command": "\"${CLAUDE_PLUGIN_ROOT}/bin/guard.exe\" --profile auto"}]}]}}"#,
        )
        .unwrap();
        assert_eq!(hook_path_problems(&ctx), vec!["hooks/hooks.json points the guard at ${CLAUDE_PLUGIN_ROOT}/bin/guard.exe, which does not exist on this machine"]);
        std::fs::create_dir_all(plugin.join("bin")).unwrap();
        std::fs::write(plugin.join("bin/guard.exe"), "").unwrap();
        assert!(hook_path_problems(&ctx).is_empty());
        std::fs::write(plugin.join("hooks/hooks.json"), "{}").unwrap();
        assert_eq!(hook_path_problems(&ctx), vec!["hooks/hooks.json registers no command that calls guard.exe"]);
        assert_eq!(expand_plugin_vars(&ctx, "$CLAUDE_PLUGIN_ROOT/x", "P"), "P/x");
    }
}
