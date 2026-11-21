// SPDX-License-Identifier: MIT
//! The generated "What the agents can use" section of `.specs/project/PROJECT.md`, between the markers
//!
//! `<!-- maestro:capabilities -->  ...  <!-- /maestro:capabilities -->`
//!
//! Server generated (repositories with role, what the agents may read, what they may edit, what is reserved) and rewritten after a planning reply, a
//! repository change and at approval. Text outside the markers is never touched (byte-identical, line endings included); a missing block is appended
//! at the end of the file. The planning AI never edits inside the markers (the guard reverts it).

use super::App;
use crate::domain::project::ProjectContext;
use engine_state::{mcp_access, subprojects};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const START_MARKER: &str = "<!-- maestro:capabilities -->";
pub const END_MARKER: &str = "<!-- /maestro:capabilities -->";
const PROJECT_FILE_PARTS: [&str; 3] = [".specs", "project", "PROJECT.md"];
const REPOS_FILE_PARTS: [&str; 3] = [".specs", "project", "REPOS.json"];
const ROLE_EDIT: &str = "edit";
const ROLE_REFERENCE: &str = "reference";
const MAP_FOLDER_SUFFIX: &str = "docs/codebase";
const SYSTEM_DOCUMENT: &str = "docs/codebase/SYSTEM.md";

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

/// `['name (tool, tool)']` from a list of names, a list of `{server|name, tools}` or a `{name: tools}` object.
pub fn describe_servers(servers: &Value) -> Vec<String> {
    let items: Vec<Value> = match servers {
        Value::Object(map) => map.iter().map(|(name, tools)| json!({"server": name, "tools": tools})).collect(),
        Value::Array(list) => list.clone(),
        _ => vec![],
    };
    let mut lines = Vec::new();
    for item in items {
        match &item {
            Value::String(s) => lines.push(s.clone()),
            Value::Object(map) => {
                let name = ["server", "name"].iter().find_map(|k| map.get(*k).filter(|v| engine_state::notify_truthy(Some(v)))).map(|v| text_of(Some(v)));
                let Some(name) = name else { continue };
                let tools: Vec<String> = map.get("tools").and_then(Value::as_array).map(|t| t.iter().map(|i| text_of(Some(i))).collect()).unwrap_or_default();
                lines.push(if tools.is_empty() { name } else { format!("{name} ({})", tools.join(", ")) });
            }
            _ => {}
        }
    }
    lines
}

fn folder_has_documents(folder: &Path) -> bool {
    std::fs::read_dir(folder).map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().to_lowercase().ends_with(".md"))).unwrap_or(false)
}

/// `text` with the marked block replaced by `block`; appended after a blank line when the markers are not there. Nothing outside the markers changes.
pub fn replace_block(text: &str, block: &str) -> String {
    if let Some(start) = text.find(START_MARKER) {
        if let Some(relative_end) = text[start + START_MARKER.len()..].find(END_MARKER) {
            let end = start + START_MARKER.len() + relative_end;
            let newline = if text[start..end].contains("\r\n") { "\r\n" } else { "\n" };
            return format!("{}{}{}", &text[..start], block.replace('\n', newline), &text[end + END_MARKER.len()..]);
        }
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let separator = if text.is_empty() {
        String::new()
    } else if text.ends_with('\n') {
        newline.to_string()
    } else {
        newline.repeat(2)
    }; // one blank line before the block
    format!("{text}{separator}{}{newline}", block.replace('\n', newline))
}

pub struct ProjectCapabilitiesService<'a> {
    pub app: &'a App,
    /// The MCP servers the agents may read (tests inject; default: `mcp_access::available_servers`).
    pub mcp_provider: Option<&'a dyn Fn() -> Value>,
}

impl App {
    pub fn capabilities(&self) -> ProjectCapabilitiesService<'_> {
        ProjectCapabilitiesService { app: self, mcp_provider: None }
    }
}

