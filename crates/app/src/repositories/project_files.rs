// SPDX-License-Identifier: MIT
//! Files behind the subprojects table and the Test environment tab, plus the chat threads and the running chat jobs.
//!
//! Subprojects: `.specs/project/SUBPROJECTS.json` (the list, keyed by repository name), `.specs/project/SUBPROJECTS-SUGGESTED.json` (the AI's
//! suggestion, read and deleted here, written only by the AI) and `.specs/project/loop/internal/subprojects/map-queue.json` (the sequential map
//! queue). Writes are atomic, UTF-8, LF, two-space indent and a final newline, so a read followed by a write of the same data leaves the same
//! bytes. Reads are tolerant: a missing file is "absent" and an unreadable or invalid one comes back as a [`SubprojectsFileError`].
//!
//! Test environment: `.specs/project/TEST-ENVIRONMENT.md` (names only) and `.specs/project/.env.test` (the project's test VALUES, its only
//! credential source). Values only ever flow INTO a file here; nothing in this module returns one, and no error message carries one.

use super::loop_command_file::{get_loop_internal_dir, now_iso, read_json_object, write_json_atomic};
use super::specs_file::SpecsFileRepository;
use crate::domain::errors::AppError;
use engine_state::access_settings;
use pycompat::json::py_json_dumps_indent;
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

// ------------------------------------------------------------------------------------------------ subprojects

const SUBPROJECTS_FILE_VERSION: i64 = 1;

/// Returned (never raised) when a file exists but cannot be used.
#[derive(Clone, Debug, PartialEq)]
pub struct SubprojectsFileError {
    pub path: String,
    pub error: String,
}

/// What a read of one of the subproject files gives.
#[derive(Clone, Debug, PartialEq)]
pub enum Read {
    Absent,
    Data(Map<String, Value>),
    Error(SubprojectsFileError),
}

pub fn render_json_text(data: &Value) -> String {
    format!("{}\n", py_json_dumps_indent(data, false, 2))
}

pub struct SubprojectsFileRepository {
    pub specs: SpecsFileRepository,
}

impl Default for SubprojectsFileRepository {
    fn default() -> Self {
        SubprojectsFileRepository { specs: SpecsFileRepository::new() }
    }
}

impl SubprojectsFileRepository {
    pub fn subprojects_path(work_dir: &str) -> PathBuf {
        Path::new(work_dir).join(".specs").join("project").join("SUBPROJECTS.json")
    }

    pub fn suggested_path(work_dir: &str) -> PathBuf {
        Path::new(work_dir).join(".specs").join("project").join("SUBPROJECTS-SUGGESTED.json")
    }

    pub fn map_queue_path(work_dir: &str) -> PathBuf {
        Path::new(work_dir).join(".specs").join("project").join("loop").join("internal").join("subprojects").join("map-queue.json")
    }

