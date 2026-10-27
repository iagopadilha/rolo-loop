// SPDX-License-Identifier: MIT
//! Error center of a project: every kind of problem the loop leaves behind, in one list for the panel (`GET /errors`).
//!
//! Sources: the loop's own stop (LOOP.json / meta.json), the failed or noisy turns (`turns/NNN.json`), the quarantined features of the ROADMAP, and
//! the guard blocks and problem lines of the last launch, which are turned into durable, turn-stamped events in `internal/events/errors.jsonl`
//! (see `error_event_sync`). Each item says what happened in plain words, which turns it happened in (`turns`, plus `turn_counts` for grouped
//! items), what the user can do about it (`hint`, empty when there is nothing to do) and which recovery buttons apply. Texts go through credential
//! redaction and are clipped before they leave through the API. Besides reading the sources, building the list appends the events it has not
//! stored yet (the only write; a failure to write is ignored).

use super::error_event_sync::{KIND_GUARD_EVENT, KIND_LAUNCH_EVENT};
use super::loop_parsers::{collect_turn_starts, parse_launch_log, TurnTimeline, LAUNCH_FATAL_PATTERN};
use super::App;
use crate::domain::project::ProjectContext;
use crate::repositories::loop_read_formatting::{clip_redacted_text, redact_credentials, text_or_empty};
use engine_state::{loop_io, loop_state, notify_truthy};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

pub const SEVERITY_ERROR: &str = "error";
pub const SEVERITY_WARNING: &str = "warning";
pub const SEVERITY_INFO: &str = "info";
pub const KIND_LOOP: &str = "loop";
pub const KIND_TURN: &str = "turn";
pub const KIND_GUARD: &str = "guard";
pub const KIND_FEATURE: &str = "feature";
pub const KIND_LAUNCH: &str = "launch";
pub const RECOVER_RETRY: &str = "retry";
pub const RECOVER_COMMIT: &str = "commit_leftovers";
pub const RECOVER_DISCARD: &str = "discard_leftovers";
const TURNS_SCANNED_LIMIT: usize = 40;
const SNAPSHOTS_SCANNED_LIMIT: usize = 40;
const TITLE_LIMIT: usize = 160;
const DETAIL_LIMIT: usize = 1200;
const HAPPENED_LIMIT: usize = 700;
const LAUNCH_LOG_NAME: &str = "monitor-relaunch.log";
const LAUNCH_LINES_LIMIT: usize = 6;
const GUARD_ATTEMPTS_LIMIT: usize = 5;
const MILLISECONDS_PER_SECOND: f64 = 1000.0;
const STATUS_ERROR: &str = "error";
const BLOCKED_STATUS: &str = "blocked";

static BACKGROUND_TASKS: LazyLock<Re> = LazyLock::new(|| Re::new(r"Background tasks still running"));
static STOP_TURN: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?:after|on|in) turn (\d+)"));

struct Cause {
    pattern: Re,
    title: &'static str,
    happened: &'static str,
    fix: &'static str,
    recover: &'static [&'static str],
}

