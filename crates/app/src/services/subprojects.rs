// SPDX-License-Identifier: MIT
//! Subprojects of a monorepo: the stored list (`.specs/project/SUBPROJECTS.json`), its edition and the Conformance of each one.
//!
//! The file holds only what a person or the AI decided (list, skip, name, stack, confirmation). Everything else is derived from disk on every read:
//! the `Conformance:` line of `<path>/CLAUDE.md`, the map documents of `<path>/docs/codebase` and the leftovers of the old single-project flow in the
//! root. Nothing is mapped or written before the user confirms the list; the root `CLAUDE.md` of a confirmed monorepo is never written; the only
//! exception is the removal of the Conformance block an earlier version of the app left there (between its markers, when the list is confirmed; text
//! outside the markers is never touched).
//!
//! The folder read and written is the work folder: the loop worktree when it exists, else the repository (like `repo_readiness`). `SUBPROJECTS.json`
//! is keyed by repository name, so a repository of `REPOS.json` can be a monorepo too (`repo_name`, default = primary).
//!
//! After "Confirm list" the capabilities block of PROJECT.md and the `maestro:context` blocks are rewritten; after a Conformance write that
//! subproject's context block is (the Python callbacks, now direct calls). `describe` builds each row in `build_project_row` and the whole answer in
//! `build_description`.

use super::code_inventory::inventory_name;
use super::App;
use crate::domain::errors::{AppError, Kind, LOOP_RUNNING_CODE};
use crate::domain::project::ProjectContext;
use crate::repositories::project_files::{Read, SubprojectsFileError, SubprojectsFileRepository};
use engine_state::{run_hints, subprojects};
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const ALLOWED_STACKS: [&str; 11] = ["dotnet", "nestjs", "react", "node", "python", "go", "java", "rust", "php", "ruby", "unknown"];
pub const VALIDATED_BY_SCRIPT: &str = "script";
pub const VALIDATED_BY_USER: &str = "user";
pub const SUBPROJECTS_NOT_CONFIRMED_CODE: &str = "subprojects_not_confirmed";
pub const SUBPROJECTS_NOT_CONFIRMED_MESSAGE: &str = "confirm the list of subprojects first";
pub const LIST_CONFIRMED_CODE: &str = "list_confirmed";
/// The block the app itself wrote (`classify_structure`), between CONFORMANCE:BEGIN and CONFORMANCE:END markers.
static ROOT_CONFORMANCE_BLOCK: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?s)<!-- CONFORMANCE:BEGIN -->.*?<!-- CONFORMANCE:END -->\n?"));
pub const MAP_RELATIVE_FOLDER: &str = "docs/codebase";
const MISSING_CONFORMANCE: &str = "conformance";
const MISSING_MAP: &str = "map";
const MISSING_FOLDER: &str = "folder";
static DRIVE_PREFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z]:"));

/// A subproject action before the list is confirmed (422, `code` = subprojects_not_confirmed).
pub fn not_confirmed() -> AppError {
    AppError::new(Kind::SubprojectsNotConfirmed, SUBPROJECTS_NOT_CONFIRMED_MESSAGE).with_code(SUBPROJECTS_NOT_CONFIRMED_CODE)
}

fn now_text() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let stamp = now.format(time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]")).unwrap_or_default();
    let offset = now.offset();
    let (hours, minutes) = (offset.whole_hours(), offset.minutes_past_hour().abs());
    format!("{stamp}{}{:02}:{:02}", if offset.is_negative() { '-' } else { '+' }, hours.abs(), minutes)
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

fn is_true(value: Option<&Value>) -> bool {
    value == Some(&Value::Bool(true))
}

fn project_folder(folder: &str, path: &str) -> PathBuf {
    path.split('/').filter(|p| !p.is_empty()).fold(PathBuf::from(folder), |a, p| a.join(p))
}

pub struct SubprojectsService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn subprojects(&self) -> SubprojectsService<'_> {
        SubprojectsService { app: self }
    }

    pub fn subprojects_files(&self) -> SubprojectsFileRepository {
        SubprojectsFileRepository::default()
    }
}

