// SPDX-License-Identifier: MIT
//! Post-job file guard: a snapshot of a folder before a `claude -p` job and, after it, the undo of every change outside the allowed files
//! (created = deleted, changed or deleted = restored from the snapshot's in-memory copy).
//!
//! It is the second barrier of the planning chat: the first is the permission rules of the CLI (`Edit(/.specs/...)` in `--allowedTools` with
//! `--permission-mode dontAsk`), which the user's own settings can widen. Files larger than the per-file copy limit (or beyond the total limit)
//! are only compared, never copied: a change to one of them is reported as `unrestorable`. Symbolic links and junctions are skipped (never
//! followed nor copied). Never fails.

use pycompat::nt_normcase;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const BACKUP_FILE_LIMIT_BYTES: u64 = 1024 * 1024;
pub const BACKUP_TOTAL_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
/// Folder names that are never compared nor copied, at ANY depth (`frontend/node_modules`, `backend/bin`): build output and dependencies change
/// by themselves and can hold tens of thousands of files, which would make the snapshot take minutes.
pub const DEFAULT_IGNORED_NAMES: [&str; 10] = ["node_modules", "bin", "obj", "dist", "build", ".git", ".vs", ".next", "coverage", "__pycache__"];

fn normalize_key(relative: &str) -> String {
    nt_normcase(&relative.replace('\\', "/")).replace('\\', "/")
}

fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

#[derive(Clone, Debug)]
pub struct WorkspaceSnapshot {
    pub root: PathBuf,
    ignored: Vec<String>,
    ignored_names: BTreeSet<String>,
    /// relative path -> (size, mtime ns)
    pub files: BTreeMap<String, (u64, u128)>,
    pub folders: BTreeSet<String>,
    copies: BTreeMap<String, Vec<u8>>,
}

