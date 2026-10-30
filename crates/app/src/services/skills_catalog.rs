// SPDX-License-Identifier: MIT
//! The "Skills" group of the Settings section "Skills and tools" (`GET /api/app/settings/skills`): every skill found on this machine.
//!
//! Read only. Sources: `<claude home>/skills/*` (user), `<project>/.claude/skills/*` (project, when a project is given) and the `skills` folder of
//! every installed plugin. Per skill the answer is the NAME (its folder), ONE line of its description and where it comes from; nothing else of the
//! file leaves (no body, no path, no front-matter field besides the description), and the description goes through the secret redaction first.
//! The list is capped at [`MAX_SKILLS`]. Also carries the saved choice (`loop_extra_skills`), the skills every loop has anyway, and the warning for
//! a root `CLAUDE.md` Standards section that is ignored (SK1: in a repository with subprojects only their own `CLAUDE.md` is read).

use super::App;
use engine_state::{runtime_isolation, secret_redaction, standards, subprojects};
use serde_json::{json, Map, Value};
use std::path::Path;

pub const MAX_SKILLS: usize = 400;
const DESCRIPTION_CHARS: usize = 200;
const HEAD_BYTES: u64 = 8192;
const LISTED_SUBPROJECTS: usize = 8;
const SOURCE_USER: &str = "user";
const SOURCE_PROJECT: &str = "project";

pub struct SkillsCatalogService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn skills_catalog(&self) -> SkillsCatalogService<'_> {
        SkillsCatalogService { app: self }
    }
}

/// The first line of the `description` of a SKILL.md front matter (also a folded `>` or literal `|` block), redacted and clipped; empty if none.
fn description_of(head: &str) -> String {
    let mut lines = head.lines();
    if lines.next().map(str::trim) != Some("---") {
        return String::new();
    }
    let mut found = String::new();
    let mut collecting = false;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if collecting {
            if line.starts_with([' ', '\t']) && !line.trim().is_empty() {
                found = line.trim().to_string();
                break;
            }
            if !line.trim().is_empty() {
                break;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("description:") {
            let value = rest.trim();
            if value.is_empty() || value.starts_with(['>', '|']) {
                collecting = true;
                continue;
            }
            found = value.trim_matches(['"', '\'']).to_string();
            break;
        }
    }
    let one_line = found.split_whitespace().collect::<Vec<_>>().join(" ");
    secret_redaction::redact(&one_line, Some(DESCRIPTION_CHARS), secret_redaction::REDACTION_MARK)
}

fn read_head(path: &Path) -> String {
    use std::io::Read;
    let mut text = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(HEAD_BYTES).read_to_end(&mut text);
    }
    String::from_utf8_lossy(&text).into_owned()
}

/// The plugin a skills folder of the plugin cache belongs to (`.../cache/<market>/<plugin>/<version>/...`), or `plugin`.
fn plugin_label(skills_dir: &Path) -> String {
    let parts: Vec<String> = skills_dir.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    match parts.iter().position(|p| p == "cache") {
        Some(i) if parts.len() > i + 2 => format!("plugin: {}", parts[i + 2]),
        _ => "plugin".to_string(),
    }
}

fn valid_skill_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 100 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) && !name.starts_with('.')
}

fn scan(dir: &Path, source: &str, rows: &mut Vec<Value>, total: &mut usize) {
    let mut folders: Vec<std::path::PathBuf> = std::fs::read_dir(dir).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
    folders.sort();
    for folder in folders {
        let Some(name) = folder.file_name().and_then(|n| n.to_str()).map(String::from).filter(|n| valid_skill_name(n)) else { continue };
        let file = folder.join("SKILL.md");
        if !file.is_file() || rows.iter().any(|r| r["name"] == name.as_str()) {
            continue;
        }
        *total += 1;
        if rows.len() < MAX_SKILLS {
            rows.push(json!({"name": name, "description": description_of(&read_head(&file)), "source": source}));
        }
    }
}

