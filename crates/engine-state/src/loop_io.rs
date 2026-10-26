// SPDX-License-Identifier: MIT
//! Small file, JSON, git and process helpers shared by the engine modules (port of `loop_io.py`).

use pycompat::json::py_json_dumps_indent;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// `(exit code, stdout stripped, stderr stripped)`; a spawn failure is `(1, "", message)`.
pub fn run(program: &str, args: &[&str], cwd: Option<&str>) -> (i32, String, String) {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    match cmd.output() {
        Ok(out) => (
            out.status.code().unwrap_or(1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ),
        Err(error) => (1, String::new(), error.to_string()),
    }
}

/// `git -C <cwd> <args...>`.
pub fn git(cwd: &str, args: &[&str]) -> (i32, String, String) {
    let mut full: Vec<&str> = vec!["-C", cwd];
    full.extend_from_slice(args);
    run("git", &full, None)
}

/// Like [`git`] but the output is NOT stripped (a `status --porcelain -z` stream starts with the space of the first status).
pub fn git_raw(cwd: &str, args: &[&str]) -> (i32, String, String) {
    let mut full: Vec<&str> = vec!["-C", cwd];
    full.extend_from_slice(args);
    match Command::new("git").args(&full).stdin(Stdio::null()).output() {
        Ok(out) => (out.status.code().unwrap_or(1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).trim().to_string()),
        Err(error) => (1, String::new(), error.to_string()),
    }
}

/// One entry of `git status --porcelain -z`.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusRow {
    pub status: String,
    pub path: String,
    pub origin: String,
}

impl std::fmt::Display for StatusRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.status, self.path)
    }
}

/// `git status --porcelain -z` output -> rows. Records are NUL separated `XY path`; a rename/copy is followed by one extra record.
pub fn parse_porcelain_z(text: &str) -> Vec<StatusRow> {
    let records: Vec<&str> = text.split('\0').collect();
    let mut rows = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        index += 1;
        let chars: Vec<char> = record.chars().collect();
        if chars.len() < 4 || chars[2] != ' ' {
            continue;
        }
        let status: String = chars[..2].iter().collect();
        let path: String = chars[3..].iter().collect();
        let mut origin = String::new();
        if (status.contains('R') || status.contains('C')) && index < records.len() {
            origin = records[index].to_string();
            index += 1;
        }
        rows.push(StatusRow { status, path, origin });
    }
    rows
}

/// `(exit code, rows, stderr)` of `git status --porcelain -z <arguments>` in `cwd`.
pub fn status_rows(cwd: &str, arguments: &[&str]) -> (i32, Vec<StatusRow>, String) {
    let mut args: Vec<&str> = vec!["status", "--porcelain", "-z"];
    args.extend_from_slice(arguments);
    let (code, out, err) = git_raw(cwd, &args);
    (code, if code == 0 { parse_porcelain_z(&out) } else { vec![] }, err)
}

/// Text of a file (UTF-8, invalid bytes replaced); empty when it cannot be read.
pub fn read(path: &Path) -> String {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

/// A JSON document, or `{}` when missing or invalid.
pub fn load_json(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| Value::Object(Default::default()))
}

pub const APP_OWNED_META_KEYS: [&str; 2] = ["delivery", "max_cost_usd"];

