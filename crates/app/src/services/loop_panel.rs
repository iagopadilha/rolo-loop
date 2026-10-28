// SPDX-License-Identifier: MIT
//! Builds a project's panel `/state` and `/graph` (Mermaid + text from the engine), and the read-only codebase maps.

use super::claude_accounts::{build_turn_claude, build_usage_totals};
use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use crate::repositories::loop_read_formatting::{now_iso, number_value, to_number, to_text_list};
use crate::repositories::specs_file::compute_bytes_version;
use engine_state::{e2e_evidence, graph_view, loop_state, notify_truthy, roadmap_settings, subprojects};
use pycompat::{last_segment, nt_normpath, partition, Re};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;

pub const QUARANTINE_OPTIONS: [&str; 2] = ["try again", "skip and continue"];
const KNOWN_ACTIONS: [&str; 4] = ["deliver", "summary", "push", "clean_worktrees"];
const RECENT_ACTIONS_LIMIT: usize = 10;
const PERMISSIONS_LOG_LIMIT: usize = 50;
const LOOP_WORKTREE_SUFFIX: &str = "-worktree-loop";
const SECONDS_PER_MINUTE: f64 = 60.0;
const COST_UNIT: &str = "USD";
const MINUTES_UNIT: &str = "min";
const USER_ANSWER_LABEL: &str = "User answer:";
const AUTOMATIC_ANSWER_MARK: &str = "automatic answer";
const ANSWER_TEXT_LIMIT: usize = 600;
const ANSWERS_LIMIT: usize = 50;
const VALIDATE_LINE_PREFIX: &str = "- ";
const CODEBASE_MAP_FOLDER: [&str; 2] = ["docs", "codebase"];
const ROOT_GROUP_FOLDER: &str = ".";
const ROOT_GROUP_LABEL: &str = "Project";
const BUILDING_STATUS: &str = "building";

static ANSWER_HEADER: LazyLock<Re> = LazyLock::new(|| Re::new(r"^## Answer (\S+)(?:\s+\[([^\]]*)\])?\s*\z"));
static QUESTION_PREFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*Question:\s*"));
static DRIVE_PREFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z]:"));

/// Caps that run_loop received (LOOP-CMD.json); a missing flag = the run_loop default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommandLimits {
    pub max_cost: f64,
    pub max_stall: i64,
    pub timeout: i64,
}

pub const DEFAULT_LIMITS: CommandLimits = CommandLimits { max_cost: 50.0, max_stall: 10, timeout: 60 };

pub fn read_command_limits(loop_command: &Map<String, Value>) -> CommandLimits {
    let mut limits = DEFAULT_LIMITS;
    let Some(Value::Array(args)) = loop_command.get("argv") else { return limits };
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => pycompat::json::py_json_dumps(other, false),
    };
    for (index, item) in args.iter().enumerate() {
        let item = text(item);
        let (name, _, joined) = partition(&item, "=");
        let raw = if !joined.is_empty() { joined.to_string() } else { args.get(index + 1).map(text).unwrap_or_default() };
        let raw = raw.trim();
        match name {
            "--max-cost" => {
                if let Ok(v) = raw.parse::<f64>() {
                    limits.max_cost = v;
                }
            }
            "--max-stall" => {
                if let Ok(v) = raw.parse::<i64>() {
                    limits.max_stall = v;
                }
            }
            "--timeout" => {
                if let Ok(v) = raw.parse::<i64>() {
                    limits.timeout = v;
                }
            }
            _ => {}
        }
    }
    limits
}

/// A whole `f64` as an integer JSON number, else as a float.
fn num_json(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9e15 {
        json!(value as i64)
    } else {
        json!(value)
    }
}

fn truthy_number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| *n != 0.0)
}

/// The cost cap in force: `meta.max_cost_usd` (raised in the panel) wins, else the loop command's --max-cost, else the default.
pub fn effective_cost_cap(meta: &Value, command_limits: &CommandLimits) -> Value {
    match number_value(meta.get("max_cost_usd")) {
        raised if truthy_number(&raised).is_some() => raised,
        _ => json!(command_limits.max_cost),
    }
}

