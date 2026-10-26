// SPDX-License-Identifier: MIT
//! Task waves of ONE feature: parses `tasks.md`, builds the waves and keeps the task status.
//!
//! `harness tasks waves <slug> [--max N] [--root DIR]` · `set <slug> <Tn> <pending|running|done|failed> [--note TEXT]` ·
//! `mirror <slug>` (copies the status into the `**Status**:` lines of tasks.md). Root = the folder that holds `.specs`.
//! Status file: `.specs/project/loop/internal/tasks/<slug>.json`.

use crate::loop_state::{feature_dir, feature_folder_name, internal_dir, parse_roadmap, read};
use crate::roadmap_settings::parse_loop_settings;
use pycompat::json::{py_json_dumps, py_json_dumps_indent};
use pycompat::{nt_abspath, nt_dirname, splitlines_keepends, Re};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const STATUS_VALUES: [&str; 4] = ["pending", "running", "done", "failed"];
const TASK_TYPES: [&str; 2] = ["impl", "wiring"];
const DEFAULT_TYPE: &str = "impl";
pub const TASKS_FILE_NAME: &str = "tasks.md";
const SYNTHETIC_TASK_ID: &str = "T1";

static TASK_HEADING: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^###\s+(T\d+)\s*:\s*(.*?)\s*\z"));
static PARALLEL_MARK: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\s*\[P\]\s*\z"));
static FIELD_LINE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^\*\*(Type|Files|Deps|Reqs|Done when|Status)(?:\*\*\s*:|\s*:\*\*)\s*(.*?)\s*\z"));
static STATUS_FIELD_LINE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^(\*\*Status(?:\*\*\s*:|\s*:\*\*)\s*)(\S*)(.*)\z"));
static HTML_COMMENT: LazyLock<Re> = LazyLock::new(|| Re::new(r"<!--.*?-->"));

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub parallel: bool,
    pub kind: String,
    pub files: Vec<String>,
    pub deps: Vec<String>,
    pub reqs: Vec<String>,
    pub done_when: String,
    pub status: String,
}

impl Task {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "title": self.title, "parallel": self.parallel, "type": self.kind, "files": self.files, "deps": self.deps,
               "reqs": self.reqs, "done_when": self.done_when, "status": self.status})
    }
}

fn split_list(value: &str) -> Vec<String> {
    HTML_COMMENT
        .replace_lit(value, "")
        .split(',')
        .map(|item| item.trim().trim_matches('`').trim().to_string())
        .filter(|item| !item.is_empty() && item.to_lowercase() != "none")
        .collect()
}

/// `(tasks, problems)` of a TASKS.md.
pub fn parse_tasks(text: &str) -> (Vec<Task>, Vec<String>) {
    let mut tasks: Vec<Task> = Vec::new();
    let mut problems = Vec::new();
    let mut have_current = false;
    for raw in pycompat::splitlines(text) {
        let line = raw.trim();
        if let Some(heading) = TASK_HEADING.match_start(line) {
            let title = heading.group(2).unwrap_or("").to_string();
            let parallel = PARALLEL_MARK.is_match(&title);
            tasks.push(Task {
                id: heading.group(1).unwrap_or("").to_uppercase(),
                title: PARALLEL_MARK.replace_lit(&title, "").trim().to_string(),
                parallel,
                kind: DEFAULT_TYPE.into(),
                files: vec![],
                deps: vec![],
                reqs: vec![],
                done_when: String::new(),
                status: "pending".into(),
            });
            have_current = true;
            continue;
        }
        if line.starts_with('#') {
            have_current = false;
            continue;
        }
        let Some(field) = FIELD_LINE.match_start(line) else { continue };
        if !have_current {
            continue;
        }
        let name = field.group(1).unwrap_or("").to_lowercase();
        let value = HTML_COMMENT.replace_lit(field.group(2).unwrap_or(""), "").trim().to_string();
        let current = tasks.last_mut().expect("have_current");
        match name.as_str() {
            "type" => current.kind = if value.is_empty() { DEFAULT_TYPE.into() } else { value.to_lowercase() },
            "files" => current.files = split_list(&value),
            "deps" => current.deps = split_list(&value).into_iter().map(|d| d.to_uppercase()).collect(),
            "reqs" => current.reqs = split_list(&value),
            "done when" => current.done_when = value,
            "status" => current.status = if value.is_empty() { "pending".into() } else { value.to_lowercase() },
            _ => {}
        }
    }
    let mut seen: Vec<String> = Vec::new();
    for task in &tasks {
        if seen.contains(&task.id) {
            problems.push(format!("duplicate task id {}", task.id));
        }
        seen.push(task.id.clone());
        if !TASK_TYPES.contains(&task.kind.as_str()) {
            problems.push(format!("{}: Type must be impl or wiring (found {})", task.id, pycompat::py_repr_str(&task.kind)));
        }
        if task.done_when.is_empty() {
            problems.push(format!("{}: no Done when", task.id));
        }
    }
    (tasks, problems)
}

