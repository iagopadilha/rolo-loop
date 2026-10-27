// SPDX-License-Identifier: MIT
//! Access to the files of `.specs/` and of a project root (read, atomic write, create without overwriting).
//!
//! Every read/write coming from a path given by the user goes through [`SpecsFileRepository::resolve_path_inside_specs`]: it refuses an
//! absolute path, `..`, a drive (C:) and a symbolic link in the middle of the path, and checks with realpath that the target is inside `.specs/`.
//! The loop layout and the ROADMAP parser come from the engine (`loop_state`), never rewritten here.

use crate::domain::errors::{AppError, Kind};
use engine_state::{loop_io, loop_state};
use pycompat::json::py_json_dumps_indent;
use pycompat::{nt_isabs, nt_normcase, nt_realpath};
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

pub const REPLACE_RETRIES: u32 = 20; // x REPLACE_RETRY_SECONDS: up to ~1 s for a reader to let go of the file
pub const REPLACE_RETRY_SECONDS: f64 = 0.05;

pub fn compute_bytes_version(content: &[u8]) -> String {
    Sha1::digest(content).iter().map(|b| format!("{b:02x}")).collect()
}

fn outside(message: &str) -> AppError {
    AppError::new(Kind::PathOutsideSpecs, message)
}

/// Parts of the relative path (separator `/` or `\`). Errors for absolute, drive, `..` and NUL.
pub fn split_relative_path(relative: &str) -> Result<Vec<String>, AppError> {
    if relative.trim().is_empty() || relative.contains('\0') {
        return Err(outside("empty or invalid path"));
    }
    let text = relative.trim().replace('\\', "/");
    if text.starts_with('/') || nt_isabs(relative) || text.contains(':') {
        return Err(outside("absolute paths are not accepted: use the path relative to .specs/"));
    }
    let parts: Vec<String> = text.split('/').filter(|p| !p.is_empty() && *p != ".").map(String::from).collect();
    if parts.is_empty() {
        return Err(outside("empty or invalid path"));
    }
    if parts.iter().any(|p| p == "..") {
        return Err(outside("paths with `..` are not accepted"));
    }
    Ok(parts)
}

/// Symbolic link or Windows junction (`mklink /J`, which does not need an administrator).
pub fn is_link_or_junction(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return false };
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const REPARSE_POINT: u32 = 0x400;
        return meta.file_attributes() & REPARSE_POINT != 0;
    }
    #[allow(unreachable_code)]
    false
}

pub fn is_inside_folder(root: &str, target: &str) -> bool {
    let root = nt_normcase(&nt_realpath(root));
    let target = nt_normcase(&nt_realpath(target));
    let root = root.trim_end_matches('\\');
    target.trim_end_matches('\\') == root || target.starts_with(&format!("{root}\\"))
}

/// Problems of TEST-ENVIRONMENT.md from the engine preflight function, without rewriting the rule: empty = not required (no
/// `regression=yes`/`--e2e`) or complete.
pub fn list_test_environment_problems(work_dir: &str, with_e2e: bool) -> Vec<String> {
    let mut opts = engine_state::loop_opts::LoopOpts::new(work_dir);
    opts.e2e = with_e2e;
    engine_state::preflight_checks::test_environment_problems(work_dir, &opts)
}

#[derive(Default)]
pub struct SpecsFileRepository {
    write_lock: Mutex<()>,
}

impl SpecsFileRepository {
    pub fn new() -> Self {
        Self::default()
    }

    // ------------------------------------------------ safe path
    /// Absolute path of `relative` inside `specs_dir`, or an error.
    pub fn resolve_path_inside_specs(&self, specs_dir: &str, relative: &str) -> Result<PathBuf, AppError> {
        let mut current = PathBuf::from(specs_dir);
        for part in split_relative_path(relative)? {
            current.push(part);
            if is_link_or_junction(&current) {
                return Err(outside("symbolic links (or junctions) inside .specs/ are not accepted"));
            }
        }
        if !is_inside_folder(specs_dir, &current.display().to_string()) {
            return Err(outside("path outside .specs/"));
        }
        Ok(current)
    }

