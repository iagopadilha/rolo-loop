// SPDX-License-Identifier: MIT
//! Reads and writes `config.json` of the app data folder (`~/.loop-harness`, or `LOOP_DATA_DIR`) for the Settings screen, and the registry of the
//! projects known to the app (`projects.json`).
//!
//! Config: only the keys asked for change: every other key (including the ones this app does not know) is kept as it is. The write is atomic and
//! UTF-8. A file that exists but is not a JSON object is never overwritten.
//!
//! Registry: atomic write guarded by a lock (several requests may open a project at the same time). The project id is stable: short hash of the
//! normalized path (same folder = same id, in any session).

use crate::domain::errors::{AppError, Kind};
use crate::domain::project::Project;
use engine_state::loop_io::atomic_write_text;
use pycompat::json::py_json_dumps_indent;
use pycompat::{nt_abspath, nt_normcase, nt_realpath};
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const CONFIG_FILE_NAME: &str = "config.json";
pub const PROJECT_ID_LENGTH: usize = 12;
pub const REGISTRY_FORMAT_VERSION: i64 = 1;

pub struct AppConfigRepository {
    pub data_dir: PathBuf,
    write_lock: Mutex<()>,
}

impl AppConfigRepository {
    pub fn new(data_dir: &Path) -> Self {
        AppConfigRepository { data_dir: data_dir.to_path_buf(), write_lock: Mutex::new(()) }
    }

    pub fn path(&self) -> PathBuf {
        self.data_dir.join(CONFIG_FILE_NAME)
    }

    /// The whole file as an object; `{}` when it is missing or empty. Errors when it holds something else.
    pub fn read_all(&self) -> Result<Map<String, Value>, AppError> {
        let bytes = match std::fs::read(self.path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => return Err(AppError::new(Kind::ConfigFileUnreadable, format!("config.json cannot be read: {error}"))),
        };
        let text = String::from_utf8_lossy(&bytes);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        if text.trim().is_empty() {
            return Ok(Map::new());
        }
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(content)) => Ok(content),
            _ => Err(AppError::new(Kind::ConfigFileUnreadable, "config.json is not a JSON object: fix or remove it, then save again")),
        }
    }

    /// Same as `read_all` for display: an unreadable file shows the defaults.
    pub fn read_all_or_empty(&self) -> Map<String, Value> {
        self.read_all().unwrap_or_default()
    }

    pub fn set_value(&self, key: &str, value: Value) -> Result<(), AppError> {
        self.set_values(&[(key.to_string(), value)], &[])
    }

    /// Sets several keys and drops the ones in `remove` in ONE atomic write (every other key stays).
    pub fn set_values(&self, values: &[(String, Value)], remove: &[&str]) -> Result<(), AppError> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut content = self.read_all()?;
        for (key, value) in values {
            content.insert(key.clone(), value.clone());
        }
        for key in remove {
            content.shift_remove(*key);
        }
        self.write_all(&content)
    }

    /// Drops the key (back to the default); a file that never had it is not touched.
    pub fn remove_key(&self, key: &str) -> Result<bool, AppError> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut content = self.read_all()?;
        if content.shift_remove(key).is_none() {
            return Ok(false);
        }
        self.write_all(&content)?;
        Ok(true)
    }

    pub fn write_all(&self, content: &Map<String, Value>) -> Result<(), AppError> {
        let text = format!("{}\n", py_json_dumps_indent(&Value::Object(content.clone()), false, 2));
        atomic_write_text(&self.path(), &text).map_err(|error| AppError::new(Kind::ConfigFileUnreadable, format!("config.json cannot be written: {error}")))
    }
}

/// Absolute path, without symbolic links and with the system's case rules (on Windows `C:\A` == `c:\a`).
pub fn normalize_project_path(path: &str) -> String {
    nt_normcase(&nt_realpath(&nt_abspath(path)))
}

