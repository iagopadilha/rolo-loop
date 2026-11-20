// SPDX-License-Identifier: MIT
//! Projects known to the app: list, open an existing folder, create from scratch, forget.
//!
//! When a project is opened with the app up, a compatible `MONITOR.json` is written to the `work` dir (loop worktree or repo): the engine
//! (`monitor_proc`) and the skill find the panel through it. It is only written when `.specs/` already exists (opening a folder creates nothing in
//! it) and it never overwrites the one of another live process. On shutdown, only the ones this process wrote are deleted.

use super::App;
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::{Project, ProjectContext};
use crate::repositories::app_config::compute_project_id;
use crate::repositories::loop_command_file::now_iso;
use engine_state::loop_state;
use pycompat::Re;
use serde_json::json;
use std::path::Path;
use std::sync::LazyLock;

pub const MONITOR_FILE_NAME: &str = "MONITOR.json";
static PROJECT_NAME_PATTERN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z0-9][A-Za-z0-9 ._-]*\z"));
pub const PROJECT_NAME_MAX_LENGTH: usize = 100;
const INITIAL_COMMIT_MESSAGE: &str = "chore: initial project structure (loop-harness)";

fn is_windows_reserved(name: &str) -> bool {
    matches!(name, "con" | "prn" | "aux" | "nul")
        || ["com", "lpt"].iter().any(|prefix| name.strip_prefix(prefix).is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")))
}

/// `/p/<id>/`
pub fn build_relative_project_url(project_id: &str) -> String {
    format!("/p/{project_id}/")
}

/// `(port, absolute project url)` of an app url such as `http://localhost:47800`.
fn monitor_location(app_url: &str, project_id: &str) -> (Option<u16>, String) {
    let rest = app_url.split_once("://").map(|p| p.1).unwrap_or(app_url);
    let scheme = app_url.split_once("://").map(|p| format!("{}://", p.0)).unwrap_or_default();
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let port = authority.rsplit_once(':').and_then(|p| p.1.parse().ok());
    (port, format!("{scheme}{authority}{}", build_relative_project_url(project_id)))
}

fn expand_user(path: &str, home: &Path) -> String {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => format!("{}{rest}", home.display()),
        _ => path.to_string(),
    }
}

pub struct ProjectService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn project_service(&self) -> ProjectService<'_> {
        ProjectService { app: self }
    }
}

