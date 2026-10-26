// SPDX-License-Identifier: MIT
//! Where the app, the plugin, the guard binary and the dev toolkit are on this machine: resolved once and handed to the engine functions.

use cfg::locator::{plugin_root_of, read_user_config};
use cfg::{Env, Locator};
use serde_json::{Map, Value};
use std::path::PathBuf;

/// The pieces of the installation the engine needs.
#[derive(Clone, Debug)]
pub struct AppCtx {
    pub env: Env,
    /// The folder holding `plugin/` next to the binary (or `LOOP_REPO_ROOT`).
    pub repo_root: PathBuf,
    pub config: Map<String, Value>,
    /// Folder of the orchestrator skill (with SKILL.md), empty when it is not installed.
    pub skill_dir: PathBuf,
    /// Plugin root that holds the orchestrator skill, empty when it is not installed.
    pub plugin_root: PathBuf,
    /// The guard binary, empty when it does not exist.
    pub guard: PathBuf,
    /// Plugin root of the dev toolkit, empty when it is not installed.
    pub toolkit_root: PathBuf,
    /// This executable.
    pub app_exe: PathBuf,
}

/// Repo root = the folder holding `plugin/`, found from the binary's location (or `LOOP_REPO_ROOT`).
pub fn repo_root(env: &Env) -> PathBuf {
    if let Some(v) = env.get("LOOP_REPO_ROOT").filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    for ancestor in exe.ancestors().skip(1) {
        if ancestor.join("plugin").is_dir() {
            return ancestor.to_path_buf();
        }
    }
    exe.parent().map(PathBuf::from).unwrap_or_default()
}

impl AppCtx {
    pub fn discover(env: &Env) -> Self {
        Self::discover_in(env, repo_root(env))
    }

    pub fn discover_in(env: &Env, repo_root: PathBuf) -> Self {
        let config = read_user_config(&env.user_data_dir());
        let locator = Locator::new(env.clone(), repo_root.clone());
        let configured = config.get("orchestrator_skill_dir").and_then(Value::as_str).unwrap_or("");
        let skill_dir = locator.locate_orchestrator_skill_dir(configured);
        let (plugin_root, guard) = if skill_dir.as_os_str().is_empty() { (PathBuf::new(), PathBuf::new()) } else { (plugin_root_of(&skill_dir), locator.locate_guard(&skill_dir)) };
        let toolkit_root = locator.find_toolkit_root(&config);
        AppCtx { env: env.clone(), repo_root, config, skill_dir, plugin_root, guard, toolkit_root, app_exe: std::env::current_exe().unwrap_or_default() }
    }

    pub fn data_dir(&self) -> PathBuf {
        self.env.user_data_dir()
    }

    pub fn plugin_root_text(&self) -> String {
        self.plugin_root.display().to_string()
    }

    pub fn guard_text(&self) -> String {
        self.guard.display().to_string()
    }

    pub fn toolkit_text(&self) -> String {
        self.toolkit_root.display().to_string()
    }

    /// Where the guard is looked for, for the "not found" message.
    pub fn describe_guard_search_paths(&self) -> String {
        let locator = Locator::new(self.env.clone(), self.repo_root.clone());
        let configured = self.config.get("orchestrator_skill_dir").and_then(Value::as_str).unwrap_or("");
        locator.list_skill_dir_candidates(configured).iter().map(|f| f.join("..").join("..").join("bin").join("guard.exe").display().to_string()).collect::<Vec<_>>().join(", ")
    }
}