pub fn compute_project_id(path: &str) -> String {
    let digest = Sha1::digest(normalize_project_path(path).as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect::<String>()[..PROJECT_ID_LENGTH].to_string()
}

pub fn project_to_json(project: &Project) -> Value {
    json!({"id": project.id, "name": project.name, "repo_path": project.repo_path, "opened_at": project.opened_at, "last_opened_at": project.last_opened_at, "favorite": project.favorite})
}

fn text_field(item: &Map<String, Value>, key: &str) -> String {
    item.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

pub fn project_from_json(item: &Map<String, Value>) -> Project {
    Project {
        id: text_field(item, "id"),
        name: text_field(item, "name"),
        repo_path: text_field(item, "repo_path"),
        opened_at: text_field(item, "opened_at"),
        last_opened_at: text_field(item, "last_opened_at"),
        favorite: engine_state::notify_truthy(item.get("favorite")),
    }
}

/// Registry of the projects known to the app (`projects.json`).
pub struct ProjectRepository {
    pub registry_path: PathBuf,
    registry_lock: Mutex<()>,
}

impl ProjectRepository {
    pub fn new(registry_path: &Path) -> Self {
        ProjectRepository { registry_path: registry_path.to_path_buf(), registry_lock: Mutex::new(()) }
    }

    pub fn list_projects(&self) -> Vec<Project> {
        let _guard = self.registry_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read_registry_items().iter().map(project_from_json).collect()
    }

    /// Project or `None`.
    pub fn get_project(&self, project_id: &str) -> Option<Project> {
        self.list_projects().into_iter().find(|p| p.id == project_id)
    }

    /// Inserts or replaces by id (keeps the position of an existing one).
    pub fn write_project(&self, project: &Project) -> std::io::Result<()> {
        let _guard = self.registry_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut items = self.read_registry_items();
        let new_item = project_to_json(project).as_object().cloned().unwrap_or_default();
        match items.iter().position(|item| item.get("id").and_then(Value::as_str) == Some(project.id.as_str())) {
            Some(position) => items[position] = new_item,
            None => items.push(new_item),
        }
        self.write_registry_items(&items)
    }

    pub fn remove_project(&self, project_id: &str) -> std::io::Result<bool> {
        let _guard = self.registry_lock.lock().unwrap_or_else(|e| e.into_inner());
        let items = self.read_registry_items();
        let remaining: Vec<Map<String, Value>> = items.iter().filter(|item| item.get("id").and_then(Value::as_str) != Some(project_id)).cloned().collect();
        if remaining.len() == items.len() {
            return Ok(false);
        }
        self.write_registry_items(&remaining)?;
        Ok(true)
    }

    fn read_registry_items(&self) -> Vec<Map<String, Value>> {
        let Ok(bytes) = std::fs::read(&self.registry_path) else { return vec![] };
        let Ok(content) = serde_json::from_slice::<Value>(&bytes) else { return vec![] };
        let Some(items) = content.get("projects").and_then(Value::as_array) else { return vec![] };
        items
            .iter()
            .filter_map(Value::as_object)
            .filter(|item| engine_state::notify_truthy(item.get("id")) && engine_state::notify_truthy(item.get("repo_path")))
            .cloned()
            .collect()
    }

    fn write_registry_items(&self, items: &[Map<String, Value>]) -> std::io::Result<()> {
        let document = json!({"version": REGISTRY_FORMAT_VERSION, "projects": items});
        atomic_write_text(&self.registry_path, &py_json_dumps_indent(&document, false, 2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_keeps_unknown_keys_and_refuses_odd_files() {
        let dir = tempfile::tempdir().unwrap();
        let repository = AppConfigRepository::new(dir.path());
        assert!(repository.read_all().unwrap().is_empty());
        std::fs::write(repository.path(), "\u{feff}{\"unknown\": [1], \"a\": 1}").unwrap();
        repository.set_values(&[("b".into(), json!("é"))], &["a"]).unwrap();
        assert_eq!(std::fs::read_to_string(repository.path()).unwrap(), "{\n  \"unknown\": [\n    1\n  ],\n  \"b\": \"é\"\n}\n");
        assert!(!repository.remove_key("zzz").unwrap() && repository.remove_key("b").unwrap());
        std::fs::write(repository.path(), "[1]").unwrap();
        assert_eq!(repository.read_all().unwrap_err().kind, Kind::ConfigFileUnreadable);
        assert!(repository.read_all_or_empty().is_empty());
        assert!(repository.set_value("x", json!(1)).is_err());
        assert_eq!(std::fs::read_to_string(repository.path()).unwrap(), "[1]");
    }

    #[test]
    fn registry_inserts_replaces_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let repository = ProjectRepository::new(&dir.path().join("sub").join("projects.json"));
        assert!(repository.list_projects().is_empty());
        let id = compute_project_id(&dir.path().display().to_string());
        assert_eq!(id.len(), 12);
        assert_eq!(id, compute_project_id(&dir.path().display().to_string().to_uppercase()));
        let mut project =
            Project { id: id.clone(), name: "p".into(), repo_path: dir.path().display().to_string(), opened_at: "t".into(), last_opened_at: "t".into(), favorite: false };
        repository.write_project(&project).unwrap();
        repository.write_project(&Project { id: "other".into(), ..project.clone() }).unwrap();
        project.favorite = true;
        repository.write_project(&project).unwrap();
        let listed = repository.list_projects();
        assert_eq!((listed.len(), listed[0].favorite, listed[1].id.as_str()), (2, true, "other"));
        assert_eq!(repository.get_project(&id).unwrap().name, "p");
        assert!(repository.remove_project("other").unwrap() && !repository.remove_project("other").unwrap());
    }
}