    /// The parsed object, `Absent` when the file does not exist, or an error (unreadable, not UTF-8, invalid JSON, or not a JSON object).
    pub fn read_json_object(&self, path: &Path) -> Read {
        let error = |text: String| Read::Error(SubprojectsFileError { path: path.display().to_string(), error: text });
        if !path.exists() {
            return Read::Absent;
        }
        let Some(content) = self.specs.read_bytes(path) else { return error("the file cannot be read".into()) };
        let Ok(text) = String::from_utf8(content) else { return error("the file is not valid JSON: invalid UTF-8".into()) };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Read::Data(map),
            Ok(_) => error("the file does not hold a JSON object".into()),
            Err(failure) => error(format!("the file is not valid JSON: {failure}")),
        }
    }

    pub fn write_json_object(&self, path: &Path, data: &Value) -> std::io::Result<()> {
        self.specs.write_bytes_atomic(path, render_json_text(data).as_bytes()) // creates the folders
    }

    /// Whole file `{"version":1,"repos":{...}}`, `Absent` when absent, error when unusable.
    pub fn read_subprojects(&self, work_dir: &str) -> Read {
        let path = Self::subprojects_path(work_dir);
        match self.read_json_object(&path) {
            Read::Data(data) if !data.get("repos").is_some_and(Value::is_object) => Read::Error(SubprojectsFileError { path: path.display().to_string(), error: "the file has no `repos` object".into() }),
            other => other,
        }
    }

    pub fn write_subprojects(&self, work_dir: &str, data: &Value) -> std::io::Result<()> {
        self.write_json_object(&Self::subprojects_path(work_dir), data)
    }

    /// Entry of one repository, `Absent` when the file or the entry is absent, error when the file is unusable.
    pub fn read_repo_entry(&self, work_dir: &str, repo_name: &str) -> Read {
        match self.read_subprojects(work_dir) {
            Read::Data(data) => match data.get("repos").and_then(|r| r.get(repo_name)) {
                Some(Value::Object(entry)) => Read::Data(entry.clone()),
                _ => Read::Absent,
            },
            other => other,
        }
    }

    /// Replaces (or adds) one repository's entry, keeping the others. An unusable existing file is replaced by a new one holding only this
    /// entry (the caller read it before and showed the error).
    pub fn write_repo_entry(&self, work_dir: &str, repo_name: &str, entry: &Value) -> std::io::Result<()> {
        let mut data = match self.read_subprojects(work_dir) {
            Read::Data(data) => data,
            _ => Map::from_iter([("version".to_string(), json!(SUBPROJECTS_FILE_VERSION)), ("repos".to_string(), json!({}))]),
        };
        data.entry("version").or_insert(json!(SUBPROJECTS_FILE_VERSION));
        if let Some(Value::Object(repos)) = data.get_mut("repos") {
            repos.insert(repo_name.to_string(), entry.clone());
        }
        self.write_subprojects(work_dir, &Value::Object(data))
    }

    /// The AI's suggestion, `Absent` when absent, error when unusable.
    pub fn read_suggested(&self, work_dir: &str) -> Read {
        self.read_json_object(&Self::suggested_path(work_dir))
    }

    /// True when a file was removed; false when there was none (or it could not be removed).
    pub fn delete_suggested(&self, work_dir: &str) -> bool {
        std::fs::remove_file(Self::suggested_path(work_dir)).is_ok()
    }

    /// `{"queue":[paths],"current":...,"done":[],"failed":{path: reason}}`.
    pub fn read_map_queue(&self, work_dir: &str) -> Read {
        self.read_json_object(&Self::map_queue_path(work_dir))
    }

    pub fn write_map_queue(&self, work_dir: &str, queue_state: &Value) -> std::io::Result<()> {
        self.write_json_object(&Self::map_queue_path(work_dir), queue_state)
    }
}

// ------------------------------------------------------------------------------------------------ test environment

pub const VALUE_LIMIT_CHARACTERS: usize = 500;
const BYTE_ORDER_MARK: char = '\u{feff}';
static QUOTE_NEEDED: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\s#&%{}?^]"));
static ENV_LINE_KEY: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*="));

/// `NAME=value`, quoted when the .env format needs it (a quoted value is read literally). The refusals never quote the value.
pub fn format_env_line(name: &str, value: &str) -> Result<String, AppError> {
    if value.trim().is_empty() {
        return Err(AppError::invalid_request(format!("the value of {name} is empty")));
    }
    if value.contains(['\n', '\r', '\0']) {
        return Err(AppError::invalid_request(format!("the value of {name} has a line break: values are a single line")));
    }
    if value.chars().count() > VALUE_LIMIT_CHARACTERS {
        return Err(AppError::invalid_request(format!("the value of {name} is longer than {VALUE_LIMIT_CHARACTERS} characters")));
    }
    if value.starts_with(['\'', '"']) || QUOTE_NEEDED.is_match(value) {
        if !value.contains('\'') {
            return Ok(format!("{name}='{value}'"));
        }
        if !value.contains('"') {
            return Ok(format!("{name}=\"{value}\""));
        }
        return Err(AppError::invalid_request(format!("the value of {name} has spaces or symbols and both kinds of quote: it cannot be stored")));
    }
    Ok(format!("{name}={value}"))
}

