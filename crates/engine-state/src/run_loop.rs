// SPDX-License-Identifier: MIT
//! Unsupervised /orchestrator loop: runs `claude -p` in turns until it finishes or gets stuck.
//!
//! Panel: by default the loop starts (or reuses) the Maestro web panel, detached and independent: it prints `[loop] monitor: <url>`, writes
//! LOOP-CMD.json and the URL in meta.json, and NEVER shuts it down when it ends. `--no-monitor` or `LOOP_MONITOR=off` turns it off. The file
//! `.specs/project/loop/internal/PAUSE` (created by the panel) makes the loop stop at awaiting_user before the next turn.
//!
//! IMPORTANT (user decision): the git worktree exists ONLY for this loop. It creates, ONCE, the sibling folder `../<project>-worktree-loop` on
//! branch `loop/<date>` and works there; the original repo (your folder, your branch) is never touched. The orchestrator inside claude
//! commits on that branch and NEVER pushes (the guard blocks it and the turn runs with every remote's pushurl disabled). The delivery is made
//! by this program, at the end, only if status=done, only from the feature branch (the integration branch/main/... are refused), without
//! force: push + PR to the integration branch (default `developer`, env LOOP_INTEGRATION_BRANCH), with the link in the notice and in the
//! summary. Default: ON (`--no-push` turns it off).
//!
//! How each turn works:
//!   claude -p "/orchestrator continue --headless --turn N --wave W ..." --output-format stream-json --verbose --permission-mode <mode>
//!          --permission-prompts none --max-budget-usd <value>
//! Each turn carries the LANES the scheduler chose (`--lanes slug:step,...`): one per feature, each at its own step, up to `--wave` of them.
//! Every lane has its own agent at every step (spec-writer, spec-reviewer, executor, verifier) and advances by itself; no feature waits for
//! another one to reach the same phase, and a quarantined feature only holds its dependents. The process ends by itself when the model gives
//! its final answer. If it "would wait for the user", nobody is there: the turn ends and the orchestrator writes
//! `.specs/project/loop/json/LOOP.json {status: awaiting_user, ...}`.
//!
//! Stops (all notify through the LOOP_NOTIFY_CHANNELS channels: console, toast, file, email, webhook):
//!   done | awaiting_user | error (LOOP.json) | turn timeout | exit!=0 | risky permission denial or one without progress | dirty tree outside
//!   .specs/ | LOOP.json missing/stale | identical state in 2 turns | `--max-stall` turns WITHOUT PROGRESS (default 10; the counter resets on
//!   every status advance in the ROADMAP) | account usage limit (first the next `--claude-fallback` command, e.g. `cc` signed in with another
//!   account; when every one is limited, waits until the first known reset (else `--limit-wait` min), up to `--limit-retries` times, and
//!   starts again from `--claude`; while on a fallback, it goes back to `--claude` by itself once that command's remembered reset moment has
//!   passed) | unusable account (logged out, revoked token, organization without access: the turn is run again on the next command with a
//!   notice on every channel and no wait; that command is not used again in the run; when none is left, error naming each command, its
//!   account and the reason).
//! No overall turn cap: as long as there is progress, it continues.
//!
//! Always on stop: writes `.specs/project/loop/json/meta.json` and generates the summary (`loop_summary`, no Claude) in `--summary-dir`
//! (default: the app's output folder; never the repo).

use crate::app_ctx::AppCtx;
use crate::auto_answers::{try_auto_answer, try_gate_tiebreak, write_answer, AutoOpts, AutoState};
use crate::claude_accounts::*;
use crate::claude_turn::{claude_command, is_caveman_on, loop_json_correction_prompt, parse_result, py_float, run_turn, run_turn_notifying, unrecognized_output};
use crate::limit_waits::{limit_wait_seconds, main_reset_moment, pause_requested, sleep_unless_paused, wait_text};
use crate::loop_io::{git, load_json, local_now, now_iso, read, save_json, save_meta, worktree_of};
use crate::loop_opts::{parse, LoopOpts, Parsed};
use crate::loop_state::{self, loop_path, state_file, turns_dir};
use crate::permission_denials::{denial_progress, denial_text, is_risky_denial, note_tolerated_denials};
use crate::preflight_checks::*;
use crate::turn_gates::*;
use crate::worktree_state::{acquire_lock, count_new_commits, fingerprint, heads_of, release_lock, rewind, roadmap_score, settle_dirty_worktrees, LeftoverOpts};
use crate::worktrees::{ensure_worktrees, Ctx, WtOpts};
use crate::{
    claude_binaries, cli_output, cost_budget, decisions, delivery, e2e_evidence, egress_proxy, graph_view, loop_summary, monitor_proc, notify, process_registry,
    project_repos, result_vocabulary, usage_windows,
};
use cfg::env::{app_env_value, load_all};
use cfg::Env;
use pycompat::json::py_json_dumps;
use pycompat::{nt_abspath, Re};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

const STATUS_OK: [&str; 4] = ["continue", "awaiting_user", "done", "error"];
static LIMIT_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"rate.?limit|usage limit|limit reached|too many requests|\b429\b|overloaded|quota"));

/// Exit code of each way the loop can stop.
pub fn exit_code(status: &str) -> i32 {
    match status {
        "done" => 0,
        "awaiting_user" | "paused" => 10,
        "error" => 20,
        _ => 30,
    }
}

fn truthy(v: Option<&Value>) -> bool {
    crate::notify_truthy(v)
}

fn text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => py_json_dumps(other, false),
    }
}