impl SubprojectsService<'_> {
    // ------------------------------------------------ target (which repository, which folders)
    /// `(key in SUBPROJECTS.json, work folder of the repository)`. The file itself always lives in the primary's work dir.
    pub fn resolve_target(&self, context: &ProjectContext, repo_name: Option<&str>) -> Result<(String, String), AppError> {
        let primary_name = pycompat::last_segment(&pycompat::nt_normpath(context.repo_path())).to_string();
        let Some(name) = repo_name.filter(|n| !n.is_empty() && *n != primary_name) else { return Ok((primary_name, context.work_dir())) };
        let repo = self.app.repo_registry().resolve(context, name)?;
        if repo.primary {
            return Ok((primary_name, context.work_dir()));
        }
        let worktree = repo.loop_worktree_path();
        Ok((repo.name.clone(), if Path::new(&worktree).is_dir() { worktree } else { repo.path.clone() }))
    }

    pub fn ensure_loop_stopped(&self, context: &ProjectContext) -> Result<(), AppError> {
        if self.app.loop_status().is_loop_running(context) {
            return Err(AppError::loop_running("a loop is running in this project: stop it first").with_code(LOOP_RUNNING_CODE));
        }
        Ok(())
    }

    // ------------------------------------------------ read
    /// The table of the design (see `build_description`). Detects and stores the list when the repository has no entry yet (DET-01). Never fails for
    /// a bad file: `error` is filled and the repository is treated as a single project.
    pub fn describe(&self, context: &ProjectContext, repo_name: Option<&str>) -> Result<Value, AppError> {
        let (key, folder) = self.resolve_target(context, repo_name)?;
        let stored = self.app.subprojects_files().read_repo_entry(&context.work_dir(), &key);
        let entry = match stored {
            Read::Error(SubprojectsFileError { error, .. }) => return Ok(self.build_description(&key, &folder, None, &error)),
            Read::Data(entry) => Some(entry),
            Read::Absent if Path::new(&folder).is_dir() => Some(self.detect_and_store(context, &key, &folder)),
            Read::Absent => None,
        };
        let description = self.build_description(&key, &folder, entry.as_ref(), "");
        self.attach_map_queue(description, context, repo_name)
    }

    /// Fills `queue` and, for the path being mapped now, the row's `map.running` / `map.job`.
    fn attach_map_queue(&self, mut description: Value, context: &ProjectContext, repo_name: Option<&str>) -> Result<Value, AppError> {
        let queue = self.app.subproject_maps().read_queue(context, repo_name)?;
        description["queue"] = queue.clone();
        description["map_queue"] = queue.clone(); // the name of the installer contract: same payload
        let items = queue.get("items").and_then(Value::as_object).cloned();
        let legacy_current = if queue.get("state").and_then(Value::as_str) == Some("running") { queue.get("current").cloned() } else { None };
        if let Some(Value::Array(rows)) = description.get_mut("projects") {
            for row in rows {
                let path = text_of(row.get("path"));
                let item_running = items.as_ref().and_then(|i| i.get(&path)).is_some_and(|item| item.is_object() && item.get("state").and_then(Value::as_str) == Some("running"));
                if item_running || legacy_current.as_ref().is_some_and(|c| c.as_str() == Some(path.as_str())) {
                    row["map"]["running"] = json!(true);
                    row["map"]["job"] = json!({"path": path, "state": "running", "queue_state": queue.get("state").cloned().unwrap_or(Value::Null)});
                }
            }
        }
        Ok(description)
    }

    pub fn build_description(&self, key: &str, folder: &str, entry: Option<&Map<String, Value>>, error: &str) -> Value {
        let monorepo = entry.is_some_and(|e| is_true(e.get("monorepo")));
        let confirmed = entry.is_some_and(|e| is_true(e.get("confirmed")));
        let projects: Vec<Value> = if monorepo { subprojects::entry_projects(entry).iter().map(|p| self.build_project_row(folder, p)).collect() } else { vec![] };
        let confirmed_monorepo = monorepo && confirmed;
        let get = |name: &str| entry.and_then(|e| e.get(name)).cloned();
        let str_or_empty = |name: &str| json!(text_of(get(name).as_ref()));
        json!({
            "ok": true, "repo": key, "monorepo": monorepo, "confirmed": confirmed,
            "validated_by": str_or_empty("validated_by"), "validated_at": str_or_empty("validated_at"),
            "confirmed_at": str_or_empty("confirmed_at"), "reason": str_or_empty("reason"),
            "projects": projects,
            "rejected": get("rejected").filter(|v| engine_state::notify_truthy(Some(v))).unwrap_or(json!([])),
            "notes": get("notes").filter(|v| engine_state::notify_truthy(Some(v))).unwrap_or(json!([])),
            "differences": get("differences").filter(|v| engine_state::notify_truthy(Some(v))).unwrap_or(json!([])), // what the AI validation changed (VAL-03)
            "validation_error": str_or_empty("validation_error"),
            "root_conformance_leftover": confirmed_monorepo && self.root_has_conformance(folder),
            "root_map_leftover": confirmed_monorepo && Self::root_has_old_map(folder),
            "error": error, "job": null, "queue": null, "map_queue": null,
        })
    }

    fn build_project_row(&self, folder: &str, project: &Map<String, Value>) -> Value {
        let path = subprojects::normalize(&text_of(project.get("path")));
        let project_dir = project_folder(folder, &path);
        let exists = project_dir.is_dir();
        let skip = is_true(project.get("skip"));
        let has_code = is_true(project.get("has_code"));
        let structure = self.app.specs_structure();
        let conformance = if exists { structure.get_conformance(&project_dir.display().to_string()) } else { None };
        let suggestion = if exists && conformance.is_none() && !skip { Some(structure.suggest_conformance(&project_dir.display().to_string())) } else { None };
        let map_dir = MAP_RELATIVE_FOLDER.split('/').fold(project_dir.clone(), |a, p| a.join(p));
        let map_files = if exists { run_hints::list_map_documents(&map_dir) } else { vec![] };
        let mut missing: Vec<&str> = Vec::new();
        if !skip {
            if !exists {
                missing.push(MISSING_FOLDER);
            } else {
                if conformance.is_none() {
                    missing.push(MISSING_CONFORMANCE);
                }
                if has_code && map_files.is_empty() {
                    missing.push(MISSING_MAP);
                }
            }
        }
        let (giant_count, giant_top) = self.read_giants(folder, &path);
        let named = |key: &str, default: String| Some(text_of(project.get(key))).filter(|s| !s.is_empty()).unwrap_or(default);
        json!({
            "path": path, "name": named("name", path.clone()), "stack": named("stack", "unknown".into()), "skill": text_of(project.get("skill")), "skip": skip,
            "reason": text_of(project.get("reason")), "has_code": has_code, "warning": text_of(project.get("warning")), "exists": exists,
            "conformance": conformance, "suggestion": suggestion,
            "map": {"files": map_files, "folder": format!("{path}/{MAP_RELATIVE_FOLDER}"),
                    "commit": if exists { run_hints::read_mapped_commit(&map_dir) } else { None }, "running": false, "job": null},
            "giant_files": giant_count, "giant": giant_top, "missing": missing, "ready": skip || (exists && missing.is_empty()),
        })
    }

    /// `(count, top 5 x {path, lines})` from the last inventory of the subproject; zeros when there is none.
    fn read_giants(&self, folder: &str, path: &str) -> (usize, Vec<Value>) {
        let name = inventory_name(path);
        (self.app.inventory.giant_count(folder, &name), self.app.inventory.giant_top(folder, &name, 5))
    }

    /// Old single-project flow left map documents in the root `docs/codebase`: anything but `SYSTEM.md` (the monorepo's own system map) and the
    /// `README.md` index (never listed as a document).
    fn root_has_old_map(folder: &str) -> bool {
        let documents = run_hints::list_map_documents(&project_folder(folder, MAP_RELATIVE_FOLDER));
        documents.iter().any(|name| name != run_hints::SYSTEM_DOCUMENT_NAME)
    }

    /// True while the root CLAUDE.md still has the app-written block between its markers (it is removed on confirm).
    fn root_has_conformance(&self, folder: &str) -> bool {
        let text = self.app.specs.read_text(&Path::new(folder).join("CLAUDE.md")).unwrap_or_default();
        ROOT_CONFORMANCE_BLOCK.search(&text).is_some()
    }

    /// Removes the app-written Conformance block (markers included) from the root CLAUDE.md of a monorepo; everything outside the markers stays as it
    /// was (only the blank line the app added before the block goes with it). True when it was removed.
    fn remove_root_conformance_block(&self, folder: &str) -> bool {
        let path = Path::new(folder).join("CLAUDE.md");
        let Some(text) = self.app.specs.read_text(&path).filter(|t| !t.is_empty()) else { return false };
        let Some(found) = ROOT_CONFORMANCE_BLOCK.search(&text) else { return false };
        let (mut before, mut after) = (text[..found.start].to_string(), text[found.end..].to_string());
        if after.trim_matches(['\r', '\n']).is_empty() {
            before = before.trim_end_matches(['\r', '\n']).to_string();
            after = String::new();
            if !before.is_empty() {
                before.push('\n');
            }
        } else if before.ends_with("\n\n") && after.starts_with('\n') {
            after = after[1..].to_string();
        }
        self.app.specs.write_text_atomic(&path, &format!("{before}{after}")).is_ok()
    }

    // ------------------------------------------------ detection
    /// Runs the script detection and stores it unconfirmed (also for a non-monorepo: `monorepo:false, confirmed:false`).
    pub fn detect_and_store(&self, context: &ProjectContext, key: &str, folder: &str) -> Map<String, Value> {
        let result = subprojects::detect(folder);
        let projects = result.get("projects").cloned().unwrap_or(json!([]));
        let warnings = result.get("warnings").cloned().filter(|w| engine_state::notify_truthy(Some(w))).unwrap_or(json!([]));
        let mut entry = Map::new();
        for (name, value) in [
            ("monorepo", json!(is_true(result.get("monorepo")) || engine_state::notify_truthy(result.get("monorepo")))),
            ("confirmed", json!(false)),
            ("validated_by", json!(VALIDATED_BY_SCRIPT)),
            ("validated_at", json!(now_text())),
            ("reason", json!(text_of(result.get("reason")))),
            ("projects", projects),
            ("rejected", json!([])),
            ("notes", warnings),
        ] {
            entry.insert(name.into(), value);
        }
        if !Path::new(&context.work_dir()).join(".specs").is_dir() {
            return entry; // a read never creates .specs/: that is the structure step's job
        }
        if let Err(failure) = self.app.subprojects_files().write_repo_entry(&context.work_dir(), key, &Value::Object(entry.clone())) {
            self.app.log.warn("subprojects_store_failed", &[("repo", json!(key)), ("type", json!("OSError")), ("message", json!(failure.to_string().chars().take(200).collect::<String>()))]);
        }
        self.app.log.info("subprojects_detected", &[("repo", json!(key)), ("monorepo", entry["monorepo"].clone()), ("projects", json!(entry["projects"].as_array().map(Vec::len).unwrap_or(0)))]);
        entry
    }

    /// Runs the detection again. Refused (409) when the list is confirmed, unless `force`.
    pub fn detect_again(&self, context: &ProjectContext, repo_name: Option<&str>, force: bool) -> Result<Value, AppError> {
        let (key, folder) = self.resolve_target(context, repo_name)?;
        if !Path::new(&folder).is_dir() {
            return Err(AppError::invalid_path(format!("the repo folder does not exist: {folder}")));
        }
        if let Read::Data(stored) = self.app.subprojects_files().read_repo_entry(&context.work_dir(), &key) {
            if is_true(stored.get("confirmed")) && !force {
                return Err(AppError::new(Kind::RepoConflict, "the list is confirmed: detect again only with force").with_code(LIST_CONFIRMED_CODE));
            }
        }
        self.ensure_loop_stopped(context)?;
        let entry = self.detect_and_store(context, &key, &folder);
        Ok(self.build_description(&key, &folder, Some(&entry), ""))
    }

    // ------------------------------------------------ edit / confirm
    /// Edits the stored list. `payload`: `{monorepo?, projects?:[{path, name?, stack?, skip?}], confirmed?}`. `projects` is the whole list: a listed
    /// existing path keeps its fields and takes the given ones, a new path is validated and added, an unlisted one is removed. `monorepo:false` =
    /// "Not a monorepo" (stored with confirmed:true). Returns the description plus `warnings` (e.g. a folder with its own .git).
    pub fn update_list(&self, context: &ProjectContext, payload: &Value, repo_name: Option<&str>) -> Result<Value, AppError> {
        let Some(payload) = payload.as_object() else { return Err(AppError::invalid_request("the body must be a JSON object")) };
        let (key, folder) = self.resolve_target(context, repo_name)?;
        if !Path::new(&folder).is_dir() {
            return Err(AppError::invalid_path(format!("the repo folder does not exist: {folder}")));
        }
        self.ensure_loop_stopped(context)?;
        let mut entry = match self.app.subprojects_files().read_repo_entry(&context.work_dir(), &key) {
            Read::Data(entry) => entry, // an unusable file: the edit starts from a fresh detection and replaces it
            _ => self.detect_and_store(context, &key, &folder),
        };
        let mut warnings: Vec<String> = Vec::new();
        if let Some(requested) = payload.get("projects") {
            let current = subprojects::entry_projects(Some(&entry));
            let edited = self.apply_project_edits(&folder, &current, requested, &mut warnings)?;
            entry.insert("projects".into(), Value::Array(edited));
            entry.insert("validated_by".into(), json!(VALIDATED_BY_USER));
            entry.insert("validated_at".into(), json!(now_text()));
        }
        if let Some(monorepo) = payload.get("monorepo") {
            let Some(flag) = monorepo.as_bool() else { return Err(AppError::invalid_request("`monorepo` must be true or false")) };
            entry.insert("monorepo".into(), json!(flag));
            if !flag {
                entry.insert("confirmed".into(), json!(true));
                entry.insert("confirmed_at".into(), json!(now_text())); // "Not a monorepo": the user's word, never overridden
            }
        }
        if let Some(confirmed) = payload.get("confirmed") {
            let Some(flag) = confirmed.as_bool() else { return Err(AppError::invalid_request("`confirmed` must be true or false")) };
            if flag {
                Self::validate_confirmation(&folder, &entry)?;
                entry.insert("confirmed".into(), json!(true));
                entry.insert("confirmed_at".into(), json!(now_text()));
            } else {
                entry.insert("confirmed".into(), json!(false));
                entry.shift_remove("confirmed_at");
            }
        }
        self.store_updated_entry(context, &key, &folder, &entry);
        if payload.get("confirmed") == Some(&Value::Bool(true)) && is_true(entry.get("monorepo")) {
            self.remove_legacy_root_conformance(&folder);
        }
        let mut description = self.build_description(&key, &folder, Some(&entry), "");
        description["warnings"] = json!(warnings);
        Ok(description)
    }

    /// The confirmation of a monorepo list cleans the block an earlier version wrote in the root CLAUDE.md (never asks).
    fn remove_legacy_root_conformance(&self, folder: &str) {
        if self.remove_root_conformance_block(folder) {
            self.app.log.info("root_conformance_block_removed", &[("folder", json!(folder))]);
        }
    }

    fn store_updated_entry(&self, context: &ProjectContext, key: &str, folder: &str, entry: &Map<String, Value>) {
        let _ = self.app.subprojects_files().write_repo_entry(&context.work_dir(), key, &Value::Object(entry.clone()));
        self.app.log.info(
            "subprojects_list_updated",
            &[("repo", json!(key)), ("monorepo", entry.get("monorepo").cloned().unwrap_or(Value::Null)), ("confirmed", entry.get("confirmed").cloned().unwrap_or(Value::Null)), ("projects", json!(subprojects::entry_projects(Some(entry)).len()))],
        );
        if is_true(entry.get("monorepo")) && is_true(entry.get("confirmed")) {
            // after Confirm list: the capabilities block of PROJECT.md, then the `maestro:context` block of every subproject
            self.app.capabilities().after_list_confirmed(context);
            self.app.context_blocks().refresh_repo(folder, Some(key), Some(&context.work_dir()));
        }
    }

    fn validate_confirmation(folder: &str, entry: &Map<String, Value>) -> Result<(), AppError> {
        if !is_true(entry.get("monorepo")) {
            return Ok(());
        }
        let active: Vec<Map<String, Value>> = subprojects::entry_projects(Some(entry)).into_iter().filter(|p| !is_true(p.get("skip"))).collect();
        if active.is_empty() {
            return Err(AppError::invalid_request("confirm needs at least one subproject that is not skipped (or choose Not a monorepo)"));
        }
        for project in &active {
            let path = text_of(project.get("path"));
            if !project_folder(folder, &subprojects::normalize(&path)).is_dir() {
                return Err(AppError::invalid_request(format!("the folder of {path} does not exist: remove it or skip it")));
            }
        }
        Ok(())
    }

    fn apply_project_edits(&self, folder: &str, current: &[Map<String, Value>], requested: &Value, warnings: &mut Vec<String>) -> Result<Vec<Value>, AppError> {
        let Some(requested) = requested.as_array() else { return Err(AppError::invalid_request("`projects` must be a list")) };
        let by_path: Vec<(String, &Map<String, Value>)> = current.iter().map(|p| (subprojects::normalize(&text_of(p.get("path"))), p)).collect();
        let mut result: Vec<Map<String, Value>> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for item in requested {
            let Some(item) = item.as_object() else { return Err(AppError::invalid_request("each project must be an object with a `path`")) };
            let raw = item.get("path").cloned().unwrap_or(Value::Null);
            let is_new = !by_path.iter().any(|(p, _)| *p == subprojects::normalize(&text_of(Some(&raw))));
            let path = Self::validate_path(folder, &raw, is_new)?;
            for other in &seen {
                if path == *other || path.starts_with(&format!("{other}/")) || other.starts_with(&format!("{path}/")) {
                    return Err(AppError::invalid_request(format!("{path} is duplicated or nested in {other}")));
                }
            }
            seen.push(path.clone());
            let taken: Vec<String> = result.iter().map(|p| text_of(p.get("name"))).collect();
            let mut project = match by_path.iter().find(|(p, _)| *p == path) {
                Some((_, existing)) => (*existing).clone(),
                None => Self::build_new_project(folder, &path, &taken),
            };
            if let Some(warning) = project.get("warning") {
                warnings.push(format!("{path}: {}", text_of(Some(warning))));
            }
            Self::apply_fields(&mut project, item)?;
            result.push(project);
        }
        let names: Vec<String> = result.iter().map(|p| text_of(p.get("name"))).collect();
        let mut distinct = names.clone();
        distinct.sort();
        distinct.dedup();
        if distinct.len() != names.len() {
            return Err(AppError::invalid_request("two subprojects have the same name"));
        }
        Ok(result.into_iter().map(Value::Object).collect())
    }

    fn apply_fields(project: &mut Map<String, Value>, item: &Map<String, Value>) -> Result<(), AppError> {
        let path = text_of(project.get("path"));
        if let Some(name) = item.get("name") {
            let text = if engine_state::notify_truthy(Some(name)) { text_of(Some(name)).trim().to_string() } else { String::new() };
            if text.is_empty() {
                return Err(AppError::invalid_request(format!("the name of {path} cannot be empty")));
            }
            project.insert("name".into(), json!(text));
        }
        if let Some(stack) = item.get("stack") {
            let Some(stack) = stack.as_str().filter(|s| ALLOWED_STACKS.contains(s)) else { return Err(AppError::invalid_request(format!("stack must be one of: {}", ALLOWED_STACKS.join(", ")))) };
            project.insert("stack".into(), json!(stack));
            project.insert("skill".into(), json!(subprojects::skill_by_stack(stack)));
        }
        if let Some(skip) = item.get("skip") {
            let Some(flag) = skip.as_bool() else { return Err(AppError::invalid_request("`skip` must be true or false")) };
            project.insert("skip".into(), json!(flag));
        }
        if let Some(reason) = item.get("reason") {
            project.insert("reason".into(), json!(if engine_state::notify_truthy(Some(reason)) { text_of(Some(reason)) } else { String::new() }));
        }
        Ok(())
    }

    /// Normalized relative path, refused when empty, absolute, leaving the repository (`..`) or (for a new one) not a folder.
    fn validate_path(folder: &str, raw_path: &Value, is_new: bool) -> Result<String, AppError> {
        let text = if engine_state::notify_truthy(Some(raw_path)) { text_of(Some(raw_path)).trim().replace('\\', "/") } else { String::new() };
        if text.is_empty() || text.starts_with('/') || DRIVE_PREFIX.is_match(&text) {
            return Err(AppError::invalid_request(format!("the path must be relative to the repository: {}", pycompat::py_repr_str(&text))));
        }
        let path = subprojects::normalize(&text);
        if path.is_empty() || path.split('/').any(|part| matches!(part, ".." | "." | "")) {
            return Err(AppError::invalid_request(format!("the path cannot leave the repository: {path}")));
        }
        let target = pycompat::nt_realpath(&project_folder(folder, &path).display().to_string());
        let root = pycompat::nt_realpath(folder);
        let (norm_target, norm_root) = (pycompat::nt_normcase(&target), pycompat::nt_normcase(&root));
        let inside = norm_target.starts_with(&format!("{}\\", norm_root.trim_end_matches('\\'))) && norm_target != norm_root;
        if !inside {
            return Err(AppError::invalid_request(format!("the path is outside the repository: {path}")));
        }
        if is_new && !Path::new(&target).is_dir() {
            return Err(AppError::invalid_request(format!("the folder does not exist: {path}")));
        }
        Ok(path)
    }

    fn build_new_project(folder: &str, path: &str, taken_names: &[String]) -> Map<String, Value> {
        let dir = project_folder(folder, path);
        let stack = subprojects::infer_stack(&dir);
        let base = path.replace('/', "-");
        let (mut unique, mut counter) = (base.clone(), 2);
        while taken_names.contains(&unique) {
            unique = format!("{base}-{counter}");
            counter += 1;
        }
        let mut project = Map::new();
        for (key, value) in [
            ("path", json!(path)),
            ("name", json!(unique)),
            ("stack", json!(stack)),
            ("skill", json!(subprojects::skill_by_stack(&stack))),
            ("skip", json!(false)),
            ("reason", json!("")),
            ("has_code", json!(subprojects::has_code_files(&dir))),
        ] {
            project.insert(key.into(), value);
        }
        if dir.join(".git").exists() {
            project.insert("warning".into(), json!("has its own .git (a separate repository inside the folder)"));
        }
        project
    }

    // ------------------------------------------------ Conformance per subproject
    /// Writes the Conformance block in `<work folder>/<path>/CLAUDE.md`. A line that already exists counts as set and is kept. Returns
    /// `{ok, path, conformance, written}`.
    pub fn set_conformance(&self, context: &ProjectContext, path: &str, value: &str, repo_name: Option<&str>) -> Result<Value, AppError> {
        let (key, folder, entry) = self.require_confirmed(context, repo_name)?;
        self.ensure_loop_stopped(context)?;
        let validated = super::specs_structure::SpecsStructureService::validate_conformance(value)?;
        let project = Self::find_active_project(&entry, path)?;
        let mut result = self.write_project_conformance(context, &key, &folder, &project, &validated)?;
        result["ok"] = json!(true);
        Ok(result)
    }

    /// Each non-skipped subproject without a Conformance gets ITS OWN suggestion. Returns `{ok, written:[{path,value}], kept:[{path,value}]}`.
    pub fn set_conformance_all(&self, context: &ProjectContext, repo_name: Option<&str>) -> Result<Value, AppError> {
        let (key, folder, entry) = self.require_confirmed(context, repo_name)?;
        self.ensure_loop_stopped(context)?;
        let (mut written, mut kept) = (Vec::new(), Vec::new());
        for project in subprojects::entry_projects(Some(&entry)) {
            if is_true(project.get("skip")) {
                continue;
            }
            let dir = project_folder(&folder, &subprojects::normalize(&text_of(project.get("path"))));
            if !dir.is_dir() {
                continue;
            }
            let structure = self.app.specs_structure();
            if let Some(existing) = structure.get_conformance(&dir.display().to_string()) {
                kept.push(json!({"path": project.get("path").cloned().unwrap_or(Value::Null), "value": existing}));
                continue;
            }
            let suggestion = structure.suggest_conformance(&dir.display().to_string())["suggestion"].as_str().unwrap_or("LEGACY").to_string();
            let result = self.write_project_conformance(context, &key, &folder, &project, &suggestion)?;
            written.push(json!({"path": result["path"], "value": result["conformance"]}));
        }
        Ok(json!({"ok": true, "written": written, "kept": kept}))
    }

    fn write_project_conformance(&self, context: &ProjectContext, key: &str, folder: &str, project: &Map<String, Value>, value: &str) -> Result<Value, AppError> {
        let path = subprojects::normalize(&text_of(project.get("path")));
        let dir = project_folder(folder, &path);
        if !dir.is_dir() {
            return Err(AppError::invalid_path(format!("the subproject folder does not exist: {path}")));
        }
        let structure = self.app.specs_structure();
        structure.ensure_specs_in_gitignore(folder); // the repo root: `.specs/` always out of git once the Conformance is defined
        if let Some(existing) = structure.get_conformance(&dir.display().to_string()) {
            return Ok(json!({"path": path, "conformance": existing, "written": false}));
        }
        let written = structure.write_conformance(&dir.display().to_string(), value)?;
        self.app.log.info("subproject_conformance_set", &[("repo", json!(key)), ("path", json!(path)), ("value", json!(written))]);
        self.app.context_blocks().refresh_folder(folder, Some(key), &path, Some(&context.work_dir()));
        Ok(json!({"path": path, "conformance": written, "written": true}))
    }

    /// `(key, folder, entry)` of the confirmed monorepo, or the "confirm the list first" refusal.
    pub fn require_confirmed(&self, context: &ProjectContext, repo_name: Option<&str>) -> Result<(String, String, Map<String, Value>), AppError> {
        let (key, folder) = self.resolve_target(context, repo_name)?;
        match self.app.subprojects_files().read_repo_entry(&context.work_dir(), &key) {
            Read::Data(entry) if is_true(entry.get("monorepo")) && is_true(entry.get("confirmed")) => Ok((key, folder, entry)),
            _ => Err(not_confirmed()),
        }
    }

    pub fn find_active_project(entry: &Map<String, Value>, path: &str) -> Result<Map<String, Value>, AppError> {
        let normalized = subprojects::normalize(&path.replace('\\', "/"));
        for project in subprojects::entry_projects(Some(entry)) {
            if subprojects::normalize(&text_of(project.get("path"))) == normalized {
                if is_true(project.get("skip")) {
                    return Err(AppError::invalid_request(format!("{normalized} is skipped: it needs no Conformance")));
                }
                return Ok(project);
            }
        }
        Err(AppError::invalid_request(format!("unknown subproject: {normalized}")))
    }
}
