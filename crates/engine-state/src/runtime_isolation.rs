// SPDX-License-Identifier: MIT
//! Runtime isolation of every `claude -p` the app starts.
//!
//! Why: a plain `claude -p` loads the user's settings: ~100 skills, every enabled plugin, the user's hooks and the plugin MCP servers (even
//! with `--strict-mcp-config`). Isolation starts the process with `--setting-sources project,local` (the project's own CLAUDE.md and
//! settings still load) plus `--add-dir=<bundle>`, a folder `<data dir>/runtime/bundle/.claude/{skills,agents}` that holds exactly the plugin
//! agents and the allowlisted skills under their plain names (`--plugin-dir` would namespace them as `<plugin>:<name>` and break dispatch by
//! name). The guard hook, which used to come from the user's `~/.claude/settings.json`, goes through inline `--settings`.
//!
//! Kill switch: `"loop_isolation": false` in `<LOOP_DATA_DIR or ~/.loop-harness>/config.json` (default true) gives back the plain argv.
//!
//! Allowlist keys (one per profile name the guard can resolve; `LOOP_SKILLS_FILE` is `{profile: [skill, ...]}`): orchestrator, planning,
//! roadmapmap, subprojects, systemmap, spec, executor, verifier; `auto` = orchestrator (the inline hook `--profile auto` of the loop turns
//! and of app jobs with no `hook_profile`; the guard resolves it to the agent's profile or, for the main session, to `orchestrator`).
//!
//! Skill sources, first folder with a SKILL.md wins: `<LOOP_CLAUDE_HOME>/skills/<name>`, the dev toolkit (`craft-dev-toolkit`, or `claude-craft-kit` for older installs), the Rolo Loop
//! plugin's `skills/`, then every other installed plugin cache (for the skills the projects bind to their standards slots). The skills bound
//! in the `## Standards` section of the project's `CLAUDE.md` files join the allowlist of every profile that already has a dev skill, plus
//! the verifier.
//!
//! The guard hook is the `guard` binary of the plugin: its command is the quoted path plus `--profile <profile>`.

use crate::loop_io::{atomic_write_text, copy2, copy_tree};
use crate::standards;
use cfg::Env;
use pycompat::json::{py_json_dumps, py_json_dumps_indent};
use pycompat::{nt_abspath, Re};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub const CONFIG_KEY: &str = "loop_isolation";
pub const EXTRA_SKILLS_KEY: &str = "loop_extra_skills";
pub const CAVEMAN_KEY: &str = "loop_caveman";
pub const CAVEMAN_MARKER: &str = "caveman";
pub const SESSION_START_EVENT: &str = "SessionStart";
pub const RUNTIME_DIR_NAME: &str = "runtime";
pub const BUNDLE_DIR_NAME: &str = "bundle";
pub const SIGNATURE_FILE_NAME: &str = ".signature.json";
pub const ALLOWLIST_FILE_NAME: &str = "skills-allowlist.json";
const MEMO_SECONDS: u64 = 30;

const PLANNING_SKILLS: [&str; 5] = ["orchestrator", "tlc-spec-driven", "repo-catalog", "backend-dev", "frontend-dev"];
/// The kit renamed two skills it replaced with its own text: `ui-trace-analysis` (was playwright-trace) and `ui-component-tests` (was
/// playwright-component-testing).
const DEFAULT_ALLOWLIST: [(&str, &[&str]); 8] = [
    (
        "orchestrator",
        &["orchestrator", "orchestrator-audit-dependencies", "orchestrator-create-block-rule", "orchestrator-show-progress-graph", "tlc-spec-driven", "repo-catalog"],
    ),
    ("planning", &PLANNING_SKILLS),
    ("roadmapmap", &PLANNING_SKILLS),
    ("subprojects", &PLANNING_SKILLS),
    ("systemmap", &PLANNING_SKILLS),
    ("spec", &["tlc-spec-driven", "repo-catalog", "backend-dev", "frontend-dev", "investigate-first"]),
    ("executor", &["backend-dev", "frontend-dev", "safe-refactor", "refactoring-guru-techniques", "migration", "surgical-patch", "playwright-cli"]),
    ("verifier", &["flow-validator", "playwright-cli", "ui-trace-analysis", "ui-component-tests", "verify-and-stop"]),
];
/// Profiles that also get the skills named in SUBPROJECTS.json.
const SUBPROJECT_SKILL_PROFILES: [&str; 2] = ["spec", "executor"];
/// Standards-slot skills go here.
const SLOT_SKILL_PROFILES: [&str; 7] = ["planning", "roadmapmap", "subprojects", "systemmap", "spec", "executor", "verifier"];
const REWRITTEN_SUFFIXES: [&str; 5] = [".md", ".json", ".yml", ".yaml", ".txt"];

