// SPDX-License-Identifier: MIT
//! Finds the `orchestrator` skill, the guard binary and the companion toolkit on this machine.
//!
//! Skill lookup order (first folder with a `SKILL.md` wins):
//! 1. `LOOP_SKILL_DIR`; 2. `orchestrator_skill_dir` from `config.json`; 3. `$CLAUDE_PLUGIN_ROOT/skills/orchestrator`;
//! 4. `<repo root>/plugin/skills/orchestrator`; 5. the installed plugin cache (`installed_plugins.json` first,
//!    then newest first); 6. `~/.claude/skills/orchestrator`.
//!
//! The toolkit is found by [`Locator::find_toolkit_root`]: highest version that satisfies the minimum wins,
//! ties keep the priority order config > installed > sibling clone.

use crate::env::{abspath, read_text_sig, Env};
use crate::version::{version_key, VersionKey};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub const SKILL_DIR_VARIABLE: &str = "LOOP_SKILL_DIR";
pub const PLUGIN_ROOT_VARIABLE: &str = "CLAUDE_PLUGIN_ROOT";
pub const PLUGIN_NAME: &str = "claude-maestro";
pub const TOOLKIT_PLUGIN_NAME: &str = "claude-craft-kit";
pub const TOOLKIT_CONFIG_KEY: &str = "dev_toolkit_dir";
/// Minimum toolkit version (layout with `bin/`, `shared/templates`, `shared/rules`).
pub const TOOLKIT_MIN_VERSION: &str = "0.3.0";
/// Files (relative to the toolkit plugin root) the loop needs.
pub const TOOLKIT_REQUIRED_FILES: &[&str] = &["bin/craft.exe"];
pub const ORPHANED_MARKER: &str = ".orphaned_at";
pub const SIBLING_VARIABLE: &str = "LOOP_TOOLKIT_SIBLING";
pub const CLAUDE_HOME_VARIABLE: &str = "LOOP_CLAUDE_HOME";

/// Everything the lookups depend on, injectable for tests.
#[derive(Clone, Debug)]
pub struct Locator {
    pub env: Env,
    /// Folder holding `plugin/` next to the binary's repo (the "repo root" of the sibling-clone logic).
    pub repo_root: PathBuf,
}

/// Reads `<data dir>/config.json` (BOM tolerated); `{}` when missing, unreadable or not an object.
pub fn read_user_config(data_dir: &Path) -> Map<String, Value> {
    read_json_object(&data_dir.join("config.json")).unwrap_or_default()
}

