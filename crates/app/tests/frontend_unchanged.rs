// SPDX-License-Identifier: MIT
//! The front end is pinned: every file under `app/frontend` hashes to what `app/frontend.sha256` says, and there is no file
//! the manifest does not list.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn files_under(folder: &Path, root: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(folder).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, root, out);
        } else {
            out.push(path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
fn every_front_end_file_matches_the_manifest() {
    let root = repo_root().join("app").join("frontend");
    let manifest = std::fs::read_to_string(repo_root().join("app").join("frontend.sha256")).unwrap();
    let mut listed: Vec<String> = Vec::new();
    for line in manifest.lines().filter(|l| !l.trim().is_empty()) {
        let (hash, relative) = line.split_once("  ").unwrap();
        let bytes = std::fs::read(root.join(relative)).unwrap_or_else(|_| panic!("missing front end file {relative}"));
        let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(actual, hash, "{relative} changed");
        listed.push(relative.to_string());
    }
    let mut present = Vec::new();
    files_under(&root, &root, &mut present);
    present.sort();
    listed.sort();
    assert_eq!(present, listed, "files outside the manifest");
    assert!(listed.len() >= 100);
}