static PLUGIN_ROOT_VARIABLE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\$\{CLAUDE_PLUGIN_ROOT\}|\$CLAUDE_PLUGIN_ROOT\b"));
static MEMO: LazyLock<Mutex<Vec<(String, Instant, Value)>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static BUILD_LOCK: Mutex<()> = Mutex::new(());

/// An allowlist keeps the profile order.
pub type Allowlist = Vec<(String, Vec<String>)>;

pub fn data_dir(env: &Env) -> PathBuf {
    env.user_data_dir()
}

pub fn claude_home(env: &Env) -> PathBuf {
    match env.get("LOOP_CLAUDE_HOME").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(nt_abspath(&p)),
        None => PathBuf::from(nt_abspath(&env.home_dir().join(".claude").display().to_string())),
    }
}

pub fn user_skills_dir(env: &Env) -> PathBuf {
    claude_home(env).join("skills")
}

pub fn read_config(env: &Env) -> Map<String, Value> {
    cfg::locator::read_user_config(&data_dir(env))
}

/// `loop_isolation` of config.json; only an explicit false (boolean) turns it off.
pub fn isolation_enabled(config: &Map<String, Value>) -> bool {
    config.get(CONFIG_KEY) != Some(&Value::Bool(false))
}

fn read_json(path: &Path) -> Option<Value> {
    let text = String::from_utf8(std::fs::read(path).ok()?).ok()?;
    serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text)).ok()
}

/// Skills named in `<root>/.specs/project/SUBPROJECTS.json` (any `skill` of a project entry, `-` and skipped entries ignored).
pub fn project_skill_names(project_root: &str) -> Vec<String> {
    if project_root.is_empty() {
        return vec![];
    }
    let Some(data) = read_json(&Path::new(project_root).join(".specs").join("project").join("SUBPROJECTS.json")) else { return vec![] };
    let mut lists: Vec<Option<Value>> = Vec::new();
    if let Value::Object(map) = &data {
        for key in ["subprojects", "projects"] {
            lists.push(map.get(key).cloned());
        }
        if let Some(Value::Object(repos)) = map.get("repos") {
            for entry in repos.values() {
                lists.push(entry.get("projects").cloned());
            }
        }
    }
    let mut names: Vec<String> = Vec::new();
    for items in lists.into_iter().flatten() {
        for item in items.as_array().into_iter().flatten() {
            let skill = match item {
                Value::Object(o) if !crate::notify_truthy(o.get("skip")) => o.get("skill").and_then(Value::as_str).unwrap_or("").trim().to_string(),
                _ => String::new(),
            };
            if !skill.is_empty() && skill != "-" && !names.contains(&skill) {
                names.push(skill);
            }
        }
    }
    names
}

/// Plain names (plugin prefix removed) of the skills bound to the standards slots of the root and of every subproject.
pub fn slot_skill_names(project_root: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (_, bindings) in standards::project_bindings(project_root) {
        for (_, value) in bindings {
            let name = standards::skill_name(&value);
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

fn push_unique(list: &mut Vec<String>, names: impl IntoIterator<Item = String>) {
    for name in names {
        if !list.contains(&name) {
            list.push(name);
        }
    }
}

/// `{profile: [skill, ...]}`: the defaults, plus the SUBPROJECTS.json skills for spec and executor, plus the skills bound to the Standards
/// slots of the project (root and subprojects) for every profile that has a dev skill and the verifier, plus `loop_extra_skills` of
/// config.json for every profile. `auto` mirrors `orchestrator`.
pub fn skills_allowlist(project_root: Option<&str>, config: &Map<String, Value>) -> Allowlist {
    let mut allowlist: Allowlist = DEFAULT_ALLOWLIST.iter().map(|(p, s)| (p.to_string(), s.iter().map(|x| x.to_string()).collect())).collect();
    let root = project_root.unwrap_or("");
    let project_skills = project_skill_names(root);
    let slot_skills = slot_skill_names(root);
    for (profile, skills) in allowlist.iter_mut() {
        if SUBPROJECT_SKILL_PROFILES.contains(&profile.as_str()) {
            push_unique(skills, project_skills.clone());
        }
    }
    for (profile, skills) in allowlist.iter_mut() {
        if SLOT_SKILL_PROFILES.contains(&profile.as_str()) {
            push_unique(skills, slot_skills.clone());
        }
    }
    let mut names: Vec<String> = match config.get(EXTRA_SKILLS_KEY) {
        Some(Value::Array(items)) => {
            items.iter().map(|i| i.as_str().map(String::from).unwrap_or_else(|| i.to_string()).trim().to_string()).filter(|s| !s.is_empty()).collect()
        }
        _ => vec![],
    };
    if config.get(CAVEMAN_KEY) == Some(&Value::Bool(true)) {
        names.push(CAVEMAN_MARKER.into()); // the carried hook tells the model to load the `caveman` skill: allow it (and bundle it) everywhere
    }
    for name in names {
        for (_, skills) in allowlist.iter_mut() {
            if !skills.contains(&name) {
                skills.push(name.clone());
            }
        }
    }
    let auto = allowlist.iter().find(|(p, _)| p == "orchestrator").map(|(_, s)| s.clone()).unwrap_or_default();
    allowlist.push(("auto".into(), auto));
    allowlist
}

pub fn all_skill_names(allowlist: &Allowlist) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (_, skills) in allowlist {
        push_unique(&mut names, skills.iter().cloned());
    }
    names.sort();
    names
}

/// Plugin root of a skill folder `<plugin>/skills/<name>` (empty when the skill folder is empty).
pub fn plugin_root_of(skill_dir: &str) -> String {
    if skill_dir.is_empty() {
        String::new()
    } else {
        nt_abspath(&Path::new(skill_dir).join("..").join("..").display().to_string())
    }
}

// ---- bundle ------------------------------------------------------------------------------------------------------------------------

fn visit(path: &Path, newest: &mut u128, total: &mut u64) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            visit(&entry.path(), newest, total);
        }
    } else if let Ok(info) = std::fs::metadata(path) {
        let ns = info.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
        *newest = (*newest).max(ns);
        *total += info.len();
    }
}