impl ProjectService<'_> {
    // ------------------------------------------------ read
    /// Favorites first, then the most recent access.
    pub fn list_projects(&self) -> Vec<Project> {
        let mut projects = self.app.projects.list_projects();
        projects.sort_by(|a, b| (b.favorite, &b.last_opened_at).cmp(&(a.favorite, &a.last_opened_at)));
        projects
    }

    pub fn get_project(&self, project_id: &str) -> Result<Project, AppError> {
        self.app.projects.get_project(project_id).ok_or_else(AppError::project_not_found)
    }

    /// Context with a re-evaluated `work_dir`; if the loop worktree was born (or `.specs/` appeared) after opening, the MONITOR.json starts to exist
    /// in the new work dir as well.
    pub fn get_project_context(&self, project_id: &str) -> Result<ProjectContext, AppError> {
        let context = ProjectContext::new(self.get_project(project_id)?);
        let registered = self.app.monitors.lock().unwrap_or_else(|e| e.into_inner()).1.get(project_id).cloned();
        if registered.as_deref() != Some(context.work_dir().as_str()) {
            self.register_monitor_in_work_dir(&context);
        }
        Ok(context)
    }

    // ------------------------------------------------ open / create / forget
    /// Registers (or updates `last_opened_at`) and writes the MONITOR.json. Idempotent: same folder = same id.
    pub fn open_existing_project(&self, path: &str) -> Result<Project, AppError> {
        let folder = Self::validate_existing_folder(path, &self.app.env.home_dir())?;
        let project_id = compute_project_id(&folder);
        let now = now_iso();
        let existing = self.app.projects.get_project(&project_id);
        let is_new = existing.is_none();
        let project = match existing {
            Some(mut project) => {
                project.last_opened_at = now;
                project
            }
            None => {
                let name = pycompat::last_segment(&pycompat::nt_normpath(&folder)).to_string();
                Project {
                    id: project_id,
                    name: if name.is_empty() { folder.clone() } else { name },
                    repo_path: folder.clone(),
                    opened_at: now.clone(),
                    last_opened_at: now,
                    favorite: false,
                }
            }
        };
        self.app.projects.write_project(&project).map_err(|e| AppError::invalid_path(format!("the project registry cannot be written: {e}")))?;
        self.register_monitor_in_work_dir(&ProjectContext::new(project.clone()));
        self.app.log.info("project_opened", &[("project", json!(project.id)), ("path", json!(folder)), ("new", json!(is_new))]);
        Ok(project)
    }

    pub fn create_project_from_scratch(&self, parent_path: &str, name: &str, init_git: bool, conformance: Option<&str>) -> Result<Project, AppError> {
        let validated_name = Self::validate_project_name(name)?;
        let parent_dir = Self::validate_existing_folder(parent_path, &self.app.env.home_dir())?;
        let destination = Path::new(&parent_dir).join(&validated_name);
        if destination.exists() && (!destination.is_dir() || std::fs::read_dir(&destination).map(|mut d| d.next().is_some()).unwrap_or(true)) {
            return Err(AppError::new(
                Kind::ProjectAlreadyExists,
                format!("{} already exists with content: choose another name or use Open folder", destination.display()),
            ));
        }
        let _ = std::fs::create_dir_all(&destination);
        let destination = destination.display().to_string();
        self.app.specs_structure().initialize_structure(&destination, conformance, &validated_name)?;
        if init_git {
            let (ok, error) = self.app.git.init_repo_with_initial_commit(&destination, INITIAL_COMMIT_MESSAGE, "");
            if !ok {
                // the folder and .specs already exist: the project opens and the checklist shows git as pending
                self.app.log.warn("project_git_init_failed", &[("path", json!(destination)), ("error", json!(error))]);
            }
        }
        self.app.log.info("project_created", &[("path", json!(destination)), ("git", json!(init_git))]);
        self.open_existing_project(&destination)
    }

    /// Only removes it from the registry (and releases this process's MONITOR.json); never deletes a project file.
    pub fn forget_project(&self, project_id: &str) -> Result<(), AppError> {
        self.get_project(project_id)?;
        self.release_monitors(Some(project_id));
        self.app.monitors.lock().unwrap_or_else(|e| e.into_inner()).1.remove(project_id);
        let _ = self.app.projects.remove_project(project_id);
        self.app.log.info("project_forgotten", &[("project", json!(project_id))]);
        Ok(())
    }

    pub fn release_projects_on_shutdown(&self) {
        self.release_monitors(None);
    }

    // ------------------------------------------------ MONITOR.json
    fn register_monitor_in_work_dir(&self, context: &ProjectContext) {
        let app_url = self.app.app_url();
        let work = context.work_dir();
        if app_url.is_empty() || !Path::new(&work).join(".specs").is_dir() {
            return;
        }
        let path = loop_state::loop_path(&work, MONITOR_FILE_NAME);
        let mut monitors = self.app.monitors.lock().unwrap_or_else(|e| e.into_inner());
        let existing = self.app.specs.read_json(&path);
        let existing_pid = existing.get("pid").and_then(|p| p.as_i64()).unwrap_or(0);
        if !existing.is_empty() && existing_pid != i64::from(std::process::id()) && self.app.processes.is_alive(existing_pid) {
            self.app.log.warn("monitor_json_owned_by_other_process", &[("path", json!(path.display().to_string())), ("pid", json!(existing_pid))]);
            monitors.1.insert(context.project.id.clone(), work);
            return;
        }
        let (port, url) = monitor_location(&app_url, &context.project.id);
        let data = json!({"pid": std::process::id(), "port": port, "url": url, "repo": context.repo_path(), "work": work, "started_at": self.app.started_at});
        if let Err(failure) = self.app.specs.write_json_atomic(&path, &data) {
            self.app.log.warn(
                "monitor_json_not_written",
                &[("path", json!(path.display().to_string())), ("message", json!(failure.to_string().chars().take(200).collect::<String>()))],
            );
            return;
        }
        let key = path.display().to_string();
        monitors.0.retain(|(p, _)| *p != key);
        monitors.0.push((key, context.project.id.clone()));
        monitors.1.insert(context.project.id.clone(), work);
    }

    /// Deletes the MONITOR.json this process wrote (of one project, or of all).
    fn release_monitors(&self, only: Option<&str>) {
        let mut monitors = self.app.monitors.lock().unwrap_or_else(|e| e.into_inner());
        let mut kept = Vec::new();
        for (path, owner) in std::mem::take(&mut monitors.0) {
            if only.is_some_and(|o| o != owner) {
                kept.push((path, owner));
                continue;
            }
            let pid = self.app.specs.read_json(Path::new(&path)).get("pid").and_then(|p| p.as_i64());
            if pid == Some(i64::from(std::process::id())) {
                let _ = std::fs::remove_file(&path);
            }
        }
        monitors.0 = kept;
    }

    // ------------------------------------------------ validation
    pub fn validate_existing_folder(path: &str, home: &Path) -> Result<String, AppError> {
        if path.trim().is_empty() {
            return Err(AppError::invalid_path("empty path"));
        }
        let folder = pycompat::nt_abspath(&expand_user(path.trim(), home));
        if !Path::new(&folder).is_dir() {
            return Err(AppError::invalid_path(format!("not an existing folder: {folder}")));
        }
        // An 8.3 short name (`C:\Users\JOHNDO~1\...`, what %TEMP% often holds) becomes the long path: the guard and git compare the long one, and
        // a project registered by its short name had every edit refused as "outside the project". Other paths stay as given (their id too).
        if folder.contains('~') {
            return Ok(pycompat::nt_realpath(&folder));
        }
        Ok(folder)
    }

    pub fn validate_project_name(name: &str) -> Result<String, AppError> {
        let text = name.trim();
        if text.is_empty() || text.chars().count() > PROJECT_NAME_MAX_LENGTH || !PROJECT_NAME_PATTERN.is_match(text) || text.ends_with(['.', ' ']) {
            return Err(AppError::invalid_path("invalid name: use letters, digits, space, dot, - and _ (no slash), starting with a letter or digit"));
        }
        if is_windows_reserved(&text.split('.').next().unwrap_or("").to_lowercase()) {
            return Err(AppError::invalid_path(format!("name reserved by Windows: {text}")));
        }
        Ok(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_folders_are_validated() {
        for bad in ["", "  ", "a/b", ".hidden", "bad.", "x".repeat(101).as_str(), "con", "COM1.txt", "nul.x"] {
            assert_eq!(ProjectService::validate_project_name(bad).unwrap_err().kind, Kind::InvalidPath, "{bad:?}");
        }
        assert_eq!(ProjectService::validate_project_name("  My app-1_x.y ").unwrap(), "My app-1_x.y");
        assert!(ProjectService::validate_project_name("com10").is_ok());
        assert_eq!(ProjectService::validate_existing_folder("", Path::new("C:\\")).unwrap_err().message, "empty path");
        assert!(ProjectService::validate_existing_folder("C:\\definitely\\not\\here", Path::new("C:\\")).unwrap_err().message.starts_with("not an existing folder: "));
        assert_eq!(monitor_location("http://localhost:47800", "abc"), (Some(47800), "http://localhost:47800/p/abc/".to_string()));
        assert_eq!(monitor_location("http://localhost", "abc").0, None);
    }

    #[test]
    fn open_create_list_and_forget() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let service = app.project_service();
        let one = dir.path().join("one");
        let two = dir.path().join("two");
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        let first = service.open_existing_project(&one.display().to_string()).unwrap();
        let again = service.open_existing_project(&one.display().to_string()).unwrap();
        assert_eq!((first.id.clone(), first.name.clone()), (again.id.clone(), "one".to_string()));
        assert_eq!(first.opened_at, again.opened_at);
        let other = service.open_existing_project(&two.display().to_string()).unwrap();
        assert_eq!(service.list_projects().len(), 2);
        let mut favorite = other.clone();
        favorite.favorite = true;
        app.projects.write_project(&favorite).unwrap();
        assert_eq!(service.list_projects()[0].id, other.id);
        assert_eq!(service.get_project("nope").unwrap_err().kind, Kind::ProjectNotFound);
        let created = service.create_project_from_scratch(&dir.path().display().to_string(), "fresh", false, Some("standard")).unwrap();
        assert!(Path::new(&created.repo_path).join(".specs/project/ROADMAP.md").is_file());
        assert_eq!(service.create_project_from_scratch(&dir.path().display().to_string(), "fresh", false, None).unwrap_err().kind, Kind::ProjectAlreadyExists);
        service.forget_project(&first.id).unwrap();
        assert!(one.is_dir() && service.get_project(&first.id).is_err() && service.forget_project(&first.id).is_err());
    }

    #[test]
    fn the_monitor_file_follows_the_app_url_and_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let service = app.project_service();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".specs")).unwrap();
        let project = service.open_existing_project(&repo.display().to_string()).unwrap();
        let monitor = loop_state::loop_path(&repo.display().to_string(), MONITOR_FILE_NAME);
        assert!(!monitor.exists(), "no app url yet: nothing is written");
        app.set_app_url("http://localhost:47801");
        service.get_project_context(&project.id).unwrap();
        let written = app.specs.read_json(&monitor);
        assert_eq!((written["port"].clone(), written["url"].clone()), (json!(47801), json!(format!("http://localhost:47801/p/{}/", project.id))));
        service.release_projects_on_shutdown();
        assert!(!monitor.exists());
    }

    #[test]
    fn a_short_8_3_folder_name_is_registered_by_its_long_path() {
        // %TEMP% is often a short 8.3 name on Windows; elsewhere this only checks that the folder is kept
        let temp = std::env::temp_dir().display().to_string();
        let folder = ProjectService::validate_existing_folder(&temp, Path::new("C:/home")).unwrap();
        assert!(Path::new(&folder).is_dir());
        if temp.contains('~') {
            assert!(!folder.contains('~'), "{folder}");
        }
    }
}
