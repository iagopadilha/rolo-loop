// SPDX-License-Identifier: MIT
//! Small file stores under `.specs/project/loop/`: answers, whiteboard, quarantine summaries, planning thread, follow-up threads, the call
//! log of `claude -p`, the error events and the read side of the task waves.

use super::loop_command_file::{get_loop_internal_dir, now_iso, read_json_object, write_json_atomic};
use engine_state::{loop_state, task_waves};
use pycompat::json::py_json_dumps;
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

// ------------------------------------------------------------------------------------------------ ANSWERS.md

pub const ANSWERS_FILE_NAME: &str = "ANSWERS.md";
pub const USER_NOTE_QUESTION: &str = "(user note from the panel)";
pub const USER_INSTRUCTION_QUESTION: &str = "(user instruction from the panel)";

/// The panel only APPENDS `[pending]` blocks (user note/instruction); the next turn applies them. Same format as the engine's `write_answer`.
pub fn append_pending_answer(work_dir: &str, question: &str, text: &str) {
    let path = loop_state::state_file(work_dir, ANSWERS_FILE_NAME);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let block = format!("\n## Answer {} [pending]\nQuestion: {question}\nUser answer: {}\n", now_iso(), text.trim());
    if let Ok(mut handle) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = handle.write_all(block.as_bytes());
    }
}

// ------------------------------------------------------------------------------------------------ whiteboard

pub const WHITEBOARD_FILE_NAME: &str = "whiteboard.json";

/// Project board: opaque content for the back, `{"data", "version", "updated_at"}`.
pub fn read_board(work_dir: &str) -> Map<String, Value> {
    read_json_object(&get_loop_internal_dir(work_dir).join(WHITEBOARD_FILE_NAME))
}

pub fn write_board(work_dir: &str, board: &Value) -> std::io::Result<()> {
    write_json_atomic(&get_loop_internal_dir(work_dir).join(WHITEBOARD_FILE_NAME), board)
}

// ------------------------------------------------------------------------------------------------ quarantine summaries

pub const QUARANTINE_FOLDER_NAME: &str = "quarantine";
pub const SUMMARY_FILE_SUFFIX: &str = ".summary.json";
static SLUG_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,80}\z"));

/// `None` for a slug outside the feature-slug format (never builds a path from free text).
pub fn get_summary_path(work_dir: &str, slug: &str) -> Option<PathBuf> {
    if !SLUG_PATTERN.is_match(slug) || slug.contains("..") {
        return None;
    }
    Some(get_loop_internal_dir(work_dir).join(QUARANTINE_FOLDER_NAME).join(format!("{slug}{SUMMARY_FILE_SUFFIX}")))
}

pub fn read_summary(work_dir: &str, slug: &str) -> Map<String, Value> {
    get_summary_path(work_dir, slug).map(|p| read_json_object(&p)).unwrap_or_default()
}

pub fn write_summary(work_dir: &str, slug: &str, summary: &Value) {
    if let Some(path) = get_summary_path(work_dir, slug) {
        let _ = write_json_atomic(&path, summary);
    }
}

// ------------------------------------------------------------------------------------------------ planning thread

pub const PLANNING_FOLDER_NAME: &str = "planning";
pub const THREAD_FILE_NAME: &str = "thread.json";
pub const PLANNING_MAX_MESSAGES: usize = 400;

pub fn create_empty_planning_thread() -> Value {
    json!({"messages": [], "session_id": null, "created_at": now_iso(), "updated_at": null})
}

pub fn get_planning_dir(work_dir: &str) -> PathBuf {
    get_loop_internal_dir(work_dir).join(PLANNING_FOLDER_NAME)
}

fn only_objects(data: &mut Map<String, Value>) {
    if let Some(Value::Array(messages)) = data.get_mut("messages") {
        messages.retain(Value::is_object);
    }
}

/// The thread, or a new empty one when the file does not exist or does not have the expected format.
pub fn read_planning_thread(work_dir: &str) -> Value {
    let mut data = read_json_object(&get_planning_dir(work_dir).join(THREAD_FILE_NAME));
    if !data.get("messages").is_some_and(Value::is_array) {
        return create_empty_planning_thread();
    }
    only_objects(&mut data);
    Value::Object(data)
}