/// `(newest mtime in ns, total size in bytes)` of a file or folder tree.
pub fn tree_stamp(path: &Path) -> (u128, u64) {
    let (mut newest, mut total) = (0u128, 0u64);
    visit(path, &mut newest, &mut total);
    (newest, total)
}

/// The `skills` folder of every installed plugin (`~/.claude/plugins/cache/*/*/*/skills`, or `.../plugin/skills`), the newest version of
/// each plugin only, newest first.
pub fn installed_plugin_skill_dirs(env: &Env) -> Vec<PathBuf> {
    let cache = claude_home(env).join("plugins").join("cache");
    let dirs = |p: &Path| -> Vec<PathBuf> { std::fs::read_dir(p).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default() };
    let mut newest: Vec<(PathBuf, std::time::SystemTime, PathBuf)> = Vec::new();
    for market in dirs(&cache) {
        for plugin in dirs(&market) {
            for version in dirs(&plugin) {
                let Some(skills) = [version.join("skills"), version.join("plugin").join("skills")].into_iter().find(|p| p.is_dir()) else { continue };
                let stamp = std::fs::metadata(&version).and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH);
                match newest.iter_mut().find(|(k, _, _)| *k == plugin) {
                    Some(slot) if stamp > slot.1 => {
                        slot.1 = stamp;
                        slot.2 = skills;
                    }
                    Some(_) => {}
                    None => newest.push((plugin.clone(), stamp, skills)),
                }
            }
        }
    }
    newest.sort_by(|a, b| b.1.cmp(&a.1));
    newest.into_iter().map(|(_, _, s)| s).collect()
}

/// `({name: source folder}, [missing names])`: `<user skills>/<name>`, then `<toolkit>/skills/<name>`, then `<plugin>/skills/<name>`, then each
/// of `other_skill_dirs` (installed plugin caches); a folder needs a SKILL.md.
pub fn resolve_skill_sources(
    plugin_dir: &str,
    user_dir: &Path,
    names: &[String],
    toolkit_dir: &str,
    other_skill_dirs: &[PathBuf],
) -> (BTreeMap<String, PathBuf>, Vec<String>) {
    let (mut found, mut skipped) = (BTreeMap::new(), Vec::new());
    let mut bases: Vec<PathBuf> = vec![user_dir.to_path_buf()];
    if !toolkit_dir.is_empty() {
        bases.push(Path::new(toolkit_dir).join("skills"));
    }
    if !plugin_dir.is_empty() {
        bases.push(Path::new(plugin_dir).join("skills"));
    }
    bases.extend(other_skill_dirs.iter().cloned());
    for name in names {
        match bases.iter().map(|b| b.join(name)).find(|c| c.join("SKILL.md").is_file()) {
            Some(candidate) => {
                found.insert(name.clone(), candidate);
            }
            None => skipped.push(name.clone()),
        }
    }
    (found, skipped)
}

pub fn list_agent_files(plugin_dir: &str) -> Vec<String> {
    if plugin_dir.is_empty() {
        return vec![];
    }
    let folder = Path::new(plugin_dir).join("agents");
    let mut names: Vec<String> =
        std::fs::read_dir(folder).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".md")).collect()).unwrap_or_default();
    names.sort();
    names
}

