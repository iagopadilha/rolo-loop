// SPDX-License-Identifier: MIT
//! Decides WHICH `CLAUDE.md` files get the generated `maestro:context` block and writes it.
//!
//! - every non-skipped, existing subproject of a CONFIRMED monorepo;
//! - the root of a single-project repository (also `monorepo:false`, or a repository with no stored list that is not a monorepo) and of a
//!   non-monorepo edit repository the app sets up;
//! - NEVER the root of a confirmed monorepo, a skipped subproject, or anything while the list is not confirmed.
//!
//! The folder is the work folder (the loop worktree when it exists, else the repository). `SUBPROJECTS.json` lives in the primary's work folder:
//! `subprojects_root` says where to read it (default = the given folder). Nothing here fails: a problem is logged.

use super::code_inventory::CodeInventoryService;
use crate::infrastructure::structured_log::StructuredLog;
use engine_state::{context_block, readiness_checks, run_hints, subprojects};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const SYSTEM_RELATIVE_PATH: &str = "docs/codebase/SYSTEM.md";
pub const MAP_RELATIVE_FOLDER: &str = "docs/codebase";

pub struct ContextBlockService<'a> {
    pub code_inventory: Option<&'a CodeInventoryService>,
    pub log: Option<&'a StructuredLog>,
}

fn join_parts(folder: &str, relative: &str) -> PathBuf {
    relative.split('/').filter(|p| !p.is_empty()).fold(PathBuf::from(folder), |a, p| a.join(p))
}