pub fn write_planning_thread(work_dir: &str, thread: &mut Value) {
    keep_last(thread, PLANNING_MAX_MESSAGES);
    thread["updated_at"] = json!(now_iso());
    let _ = write_json_atomic(&get_planning_dir(work_dir).join(THREAD_FILE_NAME), thread);
}

fn keep_last(thread: &mut Value, limit: usize) {
    if let Some(Value::Array(messages)) = thread.get_mut("messages") {
        if messages.len() > limit {
            messages.drain(..messages.len() - limit);
        }
    }
}

// ------------------------------------------------------------------------------------------------ follow-up threads

pub const FOLLOW_UP_FOLDER_NAME: &str = "follow-up";
pub const FOLLOW_UP_MAX_MESSAGES: usize = 200;
static ROUND_FILE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^round-(\d+)\.json"));

pub fn create_empty_follow_up_thread(round_number: i64) -> Value {
    json!({"round": round_number, "session_id": null, "spent_usd": 0.0, "summary_note": "", "baseline": {"roadmap": "", "flows": []}, "messages": [], "closed_at": null})
}

pub fn get_follow_up_dir(work_dir: &str) -> PathBuf {
    get_loop_internal_dir(work_dir).join(FOLLOW_UP_FOLDER_NAME)
}

pub fn get_follow_up_thread_path(work_dir: &str, round_number: i64) -> PathBuf {
    get_follow_up_dir(work_dir).join(format!("round-{round_number}.json"))
}

/// The thread of the round, or a new empty one when the file does not exist or does not have the expected format.
pub fn read_follow_up_thread(work_dir: &str, round_number: i64) -> Value {
    let mut data = read_json_object(&get_follow_up_thread_path(work_dir, round_number));
    if !data.get("messages").is_some_and(Value::is_array) {
        return create_empty_follow_up_thread(round_number);
    }
    only_objects(&mut data);
    data.entry("round").or_insert(json!(round_number));
    Value::Object(data)
}

pub fn write_follow_up_thread(work_dir: &str, round_number: i64, thread: &mut Value) {
    if !thread.get("messages").is_some_and(Value::is_array) {
        thread["messages"] = json!([]);
    }
    keep_last(thread, FOLLOW_UP_MAX_MESSAGES);
    thread["round"] = json!(round_number);
    let _ = write_json_atomic(&get_follow_up_thread_path(work_dir, round_number), thread);
}

/// The round whose stage has not run yet: 1 + the number of closed rounds (round files with `closed_at` set). Nothing on disk = round 1.
pub fn current_follow_up_round(work_dir: &str) -> i64 {
    let folder = get_follow_up_dir(work_dir);
    let Ok(names) = std::fs::read_dir(&folder) else { return 1 };
    let closed = names
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| ROUND_FILE_PATTERN.is_match(name) && name.ends_with(".json"))
        .filter(|name| read_json_object(&folder.join(name)).get("closed_at").is_some_and(engine_truthy))
        .count();
    closed as i64 + 1
}

fn engine_truthy(value: &Value) -> bool {
    engine_state::notify_truthy(Some(value))
}

// ------------------------------------------------------------------------------------------------ claude call log

pub const CALL_LOG_FILE_NAME: &str = "claude_calls.jsonl";
pub const CALL_LOG_MAX_LINES: usize = 400; // kept after a trim
pub const CALL_LOG_TRIM_AT_LINES: usize = 500; // the file is rewritten when it grows past this
static CALL_LOG_LOCK: Mutex<()> = Mutex::new(());

/// Call log of the `claude -p` runs the panel itself starts: one JSON line per call, bounded.
pub fn call_log_path(work_dir: &str) -> PathBuf {
    loop_state::internal_dir(work_dir).join(CALL_LOG_FILE_NAME)
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()).unwrap_or_default()
}

/// Appends one call; trims the file when it is too long. Errors when the folder cannot be written.
pub fn append_call(work_dir: &str, entry: &Value) -> std::io::Result<()> {
    let path = call_log_path(work_dir);
    let _guard = CALL_LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut handle = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    handle.write_all(format!("{}\n", py_json_dumps(entry, false)).as_bytes())?;
    drop(handle);
    let lines = read_lines(&path);
    if lines.len() > CALL_LOG_TRIM_AT_LINES {
        let temporary = PathBuf::from(format!("{}.tmp", path.display()));
        std::fs::write(&temporary, format!("{}\n", lines[lines.len() - CALL_LOG_MAX_LINES..].join("\n")))?;
        engine_state::loop_io::replace_file(&temporary, &path)?;
    }
    Ok(())
}

