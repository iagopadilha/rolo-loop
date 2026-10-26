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
    use std::io::Read;
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
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

/// `shutil.which`: the first `name` (with a PATHEXT extension on Windows) found on PATH.
pub fn which(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or(".COM;.EXE;.BAT;.CMD".into()).split(';').map(|e| e.to_lowercase()).collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
