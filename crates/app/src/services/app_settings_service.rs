// SPDX-License-Identifier: MIT
//! The Settings screen of the app (`GET/POST /api/app/settings`): the list of settings with their current value and default, the validated write
//! to `config.json`, the caveman availability and the caveman cost comparison over the turns already recorded.
//!
//! Read, decide, write stay apart: `build_item` and `build_caveman_comparison` only read; `update_setting` validates (domain), then asks the
//! repository to write. Nothing here starts a process.

use super::App;
use crate::config as app_config;
use crate::domain::app_settings::{self, SettingDefinition};
use crate::domain::errors::{AppError, Kind};
use crate::domain::project::ProjectContext;
use engine_state::{claude_binaries, runtime_isolation};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

/// CAV-04: fewer turns on either side is "not enough data yet".
pub const MIN_TURNS_PER_SIDE: usize = 3;
const COST_DIGITS: i32 = 4;
const TURN_COST_KEY: &str = "total_cost_usd";
const TURN_CAVEMAN_KEY: &str = "caveman";
const CAVEMAN_KEY: &str = runtime_isolation::CAVEMAN_KEY;

pub struct AppSettingsService<'a> {
    pub app: &'a App,
}

impl App {
    pub fn app_settings(&self) -> AppSettingsService<'_> {
        AppSettingsService { app: self }
    }
}

/// Python `str(item)` of a list item.
fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => pycompat::json::py_json_dumps(other, false),
    }
}

fn is_finite_number(value: &Value) -> bool {
    matches!(value, Value::Number(n) if n.as_f64().is_some_and(f64::is_finite))
}