fn build_limit(key: &str, label: &str, value: Value, maximum: Value, unit: &str) -> Value {
    let (v, m) = (value.as_f64().unwrap_or(0.0), maximum.as_f64().unwrap_or(0.0));
    let percent = if m != 0.0 { json!((v / m * 100.0 * 10.0).round_ties_even() / 10.0) } else { Value::Null };
    json!({"key": key, "label": label, "value": value, "max": if m != 0.0 { maximum } else { Value::Null }, "unit": unit, "pct": percent})
}

/// Running = the next one after the last one done; stopped = the one in LOOP.json, else the last one done (or `None`).
pub fn compute_current_turn(loop_json: &Map<String, Value>, meta: &Value, numbers: &[u64], running: bool) -> Option<Value> {
    let loop_turn = to_number(loop_json.get("turn").unwrap_or(&Value::Null));
    let meta_turns = to_number(meta.get("turns").unwrap_or(&Value::Null)).unwrap_or(0.0);
    let turns_done = [meta_turns, loop_turn.unwrap_or(0.0), numbers.last().map_or(f64::MIN, |n| *n as f64)].into_iter().fold(f64::MIN, f64::max);
    if running {
        return Some(num_json(turns_done + 1.0));
    }
    if let Some(turn) = loop_turn {
        return Some(num_json(turn));
    }
    if turns_done != 0.0 {
        Some(num_json(turns_done))
    } else {
        None
    }
}

/// Name of the repo folder (from meta.json when the engine wrote it), without the worktree suffix.
pub fn get_project_name(meta: &Value, context: &ProjectContext) -> String {
    let source = match meta.get("repo") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => context.repo_path().to_string(),
    };
    let name = last_segment(&nt_normpath(&source)).to_string();
    name.strip_suffix(LOOP_WORKTREE_SUFFIX).map(String::from).unwrap_or(name)
}

/// `## Answer <date> [marker]` blocks of ANSWERS.md (the last ANSWERS_LIMIT).
pub fn parse_answers(text: &str) -> Vec<Value> {
    struct Block {
        date: String,
        marker: String,
        lines: Vec<String>,
    }
    let mut blocks: Vec<Block> = Vec::new();
    for line in pycompat::splitlines(text) {
        if let Some(header) = ANSWER_HEADER.match_start(line.trim()) {
            blocks.push(Block { date: header[1].to_string(), marker: header.group(2).unwrap_or("").trim().to_lowercase(), lines: vec![] });
        } else if let Some(current) = blocks.last_mut() {
            current.lines.push(line.to_string());
        }
    }
    let answers: Vec<Value> = blocks
        .iter()
        .map(|block| {
            let body = block.lines.join("\n").trim().to_string();
            let (question, _, answer) = partition(&body, USER_ANSWER_LABEL);
            let question = QUESTION_PREFIX.replace_lit(question, "").trim().to_string();
            let state = if format!("{} {}", block.marker, answer).to_lowercase().contains(AUTOMATIC_ANSWER_MARK) {
                "automatic"
            } else if block.marker == "pending" || block.marker == "applied" {
                block.marker.as_str()
            } else {
                "pending"
            };
            json!({
                "date": block.date, "question": question.chars().take(ANSWER_TEXT_LIMIT).collect::<String>(),
                "answer": answer.trim().chars().take(ANSWER_TEXT_LIMIT).collect::<String>(), "state": state,
            })
        })
        .collect();
    answers[answers.len().saturating_sub(ANSWERS_LIMIT)..].to_vec()
}

