// SPDX-License-Identifier: MIT
//! `.specs/` structure of a project: creates it from the `templates/` models without ever overwriting, puts `.specs/` in `.gitignore`, links
//! `RULES.md` from `CLAUDE.md` and writes the `Conformance:` line (through the engine's `classify_structure`).
//!
//! Read (what exists), decide (what is missing) and write (only what is missing) stay separate in each operation.

use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::{ProjectContext, LOOP_WORKTREE_SUFFIX};
use crate::repositories::specs_file::list_test_environment_problems;
use engine_state::{classify_structure, loop_io, loop_state, run_hints, subprojects};
use pycompat::Re;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const CONFORMANCE_VALUES: [&str; 2] = ["STANDARD", "LEGACY"];
static CONFORMANCE_LINE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?m)^Conformance:\s*(STANDARD|LEGACY)")); // the same check as `loop --check`
/// `.specs/codebase` is no longer created (the map lives in docs/codebase); old projects keep theirs.
const STRUCTURE_FOLDERS: [&str; 2] = ["project", "features"];
const PROJECT_TEMPLATE_FILES: [&str; 4] = ["PROJECT.md", "ROADMAP.md", "STATE.md", "FLOWS.md"];
pub const RULES_FILE_NAME: &str = "RULES.md";
pub const TEST_ENVIRONMENT_FILE_NAME: &str = "TEST-ENVIRONMENT.md";
const RULES_TEMPLATE_NAME: &str = "PROJECT_RULES.md";
const TEST_ENVIRONMENT_TEMPLATE_NAME: &str = "TEST_ENVIRONMENT.md";
pub const ENV_TEST_FILE_NAME: &str = ".env.test";
const ENV_TEST_TEMPLATE_NAME: &str = "ENV_TEST.example";
const ENV_IGNORE_LINES: [&str; 5] = [".env", ".env.*", "!.env.example", ".specs/project/.env.test", ".specs/project/ACCESS.json"];
const SPECS_TEMPLATES_DIR_NAME: &str = "specs";
const SPECS_IGNORE_LINE: &str = ".specs/";
/// Where "Reset specs" moves the old state; git-ignored, never read by the app.
pub const TRASH_RELATIVE_DIR: &str = ".specs/.trash";
const TRASH_IGNORE_LINE: &str = ".specs/.trash/";
const SPECS_IGNORE_VARIANTS: [&str; 6] = [".specs", ".specs/", "/.specs", "/.specs/", ".specs/*", "/.specs/*"];
static MARKDOWN_BLOCK_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?s)```markdown\r?\n(.*?)\r?\n```"));
static EXAMPLE_RULE_LINE_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\|\s*R-\d+\s*\|"));
static TABLE_SEPARATOR_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\|[-\s|]+\|\s*\z"));
const EXAMPLE_RULE_COMMENT: &str = "<!-- example (delete this comment): | R-01 | Never create a tracking route | real cost | Reject a feature that depends on it | user, YYYY-MM-DD: \"exact phrase\" | -->";
const MINIMAL_RULES: &str = "# Limiting rules\n\n| ID | Rule | Why | How to apply | Origin |\n|----|------|-----|--------------|--------|\n";
const MINIMAL_TEST_ENVIRONMENT: &str = "# Test environment (DEV)\n\nFILL_IN\n";
const CONFORMANCE_NOTE: &str = "written by the harness app";
pub const MONOREPO_ROOT_CONFORMANCE_MESSAGE: &str = "this repository is a monorepo: set the Conformance per subproject";
pub const MONOREPO_ROOT_CONFORMANCE_CODE: &str = "monorepo_root_conformance";

/// Content of the template's first ```markdown block, or '' when there is none.
pub fn extract_markdown_block(text: &str) -> String {
    MARKDOWN_BLOCK_PATTERN.search(text).map(|m| format!("{}\n", m[1].trim())).unwrap_or_default()
}