impl ProjectCapabilitiesService<'_> {
    fn available_servers(&self) -> Value {
        match self.mcp_provider {
            Some(provider) => provider(),
            None => Value::Array(mcp_access::available_servers(&self.app.env, None, "")),
        }
    }

    // ------------------------------------------------ render
    /// `[(name, path, role, primary)]` from REPOS.json (role missing = edit; the primary is always edit); the primary alone without file.
    pub fn read_repositories(&self, context: &ProjectContext) -> Vec<(String, String, String, bool)> {
        let file = REPOS_FILE_PARTS.iter().fold(PathBuf::from(context.work_dir()), |a, p| a.join(p));
        let data = self.app.specs.read_json(&file);
        let primary_key = pycompat::nt_normcase(&pycompat::nt_normpath(context.repo_path()));
        let mut repos = Vec::new();
        let mut found_primary = false;
        for item in data.get("repos").and_then(Value::as_array).into_iter().flatten() {
            if !item.is_object() || !engine_state::notify_truthy(item.get("path")) {
                continue;
            }
            let path = text_of(item.get("path"));
            let primary = engine_state::notify_truthy(item.get("primary")) || pycompat::nt_normcase(&pycompat::nt_normpath(&path)) == primary_key;
            found_primary = found_primary || primary;
            let role = if primary || item.get("role").and_then(Value::as_str) != Some(ROLE_REFERENCE) { ROLE_EDIT } else { ROLE_REFERENCE };
            let name =
                if engine_state::notify_truthy(item.get("name")) { text_of(item.get("name")) } else { pycompat::last_segment(&pycompat::nt_normpath(&path)).to_string() };
            repos.push((name, path, role.to_string(), primary));
        }
        if !found_primary {
            repos.insert(
                0,
                (pycompat::last_segment(&pycompat::nt_normpath(context.repo_path())).to_string(), context.repo_path().to_string(), ROLE_EDIT.to_string(), true),
            );
        }
        repos
    }

    /// The whole marked block (markers included), without a trailing newline.
    pub fn render(&self, context: &ProjectContext) -> String {
        let repos = self.read_repositories(context);
        let mut lines: Vec<String> = vec![
            START_MARKER.into(),
            "<!-- Generated by the Rolo Loop app: rewritten on every change, do not edit between the markers. -->".into(),
            "## What the agents can use (inside the loop)".into(),
            String::new(),
            "**Repositories**".into(),
        ];
        for (name, path, role, primary) in &repos {
            lines.push(format!("- `{name}` ({role}{}): {}", if *primary { ", primary" } else { "" }, path.replace('\\', "/")));
        }
        lines.extend(self.render_subprojects(context, &repos));
        let edit_names: Vec<String> = repos.iter().filter(|(_, _, role, _)| role == ROLE_EDIT).map(|(name, ..)| format!("`{name}`")).collect();
        let servers = describe_servers(&self.available_servers());
        lines.extend([
            String::new(),
            "**Read** (always allowed)".into(),
            "- Every git repository on this machine (the repository catalog), read only; the `reference` repositories above are in scope.".into(),
            "- git read commands (`git log`, `git diff`, `git show`, `git status`, `git blame`).".into(),
            format!("- MCP read tools: {}.", if servers.is_empty() { "none available on this machine".to_string() } else { servers.join("; ") }),
            String::new(),
            "**Edit**".into(),
            format!(
                "- Only the PATHS of your feature, in the loop worktree of an `edit` repository ({}). A `reference` repository is never edited.",
                if edit_names.is_empty() { "none".to_string() } else { edit_names.join(", ") }
            ),
            String::new(),
            "**Reserved** (do not try: the guard blocks it)".into(),
            "- `git commit`: the orchestrator's checkpoint, never a subagent.".into(),
            "- `git push` and the pull request: `harness deliver`, once, at the end of the loop.".into(),
            "- The state files (`.specs/project/loop/`) and `harness`: the engine.".into(),
            String::new(),
            "A guard block is final: do not retry the same action or look for another way around it; say what you need and stop.".into(),
            String::new(),
            "Outside the loop (the user talking to Claude directly, tlc-spec-driven) nothing in this section restricts git or anything else.".into(),
            END_MARKER.into(),
        ]);
        lines.join("\n")
    }

    /// The `**Subprojects**` lines of every confirmed monorepo (empty list for a single project, an unconfirmed list or any failure).
    fn render_subprojects(&self, context: &ProjectContext, repos: &[(String, String, String, bool)]) -> Vec<String> {
        let (stored, _) = subprojects::read_repos(&context.work_dir()); // SUBPROJECTS.json always lives under the primary's work folder
        let mut lines = Vec::new();
        for (name, path, _, primary) in repos {
            let Some(entry) = stored.get(name).and_then(Value::as_object) else { continue };
            if entry.get("monorepo") != Some(&Value::Bool(true)) || entry.get("confirmed") != Some(&Value::Bool(true)) {
                continue;
            }
            let root = if *primary { context.work_dir() } else { path.clone() };
            lines.extend(Self::render_subprojects_of_repo(&root, name, entry));
        }
        if lines.is_empty() {
            return vec![];
        }
        let mut all = vec![String::new(), "**Subprojects**".to_string()];
        all.extend(lines);
        all
    }

    fn render_subprojects_of_repo(root: &str, name: &str, entry: &serde_json::Map<String, Value>) -> Vec<String> {
        let mut lines = Vec::new();
        for project in subprojects::entry_projects(Some(entry)) {
            let base = subprojects::normalize(&text_of(project.get("path")));
            if project.get("skip") == Some(&Value::Bool(true)) {
                let reason = if engine_state::notify_truthy(project.get("reason")) { format!(" ({})", text_of(project.get("reason"))) } else { String::new() };
                lines.push(format!("- `{name}` `{base}`: skipped{reason}"));
                continue;
            }
            let map_folder = format!("{base}/{MAP_FOLDER_SUFFIX}");
            let has_map = folder_has_documents(&map_folder.split('/').fold(PathBuf::from(root), |a, p| a.join(p)));
            let or_dash = |key: &str| Some(text_of(project.get(key))).filter(|s| !s.is_empty()).unwrap_or_else(|| "-".into());
            lines.push(format!(
                "- `{name}` `{base}`: stack {}, skill {}, Conformance {}, map `{map_folder}` ({}), CLAUDE.md `{base}/CLAUDE.md`",
                or_dash("stack"),
                or_dash("skill"),
                python_none_text(&subprojects::read_conformance(root, &base)),
                if has_map { "has documents" } else { "no documents yet" },
            ));
        }
        let system_exists = SYSTEM_DOCUMENT.split('/').fold(PathBuf::from(root), |a, p| a.join(p)).is_file();
        lines.push(format!("- `{name}` system map: `{SYSTEM_DOCUMENT}` ({})", if system_exists { "exists" } else { "does not exist yet" }));
        lines
    }

    // ------------------------------------------------ write
    /// Rewrites the marked block of PROJECT.md (appends it when missing). Returns true when the file changed; false when nothing changed or there is
    /// no PROJECT.md yet (the structure initialization creates it).
    pub fn write(&self, context: &ProjectContext) -> bool {
        let path = PROJECT_FILE_PARTS.iter().fold(PathBuf::from(context.work_dir()), |a, p| a.join(p));
        let Ok(bytes) = std::fs::read(&path) else { return false };
        let Ok(text) = String::from_utf8(bytes) else { return false };
        let updated = replace_block(&text, &self.render(context));
        if updated == text || std::fs::write(&path, updated.as_bytes()).is_err() {
            return false;
        }
        self.app.log.info("project_capabilities_written", &[("project", json!(context.project.id))]);
        true
    }

    /// Hook of "Confirm list": confirming the list rewrites the marked block of PROJECT.md only.
    pub fn after_list_confirmed(&self, context: &ProjectContext) {
        self.write(context);
    }
}