fn cause(pattern: &str, title: &'static str, happened: &'static str, fix: &'static str, recover: &'static [&'static str]) -> Cause {
    Cause { pattern: Re::new(pattern), title, happened, fix, recover }
}

/// (pattern on the reason, short title, what happened, what the user can do, recovery buttons)
static KNOWN_CAUSES: LazyLock<Vec<Cause>> = LazyLock::new(|| {
    vec![
        cause(
            r"dirty tree outside",
            "Files left uncommitted in the worktree",
            "A turn finished with files edited but not committed (the executors never run git and the orchestrator ended the turn before committing). Files under the paths of the features in progress are now committed by the engine on its own; these ones are outside those paths.",
            "Look at the list: if the work is good, commit it and go on; if it is junk, discard it.",
            &[RECOVER_COMMIT, RECOVER_DISCARD],
        ),
        cause(
            r"LOOP\.json missing, stale or invalid",
            "The turn ended without recording its state",
            "The orchestrator finished the turn without writing LOOP.json (often it stopped to wait for a background agent). The engine already tried one correction turn.",
            "Try again usually works; if it repeats, rewind to the previous turn (`harness.py loop <repo> --rewind <N>`).",
            &[RECOVER_RETRY],
        ),
        cause(
            r"invalid state after turn",
            "The turn left an invalid ROADMAP/STATE",
            "The ROADMAP or LOOP.json breaks a rule (illegal status change, verified without a commit...).",
            "Rewind to the turn before it (`harness.py loop <repo> --rewind <N>`) and try again.",
            &[],
        ),
        cause(
            r"exceeded \d+ min",
            "A turn took too long",
            "The turn went over the time limit and was terminated.",
            "Raise the turn timeout in New loop (or split the feature) and try again.",
            &[RECOVER_RETRY],
        ),
        cause(r"cost cap", "Cost cap reached", "The sum of the turns passed the cost cap.", "Raise the cap in New loop (or accept the cost) and try again.", &[]),
        cause(r"usage limit", "Account usage limit", "The Claude account hit its usage limit and the waits ran out.", "Try again after the limit resets.", &[RECOVER_RETRY]),
        cause(
            r"permission denied",
            "A command was denied",
            "The guard or the permission mode denied a command the turn needed.",
            "Read the command in the details; allow it in PERMISSIONS.json or change the feature.",
            &[],
        ),
        cause(
            r"identical state|without ROADMAP progress",
            "The loop is not advancing",
            "Turns are passing without the ROADMAP changing.",
            "Look at the last turn in History, answer what it needs, or send an instruction.",
            &[],
        ),
        cause(
            r"pre-flight failed|did not start",
            "The loop did not start",
            "The pre-flight check found a problem before spending a turn.",
            "Fix what the details say and start again.",
            &[],
        ),
    ]
});
const TIMEOUT_CAUSE: usize = 3;
const BACKGROUND_TASKS_HAPPENED: &str = "The verifier (a background agent) was still running when the turn ended, so it was stopped.";
const BACKGROUND_TASKS_HINT: &str = "The engine now waits for background agents, so this should not repeat; if the feature stays in building, try again.";
const GUARD_NOTE: &str =
    "The guard stopped it: it worked and nothing was changed. This is not a problem; many blocks of the same rule only mean an agent kept retrying a forbidden command.";

fn guard_rule_reason(rule: &str) -> &'static str {
    match rule {
        "specs-only" => "An agent tried to write outside .specs/ (planning and spec agents only write there).",
        "state-write" => "An agent tried to write the loop state, which only the orchestrator and the engine own.",
        "approval-write" => "Something tried to write the roadmap approval, which only your click on Approve roadmap can write.",
        "git-write" => "An agent tried a git write command (the engine owns commits).",
        "git-push" => "An agent tried git push (delivery is done by harness.py deliver, at the end).",
        "push-protected" => "An agent tried to push a protected branch.",
        "pr-release" => "An agent tried to open a pull request or a release.",
        "prod-write" => "An agent tried to write in production (never allowed).",
        "secrets" => "An agent tried to read a credential file.",
        "outbound-msg" => "An agent tried to send a message outside the machine.",
        "panel-trigger" => "An agent tried to operate the panel itself.",
        "repo-only" => "An agent tried to act outside the project's worktrees.",
        "loop-run" => "An agent tried to start the loop or the panel (only you do that).",
        "delivery" => "An agent tried to deliver (only the orchestrator delivers).",
        "egress-host" => "An agent tried to reach a host, path or method that is not in the project's network list.",
        "egress-content" => "An agent tried to write a URL that is not in the project's network list into a file.",
        "egress-tamper" => "An agent tried to switch off or bypass the loop's network rules.",
        "egress-runtime" => "A command the agent ran tried to connect to a host outside the project's network list (stopped at run time).",
        "env-file" => "An agent tried to use a .env file outside the project (or the project's .env.test directly).",
        _ => "",
    }
}