/// "What is running now" at a glance (G1): the turn, each lane (feature, its name, its stage and step), the process that runs them and since
/// when. `{"running": false}` when nothing the engine started is alive. Built only from the engine's process registry and the roadmap view.
fn build_now(processes: &[Value], stages: &[Value], features: &[Value], turn: Option<u64>) -> Value {
    let Some(process) = processes.first() else { return json!({"running": false, "turn": turn, "lanes": [], "label": null, "started_at": null}) };
    let lanes: Vec<Value> = process["lanes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|lane| {
            let slug = lane["feature"].as_str().unwrap_or("");
            let feature = features.iter().find(|f| f["id"] == slug);
            let stage_n = feature.map(|f| f["stage"].clone()).unwrap_or(Value::Null);
            let stage_title = stages.iter().find(|s| s["n"] == stage_n && !stage_n.is_null()).map(|s| s["title"].clone()).unwrap_or(Value::Null);
            json!({"feature": slug, "name": feature.map(|f| f["name"].clone()).unwrap_or(Value::Null), "step": lane["step"], "stage": stage_n, "stage_title": stage_title})
        })
        .collect();
    json!({
        "running": true, "turn": process["turn"], "lanes": lanes, "label": process["label"], "started_at": process["started_at"], "pid": process["pid"],
        "claude": process["claude"], "claude_account": process["claude_account"],
    })
}

fn epoch_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub struct LoopPanelService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn loop_panel(&self) -> LoopPanelService<'_> {
        LoopPanelService { app: self }
    }
}