/// The last `limit` calls, oldest first; unreadable lines and a missing file are skipped.
pub fn read_calls(work_dir: &str, limit: usize) -> Vec<Value> {
    let lines = read_lines(&call_log_path(work_dir));
    let start = lines.len().saturating_sub(limit);
    lines[start..].iter().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(Value::is_object).collect()
}

// ------------------------------------------------------------------------------------------------ task status (read side of the waves)

pub const PENDING_STATUS: &str = "pending";
pub const SOURCE_TASKS_MARKDOWN: &str = "tasks.md";
pub const SOURCE_NONE: &str = "none";

/// `{"source", "max_parallel", "tasks": [{id, title, status, wave, deps, files, parallel, note}], "waves": [{n, task_ids, split_by_limit}]}`.
///
/// Tasks come in wave order. A missing or unreadable status file = every task `pending`; a feature without tasks.md = `source` "none".
pub fn read_feature_tasks(work_dir: &str, slug: &str) -> Value {
    let max_parallel = task_waves::read_default_max_parallel(work_dir);
    let text = loop_state::read(&Path::new(&loop_state::feature_dir(work_dir, slug)).join(task_waves::TASKS_FILE_NAME));
    let (tasks, _) = task_waves::parse_tasks(&text);
    if tasks.is_empty() {
        return json!({"source": SOURCE_NONE, "max_parallel": max_parallel, "tasks": [], "waves": []});
    }
    let status = task_waves::read_status(work_dir, slug);
    let recorded = status.get("tasks").and_then(Value::as_object).cloned().unwrap_or_default();
    let planned = build_planned_waves(&tasks, max_parallel);
    let mut wave_numbers: BTreeMap<String, i64> = BTreeMap::new();
    for wave in &planned {
        for id in wave["task_ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            wave_numbers.insert(id.to_string(), wave["n"].as_i64().unwrap_or(0));
        }
    }
    let mut rows: Vec<(i64, usize, Value)> = Vec::new();
    for (position, task) in tasks.iter().enumerate() {
        let entry = recorded.get(&task.id).and_then(Value::as_object).cloned().unwrap_or_default();
        let status_text = entry.get("status").and_then(Value::as_str).filter(|s| task_waves::STATUS_VALUES.contains(s)).unwrap_or(PENDING_STATUS);
        let wave: Value = match wave_numbers.get(&task.id).filter(|n| **n != 0) {
            Some(n) => json!(n),
            None => entry.get("wave").cloned().unwrap_or(Value::Null),
        };
        let sort_wave = wave.as_i64().filter(|_| wave.is_i64() || wave.is_u64()).unwrap_or(tasks.len() as i64 + 1);
        let note = match entry.get("note") {
            Some(Value::String(s)) => s.clone(),
            Some(v) if engine_truthy(v) => py_json_dumps(v, false),
            _ => String::new(),
        };
        rows.push((sort_wave, position, json!({"id": task.id, "title": task.title, "status": status_text, "wave": wave, "deps": task.deps, "files": task.files, "parallel": task.parallel, "note": note})));
    }
    rows.sort_by_key(|row| (row.0, row.1));
    json!({"source": SOURCE_TASKS_MARKDOWN, "max_parallel": max_parallel, "tasks": rows.into_iter().map(|r| r.2).collect::<Vec<_>>(), "waves": planned})
}