impl WorkspaceSnapshot {
    /// The in-memory copy taken for a relative path (`/` separator); `None` when the file was too big or did not exist.
    pub fn copy_of(&self, relative: &str) -> Option<&Vec<u8>> {
        self.copies.get(relative)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WorkspaceFileGuard {
    pub backup_file_limit_bytes: u64,
    pub backup_total_limit_bytes: u64,
}

impl Default for WorkspaceFileGuard {
    fn default() -> Self {
        WorkspaceFileGuard { backup_file_limit_bytes: BACKUP_FILE_LIMIT_BYTES, backup_total_limit_bytes: BACKUP_TOTAL_LIMIT_BYTES }
    }
}

impl WorkspaceFileGuard {
    /// `ignored_paths`: relative paths (`/` separator) of folders or files that are neither compared nor restored. `ignored_names`: folder or
    /// file names ignored at any depth (every path segment is checked); an ignored folder is never descended.
    pub fn take_snapshot(&self, root: &str, ignored_paths: &[&str], ignored_names: &[&str]) -> WorkspaceSnapshot {
        let mut snapshot = WorkspaceSnapshot {
            root: PathBuf::from(pycompat::nt_abspath(root)),
            ignored: ignored_paths.iter().map(|i| normalize_key(i).trim_matches('/').to_string()).collect(),
            ignored_names: ignored_names.iter().map(|n| normalize_key(n).trim_matches('/').to_string()).collect(),
            files: BTreeMap::new(),
            folders: BTreeSet::new(),
            copies: BTreeMap::new(),
        };
        let mut copied_bytes = 0u64;
        for (relative, path, size, mtime) in self.walk_files(&snapshot) {
            snapshot.files.insert(relative.clone(), (size, mtime));
            if size <= self.backup_file_limit_bytes && copied_bytes + size <= self.backup_total_limit_bytes {
                if let Ok(content) = std::fs::read(&path) {
                    copied_bytes += content.len() as u64;
                    snapshot.copies.insert(relative, content);
                }
            }
        }
        snapshot.folders = self.walk_folders(&snapshot).into_iter().collect();
        snapshot
    }

    fn is_ignored(&self, snapshot: &WorkspaceSnapshot, relative: &str) -> bool {
        let key = normalize_key(relative);
        if !snapshot.ignored_names.is_empty() && key.split('/').any(|segment| snapshot.ignored_names.contains(segment)) {
            return true;
        }
        snapshot.ignored.iter().any(|item| key == *item || key.starts_with(&format!("{item}/")))
    }

    fn relative(snapshot: &WorkspaceSnapshot, path: &Path) -> String {
        path.strip_prefix(&snapshot.root).unwrap_or(path).display().to_string().replace('\\', "/")
    }

    fn walk_folders(&self, snapshot: &WorkspaceSnapshot) -> Vec<String> {
        let mut found = Vec::new();
        self.visit_folders(snapshot, &snapshot.root, &mut found);
        found
    }

    fn visit_folders(&self, snapshot: &WorkspaceSnapshot, current: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(current).into_iter().flatten().flatten() {
            let path = entry.path();
            if !path.is_dir() || is_link(&path) {
                continue;
            }
            let relative = Self::relative(snapshot, &path);
            if self.is_ignored(snapshot, &relative) {
                continue;
            }
            found.push(relative);
            self.visit_folders(snapshot, &path, found);
        }
    }

    /// `(relative, path, size, mtime_ns)` of every regular file outside the ignored paths.
    fn walk_files(&self, snapshot: &WorkspaceSnapshot) -> Vec<(String, PathBuf, u64, u128)> {
        let mut found = Vec::new();
        self.visit_files(snapshot, &snapshot.root, &mut found);
        found
    }

    fn visit_files(&self, snapshot: &WorkspaceSnapshot, current: &Path, found: &mut Vec<(String, PathBuf, u64, u128)>) {
        for entry in std::fs::read_dir(current).into_iter().flatten().flatten() {
            let path = entry.path();
            let relative = Self::relative(snapshot, &path);
            if is_link(&path) || self.is_ignored(snapshot, &relative) {
                continue;
            }
            if path.is_dir() {
                self.visit_files(snapshot, &path, found);
            } else if let Ok(info) = std::fs::metadata(&path) {
                let mtime = info.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
                found.push((relative, path, info.len(), mtime));
            }
        }
    }

    /// Undoes every change outside `allowed_paths` (relative; a folder allows everything below it). `{"reverted", "removed", "unrestorable"}`.
    pub fn revert_unexpected_changes(&self, snapshot: &WorkspaceSnapshot, allowed_paths: &[&str]) -> Value {
        let allowed: BTreeSet<String> = allowed_paths.iter().map(|i| normalize_key(i).trim_matches('/').to_string()).collect();
        let is_allowed = |relative: &str| {
            let key = normalize_key(relative);
            allowed.iter().any(|item| key == *item || key.starts_with(&format!("{item}/")))
        };
        let (mut reverted, mut removed, mut unrestorable) = (Vec::<String>::new(), Vec::<String>::new(), Vec::<String>::new());
        let mut current: BTreeMap<String, (u64, u128)> = BTreeMap::new();
        for (relative, path, size, mtime) in self.walk_files(snapshot) {
            current.insert(relative.clone(), (size, mtime));
            if is_allowed(&relative) {
                continue;
            }
            match snapshot.files.get(&relative) {
                None => match std::fs::remove_file(&path) {
                    Ok(()) => removed.push(relative),
                    Err(_) => unrestorable.push(relative),
                },
                Some(before) if *before != (size, mtime) => {
                    let same = std::fs::read(&path).ok().as_ref() == snapshot.copies.get(&relative);
                    if !same {
                        self.restore_file(snapshot, &relative, &mut reverted, &mut unrestorable);
                    }
                }
                Some(_) => {}
            }
        }
        for relative in snapshot.files.keys() {
            if !current.contains_key(relative) && !is_allowed(relative) {
                self.restore_file(snapshot, relative, &mut reverted, &mut unrestorable);
            }
        }
        self.remove_new_empty_folders(snapshot);
        json!({"reverted": reverted, "removed": removed, "unrestorable": unrestorable})
    }

    fn restore_file(&self, snapshot: &WorkspaceSnapshot, relative: &str, reverted: &mut Vec<String>, unrestorable: &mut Vec<String>) {
        let Some(content) = snapshot.copies.get(relative) else {
            unrestorable.push(relative.to_string());
            return;
        };
        let path = relative.split('/').fold(snapshot.root.clone(), |a, p| a.join(p));
        let written = path.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::write(&path, content));
        if written.is_ok() {
            reverted.push(relative.to_string())
        } else {
            unrestorable.push(relative.to_string())
        }
    }

    /// Folders born during the job that ended up empty (deepest first). An allowed file keeps its folder.
    fn remove_new_empty_folders(&self, snapshot: &WorkspaceSnapshot) {
        let mut born: Vec<String> = self.walk_folders(snapshot).into_iter().filter(|f| !snapshot.folders.contains(f)).collect();
        born.sort_by_key(|f| std::cmp::Reverse(f.len()));
        for relative in born {
            let _ = std::fs::remove_dir(relative.split('/').fold(snapshot.root.clone(), |a, p| a.join(p)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn reverts_everything_outside_the_allowed_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, ".specs/project/ROADMAP.md", "road");
        write(root, "src/a.txt", "original");
        write(root, "src/gone.txt", "keep me");
        write(root, "node_modules/x/y.js", "ignored");
        let guard = WorkspaceFileGuard::default();
        let snapshot = guard.take_snapshot(&root.display().to_string(), &[], &DEFAULT_IGNORED_NAMES);
        write(root, ".specs/project/ROADMAP.md", "edited by the job");
        write(root, "src/a.txt", "changed");
        write(root, "src/new.txt", "created");
        write(root, "fresh/deep/file.txt", "created");
        write(root, "node_modules/x/y.js", "ignored change");
        std::fs::remove_file(root.join("src/gone.txt")).unwrap();
        let report = guard.revert_unexpected_changes(&snapshot, &[".specs"]);
        let sorted = |k: &str| -> Vec<String> {
            let mut v: Vec<String> = report[k].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
            v.sort();
            v
        };
        assert_eq!(sorted("reverted"), vec!["src/a.txt", "src/gone.txt"]);
        assert_eq!(sorted("removed"), vec!["fresh/deep/file.txt", "src/new.txt"]);
        assert!(sorted("unrestorable").is_empty());
        assert_eq!(std::fs::read_to_string(root.join("src/a.txt")).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(root.join("src/gone.txt")).unwrap(), "keep me");
        assert_eq!(std::fs::read_to_string(root.join(".specs/project/ROADMAP.md")).unwrap(), "edited by the job");
        assert!(!root.join("fresh").exists());
        assert_eq!(std::fs::read_to_string(root.join("node_modules/x/y.js")).unwrap(), "ignored change");
    }

    #[test]
    fn big_files_are_compared_but_not_restorable() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "big.bin", "0123456789");
        let guard = WorkspaceFileGuard { backup_file_limit_bytes: 4, backup_total_limit_bytes: 100 };
        let snapshot = guard.take_snapshot(&dir.path().display().to_string(), &["skip"], &[]);
        write(dir.path(), "big.bin", "changed-content");
        write(dir.path(), "skip/note.txt", "x");
        let report = guard.revert_unexpected_changes(&snapshot, &[]);
        assert_eq!(report["unrestorable"], json!(["big.bin"]));
        assert!(dir.path().join("skip/note.txt").exists());
    }
}