/// `os.replace` that retries a few times on Windows, where a reader holding the target open makes it fail.
pub fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    let attempts = 6;
    for attempt in 0..attempts {
        match std::fs::rename(source, target) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && cfg!(windows) && attempt + 1 < attempts => {
                std::thread::sleep(std::time::Duration::from_millis(50 * (attempt as u64 + 1)));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Writes `text` through a temporary file in the same folder (flush + fsync + replace): a reader sees the old file or the new one, never half.
pub fn atomic_write_text(path: &Path, text: &str) -> std::io::Result<()> {
    let folder = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = folder.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// `json.dumps(data, ensure_ascii=False, indent=2)` written atomically.
pub fn save_json(path: &Path, data: &Value) -> std::io::Result<()> {
    atomic_write_text(path, &py_json_dumps_indent(data, false, 2))
}

/// Saves meta.json keeping what the app wrote there meanwhile: the disk value of the app-owned keys wins over the in-memory copy.
pub fn save_meta(path: &Path, meta: &mut Value) -> std::io::Result<()> {
    let on_disk = load_json(path);
    if let (Value::Object(disk), Value::Object(mem)) = (&on_disk, &mut *meta) {
        for key in APP_OWNED_META_KEYS {
            if let Some(v) = disk.get(key) {
                mem.insert(key.to_string(), v.clone());
            }
        }
    }
    save_json(path, meta)
}

/// `<repo>/../<repo name>-worktree-loop`.
pub fn worktree_of(repo: &str) -> String {
    let abs = pycompat::nt_abspath(repo);
    let parent = pycompat::nt_dirname(&abs);
    let base = abs.rsplit('\\').next().unwrap_or("");
    pycompat::nt_join(&parent, &format!("{base}-worktree-loop"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn porcelain_records_with_renames() {
        let rows = parse_porcelain_z(" M a.txt\0R  new.txt\0old.txt\0?? x y.txt\0");
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[0].status.as_str(), rows[0].path.as_str()), (" M", "a.txt"));
        assert_eq!((rows[1].path.as_str(), rows[1].origin.as_str()), ("new.txt", "old.txt"));
        assert_eq!(rows[2].to_string(), "?? x y.txt");
    }

    #[test]
    fn atomic_write_and_meta_keep_app_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/meta.json");
        save_json(&path, &json!({"delivery": {"push": "yes"}, "turns": 1})).unwrap();
        let mut meta = json!({"turns": 2, "delivery": {"push": "no"}});
        save_meta(&path, &mut meta).unwrap();
        let back = load_json(&path);
        assert_eq!(back["turns"], 2);
        assert_eq!(back["delivery"]["push"], "yes");
        assert_eq!(load_json(&dir.path().join("missing.json")), json!({}));
    }

    #[test]
    fn worktree_path() {
        assert_eq!(worktree_of("C:\\work\\proj"), "C:\\work\\proj-worktree-loop");
    }
}

/// Like [`run`] with a time limit and stdout NOT stripped; a timeout kills the process and reads as `(1, "", message)`.
pub fn run_timeout_raw(program: &str, args: &[&str], cwd: Option<&str>, seconds: u64) -> (i32, String, String) {
    run_timeout_env(program, args, cwd, &[], seconds)
}

/// [`run_timeout_raw`] with extra environment variables for the child.
pub fn run_timeout_env(program: &str, args: &[&str], cwd: Option<&str>, env: &[(&str, &str)], seconds: u64) -> (i32, String, String) {
    use std::io::Read;
    let mut cmd = Command::new(program);
    cmd.args(args).envs(env.iter().copied()).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => return (1, String::new(), error.to_string()),
    };
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut bytes);
        }
        bytes
    });
    let err_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut bytes);
        }
        bytes
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(error) => return (1, String::new(), error.to_string()),
        }
    };
    let out = out_reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    match status {
        Some(s) => (s.code().unwrap_or(1), String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).trim().to_string()),
        None => (1, String::new(), format!("Command '{program}' timed out after {seconds} seconds")),
    }
}

/// Like [`run`] with a time limit.
pub fn run_timeout(program: &str, args: &[&str], cwd: Option<&str>, seconds: u64) -> (i32, String, String) {
    let (code, out, err) = run_timeout_raw(program, args, cwd, seconds);
    (code, out.trim().to_string(), err)
}

/// `shutil.which`: the first `name` (with a PATHEXT extension on Windows) found on PATH. Unlike Python's, the current directory is
/// never searched first (a repository's own `claude.cmd` must not run in place of the real one).
pub fn which(name: &str) -> Option<std::path::PathBuf> {
    which_in(name, &std::env::var_os("PATH")?)
}