/// RULES.md of a new project: the table header stays, the template's example rules (R-01..) become a comment.
pub fn build_rules_without_examples(block: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in pycompat::splitlines(block) {
        if EXAMPLE_RULE_LINE_PATTERN.is_match(line.trim()) {
            continue;
        }
        lines.push(line.to_string());
        if TABLE_SEPARATOR_PATTERN.is_match(line.trim()) {
            lines.push(EXAMPLE_RULE_COMMENT.to_string());
        }
    }
    format!("{}\n", lines.join("\n").trim_end())
}

pub fn ignores_specs(gitignore_text: &str) -> bool {
    pycompat::splitlines(gitignore_text).iter().any(|line| SPECS_IGNORE_VARIANTS.contains(&line.trim()))
}

fn folder_key(project_dir: &str) -> String {
    let name = pycompat::last_segment(&pycompat::nt_normpath(project_dir)).to_string();
    name.strip_suffix(LOOP_WORKTREE_SUFFIX).map(String::from).unwrap_or(name)
}

fn today() -> String {
    loop_io::now_iso()[..10].to_string()
}

fn separator_for(text: Option<&str>) -> &'static str {
    match text {
        Some(t) if !t.is_empty() && !t.ends_with('\n') => "\n",
        _ => "",
    }
}

pub struct SpecsStructureService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn specs_structure(&self) -> SpecsStructureService<'_> {
        SpecsStructureService { app: self }
    }
}