/// `(title, what happened, what the user can do, recovery)` for a stop reason; an unknown reason is its own title and offers a retry.
pub fn explain(reason: &str) -> (String, String, String, Vec<String>) {
    for c in KNOWN_CAUSES.iter() {
        if c.pattern.search(reason).is_some() {
            return (c.title.into(), c.happened.into(), c.fix.into(), c.recover.iter().map(|r| r.to_string()).collect());
        }
    }
    let title = clip_redacted_text(reason, TITLE_LIMIT);
    (
        if title.is_empty() { "The loop stopped with an error".into() } else { title },
        "The loop stopped for a reason the panel does not recognize; the full text is in the details.".into(),
        String::new(),
        vec![RECOVER_RETRY.into()],
    )
}

/// `(title, what the user can do, recovery)` for a stop reason; unknown reasons get the reason itself as title and no hint.
pub fn classify(reason: &str) -> (String, String, Vec<String>) {
    let (title, _, fix, recover) = explain(reason);
    (title, fix, recover)
}

fn to_iso(timestamp: Option<f64>) -> Value {
    match timestamp.filter(|t| *t != 0.0).and_then(|t| loop_io::local_from_epoch(t as i64)) {
        Some(moment) => json!(loop_io::iso_seconds(moment)),
        None => Value::Null,
    }
}

/// One item of the list. `turns` = every turn it happened in (sorted, unique; empty only when unknown) and always holds `turn`; `turn` is the
/// single turn the item points at (the latest of `turns` when not given). `turn_counts` = `{"<turn>": events}`.
struct ItemSpec {
    id: String,
    kind: &'static str,
    severity: &'static str,
    title: String,
    detail: String,
    hint: String,
    when: Value,
    turn: Option<i64>,
    recover: Vec<String>,
    count: i64,
    happened: String,
    turns: Vec<i64>,
    turn_counts: BTreeMap<i64, i64>,
    attempts: Vec<String>,
    rule: Option<String>,
    profile: Option<String>,
}

impl ItemSpec {
    fn new(id: String, kind: &'static str, severity: &'static str, title: String) -> Self {
        ItemSpec {
            id,
            kind,
            severity,
            title,
            detail: String::new(),
            hint: String::new(),
            when: Value::Null,
            turn: None,
            recover: vec![],
            count: 1,
            happened: String::new(),
            turns: vec![],
            turn_counts: BTreeMap::new(),
            attempts: vec![],
            rule: None,
            profile: None,
        }
    }

    fn build(self) -> Value {
        let mut numbers: BTreeSet<i64> = self.turns.iter().copied().collect();
        numbers.extend(self.turn);
        json!({
            "id": self.id, "kind": self.kind, "severity": self.severity, "title": clip_redacted_text(&self.title, TITLE_LIMIT),
            "happened": redact_credentials(&self.happened, Some(HAPPENED_LIMIT)).trim(),
            "detail": redact_credentials(&self.detail, Some(DETAIL_LIMIT)).trim(), "hint": self.hint, "when": self.when,
            "turn": self.turn.or_else(|| numbers.iter().next_back().copied()), "turns": numbers,
            "turn_counts": self.turn_counts.iter().map(|(n, c)| (n.to_string(), json!(c))).collect::<Map<String, Value>>(),
            "recover": self.recover, "count": self.count, "attempts": self.attempts, "rule": self.rule, "profile": self.profile,
        })
    }
}

/// `{turn: how many of the events}` for the events whose turn is known.
fn count_turns(events: &[&Map<String, Value>]) -> BTreeMap<i64, i64> {
    let mut counts = BTreeMap::new();
    for turn in events.iter().filter_map(|e| e.get("turn")).filter_map(Value::as_i64) {
        *counts.entry(turn).or_insert(0) += 1;
    }
    counts
}

fn py_str(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

pub struct ErrorCenterService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn error_center(&self) -> ErrorCenterService<'_> {
        ErrorCenterService { app: self }
    }
}