fn f64_of(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn i64_of(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn set(meta: &mut Value, key: &str, value: Value) {
    if let Some(map) = meta.as_object_mut() {
        map.insert(key.into(), value);
    }
}

fn tail(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}

fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

fn round4(value: f64) -> f64 {
    (value * 10000.0).round_ties_even() / 10000.0
}

fn meta_path(work: &str) -> std::path::PathBuf {
    loop_path(work, "meta.json")
}

fn auto_opts(opts: &LoopOpts) -> AutoOpts {
    AutoOpts {
        auto_answer: opts.auto_answer,
        max_auto_answers: opts.max_auto_answers,
        auto_answers_per_feature: opts.auto_answers_per_feature,
        gate_tiebreaks: opts.gate_tiebreaks,
    }
}

/// Flags for the resume command that goes in the notice (the same as this run).
pub fn command_flags(opts: &LoopOpts) -> String {
    let mut flags = format!(" --wave {}", opts.wave);
    if opts.e2e {
        flags.push_str(" --e2e");
    }
    if opts.push_explicit {
        flags.push_str(" --push");
    } else if !opts.push {
        flags.push_str(" --no-push");
    }
    if opts.continue_on_blocked {
        flags.push_str(" --continue-on-blocked");
    }
    if opts.pause_on_blocked {
        flags.push_str(" --pause-on-blocked");
    }
    if !opts.setup.is_empty() {
        flags.push_str(&format!(" --setup \"{}\"", opts.setup));
    }
    if opts.no_monitor {
        flags.push_str(" --no-monitor");
    }
    flags
}

/// The command `--check` prints: through the app CLI (`harness loop`), with this run's flags.
pub fn ready_command(ctx: &AppCtx, repo: &str, opts: &LoopOpts) -> String {
    let flags = format!(
        " --wave {}{}{}{}{}",
        opts.wave,
        if opts.e2e { " --e2e" } else { "" },
        if !opts.push { " --no-push" } else { "" },
        if opts.setup.is_empty() { String::new() } else { format!(" --setup \"{}\"", opts.setup) },
        if opts.no_monitor { " --no-monitor" } else { "" }
    );
    format!("\"{}\" loop \"{repo}\"{flags}", ctx.app_exe.display())
}

/// One run of the loop: the installation it runs in and what the notices need.
pub struct Runner {
    pub ctx: AppCtx,
    /// The arguments after `loop` as typed (the relaunch command keeps them).
    pub raw_args: Vec<String>,
    repo: String,
    notify: bool,
    monitor_url: Option<String>,
    env_reported: bool,
}

impl Runner {
    pub fn new(ctx: AppCtx, raw_args: Vec<String>) -> Self {
        Runner { ctx, raw_args, repo: String::new(), notify: false, monitor_url: None, env_reported: false }
    }

    fn env(&self) -> Env {
        let mut env = self.ctx.env.clone();
        load_all(&mut env);
        env
    }

    /// Sends the notice through the configured channels and retries the failed mandatory ones ONCE. Never fails.
    pub fn deliver(&self, context: &Value, work: &str, only: Option<Vec<String>>) -> Map<String, Value> {
        let mut env = self.env();
        let mut results = notify::dispatch(&mut env, context, work, only);
        let required = required_channels(&self.ctx);
        let failed: Vec<String> = results.iter().filter(|(n, v)| required.contains(n) && !v.starts_with("ok")).map(|(n, _)| n.clone()).collect();
        if !failed.is_empty() {
            std::thread::sleep(Duration::from_secs(5));
            for (name, value) in notify::dispatch(&mut env, context, work, Some(failed)) {
                match results.iter_mut().find(|(n, _)| *n == name) {
                    Some(slot) => slot.1 = value,
                    None => results.push((name, value)),
                }
            }
        }
        let still: Vec<String> = results.iter().filter(|(n, v)| required.contains(n) && !v.starts_with("ok")).map(|(n, v)| format!("{n}={v}")).collect();
        if !still.is_empty() {
            println!("[loop] WARNING: mandatory notice NOT delivered on: {}", still.join("; "));
        }
        results.into_iter().map(|(n, v)| (n, json!(v))).collect()
    }

    /// Notice after a turn that verified a feature or closed a stage (the loop goes on; informative only). One notice per turn, naming the
    /// features and the stage. Off with `LOOP_NOTIFY_MILESTONES=off`. Never fails.
    fn notify_milestones(&self, work: &str, meta: &Value, opts: &LoopOpts, repo: &str, before_roadmap: &str, turn: i64) {
        if !self.notify || !notify::milestones_enabled(&self.env()) {
            return;
        }
        let after = read(&Path::new(work).join(".specs").join("project").join("ROADMAP.md"));
        let (verified, stages) = notify::milestones(before_roadmap, &after);
        if verified.is_empty() && stages.is_empty() {
            return;
        }
        let event = if stages.is_empty() { "feature_verified" } else { "stage_closed" };
        let mut context = self.build_context(work, meta, event, "", opts, repo, "");
        if let Some(map) = context.as_object_mut() {
            map.insert("verified_now".into(), json!(verified));
            map.insert("stage_closed".into(), json!(stages));
            map.insert("turn".into(), json!(turn));
            map.insert("running".into(), json!([]));
        }
        println!("[loop] milestone notice: {}", verified.join(", "));
        self.deliver(&context, work, None);
    }

    /// Stop notice that does not go through `finish` (preflight, worktree, lock, unhandled failure): the user is always notified.
    pub fn notify_early(&self, reason: &str, opts: &LoopOpts) {
        if !self.notify {
            return;
        }
        let work = worktree_of(&self.repo);
        let work = if Path::new(&work).is_dir() { work } else { String::new() };
        let context = json!({"event": "error", "project": pycompat::last_segment(&self.repo), "reason": head(reason, 1500), "script": self.ctx.app_exe.display().to_string(),
                             "repo": self.repo, "flags": command_flags(opts), "worktree": work, "features": [], "commits": [], "options": [], "to_check": [],
                             "next": null, "summary": "", "push": null, "monitor_url": self.monitor_url});
        self.deliver(&context, &work, None);
    }

    /// Notice for a wait that is not a stop (the loop keeps going): same channels as a stop (or only `only`), never fails.
    pub fn notify_wait(&self, subject: &str, body: &str, only: Option<Vec<String>>) {
        if !self.notify {
            return;
        }
        let work = worktree_of(&self.repo);
        let work = if Path::new(&work).is_dir() { work } else { String::new() };
        let project = pycompat::last_segment(&self.repo);
        let context = json!({"event": "custom", "subject": format!("[loop] {project}: {subject}"), "project": project, "extra_body": body});
        self.deliver(&context, &work, only);
    }

    /// Panel on by default; off with `--no-monitor` or `LOOP_MONITOR=off` in the app `.env`.
    pub fn monitor_enabled(&self, opts: &LoopOpts) -> bool {
        !(opts.no_monitor || monitor_proc::monitor_disabled_by_environment(&app_env_value(&self.ctx.env, "LOOP_MONITOR")))
    }

    /// Writes LOOP-CMD.json (relaunch command, without --answer/--option) and starts or reuses the panel. Never takes the loop down.
    fn start_monitor(&mut self, repo: &str, work: &str, meta: &mut Value, opts: &LoopOpts) -> Option<String> {
        let mut argv: Vec<String> = self.raw_args.clone();
        if let Some(i) = argv.iter().position(|a| *a == opts.repo) {
            argv[i] = repo.to_string();
        }
        let mut command = vec![self.ctx.app_exe.display().to_string(), "loop".to_string()];
        command.extend(argv);
        monitor_proc::write_loop_command(work, &command, &std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default());
        if !self.monitor_enabled(opts) {
            return None;
        }
        let open = !monitor_proc::browser_open_disabled(&app_env_value(&self.ctx.env, "LOOP_MONITOR_OPEN"));
        let url = monitor_proc::ensure_monitor(&self.env(), &self.ctx.app_exe, repo, work, monitor_proc::DEFAULT_WAIT_SECONDS, open);
        match &url {
            Some(u) => {
                println!("[loop] monitor: {u}");
                set(meta, "monitor_url", json!(u));
                self.monitor_url = Some(u.clone());
            }
            None => {
                if let Some(m) = meta.as_object_mut() {
                    m.shift_remove("monitor_url");
                }
            }
        }
        let _ = save_meta(&meta_path(work), meta);
        url
    }

    /// Notice data (only what comes from LOOP.json, ROADMAP and git log; never project file contents).
    fn build_context(&self, work: &str, meta: &Value, status: &str, reason: &str, opts: &LoopOpts, repo: &str, summary: &str) -> Value {
        let state = load_json(&state_file(work, "LOOP.json"));
        let features: Vec<Value> =
            loop_summary::feature_statuses(&read(&Path::new(work).join(".specs").join("project").join("ROADMAP.md"))).into_iter().map(|(s, st)| json!([s, st])).collect();
        let base = text(meta.get("base"));
        let range =
            if base.is_empty() { "-8".to_string() } else { format!("{base}..{}", if truthy(meta.get("branch")) { text(meta.get("branch")) } else { "HEAD".into() }) };
        let log = git(work, &["log", "--oneline", "--no-decorate", &range]).1;
        let graph = graph_view::load(work);
        // features in progress now (LOOP.json `feature`, else the ones being specced or built) and the stage the loop is working on
        let running: Vec<String> = match state.get("feature") {
            Some(Value::String(f)) if !f.is_empty() => f.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect(),
            Some(Value::Array(list)) => list.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            _ => graph.iter().filter(|(_, f)| ["building", "speccing"].contains(&f.status())).map(|(slug, _)| slug.clone()).collect(),
        };
        let stage = graph.iter().find(|(_, f)| !["verified", "blocked"].contains(&f.status())).map(|(_, f)| f.text("stage").to_string()).unwrap_or_default();
        json!({
            "running": running, "stage": stage,
            "event": status, "project": pycompat::last_segment(repo), "branch": meta.get("branch"), "turn": meta.get("turns"), "cost": meta.get("cost_usd"), "reason": reason,
            "next": state.get("next"), "options": if state.get("options").is_some_and(Value::is_array) { state["options"].clone() } else { json!([]) },
            "to_check": if state.get("to_check").is_some_and(Value::is_array) { state["to_check"].clone() } else { json!([]) },
            "features": features, "commits": log.lines().take(8).collect::<Vec<_>>(), "push": meta.get("push"), "summary": summary, "pr": meta.get("pr"), "pr_status": meta.get("pr_status"),
            "validate": delivery::validate_items(work).into_iter().take(8).collect::<Vec<_>>(), "graph": graph_view::ascii_graph(&graph), "graph_short": graph_view::short(&graph, 220),
            "script": self.ctx.app_exe.display().to_string(), "repo": repo, "flags": command_flags(opts), "worktree": work, "monitor_url": meta.get("monitor_url"),
        })
    }

    /// Start creates the worktrees that are missing (one per repo defined for the project), before the first turn. Idempotent. A failure is
    /// reported as a clear PROBLEM line and stops the start with it.
    fn ensure_worktrees_or_stop(&self, repo: &str, opts: &mut LoopOpts) -> Result<(String, Value), String> {
        match self.ensure_worktrees(repo, opts) {
            Ok(done) => Ok(done),
            Err(stop) => {
                println!("[check] PROBLEM: worktree not created: {stop}");
                Err(format!("the loop did not start: its worktrees could not be created. Problem: {stop}"))
            }
        }
    }

    fn ensure_worktrees(&self, repo: &str, opts: &mut LoopOpts) -> Result<(String, Value), String> {
        let mut env = self.env();
        let mut ctx = Ctx { env: &mut env, toolkit_root: self.ctx.toolkit_text() };
        let mut wt = WtOpts {
            setup: opts.setup.clone(),
            branch: opts.branch.clone(),
            dry_run: opts.dry_run,
            base_branch: opts.base_branch.clone(),
            only_repos: opts.only_repos.clone(),
            repo_path: opts.repo_path.clone(),
        };
        let result = ensure_worktrees(&mut ctx, repo, &mut wt);
        opts.repo_path = wt.repo_path;
        result
    }

    /// The loop's egress proxy (one per loop, started before the first turn, port in `loop/internal/network-runtime.json`). Never fails:
    /// without it the guard points the proxy variables of the agent's commands at a closed port, so HTTP clients fail closed.
    fn start_egress_layer(&self, work: &str, repo: &str) -> Option<egress_proxy::Layer> {
        match egress_proxy::acquire(&self.env(), work, pycompat::last_segment(repo)) {
            Ok(layer) => {
                println!("[loop] egress proxy on 127.0.0.1:{} (denials go to {})", layer.port(), layer.log_file().display());
                Some(layer)
            }
            Err(error) => {
                println!("[loop] WARNING: egress proxy not started (OSError: {error}); the agent's HTTP clients fail closed until it is fixed");
                None
            }
        }
    }

    /// Stores the cost forecast and the cap ladder in the run meta after every turn (data for the panel: raising the limit is one step to
    /// `cost_cap_ladder.suggested`), the E2E progress ("3 of 8 done"), and sends ONE notice per cap when the forecast passes the cap, BEFORE
    /// the money is spent.
    fn record_cost_forecast(&self, work: &str, meta: &mut Value, turn: i64, cost: f64, opts: &LoopOpts) {
        let specs = Path::new(work).join(".specs").join("project").display().to_string();
        let (_, total, done) = roadmap_score(work);
        let pending = if opts.e2e { e2e_pending(&specs).len() as u64 } else { 0 };
        let open = if done < total { (total - done) as u64 } else { 0 };
        let cap = effective_cost_cap(meta, opts.max_cost);
        let estimate = cost_budget::forecast(cost, turn.max(0) as u64, done.max(0) as u64, open, pending, cap);
        cost_budget::store(meta, estimate.as_ref(), cap);
        if opts.e2e {
            let (checked, verified) = e2e_progress(&specs);
            set(meta, "e2e_progress", json!({"checked": checked, "total": verified, "pending": pending}));
        }
        if let Some(f) = estimate.filter(|f| f.exceeds_cap) {
            if meta.get("cost_forecast_warned_cap").and_then(Value::as_f64) != Some(cap) {
                set(meta, "cost_forecast_warned_cap", json!(cap));
                let suggested = cost_budget::suggested_cap(cap, Some(&f)).map(|s| format!("US$ {s:.2}")).unwrap_or_else(|| "a higher limit".into());
                self.notify_wait(
                    "the forecast passes the cost cap",
                    &format!(
                        "Spent US$ {:.2}; about {} more turn(s) (US$ {:.2}) are expected, so the run needs about US$ {:.2} and the cap is US$ {cap:.2}. Raise the limit to {suggested} in the panel BEFORE it pauses.{}",
                        f.spent,
                        f.remaining_turns,
                        f.estimated_remaining,
                        f.estimated_total,
                        self.monitor_url.clone().map(|l| format!(" Panel: {l}")).unwrap_or_default()
                    ),
                    None,
                );
            }
        }
    }

    /// ONE notice when the spend reaches 80 % of the cap (again only if the cap is raised and 80 % of the new one is reached).
    fn warn_near_cost_cap(&self, meta: &mut Value, cost: f64, cap: f64) -> bool {
        if cap == 0.0 || cost < cap * COST_WARNING_RATIO || cost > cap || meta.get("cost_warned_cap").and_then(Value::as_f64) == Some(cap) {
            return false;
        }
        set(meta, "cost_warned_at", json!(now_iso()));
        set(meta, "cost_warned_cap", json!(cap));
        let link = self.monitor_url.clone();
        self.notify_wait(
            "spend at 80 % of the cap",
            &format!(
                "The loop spent US$ {cost:.2} of the US$ {cap:.2} cap ({} %). It pauses when the cap is passed; raise the limit in the panel to carry on without a pause.{}",
                (cost * 100.0 / cap).round_ties_even() as i64,
                link.map(|l| format!(" Panel: {l}")).unwrap_or_default()
            ),
            None,
        );
        true
    }

    /// Notice (every configured channel) when a turn ran on another command or account than the previous turn: `claude` -> `cc`, back to
    /// `claude`, or the `cc` picker on another account. The first turn of the first run only records it.
    fn notify_claude_change(&self, opts: &mut LoopOpts, turn: i64, name: &str, account: &str) -> Option<Value> {
        let used = (name.to_string(), account.to_string());
        let previous = opts.last_claude_used.replace(used.clone());
        let unusable = opts.unusable_notice.take(); // set when the change came from an account that cannot be used
        let previous = previous.filter(|p| *p != used)?;
        let main_name = command_base_name(&if opts.claude.is_empty() { claude_binaries::DEFAULT_BINARY_NAME.to_string() } else { opts.claude.clone() });
        let is_main = command_base_name(name) == main_name;
        let phrase_of = |key: &str| opts.limit_phrases.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default(); // which window ran out on each command
        let before = describe_claude_used(&previous.0, &previous.1, command_base_name(&previous.0) == main_name);
        let now = describe_claude_used(name, account, is_main);
        println!("[loop] turn {turn} ran on {now}; before it was {before}");
        let same_command = previous.0 == name && unusable.is_none();
        let only = if same_command { Some(local_notice_channels(&self.env())) } else { None }; // another account of the same command is noise for email/webhook
        let mut unusable_subject = String::new();
        let reason;
        if let Some(u) = &unusable {
            // it needs a human (log in again, ask the admin): always every channel
            unusable_subject = format!("{} account unusable", if command_base_name(u["command"].as_str().unwrap_or("")) == main_name { "main" } else { "backup" });
            let why = u["reason"].as_str().filter(|r| !r.is_empty()).unwrap_or("no reason in the output");
            reason = format!("The previous account is unusable ({why}); this is not a usage limit, so there is no reset time and it does not come back by itself. The loop moved to the backup command. Waiting does not fix a login: sign in again (`claude` then /login, or `claude setup-token`) or ask the admin to enable access.");
        } else if same_command {
            reason = format!(
                "The `{}` account picker moved to another account because the previous one hit its usage limit{}.",
                command_base_name(name),
                limit_suffix(&phrase_of(&command_base_name(&previous.0)))
            );
        } else if is_main {
            reason = format!("The usage limit of the main account{} should have reset, so the loop went back to it.", limit_suffix(&phrase_of(&main_name)));
        } else {
            reason = format!(
                "The previous account hit its usage limit{}, so the loop moved to the backup command.",
                limit_suffix(&phrase_of(&command_base_name(&previous.0)))
            );
        }
        let kind = if unusable.is_some() {
            "account_unusable"
        } else if same_command {
            "picker_switch"
        } else if is_main {
            "back_to_main"
        } else {
            "usage_limit_switch"
        };
        let event = json!({
            "kind": kind, "turn": turn, "at": now_iso(), "to_main": is_main,
            "from": {"command": command_base_name(&previous.0), "account": previous.1}, "to": {"command": command_base_name(name), "account": account},
            "reason": reason, "unusable": unusable.as_ref().map(|u| u["reason"].clone()).unwrap_or(Value::Null),
        });
        self.notify_wait(
            &claude_change_subject(name, account, is_main, same_command, &unusable_subject),
            &format!("Turn {turn} now runs on {now}.\nBefore it the loop ran on {before}.\n{reason} The loop goes on by itself."),
            only,
        );
        Some(event)
    }

    /// One alert per run, on every channel, when the MAIN command's weekly window ran out: until when it is unusable, where the loop goes on,
    /// and the last known 5-hour and weekly usage of every other command or account. Sent again only when the reset moment changes.
    fn alert_main_weekly_limit(&self, opts: &mut LoopOpts, meta: &Value, turn: i64, name: &str, blocking: Option<&Value>) -> bool {
        if opts.claude_index != 0 || !usage_windows::is_weekly_block(blocking) {
            return false;
        }
        let blocking = blocking.expect("weekly block");
        let main_base = command_base_name(name);
        let reset_at = blocking.get("reset_at").cloned().unwrap_or(Value::Null);
        if opts.weekly_alerts.iter().any(|(k, v)| *k == main_base && *v == reset_at) {
            return false;
        }
        opts.weekly_alerts.retain(|(k, _)| *k != main_base);
        opts.weekly_alerts.push((main_base.clone(), reset_at));
        let moment = usage_windows::reset_moment(Some(blocking));
        let until = moment.map(usage_windows::format_moment).unwrap_or_else(|| "its reset (time unknown)".to_string());
        let fallback = next_fallback_name(opts);
        let goes_on = if fallback.is_empty() {
            "no other command is configured, the loop waits for the reset".to_string()
        } else {
            format!("the loop continues on `{}`", command_base_name(&fallback))
        };
        let lines = other_usage_lines(opts, meta, name);
        println!("[loop] main account weekly limit reached on `{main_base}`, unusable until {until}; {goes_on}");
        self.notify_wait(
            &format!("main account weekly limit reached, unusable until {until}; {goes_on}"),
            &format!(
                "Turn {turn}: the weekly window of the main command `{main_base}` is used up.\nLast known usage of the other commands:\n{}",
                if lines.is_empty() { "- none configured".to_string() } else { lines.join("\n") }
            ),
            None,
        );
        true
    }

    // ---------------------------------------------------------------- end
    fn finish(&self, work: &str, meta: &mut Value, status: &str, reason: &str, opts: &LoopOpts) -> i32 {
        process_registry::clear(work);
        set(meta, "stopped", json!(status));
        set(meta, "reason", json!(reason));
        if status == "paused" {
            // resumable stop (spend cap): the panel reads the status from LOOP.json
            let mut paused = load_json(&state_file(work, "LOOP.json"));
            if let Some(map) = paused.as_object_mut() {
                map.insert("status".into(), json!("paused"));
                map.insert("reason".into(), json!(reason));
                map.insert("turn".into(), json!(i64_of(meta.get("turns"))));
                map.insert("next".into(), json!("raise the limit or resume in the panel"));
                map.insert("at".into(), json!(now_iso()));
                map.shift_remove("options");
            }
            let _ = save_json(&state_file(work, "LOOP.json"), &paused);
        }
        set(meta, "finished_at", json!(now_iso()));
        let path = meta_path(work);
        if meta.get("push").is_none() {
            set(meta, "push", json!("not done"));
        }
        if status == "done" && opts.push {
            let problem = identity_problem(&self.ctx, work, opts);
            if !problem.is_empty() {
                set(meta, "push", json!(format!("refused: {problem}")));
            } else {
                let mut env = self.env();
                let delivered = delivery::finalize_project(&mut env, work, meta, false, false, "", None, &opts.base_branch);
                set(meta, "push", delivered.get("push").cloned().unwrap_or(Value::Null));
                set(meta, "pr", delivered.get("pr").cloned().unwrap_or(Value::Null));
                set(meta, "pr_status", delivered.get("pr_status").cloned().unwrap_or(Value::Null));
            }
        }
        let _ = save_meta(&path, meta);
        graph_view::write(work);
        let env = self.env();
        let out_dir = if opts.summary_dir.is_empty() { loop_summary::default_output_dir(&env) } else { opts.summary_dir.clone() };
        let args = vec![
            work.to_string(),
            "--branch".into(),
            text(meta.get("branch")),
            "--base".into(),
            text(meta.get("base")),
            "--out".into(),
            out_dir,
            "--max-cost".into(),
            py_float(effective_cost_cap(meta, opts.max_cost)),
        ];
        let summary = loop_summary::run_cli(&args, &env).1.trim().to_string();
        let repo = if truthy(meta.get("repo")) { text(meta.get("repo")) } else { opts.repo.clone() };
        let context = self.build_context(work, meta, status, reason, opts, &nt_abspath(&repo), &summary);
        let notices = self.deliver(&context, work, None);
        set(meta, "notices", Value::Object(notices));
        let _ = save_json(&path, meta);
        exit_code(status)
    }

    /// Entry point: any exit of the loop notifies the user (`finish` covers the normal stops; this covers the rest).
    pub fn entry(&mut self, args: &[String]) -> i32 {
        let opts = match parse(args) {
            Parsed::Exit { code, stdout, stderr } => {
                print!("{stdout}");
                eprint!("{stderr}");
                return code;
            }
            Parsed::Run(o) => *o,
        };
        winproc::install_ctrl_c_handler();
        let mut shared = opts.clone();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.main(opts)));
        match outcome {
            Ok(Ok(code)) => code,
            Ok(Err(message)) => {
                // sys.exit("message"): preflight, worktree, lock, --answer without a question...
                eprintln!("{message}");
                shared.push_explicit = shared.push_flag == Some(true);
                shared.push = shared.push_flag != Some(false);
                self.notify_early(&message, &shared);
                1
            }
            Err(panic) => {
                let detail = panic.downcast_ref::<String>().cloned().or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "panic".into());
                eprintln!("{detail}");
                self.notify_early(&format!("unhandled exception in the script: panic: {detail}"), &shared);
                20
            }
        }
    }

    fn main(&mut self, mut opts: LoopOpts) -> Result<i32, String> {
        opts.push_explicit = opts.push_flag == Some(true);
        opts.push = opts.push_flag != Some(false);
        let repo = nt_abspath(&opts.repo);
        self.repo = repo.clone();
        self.notify = !(opts.check || opts.dry_run || opts.rewind.is_some());
        if git(&repo, &["rev-parse", "--is-inside-work-tree"]).1 != "true" {
            return Err(format!("{repo} is not a git repository"));
        }
        if let Some(turn) = opts.rewind {
            return rewind(&repo, turn.max(0) as u64, opts.yes);
        }
        if opts.ensure_worktrees {
            self.notify = false;
            let names: Vec<String> = opts.repos.split(',').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
            opts.only_repos = if names.is_empty() { None } else { Some(names) };
            opts.repo_path = repo.clone();
            return Ok(match self.ensure_worktrees(&repo, &mut opts) {
                Ok(_) => 0,
                Err(stop) => {
                    println!("[loop] error: {stop}");
                    1
                }
            });
        }
        let (problems, warns) = preflight(&self.ctx, &repo, &opts);
        for item in &warns {
            println!("[check] WARNING: {item}");
        }
        for item in &problems {
            println!("[check] PROBLEM: {item}");
        }
        if opts.check {
            if problems.is_empty() {
                println!("[check] READY. Command:\n  {}", ready_command(&self.ctx, &repo, &opts));
            }
            if opts.json {
                let items = check_items(&problems, &warns, &project_repos::load_repos(&repo), &repo);
                println!("[check] JSON: {}", py_json_dumps(&json!({"ready": problems.is_empty(), "problems": problems, "warnings": warns, "items": items}), false));
            }
            return Ok(if problems.is_empty() { 0 } else { 1 });
        }
        if !problems.is_empty() {
            let work_now = worktree_of(&repo);
            let root = if Path::new(&work_now).is_dir() { work_now } else { repo.clone() };
            let env_problems = test_environment_problems(&root, &opts);
            let env_reason = test_environment_stop_reason(&root, &env_problems);
            let others: Vec<&String> = problems.iter().filter(|p| !env_problems.contains(p)).collect();
            // the test environment text goes FIRST: the notice clips the reason and this is the part the user has to act on
            let tail_text = if !others.is_empty() {
                format!("Problems: {}", others.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("; "))
            } else if env_reason.is_empty() {
                format!("Problems: {}", problems.join("; "))
            } else {
                String::new()
            };
            return Err(format!(
                "pre-flight failed, the loop did not start. {}{tail_text}",
                if env_reason.is_empty() { String::new() } else { format!("{env_reason} ") }
            ));
        }
        let repos = project_repos::load_repos(&repo);
        opts.repo_names = repo_names_of(&repos);
        opts.primary_name = if repos.len() > 1 { project_repos::primary_of(&repos).name.clone() } else { String::new() };
        let (work, mut meta) = self.ensure_worktrees_or_stop(&repo, &mut opts)?;
        opts.reference_dirs = project_repos::reference_repos(&repos).iter().map(|r| r.path.clone()).collect();
        opts.extra_worktrees = meta
            .get("repos")
            .and_then(Value::as_object)
            .map(|r| r.values().filter(|i| i.is_object() && !truthy(i.get("primary")) && truthy(i.get("worktree"))).map(|i| text(i.get("worktree"))).collect())
            .unwrap_or_default();
        if opts.dry_run {
            println!(
                "[dry-run] turn 1 command: {}",
                claude_command(&self.ctx, &opts, &format!("/orchestrator continue --headless --turn 1 --wave {}", opts.wave)).join(" ")
            );
            return Ok(0);
        }
        let lock = acquire_lock(&work)?; // first thing after the worktree exists: a second loop on it stops here, before the monitor, the answer or a Claude call
        let prepared = (|| -> Result<(AutoState, i64, f64, i64), String> {
            self.start_monitor(&repo, &work, &mut meta, &opts);
            if opts.option != 0 {
                let state = load_json(&state_file(&work, "LOOP.json"));
                let options: Vec<Value> = state.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
                if opts.option < 1 || opts.option as usize > options.len() {
                    let listed = options.iter().enumerate().map(|(i, o)| format!("{}) {}", i + 1, text(Some(o)))).collect::<Vec<_>>().join("; ");
                    return Err(format!(
                        "--option {} invalid: the notice listed {} option(s) ({})",
                        opts.option,
                        options.len(),
                        if listed.is_empty() { "none".to_string() } else { listed }
                    ));
                }
                opts.answer = text(options.get(opts.option as usize - 1));
            }
            if !opts.answer.is_empty() {
                println!("{}", write_answer(&work, &opts.answer)?);
            }
            let mut auto_state = AutoState::default();
            if opts.answer.is_empty() {
                let next_turn = i64_of(meta.get("turns")) + 1;
                // turn stopped on a trivial question: answer it and carry on without you
                if !try_auto_answer(&work, next_turn, &auto_opts(&opts), &mut meta, &mut auto_state) {
                    try_gate_tiebreak(&work, next_turn, &auto_opts(&opts), &mut meta, &mut auto_state);
                    // spec that failed the gate on the previous run: break the tie
                }
            }
            let turn = i64_of(meta.get("turns"));
            opts.last_claude_used = if truthy(meta.get("claude")) { Some((text(meta.get("claude")), text(meta.get("claude_account")))) } else { None }; // a resumed run compares with the last turn
            let cost = f64_of(meta.get("cost_usd"));
            let baseline_number = turn.max(0) as u64;
            if loop_state::load_snapshot(&work, baseline_number).is_none() || (turn == 0 && loop_state::baseline_is_stale(&work, baseline_number)) {
                // baseline: state before this run's first turn (return point for --rewind), built from the roadmap approved for THIS run
                let heads = heads_map(&work, &meta);
                loop_state::snapshot(&work, turn.max(0) as u64, &git(&work, &["rev-parse", "HEAD"]).1, heads.as_ref());
            }
            Ok((auto_state, turn, cost, 0))
        })();
        let (auto_state, turn, cost, _) = match prepared {
            Ok(p) => p,
            Err(message) => {
                release_lock(&lock); // every early exit before the loop proper lets the lock go
                return Err(message);
            }
        };
        let egress_layer = self.start_egress_layer(&work, &repo);
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.turns(&repo, &work, &mut meta, &mut opts, auto_state, turn, cost, egress_layer.as_ref())));
        egress_proxy::release(egress_layer.as_ref()); // also on every crash path: the proxy stops and the runtime file goes
        release_lock(&lock);
        match result {
            Ok(code) => Ok(code),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn turns(
        &mut self,
        repo: &str,
        work: &str,
        meta: &mut Value,
        opts: &mut LoopOpts,
        mut auto_state: AutoState,
        mut turn: i64,
        mut cost: f64,
        egress: Option<&egress_proxy::Layer>,
    ) -> i32 {
        let specs = Path::new(work).join(".specs").join("project").display().to_string();
        let logs = turns_dir(work);
        let (mut stall, mut last_score, mut last_print, mut limit_hits) = (0i64, roadmap_score(work).0, String::new(), 0i64);
        loop {
            if winproc::interrupted() {
                return self.finish(work, meta, "error", "interrupted by the user (Ctrl+C)", opts);
            }
            let paused = pause_requested(work, turn); // before spending the next turn
            if !paused.is_empty() {
                println!("[loop] PAUSE found: stopping before turn {}", turn + 1);
                return self.finish(work, meta, "awaiting_user", &paused, opts);
            }
            let cap = effective_cost_cap(meta, opts.max_cost);
            if cap != 0.0 && cost > cap {
                // resumed over the cap without raising it: pause again before spending anything
                return self.finish(work, meta, "paused", &cost_cap_reason(cost, cap), opts);
            }
            turn += 1;
            let leftover = LeftoverOpts { leftovers: opts.leftovers.clone(), primary_name: opts.primary_name.clone() };
            if let Some((dirty_name, rows)) = settle_dirty_worktrees(work, meta, turn, &leftover).into_iter().next() {
                let where_ = if dirty_name.is_empty() { String::new() } else { format!("in repo {dirty_name} ") };
                return self.finish(work, meta, "error", &crate::worktree_state::dirty_stop_message(&where_, turn, &rows), opts);
            }
            opts.leftovers = String::new(); // the recovery choice applies to the first turn of this run only
            set(meta, "decisions_mark", json!(decisions::line_count(work)));
            let _ = save_meta(&meta_path(work), meta); // critical decisions written after this line stop the loop at the end of the turn
            let branch = text(meta.get("branch"));
            if git(work, &["branch", "--show-current"]).1 != branch {
                let (code, _, err) = git(work, &["checkout", &branch]);
                if code != 0 {
                    return self.finish(work, meta, "error", &format!("worktree is off the loop branch and checkout failed: {err}"), opts);
                }
            }
            {
                let mut env = self.env();
                let wctx = Ctx { env: &mut env, toolkit_root: self.ctx.toolkit_text() };
                crate::worktrees::refresh_inputs(&wctx, repo, work, "", true); // the panel writes .env.test / NETWORK.json in the repo: a running loop picks them up here
            }
            if let Some(layer) = egress {
                layer.set_turn(turn); // egress.log lines carry the turn
            }
            let applied = audit_before_turn(work);
            if !applied.is_empty() {
                println!("[loop] forgotten dependencies applied to the ROADMAP before the turn: {}", applied.join("; "));
            }
            let prompt = format!(
                "/orchestrator continue --headless --turn {turn} --wave {} --branch {branch}{}{}",
                opts.wave,
                if opts.e2e { " --e2e" } else { "" },
                plan_flags(work, opts.wave, &opts.primary_name)
            );
            let started = Instant::now();
            let clock = local_now();
            println!("[loop] turn {turn} | {:02}:{:02}:{:02}", clock.hour(), clock.minute(), clock.second());
            return_to_main_command(opts);
            if refresh_fallbacks(opts, &self.env()) {
                println!("[loop] the fallback commands changed in the settings: now {}", claude_names(opts).join(", "));
            }
            let claude_name = current_claude(opts);
            let command = {
                // final E2E phase: the serial run over the verified features gets a budget sized by their number, so one turn is not cut mid-way
                let pending = if opts.e2e { e2e_pending(&specs).len() } else { 0 };
                let (_, total_now, done_now) = roadmap_score(work);
                let mut turn_opts = opts.clone();
                turn_opts.budget = cost_budget::turn_budget(opts.budget, total_now != 0 && done_now == total_now, pending, cost, effective_cost_cap(meta, opts.max_cost));
                if turn_opts.budget != opts.budget {
                    println!("[loop] E2E phase: {pending} feature(s) to check, turn budget US$ {:.2} (base {:.2})", turn_opts.budget, opts.budget);
                }
                claude_command(&self.ctx, &turn_opts, &prompt)
            };
            let lanes = process_registry::lanes_of_prompt(&prompt);
            let known_account = opts.last_claude_used.as_ref().filter(|(n, _)| *n == claude_name).map(|(_, a)| a.clone()).unwrap_or_default();
            let mut process_id = String::new();
            let (code, out, err, timed_out) = run_turn_notifying(&self.ctx, &command, work, opts.timeout, &mut self.env_reported, &mut |pid| {
                process_id = process_registry::register(work, "claude_turn", turn, &lanes, pid, &command_base_name(&claude_name), &known_account);
            });
            if !process_id.is_empty() {
                process_registry::unregister(work, &process_id);
            }
            if winproc::interrupted() {
                return self.finish(work, meta, "error", "interrupted by the user (Ctrl+C)", opts);
            }
            let data = parse_result(&out);
            let mut log: Map<String, Value> = data.as_object().cloned().unwrap_or_default();
            if data.get("ok") != Some(&Value::Bool(true)) {
                log.insert("raw".into(), json!(tail(&out, 4000)));
            }
            let rate_limit = match log.shift_remove("rate_limit") {
                Some(v) if truthy(Some(&v)) => v,
                _ => json!({}),
            }; // kept below as `usage_windows` and `limit_status`
            let windows = usage_windows::with_local_reset(&usage_windows::windows_of(&rate_limit));
            let limit_status = text(rate_limit.get("status"));
            log.insert("usage_windows".into(), windows.clone());
            log.insert("limit_status".into(), json!(limit_status));
            // what the turn says about limits and logins: the result text and stderr (the rest of a stream-json output is the model's own work)
            let scan_text = if data.get("ok") == Some(&Value::Bool(true)) {
                format!("{}\n{err}", text(data.get("result")))
            } else {
                format!("{}\n{err}", cli_output::strip_rate_limit_events(&out))
            };
            let picked_account = claude_binaries::account_from_stderr(&err);
            let blocking = usage_windows::blocking_window(&rate_limit, None);
            if !err.is_empty() {
                log.insert("stderr".into(), json!(tail(&err, 2000)));
            }
            log.insert("exit_code".into(), json!(code));
            log.insert("timeout".into(), json!(timed_out));
            log.insert("turn".into(), json!(turn));
            log.insert("claude".into(), json!(claude_name));
            log.insert("caveman".into(), json!(is_caveman_on(&self.ctx)));
            let account = claude_account(opts, &claude_name, &err);
            log.insert("claude_account".into(), json!(account));
            let auth_fields = claude_auth_fields(&account);
            for (k, v) in &auth_fields {
                log.insert(k.clone(), v.clone());
            }
            set(meta, "claude", json!(claude_name));
            set(meta, "claude_account", json!(account));
            for (k, v) in &auth_fields {
                set(meta, k, v.clone());
            }
            record_usage(meta, &usage_key(&claude_name, &picked_account), &windows, &limit_status);
            let _ = save_json(&logs.join(format!("{turn:03}.json")), &Value::Object(log));
            cost += f64_of(data.get("total_cost_usd"));
            set(meta, "turns", json!(turn));
            set(meta, "cost_usd", json!(round4(cost)));
            self.record_cost_forecast(work, meta, turn, cost, opts);
            let _ = save_meta(&meta_path(work), meta);

            let cap = effective_cost_cap(meta, opts.max_cost);
            if cap != 0.0 && cost > cap {
                return self.finish(work, meta, "paused", &cost_cap_reason(cost, cap), opts);
            }
            if self.warn_near_cost_cap(meta, cost, cap) {
                let _ = save_meta(&meta_path(work), meta);
            }
            if timed_out {
                return self.finish(work, meta, "error", &format!("turn {turn} exceeded {} min (process terminated)", opts.timeout), opts);
            }
            let unreadable = unrecognized_output(&data);
            let failed = code != 0 || truthy(data.get("is_error")) || unreadable;
            let api_status = data.get("api_error_status").and_then(Value::as_i64);
            let limited_by_status = failed && (blocking.is_some() || api_status == Some(429)); // the output's own rate limit data
            let account_limited = failed && (claude_binaries::is_usage_limit(&scan_text) || limited_by_status);
            let mut limit_phrase = String::new();
            if account_limited {
                // the blocking window first, `[cc] reinicia:` / `limit reached|epoch` otherwise
                let reset = usage_windows::reset_moment(blocking.as_ref()).or_else(|| claude_binaries::reset_time(&scan_text));
                limit_phrase = usage_windows::phrase(blocking.as_ref());
                let key = command_base_name(&claude_name);
                opts.limit_phrases.retain(|(k, _)| *k != key);
                opts.limit_phrases.push((key, limit_phrase.clone()));
                remember_reset(opts, reset, &limit_phrase);
                if opts.claude_index == 0 {
                    // the main command: when to try it again while a fallback keeps the turns
                    opts.main_reset = Some(main_reset_moment(opts, reset));
                }
                if picked_account.is_empty() && self.alert_main_weekly_limit(opts, meta, turn, &claude_name, blocking.as_ref()) {
                    let _ = save_meta(&meta_path(work), meta);
                }
                if !picked_account.is_empty() && !opts.picker_tried.contains(&picked_account) {
                    // the `cc` picker ran on an account that ran out during the turn: run it again, it picks the next free one ("cc" + Enter)
                    opts.picker_tried.push(picked_account.clone());
                    if opts.last_claude_used.is_none() {
                        opts.last_claude_used = Some((claude_name.clone(), account.clone()));
                        // notify the change
                    }
                    println!(
                        "[loop] usage limit on `{claude_name}` account {picked_account}{}; turn {turn} again, the picker chooses another account",
                        limit_suffix(&limit_phrase)
                    );
                    turn -= 1;
                    continue;
                }
            }
            if account_limited && switch_claude(opts) {
                println!("[loop] usage limit on `{claude_name}`{}; turn {turn} again with `{}`", limit_suffix(&limit_phrase), current_claude(opts));
                set(meta, "claude_switches", json!(i64_of(meta.get("claude_switches")) + 1));
                if opts.last_claude_used.is_none() {
                    opts.last_claude_used = Some((claude_name.clone(), account.clone()));
                    // notify the change
                }
                let _ = save_meta(&meta_path(work), meta);
                turn -= 1;
                continue;
            }
            if failed && !account_limited && (claude_binaries::is_account_unusable(&scan_text) || matches!(api_status, Some(401) | Some(403))) {
                // the account cannot sign in (logged out, revoked token, organization without access): no wait fixes it, try the next command
                let mut why = claude_binaries::unusable_reason(&text(data.get("result")));
                if why.is_empty() {
                    why = claude_binaries::unusable_reason(&scan_text);
                }
                if why.is_empty() {
                    why = format!("API error {}", api_status.map(|s| s.to_string()).unwrap_or_else(|| "None".into()));
                }
                let record = mark_account_unusable(opts, &claude_name, &account, &why, turn);
                if let Some(map) = meta.as_object_mut() {
                    let slot = map.entry("claude_unusable").or_insert_with(|| json!([]));
                    if let Some(list) = slot.as_array_mut() {
                        list.push(record.clone());
                    }
                }
                if switch_claude(opts) {
                    println!("[loop] account unusable on `{claude_name}` ({}); turn {turn} again with `{}`", text(record.get("reason")), current_claude(opts));
                    set(meta, "claude_switches", json!(i64_of(meta.get("claude_switches")) + 1));
                    if opts.last_claude_used.is_none() {
                        opts.last_claude_used = Some((claude_name.clone(), account.clone()));
                        // notify the change
                    }
                    let _ = save_meta(&meta_path(work), meta);
                    turn -= 1;
                    continue;
                }
                return self.finish(work, meta, "error", &unusable_stop_message(opts), opts);
            }
            if failed && (LIMIT_RE.search(&scan_text).is_some() || limited_by_status) {
                limit_hits += 1;
                if limit_hits > opts.limit_retries {
                    return self.finish(work, meta, "error", &format!("account usage limit persisted after {} waits", opts.limit_retries), opts);
                }
                let (seconds, resume_at) = limit_wait_seconds(opts);
                let when = resume_at.map(|m| format!("{:02}/{:02} {:02}:{:02}", m.day(), u8::from(m.month()), m.hour(), m.minute())).unwrap_or_default();
                let window_text = resume_at.and_then(|r| opts.limit_reset_phrases.iter().find(|(m, _)| *m == r).map(|(_, p)| p.clone())).unwrap_or_default(); // the window that frees up first
                let names = claude_names(opts);
                println!(
                    "[loop] usage limit on every command ({}); waiting {}{} ({limit_hits}/{})",
                    names.join(", "),
                    wait_text(seconds),
                    if when.is_empty() { String::new() } else { format!(" until {when}") },
                    opts.limit_retries
                );
                let first_account = if when.is_empty() {
                    format!("The reset time is unknown: it tries again in {}.", wait_text(seconds))
                } else {
                    format!(
                        "The first account frees up at {when} (in {}{}).",
                        wait_text(seconds),
                        if window_text.is_empty() { String::new() } else { format!("; {window_text}") }
                    )
                };
                self.notify_wait(
                    &format!("credits ran out{}, back in {}", limit_suffix(&window_text), wait_text(seconds)),
                    &format!(
                        "The usage limit was hit on turn {turn} by every command the loop can use ({}). {first_account} The loop waits and starts again by itself, from `{}` (wait {limit_hits} of {}).",
                        names.join(", "),
                        names[first_usable_index(opts)],
                        opts.limit_retries
                    ),
                    None,
                );
                let paused = sleep_unless_paused(work, turn - 1, seconds);
                if !paused.is_empty() {
                    return self.finish(work, meta, "awaiting_user", &paused, opts);
                }
                opts.claude_index = first_usable_index(opts); // after the wait, the main command again (the first one whose account works)
                opts.picker_tried.clear();
                opts.limit_resets.clear();
                opts.main_reset = None;
                turn -= 1;
                continue;
            }
            limit_hits = 0;
            opts.picker_tried.clear();
            opts.limit_resets.clear();
            if unreadable && code == 0 {
                let detail = if truthy(data.get("detail")) { text(data.get("detail")) } else { "unrecognized".into() };
                let sample = if truthy(data.get("sample")) { text(data.get("sample")) } else { "(empty)".into() };
                return self.finish(
                    work,
                    meta,
                    "error",
                    &format!("claude output on turn {turn} was not recognized ({detail}; the CLI output format may have changed, run `harness doctor` to compare the installed version with the tested one): {sample}"),
                    opts,
                );
            }
            if failed {
                let why = if !err.is_empty() {
                    err.clone()
                } else if truthy(data.get("result")) {
                    text(data.get("result"))
                } else {
                    text(data.get("sample"))
                };
                return self.finish(work, meta, "error", &format!("claude exited with code {code} on turn {turn}: {}", head(&why, 300)), opts);
            }
            if let Some(event) = self.notify_claude_change(opts, turn, &claude_name, &account) {
                // a silent switch to another account is kept as an event the panel can list (newest 30)
                if let Some(map) = meta.as_object_mut() {
                    if let Some(list) = map.entry("claude_events").or_insert_with(|| json!([])).as_array_mut() {
                        list.push(event);
                        let extra = list.len().saturating_sub(30);
                        list.drain(..extra);
                    }
                }
                let _ = save_meta(&meta_path(work), meta);
            }
            let denials: Vec<Value> = data.get("permission_denials").and_then(Value::as_array).cloned().unwrap_or_default();
            let env_now = self.env();
            let risky: Vec<&Value> = denials.iter().filter(|d| is_risky_denial(&env_now, d, work)).collect();
            if !risky.is_empty() {
                let listed = risky.iter().map(|d| denial_text(d)).collect::<Vec<_>>().join("; ");
                return self.finish(work, meta, "error", &format!("permission denied on turn {turn} (risky command, not tolerated): {}", head(&listed, 400)), opts);
            }

            let mut state = load_json(&state_file(work, "LOOP.json"));
            let state_ok =
                |s: &Value| s.get("turn").and_then(Value::as_i64) == Some(turn) && s.get("status").and_then(Value::as_str).is_some_and(|st| STATUS_OK.contains(&st));
            if !state_ok(&state) && truthy(data.get("session_id")) {
                // the model finished the turn (e.g. while waiting on background subagents) without writing LOOP.json: ONE correction turn in the
                // same session, only asking for the file (no new work), before failing the loop
                println!("[loop] LOOP.json not written on turn {turn}; one correction turn in the same session");
                let mut fix = claude_command(&self.ctx, opts, &loop_json_correction_prompt(turn, &branch));
                fix.extend(["--resume".to_string(), text(data.get("session_id"))]);
                let (_, fix_out, _, _) = run_turn(&self.ctx, &fix, work, opts.timeout, &mut self.env_reported);
                let fix_data = parse_result(&fix_out);
                cost += f64_of(fix_data.get("total_cost_usd"));
                set(meta, "cost_usd", json!(round4(cost)));
                set(meta, "loop_json_corrections", json!(i64_of(meta.get("loop_json_corrections")) + 1));
                let _ = save_meta(&meta_path(work), meta);
                state = load_json(&state_file(work, "LOOP.json"));
            }
            if !state_ok(&state) {
                let heads = heads_map(work, meta);
                loop_state::snapshot(work, turn as u64, &git(work, &["rev-parse", "HEAD"]).1, heads.as_ref()); // the turn ran: the return point exists even with a bad LOOP.json
                let mut allowed: Vec<&str> = STATUS_OK.to_vec();
                allowed.sort();
                let shown_status = match state.get("status") {
                    Some(Value::String(s)) => pycompat::py_repr_str(s),
                    Some(Value::Null) | None => "None".into(),
                    Some(other) => py_json_dumps(other, false),
                };
                let shown_turn = match state.get("turn") {
                    Some(Value::Null) | None => "None".into(),
                    Some(other) => py_json_dumps(other, false),
                };
                let status_list = format!("[{}]", allowed.iter().map(|s| pycompat::py_repr_str(s)).collect::<Vec<_>>().join(", "));
                return self.finish(
                    work,
                    meta,
                    "error",
                    &format!("LOOP.json missing, stale or invalid after turn {turn} (got status={shown_status} turn={shown_turn}; expected turn={turn} and status in {status_list}). The orchestrator must write it with: harness state loop <root> --status S --turn {turn} ..."),
                    opts,
                );
            }
            // the verifier's words (pass, fail, unverified) are mapped to the ROADMAP words before the state check instead of stopping the run
            for note in result_vocabulary::repair_roadmap_e2e(&specs) {
                println!("[loop] ROADMAP e2e= normalised ({note})");
            }
            // typed state: legal edges, tries, verified only with a commit, paths; saves this turn's snapshot for --rewind
            let prev = loop_state::load_snapshot(work, (turn - 1).max(0) as u64);
            let head_now = git(work, &["rev-parse", "HEAD"]).1;
            let new_commits = count_new_commits(prev.as_ref(), work, meta, &head_now);
            let prev_text = prev.as_ref().map(|p| read(&Path::new(p.get("dir").and_then(Value::as_str).unwrap_or("")).join("ROADMAP.md"))).unwrap_or_default();
            // a checkpoint commit that already holds a feature's work stands in for that feature's own commit
            let credited = if prev.is_some() {
                crate::worktree_state::checkpoint_credit(meta, &prev_text, &read(&Path::new(&specs).join("ROADMAP.md")), new_commits, &opts.primary_name)
            } else {
                vec![]
            };
            let new_commits = new_commits + credited.len() as i64;
            let violations = loop_state::validate(
                &prev_text,
                &read(&Path::new(&specs).join("ROADMAP.md")),
                Some(&state),
                new_commits.max(0) as usize,
                state_file(work, "ANSWERS.md").exists(),
                opts.repo_names.as_deref(),
                &opts.primary_name,
            );
            let heads = heads_map(work, meta);
            loop_state::snapshot(work, turn as u64, &head_now, heads.as_ref());
            graph_view::write(work); // .specs/project/loop/GRAPH.md always current
            if violations.is_empty() && !credited.is_empty() {
                crate::worktree_state::mark_checkpoints_credited(meta, &credited);
            }
            if !violations.is_empty() {
                return self.finish(
                    work,
                    meta,
                    "error",
                    &format!(
                        "invalid state after turn {turn} (turn {turn} already cost US$ {:.2}; --rewind {} goes back and repeats it, so use it only if the state cannot be fixed by hand): {}",
                        f64_of(data.get("total_cost_usd")),
                        turn - 1,
                        violations.iter().take(4).cloned().collect::<Vec<_>>().join("; ")
                    ),
                    opts,
                );
            }
            if !denials.is_empty() {
                let progress = denial_progress(prev.as_ref(), &specs, &state, new_commits);
                if progress.is_empty() {
                    let listed = denials.iter().map(denial_text).collect::<Vec<_>>().join("; ");
                    return self.finish(
                        work,
                        meta,
                        "error",
                        &format!("permission denied on turn {turn} and the turn did not advance (no new commit, no ROADMAP change): {}", head(&listed, 400)),
                        opts,
                    );
                }
                note_tolerated_denials(work, turn, &denials, &progress);
                if let Some(map) = meta.as_object_mut() {
                    let slot = map.entry("tolerated_denials").or_insert_with(|| json!([]));
                    if let Some(list) = slot.as_array_mut() {
                        list.push(json!({"turn": turn, "count": denials.len()}));
                    }
                }
                println!("[loop] warning: {} permission denial(s) on turn {turn}, tolerated ({progress}); noted in .specs/project/loop/VALIDATE.md", denials.len());
            }
            merge_verified_contracts(work, meta, &mut state, &specs);
            let critical = pause_on_critical_decision(work, &mut state, i64_of(meta.get("decisions_mark")));
            if !critical.is_empty() {
                return self.finish(work, meta, "awaiting_user", &critical, opts);
            }
            let paused = pause_on_quarantine(&specs, prev.as_ref(), &mut state, opts.continue_on_blocked);
            if !paused.is_empty() {
                if try_auto_answer(work, turn, &auto_opts(opts), meta, &mut auto_state) {
                    continue;
                }
                return self.finish(work, meta, "awaiting_user", &paused, opts);
            }
            let status = text(state.get("status"));
            let reason = if truthy(state.get("reason")) { text(state.get("reason")) } else { text(state.get("next")) };
            if status == "done" && opts.e2e {
                // a blocked call or a missing credential is its own stop, looked at BEFORE any label (never silently "covered")
                if let Some(stop) = e2e_evidence::awaiting_stop(work, &specs) {
                    e2e_evidence::write_stop(work, &stop);
                    return self.finish(work, meta, "awaiting_user", &stop.reason, opts);
                }
                let pending = e2e_unmarked(&specs);
                if !pending.is_empty() {
                    return self.finish(
                        work,
                        meta,
                        "error",
                        &format!("E2E phase ended without recording the result in the ROADMAP (e2e=ok|failed|not-covered): {}", pending.join("; ")),
                        opts,
                    );
                }
                let unbacked = e2e_evidence::evidence_problems(work, &specs);
                if !unbacked.is_empty() {
                    return self.finish(work, meta, "error", &e2e_evidence::evidence_stop_message(&unbacked), opts);
                }
            }
            if status == "awaiting_user" && try_auto_answer(work, turn, &auto_opts(opts), meta, &mut auto_state) {
                continue;
            }
            if status == "done" && !opts.e2e && try_gate_tiebreak(work, turn, &auto_opts(opts), meta, &mut auto_state) {
                continue;
            }
            if status == "done" || status == "awaiting_user" || status == "error" {
                return self.finish(work, meta, &status, &reason, opts);
            }
            let (score, total, done) = roadmap_score(work);
            if total != 0 && done == total && !opts.e2e {
                if try_gate_tiebreak(work, turn, &auto_opts(opts), meta, &mut auto_state) {
                    continue;
                }
                return self.finish(work, meta, "done", &format!("all features verified or blocked ({done}/{total})"), opts);
            }
            let printed = fingerprint(work, meta);
            if printed == last_print {
                return self.finish(work, meta, "error", "identical state (ROADMAP+STATE+HEAD) in 2 consecutive turns: the loop is not advancing", opts);
            }
            last_print = printed;
            if score > last_score {
                stall = 0;
            } else {
                stall += 1;
                if stall >= opts.max_stall {
                    return self.finish(work, meta, "error", &format!("{stall} consecutive turns without ROADMAP progress"), opts);
                }
            }
            self.notify_milestones(work, meta, opts, repo, &prev_text, turn);
            last_score = score; // baseline always follows the score (a blocked feature lowers the score without stopping the others' progress)
            println!(
                "[loop] turn {turn} ok in {:.0}s | progress {score} ({done}/{total} closed) | no progress: {stall}/{} | cost US$ {cost:.2}",
                started.elapsed().as_secs_f64(),
                opts.max_stall
            );
        }
    }
}

