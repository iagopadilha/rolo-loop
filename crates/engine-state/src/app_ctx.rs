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
    /// The `harness` executable the engine children run (`<app_exe> loop ...`): see [`app_exe`].
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

/// Variable that names the `harness` executable explicitly (set by a host such as the desktop app).
pub const APP_EXE_VARIABLE: &str = "LOOP_APP_EXE";
const HARNESS_STEM: &str = "harness";

/// The `harness` executable: `LOOP_APP_EXE` when it names a file; else, when this process is another program (the desktop app hosting
/// the panel), `<repo root>/plugin/bin/harness.exe` if it exists; else this executable.
pub fn app_exe(env: &Env, repo_root: &std::path::Path, current: &std::path::Path) -> PathBuf {
    if let Some(named) = env.get(APP_EXE_VARIABLE).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.is_file()) {
        return named;
    }
    let is_harness = current.file_stem().is_some_and(|stem| stem.to_string_lossy().eq_ignore_ascii_case(HARNESS_STEM));
    let bundled = repo_root.join("plugin").join("bin").join(format!("{HARNESS_STEM}{}", std::env::consts::EXE_SUFFIX));
    if !is_harness && bundled.is_file() {
        return bundled;
    }
    current.to_path_buf()
}

impl AppCtx {
    pub fn discover(env: &Env) -> Self {
        Self::discover_in(env, repo_root(env))
    }

    pub fn discover_in(env: &Env, repo_root: PathBuf) -> Self {
        let config = read_user_config(&env.user_data_dir());
        crate::providers::install(&config);
        let locator = Locator::new(env.clone(), repo_root.clone());
        let configured = config.get("orchestrator_skill_dir").and_then(Value::as_str).unwrap_or("");
        let skill_dir = locator.locate_orchestrator_skill_dir(configured);
        let (plugin_root, guard) =
            if skill_dir.as_os_str().is_empty() { (PathBuf::new(), PathBuf::new()) } else { (plugin_root_of(&skill_dir), locator.locate_guard(&skill_dir)) };
        let toolkit_root = locator.find_toolkit_root(&config);
        let app_exe = app_exe(env, &repo_root, &std::env::current_exe().unwrap_or_default());
        AppCtx { env: env.clone(), repo_root, config, skill_dir, plugin_root, guard, toolkit_root, app_exe }
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
        locator
            .list_skill_dir_candidates(configured)
            .iter()
            .map(|f| f.join("..").join("..").join("bin").join("guard.exe").display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_exe_is_harness_for_a_gui_host() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("plugin").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let harness = bin.join(format!("harness{}", std::env::consts::EXE_SUFFIX));
        let gui = dir.path().join("Agents Maestro.exe");
        let none = Env::from_pairs::<[(&str, &str); 0], &str, &str>([]);
        // no bundled harness yet: the current executable
        assert_eq!(app_exe(&none, dir.path(), &gui), gui);
        std::fs::write(&harness, b"").unwrap();
        assert_eq!(app_exe(&none, dir.path(), &gui), harness);
        // harness itself stays itself
        let own = dir.path().join("elsewhere").join("harness.exe");
        assert_eq!(app_exe(&none, dir.path(), &own), own);
        // an explicit file wins; a missing one is ignored
        let named = dir.path().join("named.exe");
        std::fs::write(&named, b"").unwrap();
        assert_eq!(app_exe(&Env::from_pairs([(APP_EXE_VARIABLE, named.to_str().unwrap())]), dir.path(), &gui), named);
        assert_eq!(app_exe(&Env::from_pairs([(APP_EXE_VARIABLE, "C:/missing/x.exe")]), dir.path(), &gui), harness);
    }
}