fn norm(path: &str) -> String {
    path.replace('\\', "/").trim().trim_matches('/').to_lowercase()
}

/// `(waves, problems)` for the tasks that are not in `done`.
pub fn build_waves(tasks: &[Task], max_parallel: i64, done: &[String]) -> (Vec<Vec<String>>, Vec<String>) {
    let known: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    let mut duplicated: Vec<&str> = known.iter().copied().filter(|i| known.iter().filter(|k| *k == i).count() > 1).collect();
    duplicated.sort();
    duplicated.dedup();
    let mut problems: Vec<String> = duplicated.iter().map(|i| format!("duplicate task id {i}")).collect();
    for task in tasks {
        for dep in &task.deps {
            if !known.contains(&dep.as_str()) {
                problems.push(format!("{} depends on unknown task {dep}", task.id));
            } else if *dep == task.id {
                problems.push(format!("{} depends on itself", task.id));
            }
        }
    }
    if !problems.is_empty() {
        return (vec![], problems);
    }
    let capacity = max_parallel.max(1) as usize;
    let mut remaining: Vec<&Task> = tasks.iter().filter(|t| !done.contains(&t.id)).collect();
    let mut finished: Vec<String> = done.to_vec();
    let mut waves: Vec<Vec<String>> = Vec::new();
    while !remaining.is_empty() {
        let mut wave: Vec<&Task> = Vec::new();
        let mut wave_files: Vec<String> = Vec::new();
        for task in &remaining {
            if !task.deps.iter().all(|d| finished.contains(d)) || wave.len() >= capacity {
                continue;
            }
            let files: Vec<String> = task.files.iter().map(|p| norm(p)).collect();
            if files.iter().any(|f| wave_files.contains(f)) {
                continue;
            }
            wave.push(task);
            wave_files.extend(files);
        }
        if wave.is_empty() {
            let stuck = remaining.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", ");
            return (vec![], vec![format!("dependency cycle among: {stuck}")]);
        }
        waves.push(wave.iter().map(|t| t.id.clone()).collect());
        finished.extend(wave.iter().map(|t| t.id.clone()));
        remaining.retain(|t| !finished.contains(&t.id));
    }
    (waves, vec![])
}

// ---------------------------------------------------------------- status file

pub fn status_path(root: &str, slug: &str) -> PathBuf {
    internal_dir(root).join("tasks").join(format!("{}.json", feature_folder_name(slug)))
}

/// `{"slug", "updated_at", "tasks": {Tn: {status, wave, note}}}`; a missing or unreadable file = no tasks.
pub fn read_status(root: &str, slug: &str) -> Map<String, Value> {
    let mut empty = Map::new();
    empty.insert("slug".into(), json!(slug));
    empty.insert("updated_at".into(), json!(""));
    empty.insert("tasks".into(), json!({}));
    let parsed = std::fs::read(status_path(root, slug)).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok());
    match parsed {
        Some(Value::Object(mut data)) if data.get("tasks").is_some_and(Value::is_object) => {
            data.entry("slug").or_insert(json!(slug));
            data
        }
        _ => empty,
    }
}

fn utc_stamp() -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
    time::OffsetDateTime::now_utc().format(format).unwrap_or_default()
}

