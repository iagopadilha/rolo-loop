// SPDX-License-Identifier: MIT
//! Test environment tab: which `$NAME` the project's TEST-ENVIRONMENT.md needs, where each one is defined (never its value, only a masked preview),
//! saving values and importing names into the project's `.env.test`, the ENV-01 validation and the computed `access` card (the credential layer +
//! MCP read servers).
//!
//! One credential source, like the toolkit's `testenv`: `<repo>/.specs/project/.env.test`. Reads and writes of values go to the REPO
//! (`context.credentials_dir`), never to the loop worktree (WT-02: the engine copies the file into the worktree at every turn start). Other `.env`
//! files are only planning sources (`env_sources` of config.json): `import_names` copies the names the user picks from them. MCP access is computed
//! from `~/.claude.json` (`mcp_access`), read tools only.
//!
//! Secrets rule: values are written, never read back into a response, a log line, a file the AI reads or an error message. The only thing that leaves
//! is `preview` (first 2 + `••••` + last 2 characters of a value of 8+ characters, else the four dots only).

use super::planning_gates::{list_fill_in_lines, read_loop_max_hours};
use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use crate::repositories::project_files::TestEnvironmentRepository;
use engine_state::{credential_check, mcp_access, network_policy, planning_env_validation, test_env_fallback};
use pycompat::json::py_json_dumps_indent;
use pycompat::Re;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const TARGET_PROJECT: &str = "project";
const SOURCES_THAT_COUNT: [&str; 1] = ["project"];
const CREDENTIAL_NAMES_PARTS: [&str; 5] = [".specs", "project", "loop", "internal", "credential-names.json"];
pub const UNKNOWN_NAME_CODE: &str = "unknown_name";
pub const MAX_VALUES_PER_REQUEST: usize = 100;
const MASK: &str = "\u{2022}\u{2022}\u{2022}\u{2022}";
const PREVIEW_EDGE: usize = 2;
const PREVIEW_MIN_LENGTH: usize = 8;
static NAME_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z_][A-Za-z0-9_]{0,127}\z"));

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

/// `ab••••yz` for a value of 8+ characters, `••••` for a shorter one, '' when there is no value.
pub fn mask_value(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < PREVIEW_MIN_LENGTH {
        return MASK.into();
    }
    format!("{}{MASK}{}", chars[..PREVIEW_EDGE].iter().collect::<String>(), chars[chars.len() - PREVIEW_EDGE..].iter().collect::<String>())
}

pub struct TestEnvironmentService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn test_environment(&self) -> TestEnvironmentService<'_> {
        TestEnvironmentService { app: self }
    }
}