/// `read_conformance` gives '' when there is none; Python formats the returned value (a string, possibly empty).
fn python_none_text(value: &str) -> String {
    value.to_string()
}

impl App {
    /// After a change that moves what the loop can use (repo or role change, test environment values): rewrite the capabilities block of PROJECT.md,
    /// then check readiness again in the background. Never fails: it is a side effect of the real change.
    pub fn on_plan_changed(&self, context: &ProjectContext) {
        self.capabilities().write(context);
        self.readiness_schedule(context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::project::Project;

    #[test]
    fn the_block_is_replaced_in_place_and_appended_when_missing() {
        let block = format!("{START_MARKER}\nbody\n{END_MARKER}");
        assert_eq!(replace_block("", &block), format!("{block}\n"));
        assert_eq!(replace_block("# P\n", &block), format!("# P\n\n{block}\n"));
        assert_eq!(replace_block("# P", &block), format!("# P\n\n{block}\n"));
        let crlf = format!("# P\r\n\r\n{START_MARKER}\r\nold\r\n{END_MARKER}\r\ntail\r\n");
        assert_eq!(replace_block(&crlf, &block), format!("# P\r\n\r\n{START_MARKER}\r\nbody\r\n{END_MARKER}\r\ntail\r\n"));
        assert_eq!(replace_block("a\r\nb", &block), format!("a\r\nb\r\n\r\n{START_MARKER}\r\nbody\r\n{END_MARKER}\r\n"));
        assert_eq!(describe_servers(&json!(["a", {"server": "g", "tools": ["x", "y"]}, {"name": "n"}, {"tools": []}, 3])), ["a", "g (x, y)", "n"]);
        assert_eq!(describe_servers(&json!({"s": ["t"]})), ["s (t)"]);
    }

    #[test]
    fn rendering_lists_repositories_roles_and_the_reserved_actions() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("main-app");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        std::fs::write(
            repo.join(".specs/project/REPOS.json"),
            format!(
                r#"{{"repos": [{{"name": "ref", "path": "{}", "role": "reference"}}, {{"name": "main-app", "path": "{}", "primary": true, "role": "reference"}}]}}"#,
                dir.path().join("ref").display().to_string().replace('\\', "\\\\"),
                repo.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let context = ProjectContext::new(Project {
            id: "p".into(),
            name: "main-app".into(),
            repo_path: repo.display().to_string(),
            opened_at: String::new(),
            last_opened_at: String::new(),
            favorite: false,
        });
        let provider = || json!([{"server": "grafana", "tools": ["query"]}]);
        let service = ProjectCapabilitiesService { app: &app, mcp_provider: Some(&provider) };
        let text = service.render(&context);
        assert!(text.contains("- `ref` (reference): ") && text.contains("- `main-app` (edit, primary): "));
        assert!(text.contains("- MCP read tools: grafana (query).") && text.contains("(`main-app`). A `reference` repository is never edited."));
        assert!(text.contains("`harness deliver`") && text.ends_with(END_MARKER));
        assert!(!service.write(&context), "no PROJECT.md yet");
        std::fs::write(repo.join(".specs/project/PROJECT.md"), "# P\n\nVision.\n").unwrap();
        assert!(service.write(&context) && !service.write(&context));
        let written = std::fs::read_to_string(repo.join(".specs/project/PROJECT.md")).unwrap();
        assert!(written.starts_with("# P\n\nVision.\n\n<!-- maestro:capabilities -->\n"));
    }
}