/// Sets one task in the status file (atomic write) and returns the whole document.
pub fn write_status(root: &str, slug: &str, task_id: &str, status: &str, note: &str, wave: Option<i64>) -> Result<Map<String, Value>, String> {
    if !STATUS_VALUES.contains(&status) {
        return Err(format!("status must be one of: {}", STATUS_VALUES.join(", ")));
    }
    let mut document = read_status(root, slug);
    let previous_wave = document["tasks"].get(task_id).and_then(Value::as_object).and_then(|o| o.get("wave")).cloned().unwrap_or(Value::Null);
    let wave_value = match wave {
        Some(w) => json!(w),
        None => previous_wave,
    };
    let entry = json!({"status": status, "wave": wave_value, "note": note});
    if let Some(Value::Object(tasks)) = document.get_mut("tasks") {
        tasks.insert(task_id.to_string(), entry);
    }
    document.insert("updated_at".into(), json!(utc_stamp()));
    let path = status_path(root, slug);
    let folder = path.parent().expect("has parent").to_path_buf();
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let temp = folder.join(format!(".tasks-{}.tmp", std::process::id()));
    std::fs::write(&temp, py_json_dumps_indent(&Value::Object(document.clone()), true, 2)).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        e.to_string()
    })?;
    Ok(document)
}

/// Rewrites only the `**Status**:` line of each task block with the recorded status. Returns how many lines changed.
pub fn mirror_status_into_tasks_md(feature_dir_path: &str, status: &Map<String, Value>) -> usize {
    let path = Path::new(feature_dir_path).join(TASKS_FILE_NAME);
    let Ok(bytes) = std::fs::read(&path) else { return 0 };
    let Ok(text) = String::from_utf8(bytes) else { return 0 };
    let recorded = status.get("tasks").and_then(Value::as_object);
    let mut current: Option<String> = None;
    let mut changed = 0;
    let mut lines: Vec<String> = splitlines_keepends(&text).into_iter().map(String::from).collect();
    for index in 0..lines.len() {
        let raw = lines[index].clone();
        let stripped = raw.trim_end_matches(['\r', '\n']);
        if let Some(heading) = TASK_HEADING.match_start(stripped.trim()) {
            current = Some(heading.group(1).unwrap_or("").to_uppercase());
            continue;
        }
        let Some(found) = STATUS_FIELD_LINE.match_start(stripped.trim()) else { continue };
        let entry = current.as_ref().and_then(|c| recorded.and_then(|r| r.get(c))).and_then(Value::as_object);
        let Some(recorded_status) = entry.and_then(|e| e.get("status")).and_then(Value::as_str).filter(|s| STATUS_VALUES.contains(s)) else {
            continue;
        };
        if found.group(2).unwrap_or("").to_lowercase() == recorded_status {
            continue;
        }
        let ending = &raw[stripped.len()..];
        lines[index] = format!("{}{}{}{}", found.group(1).unwrap_or(""), recorded_status, found.group(3).unwrap_or(""), ending);
        changed += 1;
    }
    if changed > 0 {
        let _ = std::fs::write(&path, lines.concat());
    }
    changed
}

// ---------------------------------------------------------------- feature level

pub fn find_root(start: &str) -> String {
    let mut current = nt_abspath(start);
    loop {
        if Path::new(&current).join(".specs").is_dir() {
            return current;
        }
        let parent = nt_dirname(&current);
        if parent == current {
            return nt_abspath(start);
        }
        current = parent;
    }
}

pub fn read_default_max_parallel(root: &str) -> i64 {
    let text = read(&Path::new(root).join(".specs").join("project").join("ROADMAP.md"));
    parse_loop_settings(&text).settings["max_parallel_tasks"].as_i64().unwrap_or(3)
}

/// Tasks and problems from the feature's tasks.md; a legacy feature without it gets one synthetic task over its ROADMAP paths.
pub fn load_feature_tasks(root: &str, slug: &str) -> (Vec<Task>, Vec<String>) {
    let path = Path::new(&feature_dir(root, slug)).join(TASKS_FILE_NAME);
    if path.is_file() {
        return parse_tasks(&read(&path));
    }
    let (features, _) = parse_roadmap(&read(&Path::new(root).join(".specs").join("project").join("ROADMAP.md")));
    let paths: Vec<String> = features.get(slug).map(|f| f.text("paths").to_string()).unwrap_or_default().split(',').filter(|p| !p.trim().is_empty()).map(String::from).collect();
    let synthetic = Task {
        id: SYNTHETIC_TASK_ID.into(),
        title: "Build the whole feature".into(),
        parallel: false,
        kind: DEFAULT_TYPE.into(),
        files: paths,
        deps: vec![],
        reqs: vec![],
        done_when: "the spec's acceptance criteria hold".into(),
        status: "pending".into(),
    };
    (vec![synthetic], vec![format!("no tasks.md for {slug}: one synthetic task {SYNTHETIC_TASK_ID} covers the feature paths")])
}