impl TestEnvironmentService<'_> {
    fn repository(&self) -> TestEnvironmentRepository<'_> {
        TestEnvironmentRepository { specs: &self.app.specs }
    }

    // ------------------------------------------------ read
    /// `([{name, source, set, secret, path_label, preview}], resolution, document or None)`: one row per `$NAME` the document mentions (names inside
    /// comments too, like the preflight). The document is read where planning writes it (the work dir); the values come from the repo's `.env.test`
    /// (WT-02). `preview` is the masked value, never the value.
    pub fn describe_names(&self, context: &ProjectContext) -> (Vec<Value>, credential_check::Resolution, Option<String>) {
        let document = self.repository().read_document(&context.work_dir());
        let resolution = credential_check::Resolution::new(context.credentials_dir());
        let rows: Vec<Value> = credential_check::mentioned_names(document.as_deref().unwrap_or(""))
            .into_iter()
            .map(|name| {
                let (source, label) = resolution.lookup(&name);
                let counts = SOURCES_THAT_COUNT.contains(&source.as_str());
                let preview = if counts { mask_value(resolution.values.get(&name).map(String::as_str).unwrap_or("")) } else { String::new() };
                json!({"name": name, "source": source, "set": counts, "secret": credential_check::is_secret_name(&name), "path_label": label, "preview": preview})
            })
            .collect();
        (rows, resolution, document)
    }

    pub fn get_test_environment(&self, context: &ProjectContext) -> Value {
        let (rows, resolution, document) = self.describe_names(context);
        let fill_in = list_fill_in_lines(document.as_deref().unwrap_or(""));
        let ready = fill_in.is_empty() && rows.iter().all(|row| SOURCES_THAT_COUNT.contains(&text_of(row.get("source")).as_str()));
        self.repository().migrate_legacy_access(&context.work_dir()); // legacy ACCESS.json keys are dropped
        json!({"ok": true, "ready": ready, "names": rows, "fill_in": fill_in, "validation": self.validate_environment(context, document.as_deref()), "access": self.describe_access(&resolution, &rows)})
    }

    /// ENV-01 for the tab: `{exists, problems: [{code, name, message}], warnings: [...]}`, names only. The document comes from the work dir, the values
    /// and the network list from the repo (what the panel edits).
    fn validate_environment(&self, context: &ProjectContext, document: Option<&str>) -> Value {
        let Some(document) = document else { return json!({"exists": false, "problems": [], "warnings": []}) };
        let root = context.credentials_dir();
        let values = planning_env_validation::read_env_values(&planning_env_validation::ENV_RELATIVE.iter().fold(PathBuf::from(root), |a, p| a.join(p)));
        let effective = network_policy::load(root, &self.app.config.user_data_dir, None);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let mut result = planning_env_validation::validate(document, &values, &effective, now, read_loop_max_hours(self.app));
        result["exists"] = json!(true);
        result
    }

    /// The Access card: `credential_layers` `[{name: project, label, path, exists, names: [names of the document the layer defines, with their
    /// source]}]` (one layer: the project's `.env.test`) and `mcp` `[{server, tools, configured}]`. Never a value.
    fn describe_access(&self, resolution: &credential_check::Resolution, rows: &[Value]) -> Value {
        let document_names: Vec<String> = rows.iter().map(|r| text_of(r.get("name"))).collect();
        let mut layers = resolution.layer_report(&document_names);
        for layer in layers.iter_mut() {
            let layer_name = text_of(layer.get("name"));
            let names: Vec<Value> = layer["names"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|name| {
                    let source = rows.iter().find(|r| r["name"] == *name).map(|r| text_of(r.get("source"))).unwrap_or_else(|| "missing".into());
                    json!({"name": name, "source": source, "winning": source == layer_name})
                })
                .collect();
            layer["names"] = json!(names);
        }
        let policy = mcp_access::read_policy(&self.app.env, None);
        json!({"credential_layers": layers, "mcp": mcp_access::describe_servers(&self.app.env, None, "", &policy)})
    }

    // ------------------------------------------------ values
    pub fn save_values(&self, context: &ProjectContext, values: &Value, target: &str) -> Result<Value, AppError> {
        if target != TARGET_PROJECT {
            return Err(AppError::invalid_request("target must be 'project': the project's .env.test is the only credential file"));
        }
        let Some(map) = values.as_object().filter(|m| !m.is_empty()) else {
            return Err(AppError::invalid_request("values must be an object {NAME: value} with at least one name"));
        };
        if map.len() > MAX_VALUES_PER_REQUEST {
            return Err(AppError::invalid_request(format!("too many values in one request (at most {MAX_VALUES_PER_REQUEST})")));
        }
        let known = credential_check::mentioned_names(&self.repository().read_document(&context.work_dir()).unwrap_or_default());
        let mut unknown: Vec<String> = map.keys().filter(|name| !known.contains(name)).cloned().collect();
        unknown.sort();
        if !unknown.is_empty() {
            return Err(AppError::new(
                Kind::UnknownCredentialName,
                format!("not a name of TEST-ENVIRONMENT.md: {}", unknown.iter().take(8).cloned().collect::<Vec<_>>().join(", ")),
            )
            .with_code(UNKNOWN_NAME_CODE));
        }
        let pairs: Vec<(String, String)> = map.iter().map(|(name, value)| (name.clone(), text_of(Some(value)))).collect();
        {
            let _guard = self.app.test_environment_lock.lock().unwrap_or_else(|e| e.into_inner());
            self.repository().write_project_values(context.credentials_dir(), &pairs)?;
            self.app.specs_structure().ensure_env_files_in_gitignore(context.credentials_dir());
            // keeps .env.test out of git
        }
        let saved: Vec<String> = map.keys().cloned().collect();
        self.app.log.info("test_environment_values_saved", &[("project", json!(context.project.id)), ("names", json!(saved))]); // names only
        self.app.on_plan_changed(context);
        Ok(json!({"ok": true, "saved": saved}))
    }

    // ------------------------------------------------ import from machine sources
    /// `{ok, sources: [{path, exists, names}]}`: per configured source (`env_sources`, default the app data folder's `.env`) the names it defines with a
    /// value. Never a value.
    pub fn list_sources(&self, _context: &ProjectContext) -> Value {
        json!({"ok": true, "sources": test_env_fallback::source_names(&self.app.env, None)})
    }

    /// Copies the values of `names` from the first source that defines each one into the repo's `.env.test` (kept when already defined unless
    /// `replace`). Names only in the answer: `{ok, created, added, replaced, kept, missing, unwritable}`.
    pub fn import_names(&self, context: &ProjectContext, names: &Value, replace: &Value, confirm: &Value) -> Result<Value, AppError> {
        if confirm != &Value::Bool(true) {
            return Err(AppError::invalid_request("confirm must be true"));
        }
        let Some(replace) = replace.as_bool() else { return Err(AppError::invalid_request("replace must be true or false")) };
        let Some(list) = names.as_array().filter(|l| !l.is_empty()) else { return Err(AppError::invalid_request("names must be a list with at least one name")) };
        if list.len() > MAX_VALUES_PER_REQUEST {
            return Err(AppError::invalid_request(format!("too many names in one request (at most {MAX_VALUES_PER_REQUEST})")));
        }
        let mut cleaned: Vec<String> = Vec::new();
        for name in list {
            match name.as_str().filter(|n| NAME_PATTERN.is_match(n.trim())) {
                Some(n) => cleaned.push(n.trim().to_string()),
                None => return Err(AppError::invalid_request("each name must be letters, digits and _ (starting with a letter or _)")),
            }
        }
        let mut result = {
            let _guard = self.app.test_environment_lock.lock().unwrap_or_else(|e| e.into_inner());
            test_env_fallback::import_names(&self.app.env, context.credentials_dir(), &cleaned, replace)
        };
        self.app.log.info(
            "test_environment_names_imported",
            &[("project", json!(context.project.id)), ("added", result["added"].clone()), ("replaced", result["replaced"].clone())],
        );
        let changed = |key: &str| result[key].as_array().is_some_and(|a| !a.is_empty());
        if changed("added") || changed("replaced") {
            self.app.on_plan_changed(context);
        }
        result["ok"] = json!(true);
        Ok(result)
    }

    // ------------------------------------------------ planning AI
    /// `.specs/project/loop/internal/credential-names.json`: `[{name, source, set}]` WITHOUT values, for the planning AI to read (it must never ask for
    /// or write a value). Not an editable path of the planning chat. Returns the file path.
    pub fn write_credential_names_file(&self, context: &ProjectContext) -> String {
        let (rows, _, _) = self.describe_names(context);
        let path = CREDENTIAL_NAMES_PARTS.iter().fold(PathBuf::from(context.work_dir()), |a, p| a.join(p));
        let content: Vec<Value> = rows.iter().map(|row| json!({"name": row["name"], "source": row["source"], "set": row["set"]})).collect();
        let text = format!("{}\n", py_json_dumps_indent(&json!(content), true, 2));
        match engine_state::loop_io::atomic_write_text(&path, &text) {
            Ok(()) => path.display().to_string(),
            Err(_) => String::new(),
        }
    }
}