pub fn detect_line_ending(text: &str) -> &'static str {
    ["\r\n", "\n", "\r"].into_iter().find(|e| text.contains(e)).unwrap_or("\n")
}

/// `text` with the last `NAME=...` line of each name replaced and the missing names appended; everything else is untouched.
pub fn replace_env_lines(text: &str, lines_by_name: &[(String, String)]) -> String {
    let mut lines: Vec<String> = pycompat::splitlines_keepends(text).into_iter().map(String::from).collect();
    let ending = detect_line_ending(text);
    let mut last_index: Vec<(String, usize)> = Vec::new();
    for (index, raw) in lines.iter().enumerate() {
        if let Some(found) = ENV_LINE_KEY.captures(raw.trim_start_matches(BYTE_ORDER_MARK)) {
            let name = found[1].to_string();
            if lines_by_name.iter().any(|(n, _)| *n == name) {
                last_index.retain(|(n, _)| *n != name);
                last_index.push((name, index));
            }
        }
    }
    let mut appended: Vec<&String> = Vec::new();
    for (name, new_line) in lines_by_name {
        match last_index.iter().find(|(n, _)| n == name) {
            Some((_, position)) => {
                let raw = lines[*position].clone();
                let body = raw.trim_end_matches(['\r', '\n']);
                let mark = if body.starts_with(BYTE_ORDER_MARK) { BYTE_ORDER_MARK.to_string() } else { String::new() };
                lines[*position] = format!("{mark}{new_line}{}", &raw[body.len()..]);
            }
            None => appended.push(new_line),
        }
    }
    let mut result: String = lines.concat();
    if !appended.is_empty() {
        if !result.is_empty() && !result.ends_with(['\n', '\r']) {
            result.push_str(ending);
        }
        for line in appended {
            result.push_str(line);
            result.push_str(ending);
        }
    }
    result
}

/// Writes the lines of `values` into the env file at `path` (created when missing), keeping everything else byte for byte.
pub fn write_env_values(path: &Path, values: &[(String, String)]) -> Result<(), AppError> {
    let mut lines: Vec<(String, String)> = Vec::new();
    for (name, value) in values {
        lines.push((name.clone(), format_env_line(name, value)?)); // validates every value before touching the file
    }
    let current = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| AppError::invalid_request("the env file is not UTF-8 text: fix it by hand first"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(AppError::invalid_request(format!("the env file cannot be read: {error}"))),
    };
    let text = replace_env_lines(&current, &lines);
    let folder = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder).map_err(|e| AppError::invalid_request(format!("the env file cannot be written: {e}")))?;
    let temporary = tempfile::Builder::new().prefix(".env.").suffix(".tmp").tempfile_in(folder).map_err(|e| AppError::invalid_request(format!("the env file cannot be written: {e}")))?;
    std::fs::write(temporary.path(), text.as_bytes()).map_err(|e| AppError::invalid_request(format!("the env file cannot be written: {e}")))?;
    let (_, temp_path) = temporary.keep().map_err(|e| AppError::invalid_request(format!("the env file cannot be written: {e}")))?;
    engine_state::loop_io::replace_file(&temp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        AppError::invalid_request(format!("the env file cannot be written: {e}"))
    })
}

pub struct TestEnvironmentRepository<'a> {
    pub specs: &'a SpecsFileRepository,
}

impl TestEnvironmentRepository<'_> {
    /// Text of TEST-ENVIRONMENT.md or `None`.
    pub fn read_document(&self, work_dir: &str) -> Option<String> {
        self.specs.read_text(&Path::new(work_dir).join(".specs").join("project").join("TEST-ENVIRONMENT.md"))
    }

    /// Writes only `<root>/.specs/project/.env.test` (created when missing); `root` is the repo, never the loop worktree (WT-02).
    pub fn write_project_values(&self, root: &str, values: &[(String, String)]) -> Result<(), AppError> {
        write_env_values(&Path::new(root).join(".specs").join("project").join(".env.test"), values)
    }

    /// Drops the legacy keys of `.specs/project/ACCESS.json` (access is computed now). True when it changed the file.
    pub fn migrate_legacy_access(&self, work_dir: &str) -> bool {
        access_settings::migrate_access_file(work_dir)
    }
}

