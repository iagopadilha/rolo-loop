// SPDX-License-Identifier: MIT
//! Import of a spec into the planning conversation: reads a tlc-spec-driven FEATURE FOLDER (`spec.md` required; every `.md` inside it, subfolders
//! too, up to the scan limits), read only, and copies it twice (credential redaction on both copies): into
//! `.specs/project/loop/internal/planning/imported/<stamp>-<name>/` (relative paths kept, with `import.json` and `IMPORT-LOG.md`) and into
//! `.specs/features/<slug>/` of the work folder. It sends ONE fixed planning message that tells the planning AI to read the copies and build ROADMAP,
//! FLOWS, PROJECT, the repository scope and `.specs/project/IMPORT-COVERAGE.md`.
//!
//! `preview_import` shows what an import would take (no copy, no AI). The same feature (same source folder or same slug) is not imported twice:
//! `already_imported` (409) carries the earlier log path. Another feature adds to the plan. A refusal of the folder is an `InvalidRequest` (400) and
//! nothing is copied or sent.

use super::claude_jobs::ClaudeJobError;
use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use crate::domain::spec_import_scan::{build_warnings, detect_items, scan_folder, DEFAULT_LIMITS, MAIN_SPEC_NAME, STATUS_READ};
use crate::infrastructure::claude_client::redact_secrets;
use crate::repositories::loop_command_file::now_iso;
use engine_state::loop_state;
use pycompat::json::py_json_dumps_indent;
use pycompat::Re;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const IMPORTED_FOLDER_PARTS: [&str; 2] = ["planning", "imported"];
const FEATURES_FOLDER_PARTS: [&str; 2] = [".specs", "features"];
pub const META_FILE_NAME: &str = "import.json";
pub const IMPORT_LOG_NAME: &str = "IMPORT-LOG.md";
const DEFAULT_IMPORT_NAME: &str = "spec";
const NAME_LIMIT: usize = 40;
pub const ALREADY_IMPORTED_CODE: &str = "already_imported";
pub const FEATURE_FOLDER_EXISTS_CODE: &str = "feature_folder_exists";
const COVERAGE_LINE_PREFIX: &str = "Coverage (";
static IMPORT_ID_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[0-9]{8}-[0-9]{6}-[a-z0-9._-]{1,60}\z"));
static UNSAFE_NAME_CHARACTERS: LazyLock<Re> = LazyLock::new(|| Re::new(r"[^a-z0-9._-]+"));
const IMPORT_MESSAGE: &str = "Import of a spec feature. The files below are read-only copies of a tlc-spec-driven feature folder the user already wrote (main spec: {main}):\n{files}\n\nRead them with the Read tool, then build the plan of THIS project (.specs/project) from them, in the grammar of the orchestrator references:\n1. .specs/project/ROADMAP.md: one line per feature with `title=\"...\"`, `paths=<repo>:<path>,...` and `deps=`. Names must be descriptive: the slug says what the feature delivers in 3 to 6 kebab-case words (verb + object, e.g. `import-spec-feature-into-planning`, never `feature-1`, `part-a` or a bare noun like `calculator`); the title is a short sentence a reader understands without opening the spec; every stage header names the outcome of the stage, not `Stage 1`.\n2. .specs/project/FLOWS.md: one flow of 2+ steps per feature.\n3. .specs/project/PROJECT.md: the short project context (Vision filled in); never edit inside the maestro:capabilities markers.\n4. The repository scope in .specs/project/REPOS-SUGGESTED.json: every repository the spec names (absolute paths, `<repo>:path`, catalog names) with `role` edit or reference and a one-line `reason`.\n5. Whatever the spec leaves open goes to the `## Open questions` section of the ROADMAP (`<question> | recommended: <answer>`); do not ask the user for what the spec already answers.\n\nCoverage rules (nothing of the source may be lost). The source is also copied to {feature_dir}/ (the complete spec of this feature):\n- The ROADMAP is an INDEX, not a copy: every feature built from the source points to {feature_dir}/ and lists the requirement IDs it covers as `reqs=ORIG-01..06,CRIA-01..08`.\n- A Large source (SCOPE Large/Complex, or a tasks.md with waves or tracks) is split into ROADMAP features following those waves and tracks (deps from the wave order); a Small/Medium source stays one feature. Never rewrite the imported spec: the spec-writer reviews and completes it.\n- Write .specs/project/IMPORT-COVERAGE.md: under `## {slug}` one Markdown table `| Item | Kind | Destination |` with one row for EVERY id below and EVERY core section present. Kind is one of requirement, decision, task, goal, out-of-scope, edge-case, validation, environment, risk, paths, open-question, other. Destination is a location (`ROADMAP:[slug] reqs`, `RULES.md#...`, `FLOWS.md#...`, `context.md`) or `dropped: <reason>`; ranges like `ORIG-01..06` are accepted.\n- Never summarize a requirement, a limit or a validation away. `dropped: <reason>` is the only way to leave an item out.\n\nDetected in the source:\n{detected}";

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