    // ------------------------------------------------ text
    /// Raw content or `None` (missing/unreadable/folder).
    pub fn read_bytes(&self, path: &Path) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    pub fn read_text(&self, path: &Path) -> Option<String> {
        self.read_bytes(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn write_text_atomic(&self, path: &Path, text: &str) -> std::io::Result<()> {
        self.write_bytes_atomic(path, text.as_bytes())
    }

    pub fn write_bytes_atomic(&self, path: &Path, content: &[u8]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = PathBuf::from(format!("{}.{:?}.tmp", path.display(), std::thread::current().id()).replace(['(', ')'], ""));
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::write(&temporary, content)?;
        for attempt in 0..REPLACE_RETRIES {
            // Windows: the replace fails while a reader (a poll) holds the target open
            match std::fs::rename(&temporary, path) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied && attempt + 1 < REPLACE_RETRIES => {
                    std::thread::sleep(Duration::from_secs_f64(REPLACE_RETRY_SECONDS))
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// True if it created the file; false if it already existed (never overwrites).
    pub fn create_file_if_missing(&self, path: &Path, text: &str) -> std::io::Result<bool> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut handle) => handle.write_all(text.as_bytes()).map(|_| true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn append_text(&self, path: &Path, text: &str) -> std::io::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(text.as_bytes())
    }

    // ------------------------------------------------ json
    pub fn read_json(&self, path: &Path) -> Map<String, Value> {
        super::loop_command_file::read_json_object(path)
    }

    pub fn write_json_atomic(&self, path: &Path, data: &Value) -> std::io::Result<()> {
        self.write_bytes_atomic(path, py_json_dumps_indent(data, false, 2).as_bytes())
    }

    pub fn list_json_files(&self, folder: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(folder) else { return vec![] };
        let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names.into_iter().filter(|n| n.ends_with(".json")).map(|n| folder.join(n)).filter(|p| p.is_file()).collect()
    }

    // ------------------------------------------------ tree
    /// `.md` paths relative to `folder` (separator `/`), without following symbolic links and without the ignored folders (compared by
    /// relative path, e.g. `project/loop/internal`).
    pub fn list_markdown_files(&self, folder: &Path, ignored_dirs: &[&str]) -> Vec<String> {
        if !folder.is_dir() {
            return vec![];
        }
        let ignored: Vec<String> = ignored_dirs.iter().map(|i| i.trim_matches('/').to_string()).collect();
        let mut found = Vec::new();
        self.walk_markdown(folder, "", &ignored, &mut found);
        found
    }

    fn walk_markdown(&self, current: &Path, relative: &str, ignored: &[String], found: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(current) else { return };
        let (mut folders, mut files): (Vec<String>, Vec<String>) = (vec![], vec![]);
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if path.is_dir() && !is_link_or_junction(&path) {
                if !ignored.contains(&format!("{relative}/{name}").trim_matches('/').to_string()) {
                    folders.push(name);
                }
            } else if !path.is_dir() {
                files.push(name);
            }
        }
        files.sort();
        folders.sort();
        for name in files {
            if name.to_lowercase().ends_with(".md") && !is_link_or_junction(&current.join(&name)) {
                found.push(format!("{relative}/{name}").trim_matches('/').to_string());
            }
        }
        for name in folders {
            self.walk_markdown(&current.join(&name), format!("{relative}/{name}").trim_matches('/'), ignored, found);
        }
    }

    pub fn get_file_info(&self, path: &Path) -> Value {
        let Ok(info) = std::fs::metadata(path) else { return json!({"size": null, "modified_at": null}) };
        let modified = info
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| loop_io::local_from_epoch(d.as_secs() as i64))
            .map(loop_io::iso_seconds);
        json!({"size": info.len(), "modified_at": modified})
    }

    // ------------------------------------------------ loop
    /// PID written in the engine's `LOOP.lock` (0 = no lock, unreadable, or a reused pid). Whether it is alive is the process runner's call.
    pub fn read_loop_lock_pid(&self, work_dir: &str) -> i64 {
        loop_io::lock_owner_pid(&loop_state::loop_path(work_dir, "LOOP.lock"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_confined_to_specs() {
        let dir = tempfile::tempdir().unwrap();
        let specs = dir.path().join(".specs");
        std::fs::create_dir_all(specs.join("features")).unwrap();
        let repo = SpecsFileRepository::new();
        let specs_text = specs.display().to_string();
        let ok = repo.resolve_path_inside_specs(&specs_text, "features\\a/b.md").unwrap();
        assert!(ok.ends_with("features\\a\\b.md") || ok.ends_with("features/a/b.md"));
        for bad in ["", "  ", "/etc", "C:\\x", "a:b", "../x", "a/../b", "a\0b"] {
            assert_eq!(repo.resolve_path_inside_specs(&specs_text, bad).unwrap_err().kind, Kind::PathOutsideSpecs, "{bad:?}");
        }
        assert_eq!(compute_bytes_version(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn writes_listings_and_info() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SpecsFileRepository::new();
        let file = dir.path().join("a").join("one.md");
        repo.write_text_atomic(&file, "x").unwrap();
        assert_eq!(repo.create_file_if_missing(&file, "y").unwrap(), false);
        assert_eq!(repo.create_file_if_missing(&dir.path().join("b").join("two.MD"), "y").unwrap(), true);
        repo.append_text(&dir.path().join("b").join("two.MD"), "z").unwrap();
        assert_eq!(repo.read_text(&dir.path().join("b").join("two.MD")).as_deref(), Some("yz"));
        repo.write_json_atomic(&dir.path().join("j.json"), &json!({"é": 1})).unwrap();
        assert_eq!(repo.read_text(&dir.path().join("j.json")).as_deref(), Some("{\n  \"é\": 1\n}"));
        assert_eq!(repo.list_json_files(dir.path()).len(), 1);
        std::fs::create_dir_all(dir.path().join("skip").join("deep")).unwrap();
        std::fs::write(dir.path().join("skip").join("deep").join("no.md"), "").unwrap();
        std::fs::write(dir.path().join("top.md"), "").unwrap();
        assert_eq!(repo.list_markdown_files(dir.path(), &["skip/deep"]), vec!["top.md", "a/one.md", "b/two.MD"]);
        assert!(repo.get_file_info(&file)["size"] == 1 && repo.get_file_info(&dir.path().join("none"))["size"].is_null());
        assert_eq!(repo.read_loop_lock_pid(&dir.path().display().to_string()), 0);
        assert!(!repo.read_json(&dir.path().join("j.json")).is_empty());
    }
}