impl SkillsCatalogService<'_> {
    fn project_root(&self, project_id: &str) -> String {
        if project_id.is_empty() {
            return String::new();
        }
        self.app.project_service().get_project_context(project_id).map(|c| c.repo_path().to_string()).unwrap_or_default()
    }

    /// `{ok, skills: [{name, description, source, default}], total, truncated, released, standards_warning}`; `released` is the saved
    /// `loop_extra_skills`; `default` marks a skill every loop has anyway.
    pub fn get(&self, project_id: &str) -> Value {
        let root = self.project_root(project_id);
        let config: Map<String, Value> = self.app.settings.read_all().unwrap_or_default();
        let mut rows: Vec<Value> = Vec::new();
        let mut total = 0usize;
        scan(&runtime_isolation::user_skills_dir(&self.app.env), SOURCE_USER, &mut rows, &mut total);
        if !root.is_empty() {
            scan(&Path::new(&root).join(".claude").join("skills"), SOURCE_PROJECT, &mut rows, &mut total);
        }
        for dir in runtime_isolation::installed_plugin_skill_dirs(&self.app.env) {
            scan(&dir, &plugin_label(&dir), &mut rows, &mut total);
        }
        let defaults = runtime_isolation::all_skill_names(&runtime_isolation::skills_allowlist(None, &Map::new()));
        for row in &mut rows {
            let is_default = defaults.iter().any(|d| Some(d.as_str()) == row["name"].as_str());
            row["default"] = json!(is_default);
        }
        let released: Vec<String> = config
            .get(runtime_isolation::EXTRA_SKILLS_KEY)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        json!({"ok": true, "skills": rows, "total": total, "truncated": total > rows.len(), "released": released, "standards_warning": self.standards_warning(&root)})
    }

    /// One line when the root `CLAUDE.md` binds Standards slots that the loop ignores because the repository lists subprojects (SK1), else null.
    fn standards_warning(&self, root: &str) -> Value {
        if root.is_empty() || standards::read(Path::new(root)).bindings.is_empty() {
            return Value::Null;
        }
        let listed = subprojects::entry_projects(subprojects::load(root, None).0.as_ref());
        if listed.is_empty() {
            return Value::Null;
        }
        let bare: Vec<String> = standards::project_bindings(root).into_iter().filter(|(_, bindings)| bindings.is_empty()).map(|(path, _)| path).collect();
        let shown: Vec<&str> = bare.iter().take(LISTED_SUBPROJECTS).map(String::as_str).collect();
        let more = if bare.len() > shown.len() { format!(" and {} more", bare.len() - shown.len()) } else { String::new() };
        let tail = if bare.is_empty() { String::new() } else { format!(" These subprojects have none: {}{more}.", shown.join(", ")) };
        json!(format!("The root CLAUDE.md has a Standards section that is ignored: in a repository with subprojects only each subproject's CLAUDE.md is read.{tail}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(dir: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("SKILL.md"), text).unwrap();
    }

    #[test]
    fn lists_name_description_source_and_never_the_body() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let home = dir.path().join("data");
        skill(&home.join("skills"), "mine", "---\nname: mine\ndescription: Does a thing\n---\nBODY-SECRET-TEXT api_key=sk-live-0123456789abcdef0123456789abcdef\n");
        skill(&home.join("skills"), "folded", "---\ndescription: >\n  First line\n  second line\n---\n");
        skill(&home.join("skills"), "leaky", "---\ndescription: use token=abcDEF123456ghiJKL789012mnoPQR345 now\n---\n");
        std::fs::create_dir_all(home.join("skills").join("no-file")).unwrap();
        skill(&home.join("plugins").join("cache").join("market").join("kit").join("1.0.0").join("skills"), "from-plugin", "---\ndescription: Plugin skill\n---\n");
        let listed = app.skills_catalog().get("");
        let by = |n: &str| listed["skills"].as_array().unwrap().iter().find(|s| s["name"] == n).cloned().unwrap();
        assert_eq!((by("mine")["description"].clone(), by("mine")["source"].clone()), (json!("Does a thing"), json!("user")));
        assert_eq!(by("folded")["description"], "First line");
        assert_eq!((by("from-plugin")["source"].clone(), by("from-plugin")["description"].clone()), (json!("plugin: kit"), json!("Plugin skill")));
        assert!(listed["skills"].as_array().unwrap().iter().all(|s| s["name"] != "no-file"));
        let text = listed.to_string();
        for secret in ["BODY-SECRET", "sk-live", "abcDEF123456", "SKILL.md", "market"] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
        assert_eq!((listed["total"].clone(), listed["truncated"].clone(), listed["standards_warning"].clone()), (json!(4), json!(false), Value::Null));
    }

    #[test]
    fn the_list_is_capped_and_the_saved_choice_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let skills = dir.path().join("data").join("skills");
        for i in 0..(MAX_SKILLS + 5) {
            skill(&skills, &format!("s{i:03}"), "---\ndescription: x\n---\n");
        }
        app.app_settings().update_setting(&json!("loop_extra_skills"), &json!(["s001", "s002"])).unwrap();
        let listed = app.skills_catalog().get("");
        assert_eq!((listed["skills"].as_array().unwrap().len(), listed["total"].clone(), listed["truncated"].clone()), (MAX_SKILLS, json!(MAX_SKILLS + 5), json!(true)));
        assert_eq!(listed["released"], json!(["s001", "s002"]));
    }

    #[test]
    fn a_root_standards_section_is_reported_when_subprojects_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(repo.join("web")).unwrap();
        std::fs::write(repo.join("CLAUDE.md"), "# Title\n\n## Standards\n\nfrontend: style-kit\n").unwrap();
        let project = app.project_service().open_existing_project(&repo.display().to_string()).unwrap();
        let service = app.skills_catalog();
        assert_eq!(service.standards_warning(&repo.display().to_string()), Value::Null);
        std::fs::create_dir_all(repo.join(".specs").join("project")).unwrap();
        std::fs::write(repo.join(".specs").join("project").join("SUBPROJECTS.json"), r#"{"repos": {"proj": {"projects": [{"path": "web", "skill": "frontend-dev"}]}}}"#)
            .unwrap();
        let warning = service.standards_warning(&repo.display().to_string());
        let text = warning.as_str().unwrap();
        assert!(text.contains("ignored") && text.contains("web"), "{text}");
        assert!(service.get(&project.id)["standards_warning"].is_string());
        std::fs::write(repo.join("web").join("CLAUDE.md"), "## Standards\n\nfrontend: style-kit\n").unwrap();
        assert!(!service.standards_warning(&repo.display().to_string()).as_str().unwrap().contains("have none"));
    }
}