/// [`which`] over an explicit PATH value (entries separated like the OS does).
pub fn which_in(name: &str, path: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or(".COM;.EXE;.BAT;.CMD".into()).split(';').filter(|e| !e.is_empty()).map(|e| e.to_lowercase()).collect()
    } else {
        vec![String::new()]
    };
    let lower = name.to_lowercase();
    let has_ext = cfg!(windows) && exts.iter().any(|e| lower.ends_with(e.as_str()));
    let suffixes: Vec<String> = if has_ext { std::iter::once(String::new()).chain(exts.iter().cloned()).collect() } else { exts };
    for dir in std::env::split_paths(path) {
        for ext in &suffixes {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// `datetime.now().isoformat(timespec="seconds")` in local time.
pub fn now_iso() -> String {
    time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]"))
        .unwrap_or_default()
}

// ---------------------------------------------------------------- processes and the loop lock

pub fn pid_alive(pid: i64) -> bool {
    pid > 0 && u32::try_from(pid).is_ok_and(winproc::pid_alive)
}

/// When the process started (see [`winproc::process_start_time`]); only comparable with the same function's value for the same pid.
pub fn process_start_time(pid: i64) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    u32::try_from(pid).ok().and_then(winproc::process_start_time)
}

/// `taskkill /T /F`: the process and its children.
pub fn kill_tree(pid: i64) {
    let _ = run("taskkill", &["/PID", &pid.to_string(), "/T", "/F"], None);
}

/// `LOOP.lock.start`: `{"pid", "started"}` next to the lock; LOOP.lock itself stays a plain PID (the panel reads it with int()).
pub const LOCK_START_SUFFIX: &str = ".start";
/// An empty lock younger than this is being written by the process that just created it.
pub const LOCK_FRESH_SECONDS: u64 = 5;

/// `(pid, started, empty)` of a lock file. pid 0 = no lock or unreadable; `started` comes from the sidecar and only when it is about
/// that same pid; `empty` = the file exists with no content yet.
pub fn read_lock_holder(path: &Path) -> (i64, Option<u64>, bool) {
    let Ok(bytes) = std::fs::read(path) else { return (0, None, false) };
    let text = String::from_utf8_lossy(&bytes).trim().to_string();
    if text.is_empty() {
        return (0, None, true);
    }
    let Ok(pid) = text.parse::<i64>() else { return (0, None, false) };
    let sidecar = std::path::PathBuf::from(format!("{}{LOCK_START_SUFFIX}", path.display()));
    let info = std::fs::read(sidecar).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let started = info.filter(|i| i.get("pid").and_then(Value::as_i64) == Some(pid)).and_then(|i| i.get("started").and_then(Value::as_u64));
    (pid, started, false)
}

/// PID of the lock file, or 0 when there is none or the pid now belongs to another process.
pub fn lock_owner_pid(path: &Path) -> i64 {
    let (pid, started, _) = read_lock_holder(path);
    if pid != 0 {
        if let (Some(then), Some(now)) = (started, process_start_time(pid)) {
            if now != then {
                return 0;
            }
        }
    }
    pid
}

/// True when the lock file names a running process that really is its owner.
pub fn lock_holder_alive(path: &Path) -> bool {
    let (pid, started, empty) = read_lock_holder(path);
    if empty {
        return std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs_f64() < LOCK_FRESH_SECONDS as f64);
    }
    if !pid_alive(pid) {
        return false;
    }
    if let (Some(then), Some(now)) = (started, process_start_time(pid)) {
        if now != then {
            return false;
        }
    }
    true
}

/// `shutil.copy2`: content and modification time (creating nothing above the target's folder).
pub fn copy2(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::copy(source, target)?;
    if let Ok(modified) = std::fs::metadata(source).and_then(|m| m.modified()) {
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(target) {
            let _ = file.set_modified(modified);
        }
    }
    Ok(())
}

/// `shutil.copytree(source, target, dirs_exist_ok=True, ignore=ignore_patterns(*ignore))`: copies with modification times, following
/// links, skipping any entry whose name is in `ignore`.
pub fn copy_tree(source: &Path, target: &Path, ignore: &[&str]) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if ignore.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let (from, to) = (entry.path(), target.join(&name));
        if from.is_dir() {
            copy_tree(&from, &to, ignore)?;
        } else {
            copy2(&from, &to)?;
        }
    }
    Ok(())
}

pub const DELIVERY_PENDING_NAME: &str = "delivery-pending.json";