impl AppSettingsService<'_> {
    fn read_user_settings(&self) -> Value {
        runtime_isolation::read_user_settings(&self.app.env)
    }

    // ---- read -------------------------------------------------------------------------------------------------------------------
    pub fn get_settings(&self) -> Value {
        let stored = self.app.settings.read_all_or_empty();
        let user_settings = self.read_user_settings();
        json!({
            "ok": true,
            "settings": app_settings::setting_catalog().iter().map(|d| self.build_item(d, &stored, &user_settings)).collect::<Vec<_>>(),
            "info": self.build_info_rows(),
            "caveman": self.build_caveman_comparison(),
        })
    }

    fn is_caveman_installed(user_settings: &Value) -> bool {
        !runtime_isolation::find_caveman_hooks(user_settings).is_empty()
    }

    /// The value the app really uses for this key: the stored one when the code accepts it, else the default (never an error).
    pub fn read_effective_value(&self, definition: &SettingDefinition, stored: &Map<String, Value>) -> Value {
        let key = definition.key;
        if app_settings::NETWORK_KEYS.contains(&key) {
            return app_settings::read_stored_or_default(definition, stored);
        }
        match key {
            "loop_isolation" => json!(runtime_isolation::isolation_enabled(stored)),
            CAVEMAN_KEY => json!(stored.get(key) == Some(&Value::Bool(true))),
            "loop_extra_skills" => match stored.get(key) {
                Some(Value::Array(items)) => json!(items.iter().map(|i| py_str(i).trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>()),
                _ => json!([]),
            },
            "orchestrator_model" => json!(app_config::read_orchestrator_model(stored)),
            "claude_binary" => match stored.get(key) {
                Some(Value::String(s)) => json!(s),
                _ => json!(""),
            },
            "claude_fallback_binaries" => json!(claude_binaries::read_fallbacks(stored.get(key).unwrap_or(&Value::Null))),
            "idle_hours" => match stored.get(key) {
                Some(v) if is_finite_number(v) && v.as_f64().is_some_and(|n| n >= 0.0) => v.clone(),
                _ => definition.default.clone(),
            },
            "output_dir" => match stored.get(key) {
                Some(Value::String(s)) if !s.is_empty() => json!(s),
                _ => app_settings::get_default_value(definition, &self.app.env),
            },
            _ => {
                let mut notes = Vec::new();
                json!(app_config::read_positive_threshold(stored, key, definition.default.as_i64().unwrap_or(0), &mut notes))
            }
        }
    }

    pub fn build_item(&self, definition: &SettingDefinition, stored: &Map<String, Value>, user_settings: &Value) -> Value {
        let value = self.read_effective_value(definition, stored);
        let mut item = Map::new();
        item.insert("key".into(), json!(definition.key));
        item.insert("label".into(), json!(definition.label));
        item.insert("description".into(), json!(definition.description));
        item.insert("type".into(), json!(definition.kind));
        item.insert("value".into(), value.clone());
        item.insert("default".into(), app_settings::get_default_value(definition, &self.app.env));
        item.insert("restart_required".into(), json!(definition.restart_required));
        item.insert("customized".into(), json!(stored.contains_key(definition.key)));
        if definition.kind == app_settings::CHOICE {
            let mut choices: Vec<Value> = definition.choices.iter().map(|(v, l)| json!({"value": v, "label": l})).collect();
            if !definition.choices.iter().any(|(v, _)| value.as_str() == Some(*v)) {
                choices.push(json!({"value": value, "label": value})); // a model id written by hand stays selectable
            }
            item.insert("choices".into(), Value::Array(choices));
        }
        if definition.key == "claude_fallback_binaries" {
            // A3: the backup commands with the account each one signs in to (from the cached `auth status`, never waiting for it)
            item.insert("accounts".into(), json!(super::claude_accounts::command_accounts(&self.app.accounts.get_rows(false))));
        }
        if definition.key == CAVEMAN_KEY {
            let installed = Self::is_caveman_installed(user_settings);
            item.insert("available".into(), json!(installed));
            item.insert("note".into(), json!(Self::describe_caveman_note(installed, runtime_isolation::isolation_enabled(stored))));
        }
        Value::Object(item)
    }

    fn describe_caveman_note(installed: bool, isolation_on: bool) -> &'static str {
        if !installed {
            return app_settings::CAVEMAN_NOT_INSTALLED_NOTE;
        }
        if !isolation_on {
            return "Has no effect while isolation is off: your own settings then load as they are.";
        }
        ""
    }

    pub fn build_info_rows(&self) -> Value {
        let config = &self.app.config;
        let skill_dir = config.ctx.skill_dir.display().to_string();
        let plugin_root = if skill_dir.is_empty() { String::new() } else { pycompat::nt_abspath(&pycompat::nt_join(&skill_dir, "..\\..")) };
        let guard = config.guard_path().display().to_string();
        let port = if config.fixed_port != 0 { config.fixed_port.to_string() } else { format!("{}-{}", config.port_range.0, config.port_range.1) };
        let or_missing = |text: String| if text.is_empty() { "not found".to_string() } else { text };
        json!([
            {"key": "data_dir", "label": "Data folder", "value": config.user_data_dir.display().to_string()},
            {"key": "plugin_root", "label": "Plugin folder", "value": or_missing(plugin_root)},
            {"key": "guard_path", "label": "Guard hook", "value": or_missing(guard)},
            {"key": "port_range", "label": "Port range", "value": port},
        ])
    }

    /// `[(caveman, cost_usd)]` of every turn log of every project (the work folder and the repo folder, once each). A turn log with no `caveman`
    /// key was written before the option existed (and before runtime isolation): it is left out, so both sides compare turns of the same app
    /// version.
    pub fn list_recorded_turns(&self) -> Vec<(bool, f64)> {
        let mut turns = Vec::new();
        let mut folders: HashSet<String> = HashSet::new();
        for project in self.app.project_service().list_projects() {
            let context = ProjectContext::new(project);
            let work_dir = context.work_dir();
            for folder in [work_dir.as_str(), context.repo_path()] {
                if !folders.insert(pycompat::nt_abspath(folder).to_lowercase()) {
                    continue;
                }
                for number in self.app.loop_files.list_turn_numbers(folder) {
                    let record = self.app.loop_files.read_turn(folder, number);
                    let cost = record.get(TURN_COST_KEY).and_then(|c| if c.is_number() { c.as_f64() } else { None });
                    let flag = record.get(TURN_CAVEMAN_KEY).and_then(Value::as_bool);
                    if let (Some(flag), Some(cost)) = (flag, cost) {
                        turns.push((flag, cost));
                    }
                }
            }
        }
        turns
    }

    pub fn build_caveman_comparison(&self) -> Value {
        let turns = self.list_recorded_turns();
        let side = |on: bool| turns.iter().filter(|(flag, _)| *flag == on).map(|(_, cost)| *cost).collect::<Vec<f64>>();
        let (on, off) = (side(true), side(false));
        let summary = |costs: &[f64]| {
            let average = if costs.is_empty() {
                Value::Null
            } else {
                let scale = 10f64.powi(COST_DIGITS);
                json!(((costs.iter().sum::<f64>() / costs.len() as f64) * scale).round() / scale)
            };
            json!({"turns": costs.len(), "average_cost_usd": average})
        };
        let enough = on.len() >= MIN_TURNS_PER_SIDE && off.len() >= MIN_TURNS_PER_SIDE;
        json!({
            "recommendation": app_settings::CAVEMAN_RECOMMENDATION, "on": summary(&on), "off": summary(&off), "enough_data": enough,
            "message": if enough { String::new() } else { format!("Not enough data yet ({} on / {} off)", on.len(), off.len()) },
        })
    }

    // ---- write ------------------------------------------------------------------------------------------------------------------
    /// Validates and saves one value; returns the updated item. Errors: `InvalidSetting` (400), `SettingUnavailable`, `ConfigFileUnreadable` (409).
    pub fn update_setting(&self, key: &Value, value: &Value) -> Result<Value, AppError> {
        let definition = app_settings::find_definition(key)?;
        let stored_value = app_settings::validate_setting_value(&definition, value)?;
        if definition.key == CAVEMAN_KEY && stored_value == Value::Bool(true) && !Self::is_caveman_installed(&self.read_user_settings()) {
            return Err(AppError::new(Kind::SettingUnavailable, app_settings::CAVEMAN_NOT_INSTALLED_NOTE));
        }
        self.app.settings.set_value(definition.key, stored_value)?;
        Ok(self.read_item(&definition))
    }

    /// Removes the key from config.json (back to the default) and returns the item.
    pub fn reset_setting(&self, key: &Value) -> Result<Value, AppError> {
        let definition = app_settings::find_definition(key)?;
        self.app.settings.remove_key(definition.key)?;
        Ok(self.read_item(&definition))
    }

    fn read_item(&self, definition: &SettingDefinition) -> Value {
        self.build_item(definition, &self.app.settings.read_all_or_empty(), &self.read_user_settings())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_reflect_stored_values_and_writes_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let service = app.app_settings();
        let all = service.get_settings();
        let find = |all: &Value, key: &str| all["settings"].as_array().unwrap().iter().find(|s| s["key"] == key).unwrap().clone();
        assert_eq!((find(&all, "loop_isolation")["value"].clone(), find(&all, "loop_isolation")["customized"].clone()), (json!(true), json!(false)));
        assert_eq!(find(&all, "max_parallel_maps")["value"], 6);
        assert_eq!(all["caveman"]["message"], "Not enough data yet (0 on / 0 off)");
        assert_eq!(all["info"][3]["key"], "port_range");
        let saved = service.update_setting(&json!("max_parallel_maps"), &json!(3)).unwrap();
        assert_eq!((saved["value"].clone(), saved["customized"].clone()), (json!(3), json!(true)));
        assert_eq!(service.update_setting(&json!("max_parallel_maps"), &json!(0)).unwrap_err().kind, Kind::InvalidSetting);
        assert_eq!(service.update_setting(&json!("nope"), &json!(1)).unwrap_err().kind, Kind::InvalidSetting);
        assert_eq!(service.update_setting(&json!("loop_caveman"), &json!(true)).unwrap_err().kind, Kind::SettingUnavailable);
        assert_eq!(service.update_setting(&json!("orchestrator_model"), &json!("my-model")).unwrap_err().kind, Kind::InvalidSetting);
        app.settings.set_value("orchestrator_model", json!("my-model")).unwrap(); // written by hand in config.json
        let model = find(&service.get_settings(), "orchestrator_model");
        assert_eq!(model["choices"].as_array().unwrap().last().unwrap()["value"], "my-model");
        let reset = service.reset_setting(&json!("max_parallel_maps")).unwrap();
        assert_eq!((reset["value"].clone(), reset["customized"].clone()), (json!(6), json!(false)));
    }

    #[test]
    fn caveman_comparison_needs_three_turns_per_side() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let repo = dir.path().join("p");
        std::fs::create_dir_all(repo.join(".specs/project/loop/json/turns")).unwrap();
        let context = ProjectContext::new(app.project_service().open_existing_project(&repo.display().to_string()).unwrap());
        for n in 1..=6u64 {
            let path = app.loop_files.get_turn_path(context.repo_path(), n);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, json!({"caveman": n % 2 == 0, "total_cost_usd": n as f64}).to_string()).unwrap();
        }
        let comparison = app.app_settings().build_caveman_comparison();
        assert_eq!(
            (comparison["enough_data"].clone(), comparison["on"]["turns"].clone(), comparison["off"]["average_cost_usd"].clone()),
            (json!(true), json!(3), json!(3.0))
        );
    }
}
