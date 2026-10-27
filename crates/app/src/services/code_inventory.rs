// SPDX-License-Identifier: MIT
//! Runs the code inventory (`engine_state::code_inventory`) for a folder of a repository and reads the last result back.
//!
//! The inventory lists the giant files, the entry points and the totals once, deterministically, so the map job can tell the AI where not to read
//! whole files. It is written to `.specs/project/loop/internal/inventory/<name>.json` (`_root` for the repository root, the path with "/" replaced
//! by "-" for a subproject). A failure is a note, never an error and never a failed map job.
//!
//! Thresholds come from the app config (`inventory_giant_lines`, `inventory_giant_kb` of `config.json`; invalid values fell back to 1500 / 200 and
//! left a note there).

use engine_state::code_inventory as engine;
use serde_json::{json, Value};
use std::path::Path;

pub const GIANT_LIST_SIZE: usize = 5;

/// `_root` for the repository root, else the path with "/" replaced by "-" (the engine's default name).
pub fn inventory_name(folder_relative: &str) -> String {
    let normalized = folder_relative.replace('\\', "/");
    let normalized = normalized.trim_matches('/');
    let normalized = if folder_relative.is_empty() { "" } else { normalized };
    if normalized.is_empty() || normalized == "." { engine::ROOT_NAME.to_string() } else { normalized.replace('/', "-") }
}

fn format_thousands(number: i64) -> String {
    let digits = number.abs().to_string();
    let mut grouped = String::new();
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(' ');
        }
        grouped.push(c);
    }
    if number < 0 { format!("-{grouped}") } else { grouped }
}

pub fn build_summary(files: i64, giant: i64) -> String {
    format!("inventory: {} files, {} giant", format_thousands(files), format_thousands(giant))
}

/// Where the inventory of `name` lives, relative to the repository root (forward slashes: it goes into prompts).
pub fn relative_path(name: &str) -> String {
    let base = engine::INVENTORY_RELATIVE_DIR.replace('\\', "/");
    format!("{}/{name}.json", base.trim_end_matches('/'))
}

pub fn top_giants(document: &Value, limit: usize) -> Vec<Value> {
    document.get("giant").and_then(Value::as_array).map(|g| g.iter().take(limit).map(|item| json!({"path": item.get("path").cloned().unwrap_or(json!("")), "lines": item.get("lines").cloned().unwrap_or(json!(0))})).collect()).unwrap_or_default()
}

#[derive(Clone, Debug)]
pub struct CodeInventoryService {
    pub giant_lines: i64,
    pub giant_kb: i64,
    pub threshold_note: String,
}

impl CodeInventoryService {
    pub fn new(config: &crate::config::AppConfig) -> Self {
        CodeInventoryService { giant_lines: config.inventory_giant_lines, giant_kb: config.inventory_giant_kb, threshold_note: config.inventory_threshold_note.clone() }
    }

    /// `(giant_lines, giant_kb, note)`: the configured values, or the defaults with the note the config left.
    pub fn thresholds(&self) -> (i64, i64, String) {
        let mut note = self.threshold_note.clone();
        let (mut lines, mut kilobytes) = (self.giant_lines, self.giant_kb);
        if lines <= 0 {
            lines = engine::DEFAULT_GIANT_LINES;
            if note.is_empty() {
                note = "invalid inventory_giant_lines: using the default".into();
            }
        }
        if kilobytes <= 0 {
            kilobytes = engine::DEFAULT_GIANT_KB;
            if note.is_empty() {
                note = "invalid inventory_giant_kb: using the default".into();
            }
        }
        (lines, kilobytes, note)
    }