fn heads_map(work: &str, meta: &Value) -> Option<Map<String, Value>> {
    heads_of(work, meta).map(|pairs| pairs.into_iter().map(|(n, h)| (n, json!(h))).collect())
}

/// `harness loop ...`: the entry point of the CLI (everything after `loop`).
pub fn run_cli(args: &[String]) -> i32 {
    let env = Env::process();
    let ctx = AppCtx::discover(&env);
    let mut runner = Runner::new(ctx, args.to_vec());
    runner.entry(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_and_flags() {
        assert_eq!([exit_code("done"), exit_code("awaiting_user"), exit_code("paused"), exit_code("error"), exit_code("other")], [0, 10, 10, 20, 30]);
        let mut opts = LoopOpts::new("x");
        assert_eq!(command_flags(&opts), " --wave 3");
        opts.e2e = true;
        opts.push = false;
        opts.setup = "npm ci".into();
        opts.no_monitor = true;
        opts.continue_on_blocked = true;
        assert_eq!(command_flags(&opts), " --wave 3 --e2e --no-push --continue-on-blocked --setup \"npm ci\" --no-monitor");
        opts.push = true;
        opts.push_explicit = true;
        assert!(command_flags(&opts).contains(" --push"));
        assert_eq!(round4(1.234_567_8), 1.2346);
    }
}