#[allow(dead_code)]
fn unused(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::project::Project;

    fn context(app: &App, dir: &Path) -> ProjectContext {
        let repo = dir.join("proj");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        let project = app.project_service().open_existing_project(&repo.display().to_string()).unwrap();
        ProjectContext::new(Project { ..project })
    }

    #[test]
    fn values_are_saved_masked_and_never_returned() {
        assert_eq!((mask_value(""), mask_value("short"), mask_value("abcdefghij")), (String::new(), MASK.to_string(), format!("ab{MASK}ij")));
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let ctx = context(&app, dir.path());
        std::fs::write(Path::new(&ctx.work_dir()).join(".specs/project/TEST-ENVIRONMENT.md"), "needs $API_TOKEN and $DB_URL\nFILL_IN here\n").unwrap();
        let service = app.test_environment();
        let before = service.get_test_environment(&ctx);
        assert_eq!(
            (before["ready"].clone(), before["names"][0]["source"].clone(), before["fill_in"][0].clone()),
            (json!(false), json!("missing"), json!("FILL_IN here"))
        );
        assert_eq!(service.save_values(&ctx, &json!({}), "project").unwrap_err().kind, Kind::InvalidRequest);
        assert_eq!(service.save_values(&ctx, &json!({"A": "x"}), "machine").unwrap_err().kind, Kind::InvalidRequest);
        let unknown = service.save_values(&ctx, &json!({"NOPE": "x"}), "project").unwrap_err();
        assert_eq!(
            (unknown.kind, unknown.code.as_deref(), unknown.message.clone()),
            (Kind::UnknownCredentialName, Some("unknown_name"), "not a name of TEST-ENVIRONMENT.md: NOPE".to_string())
        );
        let saved = service.save_values(&ctx, &json!({"API_TOKEN": "supersecretvalue", "DB_URL": "http://localhost:5000"}), "project").unwrap();
        assert_eq!(saved["saved"], json!(["API_TOKEN", "DB_URL"]));
        let after = service.get_test_environment(&ctx);
        assert_eq!((after["names"][0]["set"].clone(), after["names"][0]["preview"].clone()), (json!(true), json!(format!("su{MASK}ue"))));
        assert!(!after.to_string().contains("supersecretvalue"));
        let names_file = service.write_credential_names_file(&ctx);
        let written = std::fs::read_to_string(&names_file).unwrap();
        assert!(written.contains("\"API_TOKEN\"") && !written.contains("supersecretvalue"));
        let ignore = std::fs::read_to_string(Path::new(ctx.repo_path()).join(".gitignore")).unwrap();
        assert!(ignore.contains(".specs/project/.env.test"));
        assert_eq!(service.import_names(&ctx, &json!(["A_B"]), &json!(false), &json!(false)).unwrap_err().message, "confirm must be true");
        assert_eq!(
            service.import_names(&ctx, &json!(["1bad"]), &json!(false), &json!(true)).unwrap_err().message,
            "each name must be letters, digits and _ (starting with a letter or _)"
        );
        let imported = service.import_names(&ctx, &json!(["API_TOKEN"]), &json!(false), &json!(true)).unwrap();
        assert_eq!((imported["kept"].clone(), imported["ok"].clone()), (json!(["API_TOKEN"]), json!(true)));
        assert_eq!(service.list_sources(&ctx)["ok"], true);
    }
}