/// `[{n, task_ids, split_by_limit}]` of the full plan (nothing done); `[]` when the engine reports a problem (unknown dependency, cycle).
///
/// `split_by_limit` is true when, without the parallel limit, a task of the next wave would have run in the same wave as one of this wave.
pub fn build_planned_waves(tasks: &[task_waves::Task], max_parallel: i64) -> Vec<Value> {
    let (limited, problems) = task_waves::build_waves(tasks, max_parallel, &[]);
    if !problems.is_empty() || limited.is_empty() {
        return vec![];
    }
    let (unlimited, _) = task_waves::build_waves(tasks, tasks.len() as i64, &[]);
    let mut unlimited_number: BTreeMap<&str, usize> = BTreeMap::new();
    for (number, wave) in unlimited.iter().enumerate() {
        for id in wave {
            unlimited_number.insert(id.as_str(), number + 1);
        }
    }
    limited
        .iter()
        .enumerate()
        .map(|(index, wave)| {
            let following: &[String] = limited.get(index + 1).map(Vec::as_slice).unwrap_or(&[]);
            let numbers: Vec<Option<usize>> = wave.iter().map(|id| unlimited_number.get(id.as_str()).copied()).collect();
            let split = following.iter().any(|id| numbers.contains(&unlimited_number.get(id.as_str()).copied()));
            json!({"n": index + 1, "task_ids": wave, "split_by_limit": split})
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------ error events

pub const EVENTS_FOLDER_NAME: &str = "events";
pub const ERRORS_FILE_NAME: &str = "errors.jsonl";

type EventCache = BTreeMap<PathBuf, ((u128, u64), Vec<(String, Map<String, Value>)>)>;

/// Durable store of the error center's events: one JSON object per line, append only. A later record with the same `id` only fills fields
/// (a non-null value wins over nothing, `null` never erases). One write per batch, so a crash leaves at most one broken last line.
#[derive(Default)]
pub struct ErrorEventRepository {
    lock: Mutex<EventCache>,
}

impl ErrorEventRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_path(&self, work_dir: &str) -> PathBuf {
        loop_state::loop_path(work_dir, EVENTS_FOLDER_NAME).join(ERRORS_FILE_NAME)
    }

    fn stamp(path: &Path) -> Option<(u128, u64)> {
        let info = std::fs::metadata(path).ok()?;
        let mtime = info.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
        Some((mtime, info.len()))
    }

    /// Events by first appearance, later records merged in. Missing file = `[]`.
    pub fn read_events(&self, work_dir: &str) -> Vec<Map<String, Value>> {
        let path = self.get_path(work_dir);
        let Some(stamp) = Self::stamp(&path) else { return vec![] };
        {
            let cache = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((cached, events)) = cache.get(&path) {
                if *cached == stamp {
                    return events.iter().map(|(_, e)| e.clone()).collect();
                }
            }
        }
        let mut merged: Vec<(String, Map<String, Value>)> = Vec::new();
        for record in read_json_lines(&path) {
            Self::merge_record(&mut merged, &record);
        }
        let result = merged.iter().map(|(_, e)| e.clone()).collect();
        self.lock.lock().unwrap_or_else(|e| e.into_inner()).insert(path, (stamp, merged));
        result
    }

    fn merge_record(merged: &mut Vec<(String, Map<String, Value>)>, record: &Map<String, Value>) {
        let Some(id) = record.get("id").and_then(Value::as_str).filter(|i| !i.is_empty()) else { return };
        let position = match merged.iter().position(|(k, _)| k == id) {
            Some(p) => p,
            None => {
                merged.push((id.to_string(), Map::from_iter([("id".to_string(), json!(id))])));
                merged.len() - 1
            }
        };
        let known = &mut merged[position].1;
        for (key, value) in record {
            if !value.is_null() || !known.contains_key(key) {
                known.insert(key.clone(), value.clone());
            }
        }
    }

    /// Appends the records in ONE write. Returns false (and writes nothing) when the folder is not writable.
    pub fn append_events(&self, work_dir: &str, records: &[Value]) -> bool {
        if records.is_empty() {
            return true;
        }
        let path = self.get_path(work_dir);
        let mut payload: Vec<u8> = records.iter().map(|r| format!("{}\n", py_json_dumps(r, false))).collect::<String>().into_bytes();
        let mut cache = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if std::fs::metadata(&path).is_ok_and(|m| m.len() > 0) {
                let mut handle = std::fs::File::open(&path)?;
                handle.seek(SeekFrom::End(-1))?;
                let mut last = [0u8; 1];
                handle.read_exact(&mut last)?;
                if last[0] != b'\n' {
                    payload.insert(0, b'\n');
                }
            }
            std::fs::OpenOptions::new().create(true).append(true).open(&path)?.write_all(&payload)
        };
        let ok = write().is_ok();
        if ok {
            cache.remove(&path);
        }
        ok
    }
}

/// Every line of a `.jsonl` that is a JSON object; a broken line (a crash mid-write) is skipped.
pub fn read_json_lines(path: &Path) -> Vec<Map<String, Value>> {
    read_lines(path).iter().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter_map(|v| if let Value::Object(m) = v { Some(m) } else { None }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().display().to_string();
        (dir, path)
    }

    #[test]
    fn answers_board_summary_threads() {
        let (_d, w) = work();
        append_pending_answer(&w, USER_NOTE_QUESTION, "  hello \n");
        let text = std::fs::read_to_string(loop_state::state_file(&w, ANSWERS_FILE_NAME)).unwrap();
        assert!(text.contains("[pending]\nQuestion: (user note from the panel)\nUser answer: hello\n"));
        assert!(read_board(&w).is_empty());
        write_board(&w, &json!({"data": {"a": 1}, "version": 2})).unwrap();
        assert_eq!(read_board(&w)["version"], 2);
        assert!(get_summary_path(&w, "../x").is_none() && get_summary_path(&w, "a..b").is_none() && get_summary_path(&w, "ok-1").is_some());
        write_summary(&w, "ok-1", &json!({"text": "t"}));
        assert_eq!(read_summary(&w, "ok-1")["text"], "t");
        assert!(read_summary(&w, "../x").is_empty());
        let mut thread = read_planning_thread(&w);
        assert_eq!(thread["session_id"], Value::Null);
        thread["messages"] = Value::Array((0..450).map(|i| json!({"i": i})).collect());
        write_planning_thread(&w, &mut thread);
        let again = read_planning_thread(&w);
        assert_eq!((again["messages"].as_array().unwrap().len(), again["messages"][0]["i"].clone()), (400, json!(50)));
        assert_eq!(current_follow_up_round(&w), 1);
        let mut round = read_follow_up_thread(&w, 1);
        round["closed_at"] = json!("2026-01-01");
        write_follow_up_thread(&w, 1, &mut round);
        assert_eq!(current_follow_up_round(&w), 2);
        assert_eq!(read_follow_up_thread(&w, 2)["round"], 2);
    }

    #[test]
    fn call_log_trims_and_events_merge() {
        let (_d, w) = work();
        for i in 0..501 {
            append_call(&w, &json!({"i": i})).unwrap();
        }
        let calls = read_calls(&w, 1000);
        assert_eq!((calls.len(), calls[0]["i"].clone()), (400, json!(101)));
        assert_eq!(read_calls(&w, 2).len(), 2);
        let events = ErrorEventRepository::new();
        assert!(events.read_events(&w).is_empty());
        assert!(events.append_events(&w, &[json!({"id": "e1", "kind": "k", "turn": null}), json!({"id": "e2", "kind": "z"})]));
        assert!(events.append_events(&w, &[json!({"id": "e1", "turn": 3, "kind": null})]));
        let read = events.read_events(&w);
        assert_eq!((read.len(), read[0]["turn"].clone(), read[0]["kind"].clone()), (2, json!(3), json!("k")));
        std::fs::write(events.get_path(&w), "{\"id\": \"e9\"}\n{broken").unwrap();
        assert!(events.append_events(&w, &[json!({"id": "e10"})]));
        assert_eq!(events.read_events(&w).iter().map(|e| e["id"].as_str().unwrap().to_string()).collect::<Vec<_>>(), vec!["e9", "e10"]);
    }

    #[test]
    fn feature_tasks_follow_the_waves() {
        let (_d, w) = work();
        assert_eq!(read_feature_tasks(&w, "f")["source"], "none");
        let folder = PathBuf::from(loop_state::feature_dir(&w, "f"));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("tasks.md"), "### T1: first\n**Files**: a.rs\n\n### T2: second [P]\n**Deps**: T1\n**Files**: b.rs\n\n### T3: third [P]\n**Deps**: T1\n**Files**: c.rs\n").unwrap();
        task_waves::set_task_status(&w, "f", "T1", "done", "ok").unwrap();
        let read = read_feature_tasks(&w, "f");
        assert_eq!(read["source"], "tasks.md");
        let ids: Vec<&str> = read["tasks"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["T1", "T2", "T3"]);
        assert_eq!((read["tasks"][0]["status"].clone(), read["tasks"][0]["note"].clone()), (json!("done"), json!("ok")));
        assert_eq!(read["waves"][0]["task_ids"], json!(["T1"]));
    }
}