pub fn compute_signature(skill_sources: &BTreeMap<String, PathBuf>, plugin_dir: &str, agent_files: &[String]) -> Value {
    let mut entries: Vec<Value> = skill_sources
        .iter()
        .map(|(name, source)| {
            let (newest, total) = tree_stamp(source);
            json!(["skill", name, source.display().to_string(), newest.to_string(), total])
        })
        .collect();
    for name in agent_files {
        let path = Path::new(plugin_dir).join("agents").join(name);
        let (newest, total) = tree_stamp(&path);
        entries.push(json!(["agent", name, path.display().to_string(), newest.to_string(), total]));
    }
    Value::Array(entries)
}

fn read_signature(bundle: &Path) -> Option<Value> {
    read_json(&bundle.join(SIGNATURE_FILE_NAME))
}

/// The plugin root of a skill folder `<root>/skills/<name>` (empty when `<root>` is not a plugin, e.g. the user's own ~/.claude/skills).
pub fn owning_plugin_root(skill_source: &Path) -> String {
    let abs = PathBuf::from(nt_abspath(&skill_source.display().to_string()));
    let root = abs.parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    if root.join(".claude-plugin").join("plugin.json").is_file() {
        root.display().to_string()
    } else {
        String::new()
    }
}

/// `${CLAUDE_PLUGIN_ROOT}` / `$CLAUDE_PLUGIN_ROOT` -> the plugin root with forward slashes. The bundle copies skills and agents as PROJECT
/// files, where Claude Code does not expand the variable (it does for installed plugins only), so the copy carries the real path.
pub fn expand_plugin_root(text: &str, plugin_root: &str) -> String {
    if plugin_root.is_empty() {
        return text.to_string();
    }
    let root = plugin_root.replace('\\', "/");
    PLUGIN_ROOT_VARIABLE.replace_lit(text, &root)
}

/// Rewrites the text files (.md .json .yml .yaml .txt) under `folder` in place; a file that is not UTF-8 is left alone.
pub fn rewrite_plugin_root(folder: &Path, plugin_root: &str) {
    if plugin_root.is_empty() {
        return;
    }
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rewrite_plugin_root(&path, plugin_root);
            continue;
        }
        let lower = entry.file_name().to_string_lossy().to_lowercase();
        if !REWRITTEN_SUFFIXES.iter().any(|s| lower.ends_with(s)) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(text) = String::from_utf8(bytes) else { continue };
        let updated = expand_plugin_root(&text, plugin_root);
        if updated != text {
            let _ = std::fs::write(&path, updated);
        }
    }
}

fn build_bundle_folder(target: &Path, skill_sources: &BTreeMap<String, PathBuf>, plugin_dir: &str, agent_files: &[String], signature: &Value) -> std::io::Result<()> {
    let (skills_dir, agents_dir) = (target.join(".claude").join("skills"), target.join(".claude").join("agents"));
    std::fs::create_dir_all(&skills_dir)?;
    std::fs::create_dir_all(&agents_dir)?;
    for (name, source) in skill_sources {
        copy_tree(source, &skills_dir.join(name), &[])?;
        rewrite_plugin_root(&skills_dir.join(name), &owning_plugin_root(source));
    }
    for name in agent_files {
        copy2(&Path::new(plugin_dir).join("agents").join(name), &agents_dir.join(name))?;
    }
    rewrite_plugin_root(&agents_dir, &if plugin_dir.is_empty() { String::new() } else { nt_abspath(plugin_dir) });
    std::fs::write(target.join(SIGNATURE_FILE_NAME), py_json_dumps(signature, true))
}

/// Puts `temp` at `bundle`: the old folder is renamed aside first (a folder cannot be replaced over a non-empty one), then removed.
fn swap_in(temp: &Path, bundle: &Path) -> std::io::Result<()> {
    let mut aside: Option<PathBuf> = None;
    if bundle.exists() {
        let stamp = Sha256::digest(format!("{:?}", Instant::now()).as_bytes()).iter().take(3).map(|b| format!("{b:02x}")).collect::<String>();
        let path = PathBuf::from(format!("{}.old-{}-{stamp}", bundle.display(), std::process::id()));
        std::fs::rename(bundle, &path)?;
        aside = Some(path);
    }
    if let Err(error) = std::fs::rename(temp, bundle) {
        if let Some(path) = &aside {
            if !bundle.exists() {
                let _ = std::fs::rename(path, bundle);
            }
        }
        return Err(error);
    }
    if let Some(path) = aside {
        let _ = std::fs::remove_dir_all(path);
    }
    Ok(())
}