impl ErrorCenterService<'_> {
    pub fn build_errors(&self, context: &ProjectContext) -> Value {
        let work_dir = context.work_dir();
        let files = &self.app.loop_files;
        let summary = self.app.loop_status().get_loop_summary(context);
        let numbers = files.list_turn_numbers(&work_dir);
        let launches = parse_launch_log(&files.read_cached_text(&loop_state::loop_path(&work_dir, LAUNCH_LOG_NAME)));
        let timeline = self.build_timeline(&work_dir, &numbers, &launches, &summary);
        let events = self.app.error_event_sync().sync_events(&work_dir, &timeline, &launches);
        let mut items: Vec<Value> = Vec::new();
        items.extend(self.build_loop_items(&work_dir, &summary, &numbers));
        items.extend(self.build_turn_items(&work_dir, &numbers));
        items.extend(self.build_feature_items(&work_dir));
        items.extend(self.build_launch_items(&launches, &events));
        items.extend(Self::build_guard_items(&events));
        let rank = |severity: &str| match severity {
            SEVERITY_ERROR => 0,
            SEVERITY_WARNING => 1,
            _ => 2,
        };
        items.sort_by_key(|item| {
            let kind = item["kind"].as_str().unwrap_or("");
            (
                rank(item["severity"].as_str().unwrap_or("")),
                if kind == KIND_LOOP { "" } else { "z" },
                if kind == KIND_GUARD { 0 } else { -item["turn"].as_i64().unwrap_or(0) },
            )
        });
        let count = |severity: &str| items.iter().filter(|i| i["severity"] == severity).count();
        let (errors, warnings, infos) = (count(SEVERITY_ERROR), count(SEVERITY_WARNING), count(SEVERITY_INFO));
        json!({
            "items": items, "counts": {SEVERITY_ERROR: errors, SEVERITY_WARNING: warnings, SEVERITY_INFO: infos}, "problems": errors + warnings,
            "loop_status": summary["status"], "running": summary["running"] == true,
        })
    }

    // ------------------------------------------------ when did each turn run
    fn build_timeline(&self, work_dir: &str, numbers: &[u64], launches: &[super::loop_parsers::Launch], summary: &Value) -> TurnTimeline {
        let files = &self.app.loop_files;
        let (mut ends, mut durations): (BTreeMap<i64, f64>, BTreeMap<i64, f64>) = Default::default();
        for number in numbers {
            let path = files.get_turn_path(work_dir, *number);
            if let Some(modified) = files.get_mtime(&path) {
                ends.insert(*number as i64, modified);
                if let Some(seconds) = self.read_duration_seconds(work_dir, *number, &path, modified) {
                    durations.insert(*number as i64, seconds);
                }
            }
        }
        let running_turn = if summary["running"] == true { Some(numbers.last().map_or(0, |n| *n as i64) + 1) } else { None };
        TurnTimeline::build(&ends, &collect_turn_starts(launches), &durations, running_turn)
    }

    /// Wall time of the turn (`duration_ms` of the claude result), read once per file version; `None` when the log does not say.
    fn read_duration_seconds(&self, work_dir: &str, number: u64, path: &std::path::Path, modified: f64) -> Option<f64> {
        let mut cache = self.app.duration_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((stamp, seconds)) = cache.get(path) {
            if *stamp == modified {
                return *seconds;
            }
        }
        let milliseconds = self.app.loop_files.read_turn(work_dir, number).get("duration_ms").filter(|m| m.is_number()).and_then(Value::as_f64).filter(|m| *m > 0.0);
        let seconds = milliseconds.map(|m| m / MILLISECONDS_PER_SECOND);
        cache.insert(path.to_path_buf(), (modified, seconds));
        seconds
    }

    // ------------------------------------------------ items
    fn build_loop_items(&self, work_dir: &str, summary: &Value, numbers: &[u64]) -> Vec<Value> {
        if summary["status"] != STATUS_ERROR || summary["running"] == true {
            return vec![];
        }
        let reason = text_or_empty(summary.get("reason"));
        let (title, happened, fix, recover) = explain(&reason);
        let meta = self.app.loop_files.read_meta(work_dir);
        let when = if notify_truthy(meta.get("finished_at")) { meta["finished_at"].clone() } else { summary["json"].get("at").cloned().unwrap_or(Value::Null) };
        let turn = match STOP_TURN.search(&reason) {
            Some(found) => found[1].parse::<i64>().ok(),
            None => summary["json"]
                .get("turn")
                .filter(|t| notify_truthy(Some(t)))
                .and_then(|t| t.as_i64().or_else(|| t.as_f64().map(|f| f as i64)))
                .or_else(|| numbers.last().map(|n| *n as i64)),
        };
        let mut item = ItemSpec::new("loop-stop".into(), KIND_LOOP, SEVERITY_ERROR, title);
        (item.detail, item.hint, item.when, item.turn, item.recover, item.happened) = (reason, fix, when, turn, recover, happened);
        vec![item.build()]
    }

    fn build_turn_items(&self, work_dir: &str, numbers: &[u64]) -> Vec<Value> {
        let files = &self.app.loop_files;
        let mut items = Vec::new();
        for number in &numbers[numbers.len().saturating_sub(TURNS_SCANNED_LIMIT)..] {
            let turn = files.read_turn(work_dir, *number);
            let when = to_iso(files.get_mtime(&files.get_turn_path(work_dir, *number)));
            let stderr = text_or_empty(turn.get("stderr")).trim().to_string();
            let exit_code = turn.get("exit_code");
            let mut item = if notify_truthy(turn.get("timeout")) {
                let cause = &KNOWN_CAUSES[TIMEOUT_CAUSE];
                let mut item = ItemSpec::new(format!("turn-{number}-timeout"), KIND_TURN, SEVERITY_ERROR, format!("Turn {number} was terminated by the time limit"));
                item.detail = stderr;
                item.hint = cause.fix.into();
                item.happened = cause.happened.into();
                item
            } else if exit_code.is_some_and(|c| !c.is_null() && c.as_f64() != Some(0.0)) || notify_truthy(turn.get("is_error")) {
                let detail = if !stderr.is_empty() {
                    stderr
                } else {
                    ["result", "sample", "reason"].iter().map(|k| text_or_empty(turn.get(*k))).find(|t| !t.is_empty()).unwrap_or_default()
                };
                let code = py_str(exit_code);
                let mut item = ItemSpec::new(format!("turn-{number}-failed"), KIND_TURN, SEVERITY_ERROR, format!("Turn {number} failed (exit code {code})"));
                item.detail = detail;
                item.happened = format!("Turn {number} ended with a failure: the `claude -p` process exited with code {code}.");
                item
            } else if !stderr.is_empty() {
                let background = BACKGROUND_TASKS.search(&stderr).is_some();
                let title = if background { format!("Turn {number} ended with its verifier still running") } else { format!("Turn {number} wrote to stderr") };
                let mut item = ItemSpec::new(format!("turn-{number}-stderr"), KIND_TURN, SEVERITY_INFO, title);
                item.detail = stderr;
                item.hint = if background { BACKGROUND_TASKS_HINT.into() } else { String::new() };
                item.happened =
                    if background { BACKGROUND_TASKS_HAPPENED.into() } else { format!("Turn {number} finished normally but wrote to stderr; it did not fail.") };
                item
            } else {
                continue;
            };
            item.when = when;
            item.turn = Some(*number as i64);
            items.push(item.build());
        }
        items
    }

    fn build_feature_items(&self, work_dir: &str) -> Vec<Value> {
        let (features, _) = loop_state::parse_roadmap(&self.app.loop_files.read_roadmap(work_dir));
        features
            .iter()
            .filter(|(_, feature)| feature.get("status") == Some(BLOCKED_STATUS))
            .map(|(slug, feature)| {
                let mut item = ItemSpec::new(format!("feature-{slug}"), KIND_FEATURE, SEVERITY_ERROR, format!("Feature {slug} is blocked (quarantine)"));
                item.detail = format!("{} failures. {}", feature.get("tries").unwrap_or("?"), feature.get("note").unwrap_or(""));
                item.hint = "Answer in the panel: try again with guidance, or skip it.".into();
                item.turns = self.find_turns_blocked(work_dir, slug);
                item.happened = format!(
                    "The feature failed {} times (gate, execution or verification) and was put in quarantine. The loop carries on with what does not depend on it.",
                    feature.get("tries").unwrap_or("several")
                );
                item.build()
            })
            .collect()
    }

    /// `[turn the feature became blocked in]`: walks the snapshots (state after each turn) back while the feature stays blocked. Empty when no
    /// snapshot says.
    fn find_turns_blocked(&self, work_dir: &str, slug: &str) -> Vec<i64> {
        let files = &self.app.loop_files;
        let numbers = files.list_snapshot_numbers(work_dir);
        let mut turn = None;
        for number in numbers[numbers.len().saturating_sub(SNAPSHOTS_SCANNED_LIMIT)..].iter().rev() {
            let snapshot = files.load_snapshot(work_dir, *number);
            let (features, _) = loop_state::parse_roadmap(&files.read_snapshot_roadmap(snapshot.as_ref()));
            if features.get(slug).and_then(|f| f.get("status")) != Some(BLOCKED_STATUS) {
                break;
            }
            turn = Some(*number as i64);
        }
        turn.into_iter().collect()
    }

    /// Problem lines of the LAST launch (the log accumulates across launches; with the log gone, the last launch the events remember).
    fn build_launch_items(&self, launches: &[super::loop_parsers::Launch], events: &[Map<String, Value>]) -> Vec<Value> {
        let launch_events: Vec<&Map<String, Value>> = events.iter().filter(|e| e.get("kind").and_then(Value::as_str) == Some(KIND_LAUNCH_EVENT)).collect();
        let latest: Option<Value> = match launches.last() {
            Some(launch) => Some(json!(launch.marker)),
            None => launch_events.last().and_then(|e| e.get("launch")).cloned(),
        };
        let problems: Vec<&Map<String, Value>> = launch_events.iter().filter(|e| latest.as_ref().is_some_and(|l| e.get("launch") == Some(l))).copied().collect();
        if problems.is_empty() {
            return vec![];
        }
        let lines = |e: &&Map<String, Value>| text_or_empty(e.get("line"));
        let fatal = problems.iter().any(|e| LAUNCH_FATAL_PATTERN.search(&lines(e)).is_some());
        let shown: Vec<String> = problems[problems.len().saturating_sub(LAUNCH_LINES_LIMIT)..].iter().map(lines).collect();
        let mut item =
            ItemSpec::new("launch-last".into(), KIND_LAUNCH, if fatal { SEVERITY_WARNING } else { SEVERITY_INFO }, "The last launch of the loop printed problems".into());
        item.detail = shown.join("\n");
        item.hint = "If the loop is running fine this is only noise.".into();
        item.when = problems.last().and_then(|e| e.get("t")).cloned().unwrap_or(Value::Null);
        item.turns = problems.iter().filter_map(|e| e.get("turn")).filter_map(Value::as_i64).collect();
        item.turn_counts = count_turns(&problems);
        item.happened = format!("The last launch of the loop printed {} problem line(s) in .specs/project/loop/internal/monitor-relaunch.log.", problems.len());
        vec![item.build()]
    }

    /// Every stored guard block, grouped by profile and rule (a block means the guard worked, so these are `info`, never a problem).
    fn build_guard_items(events: &[Map<String, Value>]) -> Vec<Value> {
        let mut grouped: Vec<((String, String), Vec<&Map<String, Value>>)> = Vec::new();
        for event in events.iter().filter(|e| e.get("kind").and_then(Value::as_str) == Some(KIND_GUARD_EVENT)) {
            let profile = Some(text_or_empty(event.get("profile"))).filter(|p| !p.is_empty()).unwrap_or_else(|| "?".into());
            let key = (profile, text_or_empty(event.get("rule")));
            match grouped.iter_mut().find(|(k, _)| *k == key) {
                Some((_, blocks)) => blocks.push(event),
                None => grouped.push((key, vec![event])),
            }
        }
        grouped.sort_by_key(|(_, blocks)| std::cmp::Reverse(blocks.len()));
        grouped
            .into_iter()
            .map(|((profile, rule), blocks)| {
                let mut attempts: Vec<String> = Vec::new();
                for attempt in blocks.iter().map(|b| text_or_empty(b.get("attempt"))).filter(|a| !a.is_empty()) {
                    if !attempts.contains(&attempt) {
                        attempts.push(attempt);
                    }
                }
                attempts.truncate(GUARD_ATTEMPTS_LIMIT);
                let counts = count_turns(&blocks);
                let last = blocks.last().unwrap();
                let mut item =
                    ItemSpec::new(format!("guard-{profile}-{rule}"), KIND_GUARD, SEVERITY_INFO, format!("Guard blocked {} x {rule} ({profile})", blocks.len()));
                item.detail = text_or_empty(last.get("detail"));
                item.when = last.get("t").cloned().unwrap_or(Value::Null);
                item.turns = counts.keys().copied().collect();
                item.turn_counts = counts;
                item.count = blocks.len() as i64;
                item.attempts = attempts;
                item.happened = format!("{} {GUARD_NOTE}", guard_rule_reason(&rule));
                item.rule = Some(rule);
                item.profile = Some(profile);
                item.build()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_and_unknown_reasons_are_explained() {
        let (title, _, _, recover) = explain("dirty tree outside the paths");
        assert_eq!((title.as_str(), recover), ("Files left uncommitted in the worktree", vec!["commit_leftovers".to_string(), "discard_leftovers".to_string()]));
        let (title, _, fix, recover) = explain("something odd");
        assert_eq!((title.as_str(), fix.as_str(), recover), ("something odd", "", vec!["retry".to_string()]));
        assert_eq!(explain("").0, "The loop stopped with an error");
    }

    #[test]
    fn errors_collect_turns_features_and_guard_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        std::fs::write(repo.join(".specs/project/ROADMAP.md"), "- [a] status=blocked tries=2 note=flaky\n").unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let w = context.work_dir();
        let write_turn = |n: u64, record: Value| {
            let path = app.loop_files.get_turn_path(&w, n);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, record.to_string()).unwrap();
        };
        write_turn(1, json!({"timeout": true, "stderr": "slow"}));
        write_turn(2, json!({"exit_code": 1, "result": "boom"}));
        write_turn(3, json!({"stderr": "Background tasks still running"}));
        write_turn(4, json!({"exit_code": 0}));
        let guard = crate::repositories::loop_command_file::get_loop_internal_dir(&w).join("guard.log");
        std::fs::create_dir_all(guard.parent().unwrap()).unwrap();
        std::fs::write(
            &guard,
            json!({"t": "2026-01-01T10:00:00", "profile": "loop", "rule": "git-write", "decision": "blocked", "detail": "git commit", "turn": 2}).to_string() + "\n",
        )
        .unwrap();
        let body = app.error_center().build_errors(&context);
        let ids: Vec<&str> = body["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["turn-2-failed", "turn-1-timeout", "feature-a", "turn-3-stderr", "guard-loop-git-write"]);
        assert_eq!((body["counts"].clone(), body["problems"].clone()), (json!({"error": 3, "warning": 0, "info": 2}), json!(3)));
        assert_eq!(body["items"][0]["title"], "Turn 2 failed (exit code 1)");
        assert_eq!(body["items"][4]["turn_counts"], json!({"2": 1}));
        assert_eq!(body["items"][2]["detail"], "2 failures. flaky");
    }
}
