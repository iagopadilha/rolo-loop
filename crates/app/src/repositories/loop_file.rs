// SPDX-License-Identifier: MIT
//! Reads the loop files of ONE project (`.specs/project/...` inside the `work_dir`). Read only; never writes.
//!
//! The layout (`loop/json`, `loop/internal`, turns, snapshots) comes from the engine's `loop_state`: the ROADMAP parser is not rewritten here.
//! Anything missing at the source becomes empty (`{}`, `""`, `[]`), never an error.

use super::loop_command_file::read_json_object;
use super::loop_read_formatting::parse_iso_date;
use cfg::Env;
use engine_state::{loop_io, loop_state, network_policy};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

pub const TEXT_CACHE_LIMIT: usize = 512;
pub const GIT_TIMEOUT_SECONDS: u64 = 10;
const PAUSE_FILE_NAME: &str = "PAUSE";
const ACTIONS_FILE_NAME: &str = "actions.log";
const GUARD_LOG_FILE_NAME: &str = "guard.log";
const EGRESS_LOG_FILE_NAME: &str = "egress.log"; // `<data dir>/egress.log`, shared by every project
const EGRESS_TAIL_BYTES: u64 = 512 * 1024;
const EGRESS_RULE: &str = "egress-runtime";
const NODE_PRELOAD_LAYER: &str = "node-preload";
pub const LOOP_WORKTREE_SUFFIX: &str = "-worktree-loop";
static TURN_FILE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"\A\d+\.json\z"));
static PATH_SPLIT: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\\/]"));
static QUERY_SPLIT: LazyLock<Re> = LazyLock::new(|| Re::new(r"[?#]"));

fn truthy(value: Option<&Value>) -> bool {
    engine_state::notify_truthy(value)
}

/// JSON objects of a .jsonl/.log file, one per line (unreadable lines are skipped). Missing file = `[]`.
pub fn read_json_lines(path: &Path) -> Vec<Map<String, Value>> {
    super::small_stores::read_json_lines(path)
}

/// Text of the last `limit` bytes of a file, starting at a line boundary (a cut first line is dropped). Unreadable = ''.
pub fn read_file_tail(path: &Path, limit: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut handle) = std::fs::File::open(path) else { return String::new() };
    let Ok(size) = handle.seek(SeekFrom::End(0)) else { return String::new() };
    let start = size.saturating_sub(limit);
    let mut data = Vec::new();
    if handle.seek(SeekFrom::Start(start)).is_err() || handle.read_to_end(&mut data).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&data).into_owned();
    if start > 0 {
        text.split_once('\n').map(|p| p.1.to_string()).unwrap_or_default()
    } else {
        text
    }
}

/// `time` of an egress.log line as local ISO text like guard.log's `t`; the Node preload writes UTC, the others local time.
pub fn egress_local_time(raw: &Map<String, Value>) -> String {
    let moment = raw.get("time").map(text_or_empty).unwrap_or_default();
    if raw.get("layer").and_then(Value::as_str) != Some(NODE_PRELOAD_LAYER) {
        return moment;
    }
    parse_iso_date(&moment).and_then(|m| loop_io::local_from_epoch(m.seconds.floor() as i64)).map(loop_io::iso_seconds).unwrap_or(moment)
}

fn text_or_empty(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other if truthy(Some(other)) => pycompat::json::py_json_dumps(other, false),
        _ => String::new(),
    }
}

fn plain_int(value: Option<&Value>) -> Option<i64> {
    value.filter(|v| v.is_i64() || v.is_u64()).and_then(Value::as_i64)
}