    /// Measures `<repo_root>/<folder_relative>` and writes its inventory. Never fails. Returns `{ok, path (relative to repo_root), files, giant,
    /// giant_list [{path, lines}], note, summary}`.
    pub fn run(&self, repo_root: &str, folder_relative: &str, name: Option<&str>) -> Value {
        let name = name.map(String::from).unwrap_or_else(|| inventory_name(folder_relative));
        let (giant_lines, giant_kb, threshold_note) = self.thresholds();
        let mut result = json!({"ok": false, "path": relative_path(&name), "files": 0, "giant": 0, "giant_list": [], "note": threshold_note, "summary": ""});
        let root = pycompat::nt_abspath(repo_root);
        let folder = if folder_relative.is_empty() || folder_relative == "." { root.clone() } else { pycompat::nt_abspath(&Path::new(&root).join(folder_relative).display().to_string()) };
        let document = engine::build_inventory(&folder, &root, giant_lines, giant_kb, &[]);
        let target = engine::inventory_path(&root, &name);
        if let Err(failure) = engine::write_atomically(&target, &document) {
            let message = format!("the code inventory failed ({}): the map runs without it", failure.to_string().chars().take(200).collect::<String>());
            result["note"] = json!([threshold_note, message].iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join("; "));
            return result;
        }
        let files = document["totals"]["files"].as_i64().unwrap_or(0);
        let giant = document.get("giant").and_then(Value::as_array).map(|g| g.len() as i64).unwrap_or(0);
        result["ok"] = json!(true);
        result["files"] = json!(files);
        result["giant"] = json!(giant);
        result["giant_list"] = json!(top_giants(&document, GIANT_LIST_SIZE));
        result["summary"] = json!(build_summary(files, giant));
        result
    }

    /// The last inventory document of `name`, or `None` (missing or unreadable).
    pub fn read(&self, repo_root: &str, name: &str) -> Option<Value> {
        let bytes = std::fs::read(engine::inventory_path(&pycompat::nt_abspath(repo_root), name)).ok()?;
        serde_json::from_slice::<Value>(&bytes).ok().filter(Value::is_object)
    }

    pub fn giant_count(&self, repo_root: &str, name: &str) -> usize {
        self.read(repo_root, name).and_then(|d| d.get("giant").and_then(Value::as_array).map(Vec::len)).unwrap_or(0)
    }

    pub fn giant_top(&self, repo_root: &str, name: &str, limit: usize) -> Vec<Value> {
        self.read(repo_root, name).map(|d| top_giants(&d, limit)).unwrap_or_default()
    }

    /// The `before_start` step of a map job: runs the inventory and reports the summary (or the note) in the job progress.
    pub fn run_for_job(&self, repo_root: &str, folder_relative: &str, name: &str, update_progress: &dyn Fn(&str)) -> Value {
        let result = self.run(repo_root, folder_relative, Some(name));
        update_progress(if result["ok"] == true { result["summary"].as_str().unwrap_or("") } else { result["note"].as_str().unwrap_or("") });
        result
    }

    /// The text a map prompt gets appended so the mapper finds the inventory (path relative to the repository root).
    pub fn prompt_argument(&self, name: &str) -> String {
        format!(" --inventory {}", relative_path(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_summaries_and_thresholds() {
        assert_eq!((inventory_name(""), inventory_name("."), inventory_name("src\\api/")), ("_root".to_string(), "_root".to_string(), "src-api".to_string()));
        assert_eq!(build_summary(12345, 7), "inventory: 12 345 files, 7 giant");
        assert!(relative_path("_root").ends_with("/_root.json"));
        let service = CodeInventoryService { giant_lines: 0, giant_kb: -1, threshold_note: String::new() };
        assert_eq!(service.thresholds(), (1500, 200, "invalid inventory_giant_lines: using the default".to_string()));
    }

    #[test]
    fn the_inventory_is_written_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("big.rs"), "x\n".repeat(30)).unwrap();
        let service = CodeInventoryService { giant_lines: 10, giant_kb: 200, threshold_note: String::new() };
        let result = service.run(&root, "", None);
        assert_eq!((result["ok"].clone(), result["giant"].clone()), (json!(true), json!(1)));
        assert_eq!(service.giant_count(&root, "_root"), 1);
        assert_eq!(service.giant_top(&root, "_root", 5)[0]["lines"], 30);
        assert!(service.read(&root, "nope").is_none() && service.prompt_argument("_root").starts_with(" --inventory "));
        let seen = std::cell::RefCell::new(String::new());
        service.run_for_job(&root, "src", "src", &|text| *seen.borrow_mut() = text.to_string());
        assert!(seen.borrow().starts_with("inventory: "));
    }
}