/// Builds `<data dir>/runtime/bundle` when the source signature changed; cheap when it did not (and a 30 s in-process memo makes a call per
/// command nearly free). Returns `{"path", "skills", "skipped", "agents", "rebuilt"}` (+ `"error"` when the swap failed and the old bundle
/// stays). The build happens in a temp folder beside it and is swapped in, so a concurrent run never sees a half bundle.
pub fn ensure_bundle(
    data_dir_path: &Path,
    plugin_root_path: &str,
    user_skills_path: &Path,
    names: &[String],
    use_memo: bool,
    toolkit_dir: &str,
    other_skill_dirs: &[PathBuf],
) -> Value {
    let mut sorted: Vec<String> = names.to_vec();
    sorted.sort();
    sorted.dedup();
    let key = format!(
        "{}|{}|{}|{}|{}|{:?}",
        nt_abspath(&data_dir_path.display().to_string()),
        plugin_root_path,
        user_skills_path.display(),
        sorted.join(","),
        toolkit_dir,
        other_skill_dirs
    );
    let now = Instant::now();
    if use_memo {
        if let Ok(memo) = MEMO.lock() {
            if let Some((_, at, value)) = memo.iter().find(|(k, _, _)| *k == key) {
                let path = value["path"].as_str().unwrap_or("");
                if now.duration_since(*at) < Duration::from_secs(MEMO_SECONDS) && Path::new(path).is_dir() {
                    let mut hit = value.clone();
                    hit["rebuilt"] = json!(false);
                    return hit;
                }
            }
        }
    }
    let runtime = PathBuf::from(nt_abspath(&data_dir_path.display().to_string())).join(RUNTIME_DIR_NAME);
    let bundle = runtime.join(BUNDLE_DIR_NAME);
    let (sources, skipped) = resolve_skill_sources(plugin_root_path, user_skills_path, &sorted, toolkit_dir, other_skill_dirs);
    let agent_files = list_agent_files(plugin_root_path);
    let signature = compute_signature(&sources, plugin_root_path, &agent_files);
    let mut result = json!({"path": bundle.display().to_string(), "skills": sources.keys().collect::<Vec<_>>(), "skipped": skipped,
                            "agents": agent_files.iter().map(|n| n.trim_end_matches(".md")).collect::<Vec<_>>(), "rebuilt": false});
    {
        let _guard = BUILD_LOCK.lock();
        if read_signature(&bundle).as_ref() != Some(&signature) {
            let _ = std::fs::create_dir_all(&runtime);
            let temp = runtime.join(format!(
                "bundle.tmp-{}-{}",
                std::process::id(),
                Sha256::digest(format!("{:?}", Instant::now()).as_bytes()).iter().take(3).map(|b| format!("{b:02x}")).collect::<String>()
            ));
            let built = build_bundle_folder(&temp, &sources, plugin_root_path, &agent_files, &signature).and_then(|_| swap_in(&temp, &bundle));
            match built {
                Ok(()) => result["rebuilt"] = json!(true),
                Err(error) => {
                    let _ = std::fs::remove_dir_all(&temp);
                    result["error"] = json!(format!("bundle not rebuilt: {error}"));
                }
            }
        }
    }
    if let Ok(mut memo) = MEMO.lock() {
        memo.retain(|(k, _, _)| *k != key);
        memo.push((key, now, result.clone()));
    }
    result
}

pub fn clear_memo() {
    if let Ok(mut memo) = MEMO.lock() {
        memo.clear();
    }
}

fn short_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().take(5).map(|b| format!("{b:02x}")).collect()
}

fn allowlist_json(allowlist: &Allowlist) -> Value {
    Value::Object(allowlist.iter().map(|(p, s)| (p.clone(), json!(s))).collect())
}

fn sorted_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|k| (k.clone(), sorted_keys(&map[k]))).collect())
        }
        other => other.clone(),
    }
}

/// Writes the allowlist JSON (atomically) and returns its path. The shared default file `skills-allowlist.json` holds the allowlist of a
/// project with no extras; an allowlist that differs (SUBPROJECTS.json skills) goes to `skills-allowlist-<hash>.json`, so two projects running
/// at once never overwrite each other's file.
pub fn write_allowlist_file(env: &Env, data_dir_path: &Path, allowlist: &Allowlist) -> PathBuf {
    let runtime = PathBuf::from(nt_abspath(&data_dir_path.display().to_string())).join(RUNTIME_DIR_NAME);
    let _ = std::fs::create_dir_all(&runtime);
    let text = py_json_dumps_indent(&sorted_keys(&allowlist_json(allowlist)), true, 2);
    let mut path = runtime.join(ALLOWLIST_FILE_NAME);
    if allowlist_json(allowlist) != allowlist_json(&skills_allowlist(None, &read_config(env))) {
        path = runtime.join(format!("skills-allowlist-{}.json", short_hash(&text)));
    }
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        let _ = atomic_write_text(&path, &text);
    }
    path
}