/// The app wrote the delivery choices before the worktree existed (`<repo>/.specs/project/loop/internal/delivery-pending.json`): they
/// become `meta["delivery"]` and the pending file is deleted.
pub fn merge_pending_delivery(repo: &str, meta: &mut Value) {
    let path = crate::loop_state::internal_dir(repo).join(DELIVERY_PENDING_NAME);
    if let Value::Object(pending) = load_json(&path) {
        if !pending.is_empty() {
            if let Some(map) = meta.as_object_mut() {
                let mut delivery = map.get("delivery").and_then(Value::as_object).cloned().unwrap_or_default();
                for (k, v) in pending {
                    delivery.insert(k, v);
                }
                map.insert("delivery".into(), Value::Object(delivery));
            }
        }
    }
    let _ = std::fs::remove_file(path);
}

/// `datetime.datetime.now()`: local, naive.
pub fn local_now() -> time::PrimitiveDateTime {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    time::PrimitiveDateTime::new(now.date(), now.time())
}

/// `datetime.datetime.fromtimestamp(epoch)`: local, naive.
pub fn local_from_epoch(epoch: i64) -> Option<time::PrimitiveDateTime> {
    let moment = time::OffsetDateTime::from_unix_timestamp(epoch).ok()?;
    let local = time::UtcOffset::current_local_offset().map(|o| moment.to_offset(o)).unwrap_or(moment);
    Some(time::PrimitiveDateTime::new(local.date(), local.time()))
}

/// `moment.isoformat(timespec="seconds")`.
pub fn iso_seconds(moment: time::PrimitiveDateTime) -> String {
    moment.format(time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]")).unwrap_or_default()
}

/// Result of [`run_process`].
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Runs a command to the end like `subprocess.Popen(..., text=True).communicate(input, timeout)`: stdin from `stdin_text` (or closed),
/// stdout and stderr captured as UTF-8 text with universal newlines, the child in its own process group, and on timeout the whole tree is
/// killed and what was read so far returned. `env` replaces the child's whole environment.
pub fn run_process(argv: &[String], cwd: &str, env: &[(String, String)], stdin_text: &str, timeout_seconds: u64) -> ProcessResult {
    match run_process_checked(argv, cwd, env, stdin_text, timeout_seconds) {
        Ok(result) => result,
        Err(error) => ProcessResult { code: 1, stdout: String::new(), stderr: error.to_string(), timed_out: false },
    }
}

/// [`run_process`] that tells a program that could not start (`Err`) from one that ran and failed.
pub fn run_process_checked(argv: &[String], cwd: &str, env: &[(String, String)], stdin_text: &str, timeout_seconds: u64) -> std::io::Result<ProcessResult> {
    use std::io::Read;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(cwd).env_clear().envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    cmd.stdin(if stdin_text.is_empty() { Stdio::null() } else { Stdio::piped() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0200); // CREATE_NEW_PROCESS_GROUP
    }
    let mut child = cmd.spawn()?;
    let pid = i64::from(child.id());
    let writer = child.stdin.take().map(|mut stdin| {
        let text = stdin_text.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        })
    });
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut bytes);
        }
        bytes
    });
    let err_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut bytes);
        }
        bytes
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_seconds);
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(s)) => break (Some(s), false),
            Ok(None) if std::time::Instant::now() >= deadline => {
                kill_tree(pid);
                let _ = child.kill();
                break (child.wait().ok(), true);
            }
            Ok(None) if winproc::interrupted() => {
                // Ctrl+C: the loop stops cleanly after this turn, so the child must not outlive it
                kill_tree(pid);
                let _ = child.kill();
                break (child.wait().ok(), false);
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
            Err(_) => break (None, false),
        }
    };
    if let Some(w) = writer {
        let _ = w.join();
    }
    let decode = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).replace("\r\n", "\n").replace('\r', "\n");
    let stdout = decode(out_reader.join().unwrap_or_default());
    let stderr = decode(err_reader.join().unwrap_or_default());
    Ok(ProcessResult { code: status.and_then(|s| s.code()).unwrap_or(-1), stdout, stderr, timed_out })
}