fn plan_wave_numbers(tasks: &[Task], max_parallel: i64) -> Vec<(String, i64)> {
    let (waves, _) = build_waves(tasks, max_parallel, &[]);
    waves.iter().enumerate().flat_map(|(n, wave)| wave.iter().map(move |id| (id.clone(), n as i64 + 1))).collect()
}

pub fn describe_waves(root: &str, slug: &str, max_parallel: Option<i64>) -> Value {
    let (tasks, parse_problems) = load_feature_tasks(root, slug);
    let limit = max_parallel.filter(|m| *m != 0).unwrap_or_else(|| read_default_max_parallel(root));
    let status = read_status(root, slug);
    let mut done: Vec<String> = status["tasks"]
        .as_object()
        .map(|m| m.iter().filter(|(_, e)| e.get("status").and_then(Value::as_str) == Some("done")).map(|(k, _)| k.clone()).collect())
        .unwrap_or_default();
    done.sort();
    let known: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    let done_known: Vec<String> = done.iter().filter(|d| known.contains(&d.as_str())).cloned().collect();
    let (waves, built_problems) = build_waves(&tasks, limit, &done_known);
    let mut problems = parse_problems;
    problems.extend(built_problems);
    json!({"slug": slug, "max_parallel": limit, "waves": waves, "done": done_known,
           "tasks": tasks.iter().filter(|t| !done.contains(&t.id)).map(Task::to_json).collect::<Vec<_>>(), "problems": problems})
}

pub fn set_task_status(root: &str, slug: &str, task_id: &str, status: &str, note: &str) -> Result<Map<String, Value>, String> {
    let (tasks, _) = load_feature_tasks(root, slug);
    let task_id = task_id.to_uppercase();
    let limit = read_default_max_parallel(root);
    let wave = plan_wave_numbers(&tasks, limit).into_iter().find(|(id, _)| *id == task_id).map(|(_, w)| w);
    write_status(root, slug, &task_id, status, note, wave)
}