impl SpecsStructureService<'_> {
    fn refresh_root_context_block(&self, folder: &str) {
        // A Conformance written at the root of a single project (from the project routes) refreshes its `maestro:context` block.
        self.app.context_blocks().refresh_folder(folder, None, "", Some(folder));
    }

    // ------------------------------------------------ from the project context (routes /p/<id>/api/...)
    /// Initializes in the `work` dir (loop worktree when it exists). Refused with the loop running: it touches CLAUDE.md and .gitignore.
    pub fn initialize_project_structure(&self, context: &ProjectContext, conformance: Option<&str>) -> Result<Value, AppError> {
        self.ensure_loop_stopped(context)?;
        let result = self.initialize_structure(&context.work_dir(), conformance, &context.project.name)?;
        if conformance.is_some_and(|c| !c.is_empty()) {
            self.refresh_root_context_block(&context.work_dir());
        }
        Ok(result)
    }

    pub fn write_project_conformance(&self, context: &ProjectContext, value: &str) -> Result<String, AppError> {
        self.ensure_loop_stopped(context)?;
        let written = self.write_conformance(&context.work_dir(), value)?;
        self.ensure_specs_in_gitignore(&context.work_dir()); // defining the Conformance always leaves `.specs/` out of git
        self.refresh_root_context_block(&context.work_dir());
        Ok(written)
    }

    pub fn ensure_loop_stopped(&self, context: &ProjectContext) -> Result<(), AppError> {
        if self.app.processes.is_alive(self.app.specs.read_loop_lock_pid(&context.work_dir())) {
            return Err(AppError::loop_running("loop running: stop the loop before changing the project structure"));
        }
        Ok(())
    }

    // ------------------------------------------------ read
    /// `{"ok", "missing": [paths relative to .specs/]}`: ok = `.specs/project/` exists (initialize completes the rest).
    pub fn check_structure(&self, project_dir: &str) -> Value {
        let specs_dir = Path::new(project_dir).join(".specs");
        let mut missing: Vec<String> = STRUCTURE_FOLDERS.iter().filter(|n| !specs_dir.join(n).is_dir()).map(|n| format!("{n}/")).collect();
        for name in PROJECT_TEMPLATE_FILES.iter().chain([&RULES_FILE_NAME]) {
            if !specs_dir.join("project").join(name).is_file() {
                missing.push(format!("project/{name}"));
            }
        }
        json!({"ok": specs_dir.join("project").is_dir(), "missing": missing})
    }

    pub fn get_conformance(&self, project_dir: &str) -> Option<String> {
        let text = self.app.specs.read_text(&Path::new(project_dir).join("CLAUDE.md")).unwrap_or_default();
        CONFORMANCE_LINE_PATTERN.search(&text).map(|m| m[1].to_string())
    }

    pub fn suggest_conformance(&self, project_dir: &str) -> Value {
        let (stack, suggestion, evidence) = classify_structure::analyze(project_dir);
        json!({"suggestion": suggestion, "stack": stack, "evidence": evidence})
    }

    // ------------------------------------------------ write
    /// Creates what is missing in `.specs/` (never overwrites). Returns `{"created","changed","existing","conformance"}`.
    pub fn initialize_structure(&self, project_dir: &str, conformance: Option<&str>, project_name: &str) -> Result<Value, AppError> {
        if !Path::new(project_dir).is_dir() {
            return Err(AppError::invalid_path(format!("project folder does not exist: {project_dir}")));
        }
        let monorepo_root = Self::is_monorepo_root(project_dir);
        let conformance_value = match conformance.filter(|c| !c.is_empty()) {
            Some(value) if !monorepo_root => Some(Self::validate_conformance(value)?),
            _ => None,
        };
        let name = if project_name.is_empty() { pycompat::last_segment(&pycompat::nt_normpath(project_dir)).to_string() } else { project_name.to_string() };
        let (mut created, mut existing, mut changed): (Vec<String>, Vec<String>, Vec<String>) = (vec![], vec![], vec![]);
        for path in Self::list_structure_folders(project_dir) {
            if path.is_dir() {
                continue;
            }
            let _ = std::fs::create_dir_all(&path);
            created.push(format!("{}/", Self::relative_to_project(project_dir, &path)));
        }
        for (path, content) in self.build_template_files(project_dir, &name) {
            let relative = Self::relative_to_project(project_dir, &path);
            if self.app.specs.create_file_if_missing(&path, &content).unwrap_or(false) {
                created.push(relative)
            } else {
                existing.push(relative)
            }
        }
        changed.extend(self.ensure_specs_in_gitignore(project_dir));
        changed.extend(self.ensure_env_files_in_gitignore(project_dir));
        changed.extend(self.ensure_rules_in_claude_md(project_dir, &name));
        if !monorepo_root {
            // the root of a monorepo is not a project: each subproject has its own run facts
            changed.extend(self.ensure_run_block(project_dir));
        }
        if let Some(value) = &conformance_value {
            self.write_conformance(project_dir, value)?;
            changed.push(format!("CLAUDE.md (Conformance: {value})"));
        }
        if self.create_test_environment_template(project_dir) {
            // always: the planning chat edits it (it cannot create files)
            created.push(format!(".specs/project/{TEST_ENVIRONMENT_FILE_NAME}"));
        }
        self.app.log.info("specs_structure_initialized", &[("folder", json!(project_dir)), ("created", json!(created.len())), ("changed", json!(changed.len()))]);
        Ok(json!({"created": created, "changed": changed, "existing": existing, "conformance": self.get_conformance(project_dir)}))
    }

    /// Writes the Conformance section of CLAUDE.md with the engine code (the same as `classify_structure --write`).
    pub fn write_conformance(&self, project_dir: &str, value: &str) -> Result<String, AppError> {
        let validated = Self::validate_conformance(value)?;
        if Self::is_confirmed_monorepo(project_dir) {
            return Err(AppError::new(Kind::RepoConflict, MONOREPO_ROOT_CONFORMANCE_MESSAGE).with_code(MONOREPO_ROOT_CONFORMANCE_CODE));
        }
        let (stack, suggestion, _) = classify_structure::analyze(project_dir);
        classify_structure::write_claude_md(project_dir, &validated, &stack, &suggestion, CONFORMANCE_NOTE);
        self.app.log.info("conformance_written", &[("folder", json!(project_dir)), ("value", json!(validated))]);
        Ok(validated)
    }

    /// True when `SUBPROJECTS.json` of that folder says the repository is a confirmed monorepo: its root gets no Conformance block. The repository
    /// key is the folder name (the loop worktree name without its suffix).
    pub fn is_confirmed_monorepo(project_dir: &str) -> bool {
        subprojects::monorepo_confirmed(project_dir, Some(&folder_key(project_dir)))
    }

    /// The structure step happens before the list is confirmed: a stored entry decides ("Not a monorepo" = false), else the free detection does. A
    /// monorepo root never gets a Conformance block from the structure initialization.
    pub fn is_monorepo_root(project_dir: &str) -> bool {
        let (entry, _) = subprojects::load(project_dir, Some(&folder_key(project_dir)));
        match entry {
            Some(entry) => entry.get("monorepo").is_some_and(|v| engine_state::notify_truthy(Some(v))),
            None => subprojects::detect(project_dir).get("monorepo").is_some_and(|v| engine_state::notify_truthy(Some(v))),
        }
    }

    /// Creates `.specs/project/TEST-ENVIRONMENT.md` from the template (with FILL_IN). False when it already existed.
    pub fn create_test_environment_template(&self, project_dir: &str) -> bool {
        let block = extract_markdown_block(&self.read_template(TEST_ENVIRONMENT_TEMPLATE_NAME));
        let content = if block.is_empty() { MINIMAL_TEST_ENVIRONMENT.to_string() } else { block };
        self.create_env_test_template(project_dir);
        self.app.specs.create_file_if_missing(&Path::new(project_dir).join(".specs").join("project").join(TEST_ENVIRONMENT_FILE_NAME), &content).unwrap_or(false)
    }

    /// Creates `.specs/project/.env.test` (the project's test VALUES, commented template) and makes sure git ignores it. Nothing fills it from the
    /// machine files: values come only from the user (edit or "Import from machine sources").
    pub fn create_env_test_template(&self, project_dir: &str) -> bool {
        self.ensure_env_files_in_gitignore(project_dir);
        let template = self.read_template(ENV_TEST_TEMPLATE_NAME);
        self.app.specs.create_file_if_missing(&Path::new(project_dir).join(".specs").join("project").join(ENV_TEST_FILE_NAME), &template).unwrap_or(false)
    }

    /// Appends the missing secret-file lines (`.env`, `.env.*`, `!.env.example`, `.specs/project/.env.test`, `.specs/project/ACCESS.json`) to .gitignore.
    pub fn ensure_env_files_in_gitignore(&self, project_dir: &str) -> Vec<String> {
        let path = Path::new(project_dir).join(".gitignore");
        let text = self.app.specs.read_text(&path).unwrap_or_default();
        let present: Vec<&str> = pycompat::splitlines(&text).iter().map(|l| l.trim()).collect();
        let missing: Vec<&str> = ENV_IGNORE_LINES.iter().copied().filter(|l| !present.contains(l)).collect();
        if missing.is_empty() {
            return vec![];
        }
        let _ = self.app.specs.append_text(&path, &format!("{}{}\n", separator_for(Some(&text)), missing.join("\n")));
        vec![".gitignore (env files)".into()]
    }

    pub fn ensure_specs_in_gitignore(&self, project_dir: &str) -> Vec<String> {
        let path = Path::new(project_dir).join(".gitignore");
        let text = self.app.specs.read_text(&path);
        if text.as_deref().is_some_and(ignores_specs) {
            return vec![];
        }
        let _ = self.app.specs.append_text(&path, &format!("{}{SPECS_IGNORE_LINE}\n", separator_for(text.as_deref())));
        vec![".gitignore (.specs/)".into()]
    }

    /// `.specs/.trash/` must never reach git. Nothing to add when the whole `.specs/` is already ignored.
    pub fn ensure_specs_trash_in_gitignore(&self, project_dir: &str) -> Vec<String> {
        let path = Path::new(project_dir).join(".gitignore");
        let text = self.app.specs.read_text(&path);
        if let Some(existing) = text.as_deref() {
            if ignores_specs(existing) || pycompat::splitlines(existing).iter().any(|l| l.trim() == TRASH_IGNORE_LINE) {
                return vec![];
            }
        }
        let _ = self.app.specs.append_text(&path, &format!("{}{TRASH_IGNORE_LINE}\n", separator_for(text.as_deref())));
        vec![".gitignore (.specs/.trash/)".into()]
    }

    /// CLAUDE.md gets the generated "How to run" block (https profile first); rewritten when the project changed.
    pub fn ensure_run_block(&self, project_dir: &str) -> Vec<String> {
        let (changed, conflicts) = run_hints::write_block(project_dir);
        for line in conflicts {
            self.app.log.warn("claude_md_run_line_without_https", &[("folder", json!(project_dir)), ("line", json!(line.chars().take(160).collect::<String>()))]);
        }
        if changed {
            vec!["CLAUDE.md (How to run)".into()]
        } else {
            vec![]
        }
    }

    /// CLAUDE.md imports RULES.md (read in every session). Appends the line; never rewrites what exists.
    pub fn ensure_rules_in_claude_md(&self, project_dir: &str, project_name: &str) -> Vec<String> {
        let path = Path::new(project_dir).join("CLAUDE.md");
        let text = self.app.specs.read_text(&path);
        let import = loop_state::RULES_IMPORT;
        match text {
            Some(existing) if existing.contains(import) => vec![],
            None => {
                let _ = self.app.specs.create_file_if_missing(&path, &format!("# {project_name}\n\n{import}\n"));
                vec![format!("CLAUDE.md (created with {import})")]
            }
            Some(existing) => {
                let separator = if existing.ends_with('\n') { "\n" } else { "\n\n" };
                let _ = self.app.specs.append_text(&path, &format!("{separator}{import}\n"));
                vec![format!("CLAUDE.md ({import})")]
            }
        }
    }

    // ------------------------------------------------ support
    /// Same rule as the engine preflight (`test_environment_problems`): a ROADMAP asking for regression and the test environment file missing or
    /// incomplete.
    pub fn roadmap_requires_test_environment(project_dir: &str) -> bool {
        !list_test_environment_problems(project_dir, false).is_empty()
    }

    pub fn validate_conformance(value: &str) -> Result<String, AppError> {
        let text = value.trim().to_uppercase();
        if !CONFORMANCE_VALUES.contains(&text.as_str()) {
            return Err(AppError::invalid_path("conformance must be STANDARD or LEGACY"));
        }
        Ok(text)
    }

    pub fn list_structure_folders(project_dir: &str) -> Vec<PathBuf> {
        let specs_dir = Path::new(project_dir).join(".specs");
        let mut folders: Vec<PathBuf> = STRUCTURE_FOLDERS.iter().map(|n| specs_dir.join(n)).collect();
        for name in ["LOOP.json", "LOOP.lock"] {
            if let Some(parent) = loop_state::loop_path(project_dir, name).parent() {
                folders.push(parent.to_path_buf());
            }
        }
        folders
    }

    pub fn build_template_files(&self, project_dir: &str, project_name: &str) -> Vec<(PathBuf, String)> {
        let project_specs_dir = Path::new(project_dir).join(".specs").join("project");
        let today = today();
        let mut files = Vec::new();
        for name in PROJECT_TEMPLATE_FILES {
            let template = self.read_template(&format!("{SPECS_TEMPLATES_DIR_NAME}/{name}"));
            files.push((project_specs_dir.join(name), template.replace("{project_name}", project_name).replace("{today}", &today)));
        }
        let rules_block = extract_markdown_block(&self.read_template(RULES_TEMPLATE_NAME));
        files
            .push((project_specs_dir.join(RULES_FILE_NAME), if rules_block.is_empty() { MINIMAL_RULES.to_string() } else { build_rules_without_examples(&rules_block) }));
        files
    }

    pub fn read_template(&self, relative: &str) -> String {
        let path = relative.split('/').fold(self.app.config.templates_dir.clone(), |a, p| a.join(p));
        match self.app.specs.read_text(&path) {
            Some(text) => text,
            None => {
                self.app.log.warn("specs_template_missing", &[("template", json!(relative))]);
                String::new()
            }
        }
    }

    pub fn relative_to_project(project_dir: &str, path: &Path) -> String {
        path.strip_prefix(project_dir).map(|p| p.display().to_string()).unwrap_or_else(|_| path.display().to_string()).replace('\\', "/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::project::Project;

    fn app_with_templates(dir: &Path) -> std::sync::Arc<App> {
        let app = App::for_tests(dir);
        // the templates sit in the repository checkout, two levels above this crate's manifest
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("app").join("templates");
        assert!(source.is_dir(), "templates copied into the repo");
        let mut config = app.config.clone();
        config.templates_dir = source;
        App::from_config(config, app.processes.clone())
    }

    #[test]
    fn markdown_blocks_and_rule_examples() {
        assert_eq!(extract_markdown_block("a\n```markdown\n# T\n\nbody\n```\nz"), "# T\n\nbody\n");
        assert_eq!(extract_markdown_block("none"), "");
        let block = "# Rules\n\n| ID | Rule |\n|----|------|\n| R-01 | one |\n| R-02 | two |\n";
        let built = build_rules_without_examples(block);
        assert_eq!(built, format!("# Rules\n\n| ID | Rule |\n|----|------|\n{EXAMPLE_RULE_COMMENT}\n"));
        assert!(ignores_specs("node_modules\n /.specs/*\n") && !ignores_specs(".specs/foo\n"));
    }

    #[test]
    fn the_structure_is_created_once_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_templates(dir.path());
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let folder = project.display().to_string();
        let service = app.specs_structure();
        assert_eq!(service.check_structure(&folder)["ok"], false);
        let first = service.initialize_structure(&folder, Some("standard"), "Proj").unwrap();
        assert_eq!(first["conformance"], "STANDARD");
        assert!(first["created"].as_array().unwrap().iter().any(|c| c == ".specs/project/RULES.md"));
        assert!(first["changed"].as_array().unwrap().iter().any(|c| c == "CLAUDE.md (Conformance: STANDARD)"));
        let structure = service.check_structure(&folder);
        assert_eq!((structure["ok"].clone(), structure["missing"].clone()), (json!(true), json!([])));
        let ignore = std::fs::read_to_string(project.join(".gitignore")).unwrap();
        assert!(ignore.contains(".specs/\n") && ignore.contains(".specs/project/.env.test"));
        let claude = std::fs::read_to_string(project.join("CLAUDE.md")).unwrap();
        assert!(claude.starts_with("# Proj\n\n@.specs/project/RULES.md\n") && claude.contains("Conformance: STANDARD"));
        std::fs::write(project.join(".specs/project/ROADMAP.md"), "mine").unwrap();
        let second = service.initialize_structure(&folder, None, "Proj").unwrap();
        assert!(second["created"].as_array().unwrap().is_empty() && second["changed"].as_array().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(project.join(".specs/project/ROADMAP.md")).unwrap(), "mine");
        assert_eq!(SpecsStructureService::validate_conformance("x").unwrap_err().kind, Kind::InvalidPath);
        assert_eq!(service.initialize_structure(&dir.path().join("none").display().to_string(), None, "").unwrap_err().kind, Kind::InvalidPath);
        let context = ProjectContext::new(Project {
            id: "p".into(),
            name: "Proj".into(),
            repo_path: folder.clone(),
            opened_at: String::new(),
            last_opened_at: String::new(),
            favorite: false,
        });
        assert_eq!(service.write_project_conformance(&context, "legacy").unwrap(), "LEGACY");
        assert_eq!(service.get_conformance(&folder).as_deref(), Some("LEGACY"));
    }
}