impl LoopPanelService<'_> {
    pub fn build_state(&self, context: &ProjectContext, project_url: Option<&str>) -> Value {
        let work_dir = context.work_dir();
        let files = &self.app.loop_files;
        let summary = self.app.loop_status().get_loop_summary(context);
        let meta = summary["meta"].clone();
        let running = summary["running"] == true;
        let numbers = files.list_turn_numbers(&work_dir);
        let roadmap_text = files.read_roadmap(&work_dir);
        let (stages, mut features, counts, graph) = self.app.roadmap_view().build_roadmap_view(&work_dir, &roadmap_text, None);
        let loop_json = summary["json"].as_object().cloned().unwrap_or_default();
        let turn = compute_current_turn(&loop_json, &meta, &numbers, running);
        let guard = self.app.guard_events().build_guard_summary(&work_dir);
        self.attach_tasks_to_features(&work_dir, &mut features);
        let running_turn = if running { turn.as_ref().and_then(Value::as_u64) } else { None };
        let turn_rows = self.app.turns().list_turns(&work_dir, running_turn);
        let calls = self.list_panel_calls(context);
        let agent_claude = build_turn_claude(&meta); // every agent runs inside the turn's process: same command and sign-in
        let agents: Vec<Value> = self
            .app
            .agent_history()
            .build_state_agents(&work_dir, &summary, &features, &numbers)
            .into_iter()
            .map(|agent| {
                let mut map = agent.as_object().cloned().unwrap_or_default();
                map.extend(agent_claude.as_object().cloned().unwrap_or_default());
                Value::Object(map)
            })
            .collect();
        let loop_settings = roadmap_settings::parse_loop_settings(&roadmap_text).to_json();
        let command_limits = read_command_limits(&files.read_loop_command(&work_dir));
        let g = |key: &str| meta.get(key).cloned().unwrap_or(Value::Null);
        let mut meta_block = Map::new();
        for (key, value) in [
            ("turns", g("turns")),
            ("cost_usd", g("cost_usd")),
            ("max_cost_usd", effective_cost_cap(&meta, &command_limits)),
            ("branch", g("branch")),
            ("pr", g("pr")),
            ("pr_status", g("pr_status")),
            ("claude", g("claude")),
            ("claude_account", g("claude_account")),
            ("claude_switches", g("claude_switches")),
            ("claude_events", if g("claude_events").is_array() { g("claude_events") } else { json!([]) }),
            ("claude_account_info", self.app.accounts.build_current_account(&meta)),
            ("claude_totals", build_usage_totals(&turn_rows, &calls)),
            ("claude_usage_view", json!(self.app.accounts.build_usage_view(&meta, &turn_rows, &calls, false))),
        ] {
            meta_block.insert(key.into(), value);
        }
        meta_block.extend(build_turn_claude(&meta).as_object().cloned().unwrap_or_default());
        meta_block.insert("max_parallel_tasks".into(), loop_settings["settings"]["max_parallel_tasks"].clone());
        meta_block
            .insert("max_parallel_tasks_in_header".into(), json!(!loop_settings["missing"].as_array().is_some_and(|m| m.iter().any(|k| k == "max_parallel_tasks"))));
        let validate: Vec<String> = pycompat::splitlines(&files.read_state_file(&work_dir, "VALIDATE.md"))
            .into_iter()
            .map(str::trim)
            .filter(|l| l.starts_with(VALIDATE_LINE_PREFIX))
            .map(String::from)
            .collect();
        let quarantine = Self::build_quarantine(&features);
        let shutdown = self.app.activity.get().and_then(|m| m.seconds_until_shutdown());
        let processes = engine_state::process_registry::list(&work_dir, running);
        let now_block = build_now(&processes, &stages, &features, running_turn);
        json!({
            "processes": processes, "now": now_block,
            "permissions_log": engine_state::access_grant::journal_view(&work_dir, PERMISSIONS_LOG_LIMIT),
            "now": now_iso(), "project": get_project_name(&meta, context), "repo": context.repo_path(), "work": work_dir, "branch": g("branch"),
            "project_ref": {"id": context.project.id, "name": context.project.name, "repo_path": context.project.repo_path},
            "loop": self.build_loop_block(&work_dir, &summary, turn.clone()),
            "meta": meta_block,
            "limits": self.build_limits(&work_dir, &meta, running, &numbers), "stages": stages, "features": features, "counts": counts,
            "graph": graph, "agents": agents, "validate": validate,
            "validations": e2e_evidence::validations(&work_dir, &Path::new(&work_dir).join(".specs").join("project").display().to_string()),
            "manual_checks": e2e_evidence::manual_checks(&work_dir, &Path::new(&work_dir).join(".specs").join("project").display().to_string()),
            "validation_report": engine_state::validation_report::build(&work_dir, &Path::new(&work_dir).join(".specs").join("project").display().to_string()),
            "decisions_summary": self.app.decisions().open_counts(context),
            "recent_actions": self.list_recent_actions(&work_dir),
            "roadmap_approval": files.read_roadmap_approval(&work_dir, Some(context.repo_path())),
            "planning_required": self.app.planning_requirement().is_planning_required(context),
            "answers": parse_answers(&files.read_state_file(&work_dir, "ANSWERS.md")),
            "turns": turn_rows, "quarantine": quarantine,
            "guard_events": guard["events"], "guard_counts": guard["counts"],
            "monitor": {"url": project_url, "idle_in_s": null, "shutdown_in_s": shutdown},
        })
    }

    /// Every kept panel call (chat, map, roadmap...), newest first.
    pub fn list_panel_calls(&self, context: &ProjectContext) -> Vec<Value> {
        self.app.call_log.list_all_calls(&[context.work_dir(), context.repo_path().to_string()])
    }

    /// Adds `tasks` to each feature that has a tasks.md and to each feature being built (empty list without tasks.md); others get no key.
    fn attach_tasks_to_features(&self, work_dir: &str, features: &mut [Value]) {
        for feature in features.iter_mut() {
            let id = feature["id"].as_str().unwrap_or("").to_string();
            let block = crate::repositories::small_stores::read_feature_tasks(work_dir, &id);
            if block["tasks"].as_array().is_some_and(|t| !t.is_empty()) || feature["status"] == BUILDING_STATUS {
                feature["tasks"] = block;
            }
        }
    }

    fn build_loop_block(&self, work_dir: &str, summary: &Value, turn: Option<Value>) -> Value {
        let loop_json = summary["json"].as_object().cloned().unwrap_or_default();
        let running = summary["running"] == true;
        let texts = |key: &str| match loop_json.get(key) {
            Some(list @ Value::Array(_)) => to_text_list(list),
            _ => vec![],
        };
        let or_null = |v: Option<&Value>| match v {
            Some(v) if notify_truthy(Some(v)) => v.clone(),
            _ => Value::Null,
        };
        json!({
            "status": summary["status"], "running": running, "pid": summary["pid"], "turn": turn,
            "feature": to_text_list(loop_json.get("feature").unwrap_or(&Value::Null)),
            "reason": if running { Value::Null } else { or_null(summary.get("reason")) },
            "next": if running { Value::Null } else { or_null(loop_json.get("next")) },
            "options": if running { vec![] } else { texts("options") }, "to_check": texts("to_check"),
            "pause_kind": if running { Value::Null } else { or_null(loop_json.get("pause_kind")) },
            "credential_request": if running { Value::Null } else { or_null(loop_json.get("credential_request")) },
            "at": loop_json.get("at").cloned().unwrap_or(Value::Null),
            "pause_requested": self.app.loop_files.is_pause_requested(work_dir),
        })
    }

    fn build_limits(&self, work_dir: &str, meta: &Value, running: bool, numbers: &[u64]) -> Vec<Value> {
        let files = &self.app.loop_files;
        let limits = read_command_limits(&files.read_loop_command(work_dir));
        let cost = match number_value(meta.get("cost_usd")) {
            c if truthy_number(&c).is_some() => c,
            _ => json!(0),
        };
        let mut result = vec![
            build_limit("cost", "cost", cost, effective_cost_cap(meta, &limits), COST_UNIT),
            build_limit("turns_without_progress", "without progress", json!(self.app.turns().count_turns_without_progress(work_dir)), json!(limits.max_stall), ""),
        ];
        if running {
            let mut paths = vec![files.get_lock_path(work_dir)];
            paths.extend(numbers.last().map(|n| files.get_turn_path(work_dir, *n)));
            let start = paths.iter().filter_map(|p| files.get_mtime(p)).fold(None, |best: Option<f64>, m| Some(best.map_or(m, |b| b.max(m)))).unwrap_or_else(epoch_now);
            let minutes = ((epoch_now() - start) / SECONDS_PER_MINUTE * 10.0).round_ties_even() / 10.0;
            result.push(build_limit("turn_minutes", "current turn", json!(minutes), json!(limits.timeout), MINUTES_UNIT));
        }
        result
    }

    pub fn build_quarantine(features: &[Value]) -> Vec<Value> {
        features
            .iter()
            .filter(|f| f["status"] == "blocked")
            .map(|feature| {
                let tries = feature["tries"].as_i64().unwrap_or(0);
                let reason = match feature["note"].as_str().filter(|n| !n.is_empty()) {
                    Some(note) => format!("{note} ({tries} failures)"),
                    None => format!("{tries} failures"),
                };
                let stuck: Vec<&Value> =
                    features.iter().filter(|o| o["deps"].as_array().is_some_and(|d| d.contains(&feature["id"])) && o["status"] != "verified").map(|o| &o["id"]).collect();
                json!({"id": feature["id"], "reason": reason, "stuck": stuck, "options": QUARANTINE_OPTIONS})
            })
            .collect()
    }

    /// Latest actions of the Act mode (`internal/actions.log`), newest first.
    pub fn list_recent_actions(&self, work_dir: &str) -> Vec<Value> {
        let rows: Vec<Value> = self
            .app
            .loop_files
            .read_action_lines(work_dir)
            .iter()
            .filter(|item| item.get("action").and_then(Value::as_str).is_some_and(|a| KNOWN_ACTIONS.contains(&a)))
            .map(|item| json!({"action": item["action"], "t": item.get("t"), "ok": notify_truthy(item.get("ok")), "summary": item.get("summary")}))
            .collect();
        rows[rows.len().saturating_sub(RECENT_ACTIONS_LIMIT)..].iter().rev().cloned().collect()
    }

    /// Dependency graph (Mermaid), state graph (Mermaid) and ASCII text, from the engine.
    pub fn build_graph(&self, context: &ProjectContext) -> Value {
        let roadmap_text = self.app.loop_files.read_roadmap(&context.work_dir());
        let (features, _) = loop_state::parse_roadmap(&roadmap_text);
        json!({"dependencies_mermaid": graph_view::mermaid(&features), "states_mermaid": loop_state::graph(&roadmap_text), "text": graph_view::ascii_graph(&features), "dependency_proposals": engine_state::audit_deps::proposals(&context.work_dir())})
    }

    // ------------------------------------------------ codebase maps (read only)
    /// Files of one `docs/codebase` folder (relative to the work folder, "/" separators), sorted; empty when the folder is missing.
    pub fn list_map_files(work_dir: &str, folder: &str) -> Vec<Value> {
        let mut map_folder = Path::new(work_dir).to_path_buf();
        if folder != ROOT_GROUP_FOLDER {
            map_folder.extend(folder.split('/'));
        }
        map_folder.extend(CODEBASE_MAP_FOLDER);
        let mut files = Vec::new();
        Self::walk_map(Path::new(work_dir), &map_folder, &mut files);
        files
    }

    fn walk_map(work_dir: &Path, current: &Path, files: &mut Vec<Value>) {
        let Ok(entries) = std::fs::read_dir(current) else { return };
        let (mut folders, mut names): (Vec<String>, Vec<String>) = (vec![], vec![]);
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                folders.push(name)
            } else {
                names.push(name)
            }
        }
        names.sort();
        folders.sort();
        for name in names {
            let lower = name.to_lowercase();
            if lower.ends_with(".md") || lower.ends_with(".json") {
                let relative = current.join(&name).strip_prefix(work_dir).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                files.push(json!({"path": relative, "name": name}));
            }
        }
        for folder in folders {
            Self::walk_map(work_dir, &current.join(folder), files);
        }
    }

    /// `GET codebase/tree`: `{groups:[{folder, label, files:[{path, name}]}]}` for the root map and the map of each non-skipped subproject of the
    /// stored list (monorepo). Groups without files are left out; nothing is written.
    pub fn build_codebase_tree(&self, context: &ProjectContext) -> Value {
        let work_dir = context.work_dir();
        let (entry, _) = subprojects::load(&work_dir, None);
        let mut candidates: Vec<(String, String)> = vec![(ROOT_GROUP_FOLDER.into(), ROOT_GROUP_LABEL.into())];
        for project in subprojects::entry_projects(entry.as_ref()).iter().filter(|p| !notify_truthy(p.get("skip"))) {
            let path = project.get("path").and_then(Value::as_str).unwrap_or("");
            let label = match project.get("name") {
                Some(Value::String(n)) if !n.is_empty() => n.clone(),
                _ => path.to_string(),
            };
            candidates.push((path.replace('\\', "/").trim_matches('/').to_string(), label));
        }
        let groups: Vec<Value> = candidates
            .into_iter()
            .filter_map(|(folder, label)| {
                let files = Self::list_map_files(&work_dir, &folder);
                if files.is_empty() {
                    None
                } else {
                    Some(json!({"folder": folder, "label": label, "files": files}))
                }
            })
            .collect();
        json!({"groups": groups})
    }

    /// `GET codebase/file?path=`: read-only content of ONE file listed by `build_codebase_tree` (same shape as `specs/file`, never editable). A
    /// path with `..`, a drive or a leading slash = `InvalidPath` (400); a path the tree does not list = `SpecFileNotFound` (404).
    pub fn read_codebase_file(&self, context: &ProjectContext, path: &str) -> Result<Value, AppError> {
        let text = path.replace('\\', "/");
        if text.trim().is_empty() || text.starts_with('/') || DRIVE_PREFIX.match_start(&text).is_some() || text.split('/').any(|p| p == "..") {
            return Err(AppError::invalid_path("invalid path"));
        }
        let not_found = || AppError::new(Kind::SpecFileNotFound, format!("file not found in the codebase maps: {text}"));
        let tree = self.build_codebase_tree(context);
        let allowed: BTreeSet<&str> =
            tree["groups"].as_array().into_iter().flatten().flat_map(|g| g["files"].as_array().into_iter().flatten()).filter_map(|f| f["path"].as_str()).collect();
        if !allowed.contains(text.as_str()) {
            return Err(not_found());
        }
        let content = std::fs::read(text.split('/').fold(Path::new(&context.work_dir()).to_path_buf(), |p, part| p.join(part))).map_err(|_| not_found())?;
        Ok(json!({
            "path": text, "text": String::from_utf8_lossy(&content), "version": compute_bytes_version(&content), "editable": false,
            "not_editable_reason": "read-only view",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_strip_names_lanes_stage_and_process() {
        assert_eq!(build_now(&[], &[], &[], Some(3))["running"], false);
        let process = json!({"turn": 3, "pid": 99, "started_at": "2026-01-01T10:00:00", "label": "Loop turn 3 \u{b7} feature a", "claude": "claude", "claude_account": "",
                             "lanes": [{"feature": "a", "step": "execute"}]});
        let stages = vec![json!({"n": 1, "title": "Stage 1", "features": ["a"]})];
        let features = vec![json!({"id": "a", "name": "Alpha", "stage": 1, "status": "building"})];
        let now = build_now(&[process], &stages, &features, Some(3));
        assert_eq!((now["running"].clone(), now["turn"].clone(), now["pid"].clone()), (json!(true), json!(3), json!(99)));
        assert_eq!(
            (now["lanes"][0]["name"].as_str(), now["lanes"][0]["step"].as_str(), now["lanes"][0]["stage_title"].as_str()),
            (Some("Alpha"), Some("execute"), Some("Stage 1"))
        );
    }

    #[test]
    fn command_limits_cost_cap_and_answers() {
        let command = json!({"argv": ["run_loop", "--max-cost=12.5", "--max-stall", "4", "--timeout", "bad"]});
        let limits = read_command_limits(command.as_object().unwrap());
        assert_eq!(limits, CommandLimits { max_cost: 12.5, max_stall: 4, timeout: 60 });
        assert_eq!(effective_cost_cap(&json!({"max_cost_usd": 30}), &limits), json!(30));
        assert_eq!(effective_cost_cap(&json!({}), &limits), json!(12.5));
        let answers = parse_answers(
            "## Answer 2026-01-01 [applied]\nQuestion: Which db?\nUser answer: pg\n## Answer 2026-01-02\nQuestion: x\nUser answer: automatic answer used\n",
        );
        assert_eq!((answers[0]["state"].clone(), answers[0]["question"].clone(), answers[0]["answer"].clone()), (json!("applied"), json!("Which db?"), json!("pg")));
        assert_eq!(answers[1]["state"], "automatic");
        assert_eq!(build_limit("cost", "cost", json!(5), json!(50.0), "USD")["pct"], 10.0);
        assert_eq!(build_limit("x", "x", json!(5), json!(0), "")["max"], Value::Null);
    }

    #[test]
    fn current_turn_rules() {
        let loop_json = json!({"turn": 4}).as_object().cloned().unwrap();
        assert_eq!(compute_current_turn(&loop_json, &json!({"turns": 3}), &[1, 2, 3], true), Some(json!(5)));
        assert_eq!(compute_current_turn(&loop_json, &json!({}), &[], false), Some(json!(4)));
        assert_eq!(compute_current_turn(&Map::new(), &json!({}), &[], false), None);
    }

    #[test]
    fn state_graph_and_codebase_maps_for_a_fresh_project() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        std::fs::create_dir_all(repo.join("docs/codebase")).unwrap();
        std::fs::write(repo.join("docs/codebase/ARCH.md"), "# arch").unwrap();
        std::fs::write(repo.join(".specs/project/ROADMAP.md"), "## Stage 1\n- [a] status=pending\n- [b] status=blocked deps=a note=x tries=2\n").unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        let panel = app.loop_panel();
        let state = panel.build_state(&context, Some("http://localhost:1"));
        assert_eq!((state["counts"]["total"].clone(), state["loop"]["running"].clone(), state["project"].clone()), (json!(2), json!(false), json!("p")));
        assert_eq!(state["quarantine"][0]["reason"], "x (2 failures)");
        assert_eq!(state["limits"].as_array().unwrap().len(), 2);
        assert_eq!(state["monitor"]["shutdown_in_s"], Value::Null);
        assert!(panel.build_graph(&context)["dependencies_mermaid"].is_string());
        let tree = panel.build_codebase_tree(&context);
        assert_eq!(tree["groups"][0]["files"][0]["path"], "docs/codebase/ARCH.md");
        assert_eq!(panel.read_codebase_file(&context, "docs/codebase/ARCH.md").unwrap()["text"], "# arch");
        assert_eq!(panel.read_codebase_file(&context, "../x").unwrap_err().kind, Kind::InvalidPath);
        assert_eq!(panel.read_codebase_file(&context, "docs/codebase/NOPE.md").unwrap_err().kind, Kind::SpecFileNotFound);
    }
}