// ------------------------------------------------------------------------------------------------ chat threads and jobs

pub const THREADS_FOLDER_NAME: &str = "chat";
pub const THREAD_FILE_EXTENSION: &str = ".json";
pub const MAX_MESSAGES_PER_THREAD: usize = 200;
pub const MAX_KEPT_JOBS: usize = 100;
const THREAD_ID_PREFIX: &str = "t-";
const JOB_ID_PREFIX: &str = "j-";
pub const JOB_STATE_RUNNING: &str = "running";
pub const THREAD_BUSY_REASON: &str = "thread_busy";
pub const TOTAL_LIMIT_REASON: &str = "total_limit";
pub const PROJECT_ID_INTERNAL_KEY: &str = "_project_id";
static THREAD_ID_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"\At-[0-9a-f]{12}\z"));

pub fn generate_thread_id() -> String {
    format!("{THREAD_ID_PREFIX}{}", crate::infrastructure::ids::random_hex(6))
}

/// One thread per file in `<work>/.specs/project/loop/internal/chat/<thread>.json` (atomic write, up to 200 messages; keeps the claude
/// `session_id`, which the API never returns).
#[derive(Clone, Copy, Debug, Default)]
pub struct ChatRepository;

impl ChatRepository {
    pub fn get_threads_dir(&self, work_dir: &str) -> PathBuf {
        get_loop_internal_dir(work_dir).join(THREADS_FOLDER_NAME)
    }

    /// `None` for an id outside the `t-<12 hex>` format (never builds a path from free text).
    pub fn get_thread_path(&self, work_dir: &str, thread_id: &str) -> Option<PathBuf> {
        THREAD_ID_PATTERN.is_match(thread_id).then(|| self.get_threads_dir(work_dir).join(format!("{thread_id}{THREAD_FILE_EXTENSION}")))
    }

    /// Thread object, or `None` when it does not exist, the id is invalid or the file does not have the expected format.
    pub fn read_thread(&self, work_dir: &str, thread_id: &str) -> Option<Map<String, Value>> {
        let data = self.get_thread_path(work_dir, thread_id).map(|p| read_json_object(&p)).unwrap_or_default();
        if data.get("thread").and_then(Value::as_str) != Some(thread_id) || !data.get("messages").is_some_and(Value::is_array) {
            return None;
        }
        Some(data)
    }

    pub fn write_thread(&self, work_dir: &str, thread: &mut Map<String, Value>) {
        if let Some(Value::Array(messages)) = thread.get_mut("messages") {
            if messages.len() > MAX_MESSAGES_PER_THREAD {
                messages.drain(..messages.len() - MAX_MESSAGES_PER_THREAD);
            }
        }
        thread.insert("updated_at".into(), json!(now_iso()));
        if let Some(path) = thread.get("thread").and_then(Value::as_str).and_then(|id| self.get_thread_path(work_dir, id)) {
            let _ = write_json_atomic(&path, &Value::Object(thread.clone()));
        }
    }

    pub fn list_threads(&self, work_dir: &str) -> Vec<Map<String, Value>> {
        let Ok(entries) = std::fs::read_dir(self.get_threads_dir(work_dir)) else { return vec![] };
        entries
            .flatten()
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(THREAD_FILE_EXTENSION).map(String::from))
            .filter_map(|id| self.read_thread(work_dir, &id))
            .collect()
    }
}

/// Jobs of every project of the app (the concurrency limit is the app's: each job is a `claude -p`). They live in the process memory.
#[derive(Default)]
pub struct ChatJobRepository {
    jobs: Mutex<Vec<(String, Map<String, Value>)>>,
}