/// One egress.log line of a project in `names` as a guard-line object, or `None` (other project, no host). The detail is
/// `<METHOD or CONNECT> <host>:<port><path> - <reason>`; the query of the path is cut. The log holds no headers.
pub fn build_egress_item(raw: &Map<String, Value>, names: &[String]) -> Option<Value> {
    let project_text = raw.get("project").map(text_or_empty).unwrap_or_default();
    let project = PATH_SPLIT.split(project_text.trim()).into_iter().last().unwrap_or_default();
    let host = raw.get("host").map(text_or_empty).unwrap_or_default().trim().to_string();
    if host.is_empty() || !names.contains(&project) {
        return None;
    }
    let where_ = match plain_int(raw.get("port")) {
        Some(port) => format!("{host}:{port}"),
        None => host,
    };
    let path_text = raw.get("path").map(text_or_empty).unwrap_or_default();
    let path = QUERY_SPLIT.split(&path_text).into_iter().next().unwrap_or_default();
    let reason = raw.get("reason").map(text_or_empty).unwrap_or_default().trim().to_string();
    let method = raw.get("method").map(text_or_empty).unwrap_or_default().trim().to_uppercase();
    let turn = plain_int(raw.get("turn")).filter(|t| *t > 0);
    Some(json!({
        "t": egress_local_time(raw),
        "turn": turn,
        "rule": EGRESS_RULE,
        "decision": "blocked",
        "detail": format!("{} {where_}{path}{}", if method.is_empty() { "CONNECT" } else { &method }, if reason.is_empty() { String::new() } else { format!(" - {reason}") }),
    }))
}

type TextCache = BTreeMap<String, ((u128, u64), Value)>;

/// Reads ROADMAP, STATE, LOOP.json, meta.json, turns, snapshots, events and actions.log of a work folder.
pub struct LoopFileRepository {
    env: Env,
    process_checker: Box<dyn Fn(i64) -> bool + Send + Sync>,
    text_cache: Mutex<TextCache>,
    commit_cache: Mutex<BTreeMap<String, Value>>,
}

fn stamp_of(path: &Path) -> Option<(u128, u64)> {
    let info = std::fs::metadata(path).ok()?;
    Some((info.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos(), info.len()))
}

impl LoopFileRepository {
    pub fn new(env: Env) -> Self {
        Self::with_process_checker(env, Box::new(loop_io::pid_alive))
    }

    pub fn with_process_checker(env: Env, process_checker: Box<dyn Fn(i64) -> bool + Send + Sync>) -> Self {
        LoopFileRepository { env, process_checker, text_cache: Mutex::new(BTreeMap::new()), commit_cache: Mutex::new(BTreeMap::new()) }
    }

    // ------------------------------------------------ generic reading
    pub fn read_json(&self, path: &Path) -> Map<String, Value> {
        read_json_object(path)
    }

    pub fn read_text(&self, path: &Path) -> String {
        loop_state::read(path)
    }