// ---- argv / env / settings -----------------------------------------------------------------------------------------------------------

/// The PreToolUse hook command line (shared by the loop turns and by the panel's `ClaudeClient`): the quoted guard path with forward
/// slashes, `--profile <profile>`.
pub fn guard_command(guard_path: &str, profile: &str) -> String {
    format!("\"{}\" --profile {profile}", nt_abspath(guard_path).replace('\\', "/"))
}

pub fn hook_block(command: &str) -> Value {
    json!({"PreToolUse": [{"matcher": "*", "hooks": [{"type": "command", "command": command}]}]})
}

pub fn read_user_settings(env: &Env) -> Value {
    match read_json(&claude_home(env).join("settings.json")) {
        Some(v @ Value::Object(_)) => v,
        _ => json!({}),
    }
}

/// The user's `SessionStart` hook groups reduced to the hooks whose command mentions `caveman` (case-insensitive); a group with no such hook
/// is dropped and the other fields of a group (`matcher`) are kept. Empty when the user has none (the option is then "not installed").
pub fn find_caveman_hooks(user_settings: &Value) -> Vec<Value> {
    let groups = user_settings.get("hooks").and_then(|h| h.get(SESSION_START_EVENT)).and_then(Value::as_array).cloned().unwrap_or_default();
    let mut found = Vec::new();
    for group in groups {
        let Value::Object(map) = &group else { continue };
        let kept: Vec<Value> = map
            .get("hooks")
            .and_then(Value::as_array)
            .map(|hooks| {
                hooks
                    .iter()
                    .filter(|h| {
                        h.is_object()
                            && h.get("command")
                                .map(|c| c.as_str().map(String::from).unwrap_or_else(|| c.to_string()))
                                .unwrap_or_default()
                                .to_lowercase()
                                .contains(CAVEMAN_MARKER)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if !kept.is_empty() {
            let mut copy = map.clone();
            copy.insert("hooks".into(), Value::Array(kept));
            found.push(Value::Object(copy));
        }
    }
    found
}

/// The caveman hook groups a `claude -p` carries: only with isolation on AND `loop_caveman` true (boolean) in config.json, else empty.
pub fn caveman_hooks_to_carry(env: &Env, config: &Map<String, Value>, user_settings: Option<&Value>) -> Vec<Value> {
    if !isolation_enabled(config) || config.get(CAVEMAN_KEY) != Some(&Value::Bool(true)) {
        return vec![];
    }
    match user_settings {
        Some(s) => find_caveman_hooks(s),
        None => find_caveman_hooks(&read_user_settings(env)),
    }
}

/// Inline `--settings` JSON: the guard as a PreToolUse hook (`--profile <profile>`) plus the user's `autoMode` block copied from
/// `~/.claude/settings.json` (or from `user_settings` when given) so `--permission-mode auto` keeps its classifier context. Empty when there
/// is neither a guard nor an `autoMode`. The user's `permissions` are not carried (the app passes `--permission-mode`). With `loop_caveman`
/// on (config.json) the user's caveman `SessionStart` hooks join the same file.
pub fn guard_settings(env: &Env, guard_path: &str, profile: &str, user_settings: Option<&Value>) -> String {
    let loaded;
    let user_settings = match user_settings {
        Some(s) => s,
        None => {
            loaded = read_user_settings(env);
            &loaded
        }
    };
    let mut settings = Map::new();
    if !guard_path.is_empty() {
        settings.insert("hooks".into(), hook_block(&guard_command(guard_path, profile)));
    }
    let caveman = caveman_hooks_to_carry(env, &read_config(env), Some(user_settings));
    if !caveman.is_empty() {
        let hooks = settings.entry("hooks").or_insert_with(|| json!({}));
        if let Some(map) = hooks.as_object_mut() {
            map.insert(SESSION_START_EVENT.into(), Value::Array(caveman));
        }
    }
    if let Some(auto) = user_settings.get("autoMode") {
        settings.insert("autoMode".into(), auto.clone());
    }
    if settings.is_empty() {
        String::new()
    } else {
        py_json_dumps(&Value::Object(settings), true)
    }
}

/// Writes the settings JSON next to the bundle (name by content hash, atomic) and returns its path. A file, never the inline JSON: a batch
/// fallback binary (`cc.cmd`) goes through cmd.exe, which splits an argument on `&`, `|`, `<`, `>` (the copied `autoMode` text has them).
pub fn write_settings_file(runtime_dir: &Path, settings_json: &str) -> PathBuf {
    let _ = std::fs::create_dir_all(runtime_dir);
    let path = runtime_dir.join(format!("settings-{}.json", short_hash(settings_json)));
    if !path.is_file() {
        let _ = atomic_write_text(&path, settings_json);
    }
    path
}

pub fn isolation_argv(env: &Env, bundle_path: &str, settings_json: &str) -> Vec<String> {
    let mut argv = vec!["--setting-sources".to_string(), "project,local".to_string(), format!("--add-dir={bundle_path}")];
    if !settings_json.is_empty() {
        argv.push("--settings".into());
        argv.push(write_settings_file(&data_dir(env).join(RUNTIME_DIR_NAME), settings_json).display().to_string());
    }
    argv
}

pub fn isolation_env(allowlist_file: &Path) -> Vec<(String, String)> {
    vec![("LOOP_ISOLATED".into(), "1".into()), ("LOOP_SKILLS_FILE".into(), allowlist_file.display().to_string())]
}

/// Everything a `claude -p` needs when isolation is on: `{"bundle": ensure_bundle result, "allowlist_file": path}`. A missing plugin is no
/// failure (the bundle then holds no agents and the report lists the skipped skills).
pub fn prepare(env: &Env, project_root: Option<&str>, config: &Map<String, Value>, plugin_root_path: &str, toolkit_dir: &str) -> Value {
    let allowlist = skills_allowlist(project_root, config);
    let bundle =
        ensure_bundle(&data_dir(env), plugin_root_path, &user_skills_dir(env), &all_skill_names(&allowlist), true, toolkit_dir, &installed_plugin_skill_dirs(env));
    json!({"bundle": bundle, "allowlist_file": write_allowlist_file(env, &data_dir(env), &allowlist).display().to_string()})
}

/// The folder (walking up from `folder`) that holds `.specs`; `folder` itself when none does.
pub fn project_root_of(folder: &str) -> String {
    let start = if folder.is_empty() { std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default() } else { folder.to_string() };
    let mut here = PathBuf::from(nt_abspath(&start));
    for _ in 0..8 {
        if here.join(".specs").is_dir() {
            return here.display().to_string();
        }
        match here.parent() {
            Some(parent) if parent != here => here = parent.to_path_buf(),
            _ => break,
        }
    }
    nt_abspath(&start)
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
    fn allowlist_defaults_extras_and_caveman() {
        let list = skills_allowlist(None, &Map::new());
        let get = |p: &str| list.iter().find(|(n, _)| n == p).map(|(_, s)| s.clone()).unwrap();
        assert_eq!(
            list.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
            vec!["orchestrator", "planning", "roadmapmap", "subprojects", "systemmap", "spec", "executor", "verifier", "auto"]
        );
        assert_eq!(get("auto"), get("orchestrator"));
        assert!(get("verifier").contains(&"ui-trace-analysis".to_string()) && !get("verifier").iter().any(|s| s.starts_with("playwright-trace")));
        let config: Map<String, Value> = serde_json::from_value(json!({"loop_extra_skills": [" mine ", ""], "loop_caveman": true})).unwrap();
        let extra = skills_allowlist(None, &config);
        assert!(extra.iter().all(|(_, s)| s.contains(&"mine".to_string()) && s.contains(&"caveman".to_string())));
        assert!(all_skill_names(&extra).windows(2).all(|w| w[0] < w[1]));
        assert!(isolation_enabled(&Map::new()) && !isolation_enabled(&serde_json::from_value(json!({"loop_isolation": false})).unwrap()));
    }

    #[test]
    fn project_skills_come_from_subprojects_json() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            ".specs/project/SUBPROJECTS.json",
            r#"{"repos": {"main": {"projects": [{"path": "a", "skill": "my-skill"}, {"path": "b", "skill": "-"}, {"path": "c", "skill": "skipped", "skip": true}]}}}"#,
        );
        let root = dir.path().display().to_string();
        assert_eq!(project_skill_names(&root), vec!["my-skill"]);
        let list = skills_allowlist(Some(&root), &Map::new());
        assert!(list.iter().find(|(p, _)| p == "executor").unwrap().1.contains(&"my-skill".to_string()));
        assert!(!list.iter().find(|(p, _)| p == "verifier").unwrap().1.contains(&"my-skill".to_string()));
    }

    #[test]
    fn bundle_is_built_once_and_rewrites_the_plugin_root() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = dir.path().join("plugin");
        write(&plugin, "skills/orchestrator/SKILL.md", "run ${CLAUDE_PLUGIN_ROOT}/bin/harness.exe");
        write(&plugin, ".claude-plugin/plugin.json", "{}");
        write(&plugin, "agents/executor-back.md", "agent $CLAUDE_PLUGIN_ROOT/x");
        let user = dir.path().join("user-skills");
        write(&user, "mine/SKILL.md", "mine");
        let data = dir.path().join("data");
        let names = vec!["orchestrator".to_string(), "mine".to_string(), "absent".to_string()];
        let first = ensure_bundle(&data, &plugin.display().to_string(), &user, &names, false, "", &[]);
        assert_eq!(
            (first["rebuilt"].clone(), first["skills"].clone(), first["skipped"].clone(), first["agents"].clone()),
            (json!(true), json!(["mine", "orchestrator"]), json!(["absent"]), json!(["executor-back"]))
        );
        let bundle = PathBuf::from(first["path"].as_str().unwrap());
        let skill = std::fs::read_to_string(bundle.join(".claude/skills/orchestrator/SKILL.md")).unwrap();
        assert!(skill.starts_with("run ") && skill.ends_with("/bin/harness.exe") && !skill.contains("${CLAUDE_PLUGIN_ROOT}"));
        assert!(std::fs::read_to_string(bundle.join(".claude/agents/executor-back.md")).unwrap().contains("/plugin/x"));
        let second = ensure_bundle(&data, &plugin.display().to_string(), &user, &names, false, "", &[]);
        assert_eq!(second["rebuilt"], json!(false));
        write(&plugin, "agents/verifier.md", "v");
        let third = ensure_bundle(&data, &plugin.display().to_string(), &user, &names, false, "", &[]);
        assert_eq!(third["rebuilt"], json!(true));
        assert!(bundle.join(".claude/agents/verifier.md").is_file());
    }

    #[test]
    fn settings_carry_the_guard_and_auto_mode() {
        let dir = tempfile::tempdir().unwrap();
        let env =
            Env::from_pairs([("LOOP_DATA_DIR", dir.path().join("data").display().to_string()), ("LOOP_CLAUDE_HOME", dir.path().join("home").display().to_string())]);
        assert_eq!(guard_command("C:\\Users\\Me\\plugin\\bin\\guard.exe", "auto"), "\"C:/Users/Me/plugin/bin/guard.exe\" --profile auto");
        let settings = guard_settings(&env, "C:\\p\\guard.exe", "auto", Some(&json!({"autoMode": {"allow": ["x & y"]}, "permissions": {"allow": ["Bash"]}})));
        let parsed: Value = serde_json::from_str(&settings).unwrap();
        assert_eq!(parsed["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "\"C:/p/guard.exe\" --profile auto");
        assert_eq!(parsed["autoMode"], json!({"allow": ["x & y"]}));
        assert!(parsed.get("permissions").is_none());
        assert_eq!(guard_settings(&env, "", "auto", Some(&json!({}))), "");
        let argv = isolation_argv(&env, "C:\\b", &settings);
        assert_eq!(&argv[..3], &["--setting-sources", "project,local", "--add-dir=C:\\b"]);
        assert!(Path::new(&argv[4]).is_file());
        let caveman = json!({"hooks": {"SessionStart": [{"matcher": "startup", "hooks": [{"type": "command", "command": "node caveman.js"}, {"type": "command", "command": "other"}]}, {"hooks": [{"command": "z"}]}]}});
        assert_eq!(find_caveman_hooks(&caveman), vec![json!({"matcher": "startup", "hooks": [{"type": "command", "command": "node caveman.js"}]})]);
        let off: Map<String, Value> = Map::new();
        assert!(caveman_hooks_to_carry(&env, &off, Some(&caveman)).is_empty());
    }

    #[test]
    fn allowlist_file_is_shared_or_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::from_pairs([("LOOP_DATA_DIR", dir.path().display().to_string())]);
        let default = skills_allowlist(None, &Map::new());
        let shared = write_allowlist_file(&env, dir.path(), &default);
        assert!(shared.ends_with("skills-allowlist.json"));
        let mut other = default.clone();
        other[0].1.push("extra".into());
        let hashed = write_allowlist_file(&env, dir.path(), &other);
        assert!(hashed.file_name().unwrap().to_string_lossy().starts_with("skills-allowlist-"));
        let text = std::fs::read_to_string(&shared).unwrap();
        assert!(text.starts_with("{\n  \"auto\": ["));
        assert_eq!(isolation_env(&shared)[0], ("LOOP_ISOLATED".to_string(), "1".to_string()));
        assert_eq!(project_root_of(&dir.path().join("a").join("b").display().to_string()), nt_abspath(&dir.path().join("a").join("b").display().to_string()));
    }
}