impl ChatJobRepository {
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks the limits and records the job as `running` in one step. `Ok(job_id)` or `Err(reason)`.
    pub fn reserve_job(&self, project_id: &str, thread_id: Option<&str>, total_limit: usize, initial_fields: Option<&Map<String, Value>>) -> Result<String, &'static str> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let running = |job: &Map<String, Value>| job.get("state").and_then(Value::as_str) == Some(JOB_STATE_RUNNING);
        if let Some(thread) = thread_id {
            if jobs.iter().any(|(_, job)| running(job) && job.get("thread").and_then(Value::as_str) == Some(thread)) {
                return Err(THREAD_BUSY_REASON);
            }
        }
        if jobs.iter().filter(|(_, job)| running(job)).count() >= total_limit {
            return Err(TOTAL_LIMIT_REASON);
        }
        let job_id = format!("{JOB_ID_PREFIX}{}", crate::infrastructure::ids::random_hex(6));
        let mut job = Map::new();
        job.insert("state".into(), json!(JOB_STATE_RUNNING));
        job.insert("reply".into(), Value::Null);
        job.insert("cost_usd".into(), Value::Null);
        job.insert("error".into(), Value::Null);
        job.insert("thread".into(), thread_id.map(Value::from).unwrap_or(Value::Null));
        for (key, value) in initial_fields.into_iter().flatten() {
            job.insert(key.clone(), value.clone());
        }
        job.insert(PROJECT_ID_INTERNAL_KEY.into(), json!(project_id));
        jobs.push((job_id.clone(), job));
        // keeps at most MAX_KEPT_JOBS, dropping the oldest finished ones
        while jobs.len() > MAX_KEPT_JOBS {
            match jobs.iter().position(|(_, job)| !running(job)) {
                Some(oldest) => {
                    jobs.remove(oldest);
                }
                None => break,
            }
        }
        Ok(job_id)
    }

    pub fn finish_job(&self, job_id: &str, result: &Map<String, Value>) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let position = jobs.iter().position(|(id, _)| id == job_id);
        let project_id = position.and_then(|p| jobs[p].1.get(PROJECT_ID_INTERNAL_KEY).cloned()).unwrap_or(Value::Null);
        let mut job = result.clone();
        job.insert(PROJECT_ID_INTERNAL_KEY.into(), project_id);
        match position {
            Some(p) => jobs[p].1 = job,
            None => jobs.push((job_id.to_string(), job)),
        }
    }

    /// Copy of the job with `project_id` (internal field), or `None`.
    pub fn get_job(&self, job_id: &str) -> Option<Map<String, Value>> {
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let (_, job) = jobs.iter().find(|(id, _)| id == job_id)?;
        let mut copy: Map<String, Value> = job.iter().filter(|(k, _)| k.as_str() != PROJECT_ID_INTERNAL_KEY).map(|(k, v)| (k.clone(), v.clone())).collect();
        copy.insert("project_id".into(), job.get(PROJECT_ID_INTERNAL_KEY).cloned().unwrap_or(Value::Null));
        Some(copy)
    }

    pub fn count_running_jobs(&self) -> usize {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, j)| j.get("state").and_then(Value::as_str) == Some(JOB_STATE_RUNNING)).count()
    }

    pub fn has_running_job_in_thread(&self, thread_id: &str) -> bool {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(_, j)| j.get("state").and_then(Value::as_str) == Some(JOB_STATE_RUNNING) && j.get("thread").and_then(Value::as_str) == Some(thread_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::errors::Kind;

    #[test]
    fn subprojects_entries_roundtrip_and_errors_are_values() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let repository = SubprojectsFileRepository::default();
        assert_eq!(repository.read_subprojects(&work), Read::Absent);
        repository.write_repo_entry(&work, "api", &json!({"role": "back"})).unwrap();
        repository.write_repo_entry(&work, "web", &json!({"role": "front"})).unwrap();
        assert!(matches!(repository.read_repo_entry(&work, "api"), Read::Data(d) if d["role"] == "back"));
        assert_eq!(repository.read_repo_entry(&work, "none"), Read::Absent);
        let text = std::fs::read_to_string(SubprojectsFileRepository::subprojects_path(&work)).unwrap();
        assert!(text.starts_with("{\n  \"version\": 1,\n  \"repos\": {") && text.ends_with("}\n"));
        std::fs::write(SubprojectsFileRepository::subprojects_path(&work), "[1]").unwrap();
        assert!(matches!(repository.read_subprojects(&work), Read::Error(e) if e.error == "the file does not hold a JSON object"));
        std::fs::write(SubprojectsFileRepository::subprojects_path(&work), "{\"repos\": 1}").unwrap();
        assert!(matches!(repository.read_subprojects(&work), Read::Error(e) if e.error == "the file has no `repos` object"));
        repository.write_repo_entry(&work, "api", &json!({})).unwrap();
        assert!(matches!(repository.read_subprojects(&work), Read::Data(_)));
        assert!(!repository.delete_suggested(&work));
        repository.write_map_queue(&work, &json!({"queue": []})).unwrap();
        assert!(matches!(repository.read_map_queue(&work), Read::Data(_)));
    }

    #[test]
    fn env_values_are_replaced_in_place_and_never_echoed() {
        assert_eq!(format_env_line("A", "x y").unwrap(), "A='x y'");
        assert_eq!(format_env_line("A", "it's x").unwrap(), "A=\"it's x\"");
        assert_eq!(format_env_line("A", "plain").unwrap(), "A=plain");
        for bad in ["", "  ", "a\nb", "it's \"x\" y"] {
            let error = format_env_line("SECRET_NAME", bad).unwrap_err();
            assert_eq!(error.kind, Kind::InvalidRequest);
            assert!(!error.message.contains(bad) || bad.trim().is_empty());
        }
        let text = "\u{feff}# keep\r\nA=old\r\nB=2\r\nA=older";
        let out = replace_env_lines(text, &[("A".into(), "A=new".into()), ("C".into(), "C=3".into())]);
        assert_eq!(out, "\u{feff}# keep\r\nA=old\r\nB=2\r\nA=new\r\nC=3\r\n");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".specs").join("project").join(".env.test");
        write_env_values(&path, &[("TOKEN".into(), "abc".into())]).unwrap();
        write_env_values(&path, &[("TOKEN".into(), "def".into()), ("OTHER".into(), "x".into())]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "TOKEN=def\nOTHER=x\n");
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(write_env_values(&path, &[("A".into(), "b".into())]).is_err());
    }

    #[test]
    fn chat_threads_and_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let chat = ChatRepository;
        assert!(chat.get_thread_path(&work, "../x").is_none() && chat.read_thread(&work, "t-000000000000").is_none());
        let id = generate_thread_id();
        let mut thread = Map::from_iter([("thread".to_string(), json!(id)), ("messages".to_string(), Value::Array((0..250).map(|i| json!({"i": i})).collect()))]);
        chat.write_thread(&work, &mut thread);
        let read = chat.read_thread(&work, &id).unwrap();
        assert_eq!((read["messages"].as_array().unwrap().len(), read["messages"][0]["i"].clone()), (200, json!(50)));
        assert_eq!(chat.list_threads(&work).len(), 1);
        let jobs = ChatJobRepository::new();
        let first = jobs.reserve_job("p1", Some(&id), 2, None).unwrap();
        assert_eq!(jobs.reserve_job("p1", Some(&id), 2, None), Err(THREAD_BUSY_REASON));
        let second = jobs.reserve_job("p2", None, 2, Some(&Map::from_iter([("kind".to_string(), json!("map"))]))).unwrap();
        assert_eq!(jobs.reserve_job("p3", None, 2, None), Err(TOTAL_LIMIT_REASON));
        assert_eq!((jobs.count_running_jobs(), jobs.has_running_job_in_thread(&id)), (2, true));
        jobs.finish_job(&first, &Map::from_iter([("state".to_string(), json!("done")), ("reply".to_string(), json!("ok"))]));
        let job = jobs.get_job(&first).unwrap();
        assert_eq!((job["state"].clone(), job["project_id"].clone(), job.contains_key(PROJECT_ID_INTERNAL_KEY)), (json!("done"), json!("p1"), false));
        assert_eq!(jobs.get_job(&second).unwrap()["kind"], "map");
        assert!(jobs.get_job("j-nope").is_none() && !jobs.has_running_job_in_thread(&id));
    }
}