    /// Text of the file, re-read only when (mtime, size) changes; missing = ''.
    pub fn read_cached_text(&self, path: &Path) -> String {
        let Some(stamp) = stamp_of(path) else { return String::new() };
        let key = path.display().to_string();
        if let Some((stored, Value::String(text))) = self.text_cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            if *stored == stamp {
                return text.clone();
            }
        }
        let text = loop_state::read(path);
        let mut cache = self.text_cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > TEXT_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, (stamp, Value::String(text.clone())));
        text
    }

    pub fn file_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    /// Modification time in epoch seconds.
    pub fn get_mtime(&self, path: &Path) -> Option<f64> {
        Some(std::fs::metadata(path).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64())
    }

    // ------------------------------------------------ loop state
    pub fn read_loop_json(&self, work_dir: &str) -> Map<String, Value> {
        self.read_json(&loop_state::state_file(work_dir, "LOOP.json"))
    }

    pub fn read_meta(&self, work_dir: &str) -> Map<String, Value> {
        self.read_json(&loop_state::loop_path(work_dir, "meta.json"))
    }

    pub fn read_loop_command(&self, work_dir: &str) -> Map<String, Value> {
        self.read_json(&loop_state::loop_path(work_dir, "LOOP-CMD.json"))
    }

    pub fn get_lock_path(&self, work_dir: &str) -> PathBuf {
        loop_state::loop_path(work_dir, "LOOP.lock")
    }

    pub fn read_lock_pid(&self, work_dir: &str) -> i64 {
        loop_io::lock_owner_pid(&self.get_lock_path(work_dir))
    }

    pub fn is_process_alive(&self, pid: i64) -> bool {
        (self.process_checker)(pid)
    }

    pub fn is_pause_requested(&self, work_dir: &str) -> bool {
        loop_state::loop_path(work_dir, PAUSE_FILE_NAME).exists()
    }

    pub fn read_roadmap(&self, work_dir: &str) -> String {
        loop_state::read(&Path::new(work_dir).join(".specs").join("project").join("ROADMAP.md"))
    }

    /// STATE.md, VALIDATE.md, ANSWERS.md... at the place the engine uses.
    pub fn read_state_file(&self, work_dir: &str, name: &str) -> String {
        loop_state::read(&loop_state::state_file(work_dir, name))
    }

    pub fn read_roadmap_approval(&self, work_dir: &str, primary_repo: Option<&str>) -> Value {
        loop_state::roadmap_approval_for_run(work_dir, primary_repo.unwrap_or(""))
    }

    pub fn read_feature_spec(&self, work_dir: &str, slug: &str) -> String {
        self.read_cached_text(&Path::new(&loop_state::feature_dir(work_dir, slug)).join("spec.md"))
    }

    /// First lines of spec.md (only for the title); missing = `[]`.
    pub fn read_feature_spec_head(&self, work_dir: &str, slug: &str, max_lines: usize) -> Vec<String> {
        let text = loop_state::read(&Path::new(&loop_state::feature_dir(work_dir, slug)).join("spec.md"));
        pycompat::splitlines_keepends(&text).into_iter().take(max_lines).map(String::from).collect()
    }

    pub fn read_stage_closing(&self, work_dir: &str, number: u64) -> String {
        self.read_cached_text(&Path::new(work_dir).join(".specs").join("project").join("phases").join(format!("stage-{number:02}.md")))
    }

    // ------------------------------------------------ turns and snapshots
    pub fn list_turn_numbers(&self, work_dir: &str) -> Vec<u64> {
        let Ok(entries) = std::fs::read_dir(loop_state::turns_dir(work_dir)) else { return vec![] };
        let mut numbers: Vec<u64> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| TURN_FILE_PATTERN.is_match(n))
            .filter_map(|n| n[..n.len() - 5].parse().ok())
            .collect();
        numbers.sort();
        numbers
    }

    pub fn get_turn_path(&self, work_dir: &str, number: u64) -> PathBuf {
        loop_state::turns_dir(work_dir).join(format!("{number:03}.json"))
    }

    pub fn read_turn(&self, work_dir: &str, number: u64) -> Map<String, Value> {
        self.read_json(&self.get_turn_path(work_dir, number))
    }

    pub fn load_snapshot(&self, work_dir: &str, number: u64) -> Option<Map<String, Value>> {
        loop_state::load_snapshot(work_dir, number)
    }

    pub fn read_snapshot_loop_json(&self, snapshot: Option<&Map<String, Value>>) -> Map<String, Value> {
        snapshot.map(|s| self.read_json(&Path::new(s.get("dir").and_then(Value::as_str).unwrap_or("")).join("LOOP.json"))).unwrap_or_default()
    }

    pub fn read_snapshot_roadmap(&self, snapshot: Option<&Map<String, Value>>) -> String {
        snapshot.map(|s| loop_state::read(&Path::new(s.get("dir").and_then(Value::as_str).unwrap_or("")).join("ROADMAP.md"))).unwrap_or_default()
    }

    pub fn list_snapshot_numbers(&self, work_dir: &str) -> Vec<u64> {
        let Ok(entries) = std::fs::read_dir(loop_state::loop_path(work_dir, "snapshots")) else { return vec![] };
        let mut numbers: Vec<u64> = entries.flatten().filter_map(|e| e.file_name().to_string_lossy().parse().ok()).collect();
        numbers.sort();
        numbers
    }

    /// LOOP.json of snapshot NNN with an mtime cache; unreadable = `None`.
    pub fn read_cached_snapshot_loop_json(&self, work_dir: &str, number: u64) -> Option<Map<String, Value>> {
        let text = self.read_cached_text(&loop_state::snap_dir(work_dir, number).join("LOOP.json"));
        match serde_json::from_str::<Value>(if text.is_empty() { "{}" } else { &text }) {
            Ok(Value::Object(map)) => Some(map),
            Ok(_) => Some(Map::new()),
            Err(_) => None,
        }
    }

    /// Live agent events of the turn (`NNN.events.jsonl`, optional). Missing = empty list.
    pub fn read_turn_events(&self, work_dir: &str, number: u64) -> Vec<Map<String, Value>> {
        read_json_lines(&loop_state::turns_dir(work_dir).join(format!("{number:03}.events.jsonl")))
    }

    /// `git rev-list --count previous..current` (read only), cached; failure = `None`.
    pub fn count_commits_between(&self, work_dir: &str, previous_head: &str, current_head: &str) -> Option<u64> {
        let key = format!("count\0{work_dir}\0{previous_head}\0{current_head}");
        if let Some(cached) = self.commit_cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return cached.as_u64();
        }
        let range = format!("{previous_head}..{current_head}");
        let (code, out, _) = loop_io::run_timeout_raw("git", &["-C", work_dir, "rev-list", "--count", &range], None, GIT_TIMEOUT_SECONDS);
        let text = out.trim();
        let count = if code == 0 && !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) { text.parse::<u64>().ok() } else { None };
        self.commit_cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key, count.map(Value::from).unwrap_or(Value::Null));
        count
    }

    /// `[{"path","add","del"}]` of `git diff --numstat previous..current` (read only), cached; failure = `None`. A binary file counts 0/0.
    pub fn read_numstat_between(&self, work_dir: &str, previous_head: &str, current_head: &str) -> Option<Vec<Value>> {
        let key = format!("numstat\0{work_dir}\0{previous_head}\0{current_head}");
        if let Some(cached) = self.commit_cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return cached.as_array().cloned();
        }
        let args = ["-c", "core.quotepath=false", "-C", work_dir, "diff", "--numstat", "--no-renames", previous_head, current_head];
        let (code, out, _) = loop_io::run_timeout_raw("git", &args, None, GIT_TIMEOUT_SECONDS);
        let rows = (code == 0).then(|| {
            let number = |t: &str| if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) { t.parse::<u64>().unwrap_or(0) } else { 0 };
            pycompat::splitlines(&out)
                .into_iter()
                .filter_map(|line| {
                    let (added, rest) = line.split_once('\t').unwrap_or((line, ""));
                    let (removed, path) = rest.split_once('\t').unwrap_or((rest, ""));
                    (!path.is_empty()).then(|| json!({"path": path, "add": number(added), "del": number(removed)}))
                })
                .collect::<Vec<_>>()
        });
        self.commit_cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key, rows.clone().map(Value::Array).unwrap_or(Value::Null));
        rows
    }

    /// Turn whose `NNN.events.jsonl` holds the live agent events: the turn in progress (the one after the last finished turn) when its file
    /// exists, else the last finished turn; `None` without turns or events.
    pub fn get_live_events_turn(&self, work_dir: &str, numbers: &[u64]) -> Option<u64> {
        let following = numbers.last().copied().unwrap_or(0) + 1;
        if loop_state::turns_dir(work_dir).join(format!("{following:03}.events.jsonl")).is_file() {
            return Some(following);
        }
        numbers.last().copied()
    }

    // ------------------------------------------------ guard (written by the plugin's PreToolUse hook)
    /// JSON objects of `internal/guard.log` (`{t, profile, rule, decision, detail}` per line; unreadable lines are skipped) plus the run-time
    /// egress denials of this project (rule `egress-runtime`, same shape, merged by time). Re-read only when a file changes.
    pub fn read_guard_lines(&self, work_dir: &str) -> Vec<Value> {
        let text = self.read_cached_text(&loop_state::loop_path(work_dir, GUARD_LOG_FILE_NAME));
        let mut items: Vec<Value> = pycompat::splitlines(&text).into_iter().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(Value::is_object).collect();
        let egress = self.read_egress_lines(work_dir);
        if !egress.is_empty() {
            items.extend(egress);
            items.sort_by_key(|item| text_or_empty(item.get("t").unwrap_or(&Value::Null)));
            // stable: equal times keep the file order
        }
        items
    }

    /// Denials of THIS project in `<data dir>/egress.log`, as guard-line objects. Only the last `EGRESS_TAIL_BYTES` of the file are read, and
    /// again only when it changes; an unreadable line is skipped. The file holds one shared line list for every project: the `project` field of
    /// a line is the repo folder name (loop) or the folder name of the project root (app job), so a line matches the work folder's own name or
    /// that name without the `-worktree-loop` suffix. Missing file = `[]`.
    pub fn read_egress_lines(&self, work_dir: &str) -> Vec<Value> {
        let path = network_policy::default_data_dir(&self.env).join(EGRESS_LOG_FILE_NAME);
        let base = pycompat::last_segment(&pycompat::nt_abspath(work_dir)).to_string();
        let stripped = base.strip_suffix(LOOP_WORKTREE_SUFFIX).unwrap_or(&base).to_string();
        let mut names = vec![base.clone()];
        if stripped != base {
            names.push(stripped);
        }
        let Some(stamp) = stamp_of(&path) else { return vec![] };
        let key = format!("egress\0{}\0{}", path.display(), names.join("\0"));
        if let Some((stored, Value::Array(items))) = self.text_cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            if *stored == stamp {
                return items.clone();
            }
        }
        let items: Vec<Value> = pycompat::splitlines(&read_file_tail(&path, EGRESS_TAIL_BYTES))
            .into_iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|raw| raw.as_object().and_then(|o| build_egress_item(o, &names)))
            .collect();
        let mut cache = self.text_cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > TEXT_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, (stamp, Value::Array(items.clone())));
        items
    }

    // ------------------------------------------------ actions (written by the commands module)
    /// JSON objects of `internal/actions.log` (unreadable lines are skipped).
    pub fn read_action_lines(&self, work_dir: &str) -> Vec<Map<String, Value>> {
        read_json_lines(&loop_state::loop_path(work_dir, ACTIONS_FILE_NAME))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_snapshots_guard_and_egress() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let env = Env::from_pairs([("LOOP_DATA_DIR", data.to_str().unwrap())]);
        let work = dir.path().join("app-worktree-loop").display().to_string();
        let repository = LoopFileRepository::new(env);
        assert!(repository.read_loop_json(&work).is_empty() && repository.list_turn_numbers(&work).is_empty());
        let turns = loop_state::turns_dir(&work);
        std::fs::create_dir_all(&turns).unwrap();
        std::fs::write(turns.join("002.json"), r#"{"x": 1}"#).unwrap();
        std::fs::write(turns.join("010.json"), "{}").unwrap();
        std::fs::write(turns.join("010.events.jsonl"), "{\"a\": 1}\nbroken\n{\"b\": 2}\n").unwrap();
        std::fs::write(turns.join("note.json"), "{}").unwrap();
        let numbers = repository.list_turn_numbers(&work);
        assert_eq!(numbers, vec![2, 10]);
        assert_eq!(repository.read_turn(&work, 2)["x"], 1);
        assert_eq!(repository.read_turn_events(&work, 10).len(), 2);
        assert_eq!(repository.get_live_events_turn(&work, &numbers), Some(10));
        assert_eq!(repository.get_live_events_turn(&work, &[]), None);
        let guard = loop_state::loop_path(&work, "guard.log");
        std::fs::write(&guard, "{\"t\": \"2026-01-01T10:00:02\", \"rule\": \"a\"}\nnot json\n").unwrap();
        std::fs::write(data.join("egress.log"), "{\"time\": \"2026-01-01T10:00:01\", \"project\": \"C:\\\\x\\\\app\", \"host\": \"evil.example.com\", \"port\": 443, \"method\": \"get\", \"path\": \"/p?q=1\", \"reason\": \"not allowed\", \"turn\": 3}\n{\"project\": \"other\", \"host\": \"h\"}\n").unwrap();
        let lines = repository.read_guard_lines(&work);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            (lines[0]["rule"].clone(), lines[0]["detail"].clone(), lines[0]["turn"].clone()),
            (json!("egress-runtime"), json!("GET evil.example.com:443/p - not allowed"), json!(3))
        );
        assert_eq!(lines[1]["rule"], "a");
        assert_eq!(repository.read_cached_text(&guard), repository.read_text(&guard));
        assert!(repository.read_action_lines(&work).is_empty());
        assert_eq!(repository.count_commits_between(&work, "a", "b"), None);
        assert!(!repository.is_process_alive(0) && !repository.is_pause_requested(&work));
        assert_eq!(repository.read_feature_spec_head(&work, "x", 3), Vec::<String>::new());
    }
}