/// `Some(map)` only when the file parses to a JSON object.
pub fn read_json_object(path: &Path) -> Option<Map<String, Value>> {
    let text = read_text_sig(path)?;
    match serde_json::from_str::<Value>(&text).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn same_text(a: &Path, b: &Path) -> bool {
    a.as_os_str() == b.as_os_str()
}

fn dedup_push(list: &mut Vec<PathBuf>, path: PathBuf) {
    if !list.iter().any(|p| same_text(p, &path)) {
        list.push(path);
    }
}

fn mtime(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

impl Locator {
    pub fn new(env: Env, repo_root: PathBuf) -> Self {
        Locator { env, repo_root }
    }

    pub fn claude_home_dir(&self) -> PathBuf {
        match self.env.get(CLAUDE_HOME_VARIABLE).filter(|v| !v.is_empty()) {
            Some(v) => PathBuf::from(v),
            None => self.env.home_dir().join(".claude"),
        }
    }

    /// `installPath` of every `<plugin_name>@<marketplace>` entry of `installed_plugins.json`.
    pub fn installed_plugin_paths(&self, plugin_name: &str) -> Vec<PathBuf> {
        let file = self
            .claude_home_dir()
            .join("plugins")
            .join("installed_plugins.json");
        let Some(text) = read_text_sig(&file) else {
            return vec![];
        };
        let Ok(Value::Object(root)) = serde_json::from_str::<Value>(&text) else {
            return vec![];
        };
        let Some(Value::Object(plugins)) = root.get("plugins") else {
            return vec![];
        };
        let mut found = Vec::new();
        for (key, entries) in plugins {
            if key.split('@').next() != Some(plugin_name) {
                continue;
            }
            let Value::Array(entries) = entries else {
                continue;
            };
            for entry in entries {
                if let Some(path) = entry.get("installPath").and_then(Value::as_str) {
                    let trimmed = path.trim();
                    if !trimmed.is_empty() {
                        found.push(abspath(Path::new(trimmed)));
                    }
                }
            }
        }
        found
    }

    /// Cache folders `<claude home>/plugins/cache/*/<plugin_name>/*`, newest first.
    fn cached_plugin_dirs(&self, plugin_name: &str) -> Vec<PathBuf> {
        let cache = self.claude_home_dir().join("plugins").join("cache");
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut markets: Vec<PathBuf> = std::fs::read_dir(&cache)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        markets.sort();
        for market in markets {
            let mut versions: Vec<PathBuf> = std::fs::read_dir(market.join(plugin_name))
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            versions.sort();
            dirs.extend(versions);
        }
        dirs.sort_by_key(|p| std::cmp::Reverse(mtime(p)));
        dirs
    }

    /// Installed folders of a plugin: `installPath`s first, then the other cache folders; orphaned ones skipped.
    fn installed_plugin_roots(&self, plugin_name: &str) -> Vec<PathBuf> {
        let mut ordered: Vec<PathBuf> = self
            .installed_plugin_paths(plugin_name)
            .into_iter()
            .filter(|p| p.is_dir())
            .collect();
        for path in self.cached_plugin_dirs(plugin_name) {
            let path = abspath(&path);
            if !ordered.iter().any(|p| same_text(p, &path)) {
                ordered.push(path);
            }
        }
        ordered
            .into_iter()
            .filter(|p| !p.join(ORPHANED_MARKER).exists())
            .collect()
    }

    pub fn list_installed_plugin_skill_dirs(&self) -> Vec<PathBuf> {
        self.installed_plugin_roots(PLUGIN_NAME)
            .into_iter()
            .map(|p| p.join("skills").join("orchestrator"))
            .filter(|p| p.is_dir())
            .collect()
    }

    pub fn list_skill_dir_candidates(&self, configured: &str) -> Vec<PathBuf> {
        let plugin_root = self.env.text(PLUGIN_ROOT_VARIABLE);
        let mut raw: Vec<PathBuf> = vec![
            PathBuf::from(self.env.text(SKILL_DIR_VARIABLE)),
            PathBuf::from(configured),
        ];
        raw.push(if plugin_root.is_empty() {
            PathBuf::new()
        } else {
            Path::new(&plugin_root).join("skills").join("orchestrator")
        });
        raw.push(
            self.repo_root
                .join("plugin")
                .join("skills")
                .join("orchestrator"),
        );
        raw.extend(self.list_installed_plugin_skill_dirs());
        raw.push(
            self.env
                .home_dir()
                .join(".claude")
                .join("skills")
                .join("orchestrator"),
        );
        let mut unique = Vec::new();
        for path in raw {
            if !path.as_os_str().is_empty() {
                dedup_push(&mut unique, abspath(&path));
            }
        }
        unique
    }

    /// First candidate folder with a `SKILL.md`; empty when the skill is not installed.
    pub fn locate_orchestrator_skill_dir(&self, configured: &str) -> PathBuf {
        self.list_skill_dir_candidates(configured)
            .into_iter()
            .find(|p| p.join("SKILL.md").is_file())
            .unwrap_or_default()
    }

    /// The skill's guard binary, or empty when it does not exist. The guard ships next to the plugin: `<plugin>/bin/guard.exe`.
    pub fn locate_guard(&self, skill_dir: &Path) -> PathBuf {
        if skill_dir.as_os_str().is_empty() {
            return PathBuf::new();
        }
        let plugin = plugin_root_of(skill_dir);
        let candidate = plugin.join("bin").join("guard.exe");
        if candidate.is_file() {
            candidate
        } else {
            PathBuf::new()
        }
    }

    // ---- toolkit -------------------------------------------------------------------------------

    /// The folder holding `skills/` for a toolkit path: the path itself or its `plugin/` child.
    pub fn toolkit_plugin_folder(path: &Path) -> PathBuf {
        if path.as_os_str().is_empty() {
            return PathBuf::new();
        }
        for candidate in [path.to_path_buf(), path.join("plugin")] {
            if candidate.join("skills").is_dir() {
                return abspath(&candidate);
            }
        }
        PathBuf::new()
    }

    pub fn list_installed_toolkit_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for path in self.installed_plugin_roots(TOOLKIT_PLUGIN_NAME) {
            let root = Self::toolkit_plugin_folder(&path);
            if !root.as_os_str().is_empty() {
                dedup_push(&mut roots, root);
            }
        }
        roots
    }

    /// `version` of `<root>/.claude-plugin/plugin.json`, or empty.
    pub fn read_toolkit_version(root: &Path) -> String {
        read_json_object(&root.join(".claude-plugin").join("plugin.json"))
            .and_then(|m| {
                m.get("version")
                    .and_then(Value::as_str)
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_default()
    }

    /// `(ok, version)` against [`TOOLKIT_MIN_VERSION`]. An unreadable version falls back to the presence of the toolkit binary.
    pub fn toolkit_version_status(root: &Path) -> (bool, String) {
        if root.as_os_str().is_empty() {
            return (true, String::new());
        }
        let version = Self::read_toolkit_version(root);
        match version_key(&version) {
            None => (
                TOOLKIT_REQUIRED_FILES
                    .iter()
                    .all(|f| root.join(f).is_file()),
                String::new(),
            ),
            Some(key) => (Some(key) >= version_key(TOOLKIT_MIN_VERSION), version),
        }
    }

    /// Required files the toolkit at `root` lacks (empty without a root).
    pub fn toolkit_missing_files(root: &Path) -> Vec<String> {
        if root.as_os_str().is_empty() {
            return vec![];
        }
        TOOLKIT_REQUIRED_FILES
            .iter()
            .filter(|f| !root.join(f).is_file())
            .map(|f| f.to_string())
            .collect()
    }

    pub fn list_toolkit_candidates(&self, config: &Map<String, Value>) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(Value::String(s)) = config.get(TOOLKIT_CONFIG_KEY) {
            let s = s.trim();
            if !s.is_empty() {
                roots.push(Self::toolkit_plugin_folder(&expand_user(s, &self.env)));
            }
        }
        roots.extend(self.list_installed_toolkit_roots());
        let sibling = self.env.text(SIBLING_VARIABLE);
        let sibling = sibling.trim();
        if !sibling.eq_ignore_ascii_case("off") {
            let path = if sibling.is_empty() {
                self.repo_root
                    .join("..")
                    .join(TOOLKIT_PLUGIN_NAME)
                    .join("plugin")
            } else {
                PathBuf::from(sibling)
            };
            roots.push(Self::toolkit_plugin_folder(&path));
        }
        let mut unique = Vec::new();
        for root in roots {
            if !root.as_os_str().is_empty() {
                dedup_push(&mut unique, root);
            }
        }
        unique
    }

    /// Plugin root of the toolkit (folder holding `skills/`), or empty when it is not installed.
    pub fn find_toolkit_root(&self, config: &Map<String, Value>) -> PathBuf {
        let mut best = PathBuf::new();
        let mut best_rank: Option<(bool, Option<VersionKey>)> = None;
        for root in self.list_toolkit_candidates(config) {
            let (ok, version) = Self::toolkit_version_status(&root);
            let rank = (ok, version_key(&version));
            if best_rank.is_none() || Some(&rank) > best_rank.as_ref() {
                best = root;
                best_rank = Some(rank);
            }
        }
        best
    }
}

/// Plugin root of a skill folder (`<plugin>/skills/orchestrator` -> `<plugin>`); empty without a skill folder.
pub fn plugin_root_of(skill_dir: &Path) -> PathBuf {
    if skill_dir.as_os_str().is_empty() {
        return PathBuf::new();
    }
    abspath(&skill_dir.join("..").join(".."))
}

fn expand_user(text: &str, env: &Env) -> PathBuf {
    if text == "~" {
        return env.home_dir();
    }
    if let Some(rest) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        return env.home_dir().join(rest);
    }
    PathBuf::from(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::time::{Duration, SystemTime};

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        claude_home: PathBuf,
        repo: PathBuf,
    }

    fn fx() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let claude_home = root.join("home").join(".claude");
        let repo = root.join("workspace").join("claude-maestro");
        fs::create_dir_all(&repo).unwrap();
        Fixture {
            _dir: dir,
            root,
            claude_home,
            repo,
        }
    }

    impl Fixture {
        fn locator(&self) -> Locator {
            Locator::new(
                Env::from_pairs([
                    ("LOOP_CLAUDE_HOME", self.claude_home.to_str().unwrap()),
                    ("LOOP_DATA_DIR", self.root.join("data").to_str().unwrap()),
                    ("USERPROFILE", self.root.join("home").to_str().unwrap()),
                ]),
                self.repo.clone(),
            )
        }

        fn make_skills(folder: &Path, version: Option<&str>) -> PathBuf {
            if let Some(v) = version {
                fs::create_dir_all(folder.join(".claude-plugin")).unwrap();
                fs::write(
                    folder.join(".claude-plugin").join("plugin.json"),
                    json!({"name": TOOLKIT_PLUGIN_NAME, "version": v}).to_string(),
                )
                .unwrap();
            }
            fs::create_dir_all(folder.join("skills").join("backend-dev")).unwrap();
            fs::write(
                folder.join("skills").join("backend-dev").join("SKILL.md"),
                "x",
            )
            .unwrap();
            abspath(folder)
        }

        fn cached(
            &self,
            market: &str,
            version: &str,
            nested: bool,
            mtime_secs: Option<u64>,
            declared: Option<&str>,
        ) -> PathBuf {
            let folder = self
                .claude_home
                .join("plugins")
                .join("cache")
                .join(market)
                .join(TOOLKIT_PLUGIN_NAME)
                .join(version);
            let target = if nested {
                folder.join("plugin")
            } else {
                folder.clone()
            };
            let root = Self::make_skills(&target, declared);
            if let Some(secs) = mtime_secs {
                set_dir_mtime(&folder, SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
            }
            root
        }

        fn sibling(&self, version: Option<&str>) -> PathBuf {
            Self::make_skills(
                &self
                    .root
                    .join("workspace")
                    .join(TOOLKIT_PLUGIN_NAME)
                    .join("plugin"),
                version,
            )
        }
    }

    fn empty() -> Map<String, Value> {
        Map::new()
    }

    /// Sets a directory's modified time (Windows needs backup semantics to open a directory for writing attributes).
    fn set_dir_mtime(dir: &Path, when: SystemTime) {
        let mut options = fs::OpenOptions::new();
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options
                .write(true)
                .access_mode(0x100)
                .custom_flags(0x0200_0000);
        }
        #[cfg(not(windows))]
        options.read(true);
        options.open(dir).unwrap().set_modified(when).unwrap();
    }

    #[test]
    fn nothing_installed_gives_empty() {
        let f = fx();
        assert_eq!(f.locator().find_toolkit_root(&empty()), PathBuf::new());
    }

    #[test]
    fn config_wins_over_cache_and_sibling() {
        let f = fx();
        let configured = Fixture::make_skills(&f.root.join("configured"), None);
        f.cached("market", "1.0.0", false, None, None);
        f.sibling(None);
        let cfg = json!({"dev_toolkit_dir": configured.to_str().unwrap()});
        assert_eq!(
            f.locator().find_toolkit_root(cfg.as_object().unwrap()),
            configured
        );
    }

    #[test]
    fn config_accepts_repo_root_and_uses_plugin_folder() {
        let f = fx();
        let repo = f.root.join("toolkit-repo");
        let plugin = Fixture::make_skills(&repo.join("plugin"), None);
        let cfg = json!({"dev_toolkit_dir": repo.to_str().unwrap()});
        assert_eq!(
            f.locator().find_toolkit_root(cfg.as_object().unwrap()),
            plugin
        );
    }

    #[test]
    fn config_folder_without_skills_falls_through() {
        let f = fx();
        let empty_dir = f.root.join("empty");
        fs::create_dir_all(&empty_dir).unwrap();
        let cached = f.cached("market", "1.0.0", false, None, None);
        let l = f.locator();
        for v in [json!(empty_dir.to_str().unwrap()), json!(42), json!("  ")] {
            let cfg = json!({"dev_toolkit_dir": v});
            assert_eq!(l.find_toolkit_root(cfg.as_object().unwrap()), cached);
        }
    }

    #[test]
    fn cache_wins_over_sibling_on_equal_versions() {
        let f = fx();
        f.sibling(Some("0.3.0"));
        let cached = f.cached("market", "0.3.0", false, None, Some("0.3.0"));
        assert_eq!(f.locator().find_toolkit_root(&empty()), cached);
    }

    #[test]
    fn newer_clone_beats_older_cache() {
        let f = fx();
        f.cached("market", "0.3.0", false, None, Some("0.3.0"));
        let sibling = f.sibling(Some("0.4.0"));
        assert_eq!(f.locator().find_toolkit_root(&empty()), sibling);
    }

    #[test]
    fn clone_that_satisfies_minimum_beats_newer_looking_old_cache() {
        let f = fx();
        f.cached("market", "0.2.0", false, Some(9000), Some("0.2.0"));
        let sibling = f.sibling(Some("0.3.0"));
        assert_eq!(f.locator().find_toolkit_root(&empty()), sibling);
    }

    #[test]
    fn versions_compare_as_numbers_not_text() {
        let f = fx();
        f.cached("market", "0.3.0", false, Some(9000), Some("0.3.0"));
        let newest = f.cached("market", "0.10.0", false, Some(1000), Some("0.10.0"));
        assert_eq!(f.locator().find_toolkit_root(&empty()), newest);
    }

    #[test]
    fn prerelease_sorts_below_release_and_never_raises() {
        let f = fx();
        f.cached("market", "0.4.0-beta", false, None, Some("0.4.0-beta"));
        let release = f.cached("other", "0.4.0", false, None, Some("0.4.0"));
        f.cached("third", "weird", false, None, Some("not-a-version"));
        assert_eq!(f.locator().find_toolkit_root(&empty()), release);
    }

    #[test]
    fn orphaned_cache_folder_is_ignored() {
        let f = fx();
        let old = f.cached("market", "0.4.0", false, None, Some("0.4.0"));
        fs::write(old.join(ORPHANED_MARKER), "1").unwrap();
        let live = f.cached("market", "0.3.0", false, None, Some("0.3.0"));
        assert_eq!(f.locator().find_toolkit_root(&empty()), live);
    }

    #[test]
    fn when_none_satisfies_minimum_the_highest_is_returned() {
        let f = fx();
        f.cached("market", "0.1.0", false, None, Some("0.1.0"));
        let best = f.cached("other", "0.2.0", false, None, Some("0.2.0"));
        assert_eq!(f.locator().find_toolkit_root(&empty()), best);
        assert_eq!(
            Locator::toolkit_version_status(&best),
            (false, "0.2.0".to_string())
        );
    }

    #[test]
    fn installed_plugins_json_names_the_install_path() {
        let f = fx();
        f.cached("market", "0.3.0", false, Some(9000), Some("0.3.0"));
        let chosen = f.cached("market", "0.3.1", false, Some(1000), Some("0.3.0"));
        fs::create_dir_all(f.claude_home.join("plugins")).unwrap();
        let body = json!({"version": 2, "plugins": {format!("{TOOLKIT_PLUGIN_NAME}@market"): [{"installPath": chosen.to_str().unwrap()}]}});
        fs::write(
            f.claude_home.join("plugins").join("installed_plugins.json"),
            body.to_string(),
        )
        .unwrap();
        let l = f.locator();
        assert_eq!(l.list_installed_toolkit_roots()[0], chosen);
        assert_eq!(l.find_toolkit_root(&empty()), chosen);
    }

    #[test]
    fn missing_required_files_are_named() {
        let f = fx();
        let root = f.cached("market", "0.3.0", false, None, Some("0.3.0"));
        assert_eq!(
            Locator::toolkit_missing_files(&root),
            vec!["bin/craft.exe".to_string()]
        );
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin").join("craft.exe"), "").unwrap();
        assert!(Locator::toolkit_missing_files(&root).is_empty());
        assert!(Locator::toolkit_missing_files(Path::new("")).is_empty());
    }

    #[test]
    fn newest_cached_version_wins_in_either_layout() {
        let f = fx();
        f.cached("market", "0.1.0", false, Some(1000), None);
        let newest = f.cached("market", "0.2.0", true, Some(3000), None);
        f.cached("other-market", "0.1.5", false, Some(2000), None);
        assert_eq!(f.locator().find_toolkit_root(&empty()), newest);
    }

    #[test]
    fn cache_folder_without_skills_is_ignored() {
        let f = fx();
        fs::create_dir_all(
            f.claude_home
                .join("plugins/cache/market")
                .join(TOOLKIT_PLUGIN_NAME)
                .join("0.9.0"),
        )
        .unwrap();
        assert_eq!(f.locator().find_toolkit_root(&empty()), PathBuf::new());
    }

    #[test]
    fn sibling_clone_is_the_last_resort() {
        let f = fx();
        let sibling = f.sibling(None);
        assert_eq!(f.locator().find_toolkit_root(&empty()), sibling);
    }

    #[test]
    fn sibling_off_disables_the_sibling() {
        let f = fx();
        f.sibling(None);
        let mut l = f.locator();
        l.env.set(SIBLING_VARIABLE, "off");
        assert_eq!(l.find_toolkit_root(&empty()), PathBuf::new());
    }

    #[test]
    fn broken_or_missing_config_is_ignored() {
        let f = fx();
        fs::create_dir_all(f.root.join("data")).unwrap();
        fs::write(f.root.join("data").join("config.json"), "{not json").unwrap();
        assert!(read_user_config(&f.root.join("data")).is_empty());
        fs::write(
            f.root.join("data").join("config.json"),
            "\u{feff}{\"a\": 1}",
        )
        .unwrap();
        assert_eq!(read_user_config(&f.root.join("data")).len(), 1);
        fs::write(f.root.join("data").join("config.json"), "[1]").unwrap();
        assert!(read_user_config(&f.root.join("data")).is_empty());
    }

    #[test]
    fn skill_dir_env_beats_everything() {
        let f = fx();
        let skill = f.root.join("plugin").join("skills").join("orchestrator");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "fake").unwrap();
        let mut l = f.locator();
        l.env.set(SKILL_DIR_VARIABLE, skill.to_str().unwrap());
        assert_eq!(l.locate_orchestrator_skill_dir(""), abspath(&skill));
        assert_eq!(plugin_root_of(&skill), abspath(&f.root.join("plugin")));
    }
}