/// `harness tasks ...`; returns `(exit_code, stdout, stderr)`.
pub fn run_cli(args: &[String], cwd: &str) -> (i32, String, String) {
    let usage = "usage: harness.py tasks [-h] {waves,set,mirror} ...\n".to_string();
    let Some(command) = args.first().map(String::as_str) else {
        return (2, String::new(), format!("{usage}harness.py tasks: error: the following arguments are required: command\n"));
    };
    let rest = &args[1..];
    let mut positional: Vec<String> = Vec::new();
    let (mut max, mut root_opt, mut note) = (None::<i64>, String::new(), String::new());
    let mut index = 0;
    while index < rest.len() {
        match rest[index].as_str() {
            "--max" if index + 1 < rest.len() => {
                match rest[index + 1].parse::<i64>() {
                    Ok(n) => max = Some(n),
                    Err(_) => return (2, String::new(), format!("{usage}harness.py tasks: error: argument --max: invalid int value: {}\n", pycompat::py_repr_str(&rest[index + 1]))),
                }
                index += 2;
            }
            "--root" if index + 1 < rest.len() => {
                root_opt = rest[index + 1].clone();
                index += 2;
            }
            "--note" if index + 1 < rest.len() => {
                note = rest[index + 1].clone();
                index += 2;
            }
            other => {
                positional.push(other.to_string());
                index += 1;
            }
        }
    }
    let root = if root_opt.is_empty() { find_root(cwd) } else { nt_abspath(&root_opt) };
    match command {
        "waves" if positional.len() == 1 => {
            let result = describe_waves(&root, &positional[0], max);
            let no_waves = result["waves"].as_array().is_some_and(Vec::is_empty);
            let has_tasks = result["tasks"].as_array().is_some_and(|t| !t.is_empty());
            let has_problems = result["problems"].as_array().is_some_and(|t| !t.is_empty());
            (if has_problems && no_waves && has_tasks { 1 } else { 0 }, format!("{}\n", py_json_dumps_indent(&result, true, 2)), String::new())
        }
        "set" if positional.len() == 3 => {
            if !STATUS_VALUES.contains(&positional[2].as_str()) {
                return (
                    2,
                    String::new(),
                    format!(
                        "usage: harness.py tasks set [-h] [--note NOTE] [--root ROOT] slug task {{pending,running,done,failed}}\nharness.py tasks set: error: argument status: invalid choice: {} (choose from 'pending', 'running', 'done', 'failed')\n",
                        pycompat::py_repr_str(&positional[2])
                    ),
                );
            }
            match set_task_status(&root, &positional[0], &positional[1], &positional[2], &note) {
                Ok(doc) => (0, format!("{}\n", py_json_dumps_indent(&Value::Object(doc), true, 2)), String::new()),
                Err(message) => (1, String::new(), format!("{message}\n")),
            }
        }
        "mirror" if positional.len() == 1 => {
            let status = read_status(&root, &positional[0]);
            let changed = mirror_status_into_tasks_md(&feature_dir(&root, &positional[0]), &status);
            (0, format!("{}\n", py_json_dumps(&json!({"slug": positional[0], "lines_changed": changed}), true)), String::new())
        }
        "waves" | "set" | "mirror" => (2, String::new(), format!("{usage}harness.py tasks {command}: error: the following arguments are required\n")),
        _ => (
            2,
            String::new(),
            format!("{usage}harness.py tasks: error: argument command: invalid choice: {} (choose from 'waves', 'set', 'mirror')\n", pycompat::py_repr_str(command)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASKS: &str = "# Tasks\n### T1: Models [P]\n**Type**: impl\n**Files**: src/a.rs, src/b.rs <!-- note -->\n**Deps**: none\n**Done when**: it builds\n**Status**: pending\n\n### T2: Wire it\n**Type**: wiring\n**Files**: src/a.rs\n**Deps**: T1\n**Done when**: wired\n\n### T3: Docs\n**Files**: docs\n**Done when**: documented\n";

    #[test]
    fn parses_fields_and_problems() {
        let (tasks, problems) = parse_tasks(TASKS);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(tasks.len(), 3);
        assert!(tasks[0].parallel && tasks[0].title == "Models");
        assert_eq!(tasks[0].files, vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(tasks[1].deps, vec!["T1"]);
        let (_, problems) = parse_tasks("### T1: x\n**Type**: weird\n### T1: y\n");
        assert_eq!(problems, vec!["T1: Type must be impl or wiring (found 'weird')", "T1: no Done when", "duplicate task id T1", "T1: no Done when"]);
    }

    #[test]
    fn waves_respect_deps_files_and_capacity() {
        let (tasks, _) = parse_tasks(TASKS);
        let (waves, problems) = build_waves(&tasks, 3, &[]);
        assert!(problems.is_empty());
        assert_eq!(waves, vec![vec!["T1", "T3"], vec!["T2"]]);
        let single = build_waves(&tasks, 1, &[]).0;
        assert_eq!(single.len(), 3);
        assert_eq!(single[0], vec!["T1"]);
        let (_, cycle) = build_waves(&parse_tasks("### T1: a\n**Deps**: T2\n**Done when**: x\n### T2: b\n**Deps**: T1\n**Done when**: x\n").0, 3, &[]);
        assert_eq!(cycle, vec!["dependency cycle among: T1, T2"]);
    }

    #[test]
    fn status_file_round_trip_and_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        let feature = dir.path().join(".specs/features/f1");
        std::fs::create_dir_all(&feature).unwrap();
        std::fs::write(feature.join("tasks.md"), TASKS.replace('\n', "\r\n")).unwrap();
        let doc = set_task_status(&root, "f1", "t1", "done", "ok").unwrap();
        assert_eq!(doc["tasks"]["T1"]["wave"], 1);
        assert_eq!(read_status(&root, "f1")["tasks"]["T1"]["status"], "done");
        let changed = mirror_status_into_tasks_md(&feature.display().to_string(), &read_status(&root, "f1"));
        assert_eq!(changed, 1);
        let text = std::fs::read_to_string(feature.join("tasks.md")).unwrap();
        assert!(text.contains("**Status**: done\r\n"));
        assert!(set_task_status(&root, "f1", "T1", "bogus", "").is_err());
        let described = describe_waves(&root, "f1", None);
        assert_eq!(described["done"], json!(["T1"]));
    }
}