fn expand_user(path: &str) -> String {
    if path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
        return format!("{}{}", cfg::Env::process().home_dir().display(), &path[1..]);
    }
    path.to_string()
}

fn normalize_source(path: &str) -> String {
    pycompat::nt_normcase(&pycompat::nt_abspath(&expand_user(path)))
}

fn format_size(size: u64) -> String {
    format!("{} KB", ((size as f64 / 1024.0).round_ties_even() as u64).max(1))
}

/// The detected ids and the core sections present, as the lines of the import message.
pub fn describe_detected(items: &Value) -> String {
    let mut lines: Vec<String> = ["requirements", "decisions", "tasks"]
        .iter()
        .map(|key| {
            let list: Vec<String> = items.get(*key).and_then(Value::as_array).map(|a| a.iter().map(|i| text_of(Some(i))).collect()).unwrap_or_default();
            format!("- {key}: {}", if list.is_empty() { "none".to_string() } else { list.join(", ") })
        })
        .collect();
    let sections: Vec<String> = items.get("sections").and_then(Value::as_object).map(|m| m.iter().filter(|(_, present)| engine_state::notify_truthy(Some(present))).map(|(name, _)| name.clone()).collect()).unwrap_or_default();
    lines.push(format!("- core sections present: {}", if sections.is_empty() { "none".to_string() } else { sections.join(", ") }));
    lines.join("\n")
}

pub fn sanitize_name(name: &str) -> String {
    let lowered = UNSAFE_NAME_CHARACTERS.replace_lit(&name.trim().to_lowercase(), "-");
    let base = lowered.trim_matches(['-', '.']);
    let cut: String = base.chars().take(NAME_LIMIT).collect();
    let cut = cut.trim_matches(['-', '.']).to_string();
    if cut.is_empty() { DEFAULT_IMPORT_NAME.into() } else { cut }
}

pub fn imported_root(work_dir: &str) -> PathBuf {
    IMPORTED_FOLDER_PARTS.iter().fold(loop_state::internal_dir(work_dir), |a, p| a.join(p))
}

pub struct PlanningImportService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn planning_imports(&self) -> PlanningImportService<'_> {
        PlanningImportService { app: self }
    }
}

