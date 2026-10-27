// SPDX-License-Identifier: MIT
//! AI validation of the subprojects list (job type `subprojects`).
//!
//! The script detection (the engine's `subprojects`) stores a list in `.specs/project/SUBPROJECTS.json`. This service asks claude (pinned to the job
//! model ceiling, no MCP, guard profile `subprojects`) to read the root and every candidate and to write ONLY
//! `.specs/project/SUBPROJECTS-SUGGESTED.json`. When the job ends, the job thread (`finalize`) validates each suggested entry and merges the valid
//! ones into the stored entry. The AI never edits the real file, so a bad suggestion cannot corrupt the state, and the list stays `confirmed: false`:
//! only the user's click confirms it.
//!
//! Merge result stored in the repository entry: `validated_by: "ai"`, `validated_at`, `confirmed: false`, `notes` (the AI's notes), `rejected:
//! [{path, reason}]` (entries that were not applied) and `differences: [{path, change}]` where change is "added", "skipped: <reason>", "stack
//! changed: a -> b" (and similar). A failed job or an unreadable suggestion keeps the script list and stores `validation_error`; the job then ends as
//! an error so the panel shows the failure (VAL-05).

use super::claude_jobs::{ClaudeJobCommand, JOB_MODEL_CEILING};
use super::map_job::JOB_PERMISSION_MODE;
use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use crate::infrastructure::claude_client::CommandOptions;
use crate::repositories::loop_read_formatting::now_iso;
use crate::repositories::project_files::Read;
use engine_state::subprojects as engine;
use pycompat::{py_repr_json, Re};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::LazyLock;

pub const JOB_TYPE_SUBPROJECTS: &str = "subprojects";
const HOOK_PROFILE: &str = "subprojects";
const ALLOWED_STACKS: [&str; 11] = ["dotnet", "nestjs", "react", "node", "python", "go", "java", "rust", "php", "ruby", "unknown"];
const VALIDATED_BY_AI: &str = "ai";
const SUGGESTED_RELATIVE_TEXT: &str = ".specs/project/SUBPROJECTS-SUGGESTED.json";
const REASON_LIMIT: usize = 300;
const NOTES_LIMIT: usize = 20;
const NOTE_LIMIT: usize = 500;

static DRIVE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z]:"));

const VALIDATION_PROMPT: &str = "You validate the list of subprojects of a repository that may be a monorepo. Work read-only, except for ONE file.

Read the repository root and every candidate folder of the script list below: project files (*.sln, *.csproj, package.json,
pyproject.toml, go.mod, pom.xml, ...), README, CLAUDE.md, docs/ and the first level of the source folders. Decide, for each candidate, its
name, its stack and whether it is a real subproject. You may add a folder the script missed (a folder with its own project file that is
not inside another listed project) and you may propose `skip: true` (with a short `reason`) for a folder that has no code of its own
(scripts, tooling, data, docs, infra).

Write ONLY the file `%(suggested)s` (create the folder if needed) with exactly this JSON shape, and finish:
{\"projects\": [{\"path\": \"backend\", \"name\": \"backend\", \"stack\": \"dotnet\", \"skill\": \"backend-dev\", \"skip\": false, \"reason\": \"\"}], \"notes\": [\"short note\"]}

Rules:
- `path` is relative to the repository root, uses `/`, exists and is a folder inside the repository. Never list a folder nested inside another listed folder, and never list a folder twice.
- `stack` is one of: %(stacks)s.
- `skill` is `backend-dev` for dotnet and nestjs, `frontend-dev` for react, and an empty string for every other stack.
- `reason` explains a skip (or any change you made); `notes` holds at most a few short sentences for the user.
- Do not edit, create or delete any other file, do not run git, and do not ask questions.

Script list (JSON):
%(projects)s
";

fn short_text(value: Option<&Value>, limit: usize) -> String {
    crate::repositories::loop_read_formatting::text_or_empty(value).trim().chars().take(limit).collect()
}

fn is_inside(folder: &str, target: &str) -> bool {
    let root = pycompat::nt_normcase(&pycompat::nt_realpath(folder));
    let resolved = pycompat::nt_normcase(&pycompat::nt_realpath(target));
    resolved == root || resolved.starts_with(&format!("{root}\\"))
}

pub struct SubprojectValidationService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn subproject_validation(&self) -> SubprojectValidationService<'_> {
        SubprojectValidationService { app: self }
    }
}