impl ContextBlockService<'_> {
    /// Rewrites the block in every folder of that repository that gets one. Returns the relative paths written ("" = the root).
    pub fn refresh_repo(&self, work_folder: &str, repo_key: Option<&str>, subprojects_root: Option<&str>) -> Vec<String> {
        self.target_paths(work_folder, repo_key, subprojects_root).into_iter().filter(|relative| self.write_folder(work_folder, relative)).collect()
    }

    /// Rewrites the block of one folder ("" = the root) when the rules give it one. True when the file was written.
    pub fn refresh_folder(&self, work_folder: &str, repo_key: Option<&str>, relative_path: &str, subprojects_root: Option<&str>) -> bool {
        let wanted = if relative_path.is_empty() { String::new() } else { subprojects::normalize(relative_path) };
        if !self.target_paths(work_folder, repo_key, subprojects_root).contains(&wanted) {
            return false;
        }
        self.write_folder(work_folder, &wanted)
    }

    /// Relative paths ("" = the root) that get a block: the non-skipped existing subprojects of a confirmed monorepo, `[""]` for a single project,
    /// `[]` for an unconfirmed monorepo, a monorepo root or a missing folder.
    pub fn target_paths(&self, work_folder: &str, repo_key: Option<&str>, subprojects_root: Option<&str>) -> Vec<String> {
        if work_folder.is_empty() || !Path::new(work_folder).is_dir() {
            return vec![];
        }
        let key = repo_key.filter(|k| !k.is_empty()).map(String::from).unwrap_or_else(|| subprojects::default_repo_name(work_folder));
        let (entry, _) = subprojects::load(subprojects_root.filter(|r| !r.is_empty()).unwrap_or(work_folder), Some(&key));
        let Some(entry) = entry else {
            // never listed: a repository that is not a monorepo is a single project; a monorepo not confirmed yet gets nothing
            return if subprojects::detect(work_folder).get("monorepo").is_some_and(|v| engine_state::notify_truthy(Some(v))) { vec![] } else { vec![String::new()] };
        };
        if entry.get("monorepo") != Some(&Value::Bool(true)) {
            return vec![String::new()];
        }
        if entry.get("confirmed") != Some(&Value::Bool(true)) {
            return vec![];
        }
        let mut paths = Vec::new();
        for project in subprojects::entry_projects(Some(&entry)) {
            let path = subprojects::normalize(project.get("path").and_then(Value::as_str).unwrap_or(""));
            if project.get("skip") == Some(&Value::Bool(true)) || path.is_empty() {
                continue;
            }
            if join_parts(work_folder, &path).is_dir() {
                paths.push(path);
            }
        }
        paths
    }

    pub fn write_folder(&self, work_folder: &str, relative_path: &str) -> bool {
        let folder = if relative_path.is_empty() { PathBuf::from(work_folder) } else { join_parts(work_folder, relative_path) };
        let facts = self.gather_facts(work_folder, relative_path);
        let result = context_block::write(&folder.display().to_string(), &facts, None);
        let error = result["error"].as_str().unwrap_or("");
        if !error.is_empty() {
            if let Some(log) = self.log {
                log.warn("context_block_not_written", &[("folder", json!(folder.display().to_string())), ("error", json!(error))]);
            }
        } else if result["written"] == true {
            if let Some(log) = self.log {
                log.info("context_block_written", &[("folder", json!(folder.display().to_string()))]);
            }
        }
        result["written"] == true
    }

    pub fn gather_facts(&self, work_folder: &str, relative_path: &str) -> Value {
        let folder = if relative_path.is_empty() { PathBuf::from(work_folder) } else { join_parts(work_folder, relative_path) };
        json!({
            "conformance": Self::read_conformance(work_folder, relative_path),
            "map_documents": run_hints::list_map_documents(&join_parts(&folder.display().to_string(), MAP_RELATIVE_FOLDER)),
            "system_pointer": Self::system_pointer(work_folder, relative_path),
            "commands": Value::Object(readiness_checks::known_commands(&folder.display().to_string())),
            "giant_lines": self.giant_lines(),
        })
    }

    /// `STANDARD`, `LEGACY` or '' (the Conformance of the folder as the project context shows it).
    pub fn read_conformance_text(work_folder: &str, relative_path: &str) -> String {
        let found = subprojects::read_conformance(work_folder, relative_path);
        if matches!(found.as_str(), "STANDARD" | "LEGACY") {
            found
        } else {
            String::new()
        }
    }

    fn read_conformance(work_folder: &str, relative_path: &str) -> Value {
        let found = subprojects::read_conformance(work_folder, relative_path);
        if matches!(found.as_str(), "STANDARD" | "LEGACY") {
            json!(found)
        } else {
            Value::Null
        }
    }

    /// Relative path from the subproject to the root `SYSTEM.md` ("" for a root or when the file does not exist).
    pub fn system_pointer(work_folder: &str, relative_path: &str) -> String {
        if relative_path.is_empty() || !join_parts(work_folder, SYSTEM_RELATIVE_PATH).is_file() {
            return String::new();
        }
        let depth = relative_path.split('/').count();
        let mut parts: Vec<&str> = vec![".."; depth];
        parts.extend(SYSTEM_RELATIVE_PATH.split('/'));
        parts.join("/")
    }

    fn giant_lines(&self) -> i64 {
        self.code_inventory.map(|c| c.thresholds().0).unwrap_or(context_block::DEFAULT_GIANT_LINES)
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
    fn single_projects_get_the_root_block_and_monorepos_the_subprojects() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app");
        std::fs::create_dir_all(&root).unwrap();
        let folder = root.display().to_string();
        let service = ContextBlockService { code_inventory: None, log: None };
        assert_eq!(service.target_paths(&folder, None, None), vec![String::new()]);
        assert!(service.refresh_repo(&folder, None, None) == vec![String::new()]);
        assert!(root.join("CLAUDE.md").is_file());
        assert!(service.refresh_repo(&folder, None, None).is_empty());
        write(&root, ".specs/project/SUBPROJECTS.json", r#"{"version": 1, "repos": {"app": {"monorepo": true, "confirmed": false, "projects": []}}}"#);
        assert!(service.target_paths(&folder, None, None).is_empty());
        write(
            &root,
            ".specs/project/SUBPROJECTS.json",
            r#"{"version": 1, "repos": {"app": {"monorepo": true, "confirmed": true, "projects": [{"path": "api"}, {"path": "web", "skip": true}, {"path": "gone"}]}}}"#,
        );
        std::fs::create_dir_all(root.join("api")).unwrap();
        std::fs::create_dir_all(root.join("web")).unwrap();
        assert_eq!(service.target_paths(&folder, None, None), vec!["api".to_string()]);
        write(&root, "docs/codebase/SYSTEM.md", "x");
        assert_eq!(ContextBlockService::system_pointer(&folder, "api"), "../docs/codebase/SYSTEM.md");
        assert_eq!(ContextBlockService::system_pointer(&folder, "a/b"), "../../docs/codebase/SYSTEM.md");
        assert_eq!(ContextBlockService::system_pointer(&folder, ""), "");
        assert!(service.refresh_folder(&folder, None, "api", None));
        assert!(!service.refresh_folder(&folder, None, "web", None));
        let text = std::fs::read_to_string(root.join("api/CLAUDE.md")).unwrap();
        assert!(text.contains("`../docs/codebase/SYSTEM.md`"));
    }
}