impl PlanningImportService<'_> {
    // ------------------------------------------------ paths
    fn relative_folder(context: &ProjectContext, import_id: &str) -> String {
        let root = imported_root(&context.work_dir());
        let relative = root.strip_prefix(context.work_dir()).map(|p| p.display().to_string()).unwrap_or_default().replace('\\', "/");
        format!("{relative}/{import_id}")
    }

    fn feature_relative_folder(slug: &str) -> String {
        format!("{}/{slug}", FEATURES_FOLDER_PARTS.join("/"))
    }

    fn feature_folder_exists(context: &ProjectContext, slug: &str) -> bool {
        FEATURES_FOLDER_PARTS.iter().fold(PathBuf::from(context.work_dir()), |a, p| a.join(p)).join(slug).exists()
    }

    // ------------------------------------------------ preview
    /// `{slug, files, skipped, counts, items, warnings, refused, already_imported}`: what an import would take. Nothing is copied and no AI is called.
    pub fn preview_import(&self, context: &ProjectContext, body: &Value) -> Result<Value, AppError> {
        let source = Self::read_source_path(body)?;
        let scan = scan_folder(&source, &DEFAULT_LIMITS)?;
        let files = scan["files"].as_array().cloned().unwrap_or_default();
        let read_files: Vec<Value> = files.iter().filter(|e| e["status"] == STATUS_READ).cloned().collect();
        let slug = Self::slug_of(body, &source);
        let refused = t(scan.get("refused"));
        let items = detect_items(if refused { &[] } else { &read_files });
        let conformance = super::context_block::ContextBlockService::read_conformance_text(&context.work_dir(), "");
        let mut warnings: Vec<String> = if refused { vec![] } else { build_warnings(&scan, &items, &conformance) };
        let earlier = self.find_earlier_import(context, &source, &slug);
        if earlier.is_none() && Self::feature_folder_exists(context, &slug) {
            warnings.push(format!("{} already exists without an import record: the import will be refused", Self::feature_relative_folder(&slug)));
        }
        let mut counts = scan["counts"].as_object().cloned().unwrap_or_default();
        let len = |key: &str| items.get(key).and_then(Value::as_array).map(Vec::len).unwrap_or(0);
        counts.insert("requirements".into(), json!(len("requirements")));
        counts.insert("decisions".into(), json!(len("decisions")));
        counts.insert("tasks".into(), json!(len("tasks")));
        for (key, source_key) in [("out_of_scope_rows", "out_of_scope_rows"), ("edge_cases", "edge_cases"), ("success_criteria", "success_criteria")] {
            counts.insert(key.into(), items.get(source_key).cloned().unwrap_or(Value::Null));
        }
        Ok(json!({
            "slug": slug, "files": read_files.iter().map(Self::public_entry).collect::<Vec<_>>(),
            "skipped": files.iter().filter(|e| e["status"] != STATUS_READ).map(Self::public_entry).collect::<Vec<_>>(),
            "counts": counts, "items": items, "warnings": warnings, "refused": scan["refused"], "already_imported": earlier,
        }))
    }

    fn public_entry(entry: &Value) -> Value {
        Value::Object(entry.as_object().map(|m| m.iter().filter(|(k, _)| k.as_str() != "text" && k.as_str() != "refusal").map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default())
    }

    fn read_source_path(body: &Value) -> Result<String, AppError> {
        match body.get("path").and_then(Value::as_str).filter(|p| !p.trim().is_empty()) {
            Some(path) => Ok(path.trim().to_string()),
            None => Err(AppError::invalid_request("send `path`: the folder of a spec feature (with spec.md)")),
        }
    }

    fn slug_of(body: &Value, source: &str) -> String {
        let name = body.get("name").filter(|n| t(Some(n))).map(|n| text_of(Some(n)));
        sanitize_name(&name.unwrap_or_else(|| pycompat::last_segment(&pycompat::nt_normpath(&expand_user(source))).to_string()))
    }

    // ------------------------------------------------ import
    /// `{import_id, files:[relative path], main, at, source}` after copying; errors with the reason, or a conflict `already_imported` (earlier log
    /// path in the response) / `feature_folder_exists`.
    pub fn import_spec(&self, context: &ProjectContext, body: &Value) -> Result<Value, AppError> {
        let source = Self::read_source_path(body)?;
        let scan = scan_folder(&source, &DEFAULT_LIMITS)?;
        if t(scan.get("refused")) {
            return Err(AppError::invalid_request(text_of(scan["refused"].get("reason"))));
        }
        let slug = Self::slug_of(body, &source);
        self.refuse_second_import(context, &source, &slug)?;
        let read_files: Vec<Value> = scan["files"].as_array().cloned().unwrap_or_default().into_iter().filter(|e| e["status"] == STATUS_READ).collect();
        let items = detect_items(&read_files);
        let conformance = super::context_block::ContextBlockService::read_conformance_text(&context.work_dir(), "");
        let warnings = build_warnings(&scan, &items, &conformance);
        let stamp = {
            let now = crate::repositories::loop_command_file::now_iso();
            now.replace(['-', ':'], "").replace('T', "-")
        };
        let import_id = self.unique_import_id(context, &format!("{stamp}-{slug}"));
        let folder = imported_root(&context.work_dir()).join(&import_id);
        let feature_folder = FEATURES_FOLDER_PARTS.iter().fold(PathBuf::from(context.work_dir()), |a, p| a.join(p)).join(&slug);
        for entry in &read_files {
            let redacted = redact_secrets(&text_of(entry.get("text")), None);
            let relative = text_of(entry.get("relative_path"));
            let _ = self.app.specs.write_text_atomic(&relative.split('/').fold(folder.clone(), |a, p| a.join(p)), &redacted);
            let _ = self.app.specs.write_text_atomic(&relative.split('/').fold(feature_folder.clone(), |a, p| a.join(p)), &redacted);
        }
        let _ = self.app.specs.write_text_atomic(&folder.join(IMPORT_LOG_NAME), &Self::build_log_text(&source, &slug, &import_id, &scan, &items, &warnings));
        let mut files: Vec<Value> = read_files.iter().map(|e| e["relative_path"].clone()).collect();
        files.push(json!(IMPORT_LOG_NAME));
        let meta = json!({
            "import_id": import_id, "source": "folder", "source_path": pycompat::nt_abspath(&expand_user(&source)), "slug": slug,
            "feature_dir": Self::feature_relative_folder(&slug), "files": files, "main": MAIN_SPEC_NAME, "at": now_iso(), "sent_at": null,
            "items": {"requirements": items["requirements"], "decisions": items["decisions"], "tasks": items["tasks"], "sections": items["sections"]},
        });
        let _ = self.app.specs.write_text_atomic(&folder.join(META_FILE_NAME), &py_json_dumps_indent(&meta, true, 2));
        self.app.log.info("planning_spec_imported", &[("project", json!(context.project.id)), ("import_id", json!(import_id)), ("files", json!(read_files.len()))]);
        Ok(json!({"import_id": meta["import_id"], "files": meta["files"], "main": meta["main"], "at": meta["at"], "source": meta["source"]}))
    }

    fn refuse_second_import(&self, context: &ProjectContext, source: &str, slug: &str) -> Result<(), AppError> {
        if let Some(earlier) = self.find_earlier_import(context, source, slug) {
            let message = format!(
                "this spec feature was already imported ({}): see the log {}. A changed spec goes through the planning chat (Back to planning), not a second import",
                text_of(earlier.get("import_id")),
                text_of(earlier.get("log_path"))
            );
            let mut error = AppError::new(Kind::RepoConflict, message).with_code(ALREADY_IMPORTED_CODE);
            if let Some(extras) = earlier.as_object() {
                error.extras = extras.clone();
            }
            return Err(error);
        }
        if Self::feature_folder_exists(context, slug) {
            return Err(AppError::new(Kind::RepoConflict, format!("{} already exists and was not imported by the app: rename the source folder or remove it by hand", Self::feature_relative_folder(slug)))
                .with_code(FEATURE_FOLDER_EXISTS_CODE)
                .with_extra("feature_dir", json!(Self::feature_relative_folder(slug))));
        }
        Ok(())
    }

    /// `{import_id, log_path}` of an import with the same normalized source folder or the same slug, or `None`.
    fn find_earlier_import(&self, context: &ProjectContext, source: &str, slug: &str) -> Option<Value> {
        let wanted = normalize_source(source);
        let root = imported_root(&context.work_dir());
        let mut entries: Vec<String> = std::fs::read_dir(&root).ok()?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        entries.sort();
        for entry in entries {
            let meta = self.app.specs.read_json(&root.join(&entry).join(META_FILE_NAME));
            if meta.get("import_id").and_then(Value::as_str) != Some(entry.as_str()) {
                continue;
            }
            let same_source = t(meta.get("source_path")) && normalize_source(&text_of(meta.get("source_path"))) == wanted;
            if meta.get("slug").and_then(Value::as_str) == Some(slug) || same_source {
                return Some(json!({"import_id": entry, "log_path": format!("{}/{IMPORT_LOG_NAME}", Self::relative_folder(context, &entry))}));
            }
        }
        None
    }

    fn build_log_text(source: &str, slug: &str, import_id: &str, scan: &Value, items: &Value, warnings: &[String]) -> String {
        let rows: Vec<String> = scan["files"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|entry| {
                let sha = Some(text_of(entry.get("sha256"))).filter(|s| !s.is_empty()).unwrap_or_else(|| "-".into());
                let result = if entry["status"] == STATUS_READ { "read".to_string() } else { format!("skipped: {}", text_of(entry.get("reason"))) };
                format!("| {} | {} | {} | {} |", text_of(entry.get("relative_path")), format_size(entry["size"].as_u64().unwrap_or(0)), sha, result)
            })
            .collect();
        let count = |key: &str| items.get(key).and_then(Value::as_array).map(Vec::len).unwrap_or(0);
        let number = |key: &str| text_of(items.get(key));
        let detected = format!(
            "Detected: {} requirements, {} decisions, {} tasks, {} out-of-scope rows, {} edge cases, {} success criteria.",
            count("requirements"),
            count("decisions"),
            count("tasks"),
            number("out_of_scope_rows"),
            number("edge_cases"),
            number("success_criteria")
        );
        let mut lines = vec![
            format!("# Import log \u{2014} {slug}"),
            String::new(),
            format!("- Source: {}", source.replace('\\', "/")),
            format!("- Slug: {slug} \u{b7} Import id: {import_id}"),
            String::new(),
            "| File | Size | SHA-256 | Result |".into(),
            "| --- | --- | --- | --- |".into(),
        ];
        lines.extend(rows);
        lines.extend([String::new(), detected, format!("Warnings: {}", if warnings.is_empty() { "none".to_string() } else { warnings.join("; ") })]);
        format!("{}\n", lines.join("\n"))
    }

    fn unique_import_id(&self, context: &ProjectContext, base: &str) -> String {
        let root = imported_root(&context.work_dir());
        let (mut candidate, mut number) = (base.to_string(), 2);
        while root.join(&candidate).exists() {
            candidate = format!("{base}-{number}");
            number += 1;
        }
        candidate
    }

    // ------------------------------------------------ list
    /// `{imports:[{import_id, files, main, at, source, sent_at}]}` newest first (the chat shows them as attachments).
    pub fn list_imports(&self, context: &ProjectContext) -> Value {
        let root = imported_root(&context.work_dir());
        let mut names: Vec<String> = std::fs::read_dir(&root).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        names.sort();
        names.reverse();
        let imports: Vec<Value> = names
            .into_iter()
            .filter_map(|entry| {
                let meta = self.app.specs.read_json(&root.join(&entry).join(META_FILE_NAME));
                (meta.get("import_id").and_then(Value::as_str) == Some(entry.as_str())).then(|| {
                    Value::Object(["import_id", "files", "main", "at", "source", "sent_at"].iter().map(|k| (k.to_string(), meta.get(*k).cloned().unwrap_or(Value::Null))).collect::<Map<String, Value>>())
                })
            })
            .collect();
        json!({"imports": imports})
    }

    // ------------------------------------------------ send
    /// Sends the ONE fixed message of an import through the planning chat; returns what `planning/send` returns (`{ok, job_id}`).
    pub fn send_import(&self, context: &ProjectContext, import_id: &str) -> Result<Value, AppError> {
        let meta_path = self.meta_path(context, import_id)?;
        let meta = self.app.specs.read_json(&meta_path);
        if meta.is_empty() {
            return Err(AppError::new(Kind::JobNotFound, "import not found"));
        }
        let folder = Self::relative_folder(context, import_id);
        let files: Vec<String> = meta.get("files").and_then(Value::as_array).map(|a| a.iter().map(|n| format!("- {folder}/{}", text_of(Some(n)))).collect()).unwrap_or_default();
        let slug = Some(text_of(meta.get("slug"))).filter(|s| !s.is_empty()).unwrap_or_else(|| import_id.to_string());
        let feature_dir = Some(text_of(meta.get("feature_dir"))).filter(|s| !s.is_empty()).unwrap_or_else(|| Self::feature_relative_folder(&slug));
        let items = meta.get("items").filter(|i| t(Some(i))).cloned().unwrap_or(json!({}));
        let message = IMPORT_MESSAGE
            .replace("{main}", &format!("{folder}/{}", text_of(meta.get("main"))))
            .replace("{files}", &files.join("\n"))
            .replace("{slug}", &slug)
            .replace("{feature_dir}", &feature_dir)
            .replace("{detected}", &describe_detected(&items));
        let log_path = meta_path.parent().map(|p| p.join(IMPORT_LOG_NAME)).unwrap_or_default();
        let mut sent_meta = meta.clone();
        let sent_at = now_iso();
        sent_meta.insert("sent_at".into(), json!(sent_at));
        let original = Value::Object(meta);
        let _ = self.app.specs.write_text_atomic(&meta_path, &py_json_dumps_indent(&Value::Object(sent_meta), true, 2)); // before the send: a job that fails at once unmarks it
        let (app, undo_meta_path, undo_original, undo_log) = (self.app.shared(), meta_path.clone(), original.clone(), log_path.clone());
        let unmark: Box<dyn FnOnce(&ClaudeJobError) + Send> = Box::new(move |failure| {
            let _ = app.specs.write_text_atomic(&undo_meta_path, &py_json_dumps_indent(&undo_original, true, 2));
            app.planning_imports().append_to_log(&undo_log, &format!("Not sent: {}", failure.message.chars().take(200).collect::<String>()));
        });
        match self.app.planning().send_message(context, &json!(message), Some(unmark)) {
            Ok(reply) => {
                self.append_to_log(&log_path, &format!("Sent: {sent_at}"));
                Ok(reply)
            }
            Err(failure) => {
                let _ = self.app.specs.write_text_atomic(&meta_path, &py_json_dumps_indent(&original, true, 2));
                Err(failure)
            }
        }
    }

    fn meta_path(&self, context: &ProjectContext, import_id: &str) -> Result<PathBuf, AppError> {
        if !IMPORT_ID_PATTERN.is_match(import_id) {
            return Err(AppError::new(Kind::JobNotFound, "import not found"));
        }
        Ok(imported_root(&context.work_dir()).join(import_id).join(META_FILE_NAME))
    }

    // ------------------------------------------------ coverage
    /// Appends `Coverage (<at>): N covered, N dropped, N missing.` to the IMPORT-LOG.md of the latest import when it differs from the last recorded
    /// one. `result`: `{covered, dropped, missing}`, each a list of ids or a count.
    pub fn record_coverage(&self, context: &ProjectContext, result: &Value) -> bool {
        let imports = self.list_imports(context);
        let Some(first) = imports["imports"].as_array().and_then(|i| i.first()) else { return false };
        if !result.is_object() {
            return false;
        }
        let log_path = imported_root(&context.work_dir()).join(text_of(first.get("import_id"))).join(IMPORT_LOG_NAME);
        let count = |key: &str| -> i64 {
            match result.get(key) {
                Some(Value::Array(list)) => list.len() as i64,
                Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
                _ => 0,
            }
        };
        let mut summary = format!("{} covered, {} dropped, {} missing.", count("covered"), count("dropped"), count("missing"));
        if let Some(missing) = result.get("missing").and_then(Value::as_array).filter(|m| !m.is_empty()) {
            let mut names: Vec<String> = missing.iter().map(|i| text_of(Some(i))).collect();
            names.sort();
            summary.push_str(&format!(" Missing: {}.", names.join(", ")));
        }
        let log = self.app.specs.read_text(&log_path).unwrap_or_default();
        let recorded: Vec<&str> = pycompat::splitlines(&log).into_iter().filter(|l| l.starts_with(COVERAGE_LINE_PREFIX)).collect();
        if let Some(last) = recorded.last() {
            if last.rsplit_once("): ").map(|p| p.1).unwrap_or(last) == summary {
                return false;
            }
        }
        self.append_to_log(&log_path, &format!("{COVERAGE_LINE_PREFIX}{}): {summary}", now_iso()))
    }

    fn append_to_log(&self, log_path: &Path, line: &str) -> bool {
        let Some(current) = self.app.specs.read_text(log_path) else { return false };
        self.app.specs.write_text_atomic(log_path, &format!("{}\n{line}\n", current.trim_end_matches('\n'))).is_ok()
    }
}

fn t(value: Option<&Value>) -> bool {
    engine_state::notify_truthy(value)
}