impl SubprojectValidationService<'_> {
    // ------------------------------------------------ start
    /// Registers the job and returns it (public record, state `running`). The AI runs in the job thread. No spend cap: `budget_usd` is accepted and
    /// ignored by the caller (an old client may still send it).
    pub fn start(&self, context: &ProjectContext, repo_name: Option<&str>) -> Result<Value, AppError> {
        let (key, folder) = self.app.subprojects().resolve_target(context, repo_name)?;
        self.app.log.info("subprojects_validation_requested", &[("repo", json!(key))]);
        let prepared = (|| {
            if !Path::new(&folder).is_dir() {
                return Err(AppError::invalid_path(format!("the repo folder does not exist: {folder}")));
            }
            self.ensure_repo_free_for_job(context)?;
            self.load_or_detect_entry(context, &key, &folder)
        })();
        let entry = match prepared {
            Ok(entry) => entry,
            Err(refusal) => {
                self.app.log.info(
                    "subprojects_validation_refused",
                    &[("repo", json!(key)), ("reason", json!(format!("{:?}", refusal.kind))), ("message", json!(refusal.message.chars().take(200).collect::<String>()))],
                );
                return Err(refusal);
            }
        };
        self.app.subprojects_files().delete_suggested(&context.work_dir()); // a stale suggestion must never be applied
        let options = CommandOptions {
            model: JOB_MODEL_CEILING.into(),
            budget_usd: None,
            no_mcp: true,
            prompt_argument: Self::build_prompt(&entry),
            permission_mode: JOB_PERMISSION_MODE.into(),
            hook_profile: HOOK_PROFILE.into(),
            ..CommandOptions::default()
        };
        let mut command = ClaudeJobCommand::new(self.app.claude.build_command(&options), "");
        command.work_dir = Some(context.work_dir());
        let (app, finish_context, finish_key, finish_folder) = (self.app.shared(), context.clone(), key, folder);
        command.finalize = Some(Box::new(move |result| app.subproject_validation().finalize(&finish_context, &finish_key, &finish_folder, result)));
        self.app.jobs().start_job(context.repo_path(), JOB_TYPE_SUBPROJECTS, command)
    }

    /// The job writes in the work folder, so it needs the loop stopped and no other claude job running.
    fn ensure_repo_free_for_job(&self, context: &ProjectContext) -> Result<(), AppError> {
        if self.app.loop_status().is_loop_running(context) {
            return Err(AppError::loop_running("a loop is running in this project: stop it first"));
        }
        if self.app.jobs().get_running_job(context.repo_path()).is_some() {
            return Err(AppError::new(Kind::JobInProgress, "a claude job is already running in this project: wait for it to finish"));
        }
        Ok(())
    }

    fn load_or_detect_entry(&self, context: &ProjectContext, key: &str, folder: &str) -> Result<Map<String, Value>, AppError> {
        match self.app.subprojects_files().read_repo_entry(&context.work_dir(), key) {
            Read::Error(error) => return Err(AppError::invalid_request(format!("SUBPROJECTS.json cannot be read: {}", error.error))),
            Read::Data(entry) => return Ok(entry),
            Read::Absent => {}
        }
        let result = engine::detect(folder);
        let mut entry = Map::new();
        entry.insert("monorepo".into(), json!(result["monorepo"] == true));
        entry.insert("confirmed".into(), json!(false));
        entry.insert("validated_by".into(), json!("script"));
        entry.insert("validated_at".into(), json!(now_iso()));
        entry.insert("reason".into(), json!(crate::repositories::loop_read_formatting::text_or_empty(result.get("reason"))));
        entry.insert("projects".into(), json!(result["projects"].as_array().cloned().unwrap_or_default()));
        entry.insert("rejected".into(), json!([]));
        entry.insert("notes".into(), json!(result["warnings"].as_array().cloned().unwrap_or_default()));
        let _ = self.app.subprojects_files().write_repo_entry(&context.work_dir(), key, &Value::Object(entry.clone()));
        Ok(entry)
    }

    fn build_prompt(entry: &Map<String, Value>) -> String {
        let projects: Vec<Value> = engine::entry_projects(Some(entry))
            .iter()
            .map(|project| {
                let mut row = Map::new();
                for field in ["path", "name", "stack", "skill", "skip", "reason", "has_code"] {
                    row.insert(field.into(), project.get(field).cloned().unwrap_or(Value::Null));
                }
                Value::Object(row)
            })
            .collect();
        VALIDATION_PROMPT
            .replace("%(suggested)s", SUGGESTED_RELATIVE_TEXT)
            .replace("%(stacks)s", &ALLOWED_STACKS.join(", "))
            .replace("%(projects)s", &pycompat::json::py_json_dumps_indent(&Value::Array(projects), false, 2))
    }

    // ------------------------------------------------ finalize (job thread)
    /// Reads and deletes the suggestion, validates, merges and stores. Returns the job result (`ok=false` when nothing was applied).
    fn finalize(&self, context: &ProjectContext, key: &str, folder: &str, result: Value) -> Value {
        let files = self.app.subprojects_files();
        let work_dir = context.work_dir();
        let suggestion = files.read_suggested(&work_dir);
        files.delete_suggested(&work_dir);
        if result["ok"] != true {
            let reason = match result.get("error") {
                Some(e) if engine_state::notify_truthy(Some(e)) => crate::repositories::loop_read_formatting::text_or_empty(Some(e)),
                _ => "no reason given".to_string(),
            };
            return self.fail(context, key, result, &format!("the validation job failed: {reason}"));
        }
        let suggestion = match suggestion {
            Read::Absent => return self.fail(context, key, result, &format!("the AI did not write {SUGGESTED_RELATIVE_TEXT}")),
            Read::Error(error) => return self.fail(context, key, result, &format!("the AI suggestion is unreadable: {}", error.error)),
            Read::Data(map) => map,
        };
        if !suggestion.get("projects").is_some_and(Value::is_array) {
            return self.fail(context, key, result, "the AI suggestion has no `projects` list");
        }
        let Read::Data(stored) = files.read_repo_entry(&work_dir, key) else {
            return self.fail(context, key, result, "the stored list is not available to merge the suggestion");
        };
        let merged = self.merge_suggestion(folder, &stored, &suggestion);
        let _ = files.write_repo_entry(&work_dir, key, &Value::Object(merged.clone()));
        self.app.log.info(
            "subprojects_validated",
            &[
                ("repo", json!(key)),
                ("projects", json!(merged["projects"].as_array().map_or(0, Vec::len))),
                ("rejected", json!(merged["rejected"].as_array().map_or(0, Vec::len))),
                ("differences", json!(merged["differences"].as_array().map_or(0, Vec::len))),
            ],
        );
        result
    }

    /// Keeps the script list and stores why the validation did not apply (VAL-05); the job ends as an error.
    fn fail(&self, context: &ProjectContext, key: &str, result: Value, message: &str) -> Value {
        self.app.log.warn("subprojects_validation_failed", &[("repo", json!(key)), ("message", json!(message.chars().take(200).collect::<String>()))]);
        if let Read::Data(mut stored) = self.app.subprojects_files().read_repo_entry(&context.work_dir(), key) {
            stored.insert("validation_error".into(), json!(message.chars().take(NOTE_LIMIT).collect::<String>()));
            if let Err(failure) = self.app.subprojects_files().write_repo_entry(&context.work_dir(), key, &Value::Object(stored)) {
                self.app.log.warn("subprojects_store_failed", &[("repo", json!(key)), ("type", json!(format!("{:?}", failure.kind())))]);
            }
        }
        let mut out = result.as_object().cloned().unwrap_or_default();
        out.insert("ok".into(), json!(false));
        out.insert("error".into(), json!(message));
        Value::Object(out)
    }

    // ------------------------------------------------ validation and merge
    /// The stored entry with the valid suggested entries applied (the input maps are not modified).
    fn merge_suggestion(&self, folder: &str, stored: &Map<String, Value>, suggestion: &Map<String, Value>) -> Map<String, Value> {
        let mut projects: Vec<Map<String, Value>> = engine::entry_projects(Some(stored));
        let (mut rejected, mut differences, mut seen): (Vec<Value>, Vec<Value>, Vec<String>) = (vec![], vec![], vec![]);
        for raw in suggestion["projects"].as_array().into_iter().flatten() {
            let raw_path = raw.get("path");
            let problem = self.find_entry_problem(folder, raw, &projects, &seen);
            if !problem.is_empty() {
                rejected.push(json!({"path": short_text(raw_path, REASON_LIMIT), "reason": problem}));
                continue;
            }
            let path = engine::normalize(raw["path"].as_str().unwrap_or(""));
            seen.push(path.clone());
            let index = match projects.iter().position(|p| engine::normalize(p.get("path").and_then(Value::as_str).unwrap_or("")) == path) {
                Some(index) => index,
                None => {
                    let has_code = engine::has_code_files(&path.split('/').fold(Path::new(folder).to_path_buf(), |p, part| p.join(part)));
                    let fresh = json!({"path": path, "name": "", "stack": "unknown", "skill": "", "skip": false, "reason": "", "has_code": has_code});
                    projects.push(fresh.as_object().cloned().unwrap_or_default());
                    differences.push(json!({"path": path, "change": "added"}));
                    projects.len() - 1
                }
            };
            Self::apply_entry(&mut projects[index], raw, &path, &mut differences);
        }
        let notes: Vec<String> = match suggestion.get("notes") {
            Some(Value::Array(list)) => list.iter().map(|n| short_text(Some(n), NOTE_LIMIT)).filter(|n| !n.is_empty()).take(NOTES_LIMIT).collect(),
            Some(single) if engine_state::notify_truthy(Some(single)) => Some(short_text(Some(single), NOTE_LIMIT)).filter(|n| !n.is_empty()).into_iter().collect(),
            _ => vec![],
        };
        let mut merged: Map<String, Value> = stored.iter().filter(|(k, _)| k.as_str() != "validation_error").map(|(k, v)| (k.clone(), v.clone())).collect();
        merged.insert("projects".into(), json!(projects));
        merged.insert("confirmed".into(), json!(false));
        merged.insert("validated_by".into(), json!(VALIDATED_BY_AI));
        merged.insert("validated_at".into(), json!(now_iso()));
        merged.insert("notes".into(), json!(notes));
        merged.insert("rejected".into(), json!(rejected));
        merged.insert("differences".into(), json!(differences));
        merged
    }

    fn apply_entry(project: &mut Map<String, Value>, raw: &Value, path: &str, differences: &mut Vec<Value>) {
        let previous = project.clone();
        let name = short_text(raw.get("name"), REASON_LIMIT);
        let old_name = crate::repositories::loop_read_formatting::text_or_empty(previous.get("name"));
        project.insert(
            "name".into(),
            json!(if !name.is_empty() {
                name
            } else if !old_name.is_empty() {
                old_name.clone()
            } else {
                path.replace('/', "-")
            }),
        );
        project.insert("stack".into(), raw["stack"].clone());
        project.insert("skill".into(), json!(crate::repositories::loop_read_formatting::text_or_empty(raw.get("skill"))));
        project.insert("skip".into(), json!(engine_state::notify_truthy(raw.get("skip"))));
        project.insert("reason".into(), json!(short_text(raw.get("reason"), REASON_LIMIT)));
        let (skip_now, reason) = (project["skip"] == true, project["reason"].as_str().unwrap_or("").to_string());
        let was_skipped = engine_state::notify_truthy(previous.get("skip"));
        if engine_state::notify_truthy(previous.get("path")) && !was_skipped && skip_now {
            differences.push(json!({"path": path, "change": format!("skipped: {}", if reason.is_empty() { "no reason given" } else { &reason })}));
        } else if was_skipped && !skip_now {
            differences.push(json!({"path": path, "change": "no longer skipped"}));
        }
        let old_stack = crate::repositories::loop_read_formatting::text_or_empty(previous.get("stack"));
        if !old_stack.is_empty() && previous.get("stack") != project.get("stack") && !old_name.is_empty() {
            differences.push(json!({"path": path, "change": format!("stack changed: {old_stack} -> {}", crate::repositories::loop_read_formatting::text_or_empty(project.get("stack")))}));
        }
        if !old_name.is_empty() && Some(&json!(old_name)) != project.get("name") {
            differences.push(
                json!({"path": path, "change": format!("renamed: {old_name} -> {}", crate::repositories::loop_read_formatting::text_or_empty(project.get("name")))}),
            );
        }
    }

    /// The reason a suggested entry is not applied, or '' when it is valid.
    fn find_entry_problem(&self, folder: &str, raw: &Value, projects: &[Map<String, Value>], seen: &[String]) -> String {
        let Some(raw_map) = raw.as_object() else { return "the entry is not an object".into() };
        let Some(raw_path) = raw_map.get("path").and_then(Value::as_str).filter(|p| !p.trim().is_empty()) else { return "the path is missing".into() };
        let text = raw_path.trim().replace('\\', "/");
        let segments: Vec<&str> = text.split('/').filter(|s| !s.is_empty()).collect();
        if text.starts_with('/') || DRIVE.match_start(&text).is_some() || segments.contains(&"..") || segments.is_empty() || segments == ["."] {
            return "the path is outside the repository".into();
        }
        let path = engine::normalize(&text);
        let target = path.split('/').fold(Path::new(folder).to_path_buf(), |p, part| p.join(part));
        if !is_inside(folder, &target.display().to_string()) {
            return "the path is outside the repository".into();
        }
        if !target.is_dir() {
            return "the folder does not exist".into();
        }
        if seen.contains(&path) {
            return "the path is listed twice".into();
        }
        let Some(stack) = raw_map.get("stack").and_then(Value::as_str).filter(|s| ALLOWED_STACKS.contains(s)) else {
            return format!("the stack {} is not one of: {}", py_repr_json(raw_map.get("stack").unwrap_or(&Value::Null)), ALLOWED_STACKS.join(", "));
        };
        let skill = crate::repositories::loop_read_formatting::text_or_empty(raw_map.get("skill"));
        if skill != engine::skill_by_stack(stack) {
            return format!("the skill {} does not match the stack {stack}", pycompat::py_repr_str(&skill));
        }
        if raw_map.get("skip").is_some_and(|s| !s.is_boolean()) {
            return "`skip` is not true or false".into();
        }
        for other in projects {
            let base = engine::normalize(other.get("path").and_then(Value::as_str).unwrap_or(""));
            if base == path {
                continue;
            }
            if path.starts_with(&format!("{base}/")) {
                return format!("nested inside the listed project {base}");
            }
            if base.starts_with(&format!("{path}/")) {
                return format!("contains the listed project {base}");
            }
        }
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_with_folder() -> (tempfile::TempDir, std::sync::Arc<App>, String) {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let root = dir.path().join("mono");
        for sub in ["api", "web", "web/inner", "tools"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        (dir, app, root.display().to_string())
    }

    #[test]
    fn suggestions_are_validated_entry_by_entry() {
        let (_dir, app, folder) = service_with_folder();
        let service = app.subproject_validation();
        let stored = json!({"projects": [{"path": "api", "name": "api", "stack": "node", "skill": "", "skip": false}, {"path": "web", "name": "web", "stack": "react", "skill": "frontend-dev", "skip": false}]});
        let stored = stored.as_object().unwrap();
        let projects: Vec<Map<String, Value>> = stored["projects"].as_array().unwrap().iter().map(|p| p.as_object().unwrap().clone()).collect();
        let check = |raw: Value, seen: &[String]| service.find_entry_problem(&folder, &raw, &projects, seen);
        assert_eq!(check(json!(5), &[]), "the entry is not an object");
        assert_eq!(check(json!({"path": " "}), &[]), "the path is missing");
        assert_eq!(check(json!({"path": "../x", "stack": "node"}), &[]), "the path is outside the repository");
        assert_eq!(check(json!({"path": "C:/x"}), &[]), "the path is outside the repository");
        assert_eq!(check(json!({"path": "nope", "stack": "node"}), &[]), "the folder does not exist");
        assert_eq!(check(json!({"path": "tools", "stack": "node"}), &["tools".into()]), "the path is listed twice");
        assert_eq!(
            check(json!({"path": "tools", "stack": "cobol"}), &[]),
            "the stack 'cobol' is not one of: dotnet, nestjs, react, node, python, go, java, rust, php, ruby, unknown"
        );
        assert_eq!(check(json!({"path": "tools", "stack": "react", "skill": ""}), &[]), "the skill '' does not match the stack react");
        assert_eq!(check(json!({"path": "tools", "stack": "node", "skip": "yes"}), &[]), "`skip` is not true or false");
        assert_eq!(check(json!({"path": "web/inner", "stack": "node"}), &[]), "nested inside the listed project web");
        assert_eq!(check(json!({"path": "tools", "stack": "node", "skip": true, "reason": "scripts"}), &[]), "");
    }

    #[test]
    fn merging_records_differences_and_never_confirms() {
        let (_dir, app, folder) = service_with_folder();
        let service = app.subproject_validation();
        let stored = json!({"confirmed": true, "validation_error": "old", "projects": [{"path": "api", "name": "api", "stack": "node", "skill": "", "skip": false}]});
        let suggestion = json!({"projects": [
            {"path": "api", "name": "backend", "stack": "dotnet", "skill": "backend-dev", "skip": true, "reason": "tooling"},
            {"path": "tools", "name": "tools", "stack": "unknown", "skill": "", "skip": false},
            {"path": "ghost", "stack": "node"}], "notes": ["one", "", "two"]});
        let merged = service.merge_suggestion(&folder, stored.as_object().unwrap(), suggestion.as_object().unwrap());
        assert_eq!((merged["confirmed"].clone(), merged["validated_by"].clone(), merged["notes"].clone()), (json!(false), json!("ai"), json!(["one", "two"])));
        assert!(!merged.contains_key("validation_error"));
        assert_eq!(merged["rejected"], json!([{"path": "ghost", "reason": "the folder does not exist"}]));
        let changes: Vec<&str> = merged["differences"].as_array().unwrap().iter().map(|d| d["change"].as_str().unwrap()).collect();
        assert_eq!(changes, ["skipped: tooling", "stack changed: node -> dotnet", "renamed: api -> backend", "added"]);
    }

    #[test]
    fn a_failed_job_keeps_the_script_list_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("mono");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        app.subprojects_files().write_repo_entry(&context.work_dir(), "mono", &json!({"monorepo": true, "projects": []})).unwrap();
        let out = app.subproject_validation().finalize(&context, "mono", &repo.display().to_string(), json!({"ok": false, "error": "boom"}));
        assert_eq!((out["ok"].clone(), out["error"].clone()), (json!(false), json!("the validation job failed: boom")));
        let Read::Data(entry) = app.subprojects_files().read_repo_entry(&context.work_dir(), "mono") else { panic!("entry") };
        assert_eq!(entry["validation_error"], "the validation job failed: boom");
        let none = app.subproject_validation().finalize(&context, "mono", &repo.display().to_string(), json!({"ok": true}));
        assert_eq!(none["error"], format!("the AI did not write {SUGGESTED_RELATIVE_TEXT}"));
    }
}
