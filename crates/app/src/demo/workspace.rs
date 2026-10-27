// SPDX-License-Identifier: MIT
//! Folders of one demo run, atomic file writes and the demo's own git repo.
//!
//! Layout (under a new temporary folder, or `<data-dir>/demo/`):
//!   acme-checkout/          the fake project (a real git repo with `.specs/`)
//!   claude-projects/        Claude Code style transcripts the demo app reads (`claude_transcripts_dir`)
//!   app-data/               the app data folder (`LOOP_DATA_DIR`) when no --data-dir was given

use super::catalog::PROJECT_NAME;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const TEMP_PREFIX: &str = "loop-harness-demo-";
const DEMO_FOLDER_NAME: &str = "demo";
const TRANSCRIPTS_FOLDER_NAME: &str = "claude-projects";
const APP_DATA_FOLDER_NAME: &str = "app-data";
const REPLACE_ATTEMPTS: u32 = 20;
const REPLACE_RETRY_MILLISECONDS: u64 = 50;
const DEMO_AUTHOR_NAME: &str = "Loop Harness demo";
const DEMO_AUTHOR_EMAIL: &str = "demo@example.invalid";

#[derive(Clone, Debug)]
pub struct DemoWorkspace {
    pub base_dir: PathBuf,
    pub app_data_dir: PathBuf,
    pub is_temporary: bool,
    pub repo_dir: PathBuf,
    pub transcripts_dir: PathBuf,
}

impl DemoWorkspace {
    fn new(base_dir: PathBuf, app_data_dir: PathBuf, is_temporary: bool) -> Self {
        DemoWorkspace { repo_dir: base_dir.join(PROJECT_NAME), transcripts_dir: base_dir.join(TRANSCRIPTS_FOLDER_NAME), base_dir, app_data_dir, is_temporary }
    }

    /// `data_dir` given: the demo lives in `<data_dir>/demo/` (recreated) and the app uses `data_dir`; otherwise everything goes into a new temporary
    /// folder.
    pub fn create(data_dir: &str) -> std::io::Result<Self> {
        if !data_dir.is_empty() {
            let data = PathBuf::from(pycompat::nt_realpath(&pycompat::nt_abspath(data_dir)));
            let base = data.join(DEMO_FOLDER_NAME);
            remove_tree(&base);
            std::fs::create_dir_all(&base)?;
            return Ok(Self::new(base, data, false));
        }
        let temporary = tempfile::Builder::new().prefix(TEMP_PREFIX).tempdir()?.keep();
        let base = PathBuf::from(pycompat::nt_realpath(&temporary.display().to_string()));
        Ok(Self::new(base.clone(), base.join(APP_DATA_FOLDER_NAME), true))
    }

    /// Deletes the demo folder (the whole temporary folder, or `<data-dir>/demo/`).
    pub fn remove(&self) {
        remove_tree(&self.base_dir);
    }
}

/// `rmtree` that also removes read-only files (git objects on Windows); a missing folder is fine.
pub fn remove_tree(path: &Path) {
    if !path.is_dir() {
        return;
    }
    if std::fs::remove_dir_all(path).is_ok() {
        return;
    }
    clear_read_only(path);
    let _ = std::fs::remove_dir_all(path);
}

fn clear_read_only(path: &Path) {
    let Ok(entries) = std::fs::read_dir(path) else { return };
    for entry in entries.flatten() {
        let target = entry.path();
        if let Ok(meta) = entry.metadata() {
            let mut permissions = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            let _ = std::fs::set_permissions(&target, permissions);
            if meta.is_dir() {
                clear_read_only(&target);
            }
        }
    }
}

/// Writes through a temporary file + rename, retrying while a reader holds the file (Windows).
pub fn write_text_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = PathBuf::from(format!("{}.{}.tmp", path.display(), std::process::id()));
    std::fs::write(&temporary, text.as_bytes())?;
    for attempt in 0..REPLACE_ATTEMPTS {
        match std::fs::rename(&temporary, path) {
            Ok(()) => return Ok(()),
            Err(error) if attempt + 1 == REPLACE_ATTEMPTS => return Err(error),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(REPLACE_RETRY_MILLISECONDS)),
        }
    }
    Ok(())
}

pub fn write_json_atomically(path: &Path, data: &Value) -> std::io::Result<()> {
    write_text_atomically(path, &pycompat::json::py_json_dumps_indent(data, false, 2))
}

pub fn append_json_line(path: &Path, data: &Value) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut handle = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    handle.write_all(format!("{}\n", pycompat::json::py_json_dumps(data, false)).as_bytes())
}

pub fn remove_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// git on the demo repo only (never the user's repos). Identity and signing are set in the demo repo's local config.
#[derive(Clone, Debug)]
pub struct DemoGitRepository {
    pub repo_dir: PathBuf,
}

impl DemoGitRepository {
    pub fn new(repo_dir: &Path) -> Self {
        DemoGitRepository { repo_dir: repo_dir.to_path_buf() }
    }

    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new("git").arg("-C").arg(&self.repo_dir).args(args).env("GIT_TERMINAL_PROMPT", "0").output().map_err(|e| format!("git cannot start: {e}"))?;
        if !output.status.success() {
            let text = String::from_utf8_lossy(if output.stderr.is_empty() { &output.stdout } else { &output.stderr }).trim().to_string();
            return Err(format!("git {} failed: {}", args.join(" "), text.chars().take(300).collect::<String>()));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    pub fn initialize(&self, branch: &str) -> Result<(), String> {
        std::fs::create_dir_all(&self.repo_dir).map_err(|e| e.to_string())?;
        self.run(&["init", "-q"])?;
        self.run(&["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")])?;
        for (key, value) in [("user.name", DEMO_AUTHOR_NAME), ("user.email", DEMO_AUTHOR_EMAIL), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
            self.run(&["config", key, value])?;
        }
        Ok(())
    }

    /// Adds the paths by name and commits; returns the short hash.
    pub fn commit_paths(&self, paths: &[String], message: &str) -> Result<String, String> {
        let mut add = vec!["add", "--"];
        add.extend(paths.iter().map(String::as_str));
        self.run(&add)?;
        self.run(&["commit", "-q", "-m", message])?;
        self.get_head(true)
    }

    pub fn create_branch(&self, branch: &str) -> Result<(), String> {
        self.run(&["checkout", "-q", "-b", branch]).map(|_| ())
    }

    pub fn get_head(&self, short: bool) -> Result<String, String> {
        if short { self.run(&["rev-parse", "--short=7", "HEAD"]) } else { self.run(&["rev-parse", "HEAD"]) }
    }

    /// Back to `head`, dropping the story's commits and files (ignored `.specs/` stays).
    pub fn reset_to(&self, head: &str) -> Result<(), String> {
        self.run(&["reset", "-q", "--hard", head])?;
        self.run(&["clean", "-q", "-f", "-d"]).map(|_| ())
    }
}
