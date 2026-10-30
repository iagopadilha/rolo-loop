// SPDX-License-Identifier: MIT
//! The optional planning fields of a ROADMAP feature line, resolved with today's behavior as the default.
//!
//! `split=none|group|task` (one executor for the whole feature | one per group of tasks that share files | one per task, in sequence),
//! `gate=default|lint|agent|both` (the spec gate), `quality=on|off` (the `quality-gate` agent), `models=role:model,role:model`,
//! `reads=a,b` (the exact docs/skills the executor loads), `tools=a,b` (extra tools or MCP servers allowed).
//!
//! Never a violation: a field that is missing means the default, a field that cannot be understood becomes a WARNING and the default
//! (planning fields must not block the loop). `harness state plan <folder> [slug]` prints the resolved plan.

use crate::loop_state::Fields;
use serde_json::{json, Map, Value};

pub const SPLITS: [&str; 3] = ["none", "group", "task"];
pub const GATES: [&str; 4] = ["default", "lint", "agent", "both"];
pub const QUALITY: [&str; 2] = ["on", "off"];

fn list(value: &str) -> Vec<String> {
    value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

fn choice(fields: &Fields, key: &str, valid: &[&str], default: &str, warnings: &mut Vec<String>) -> String {
    match fields.get(key).filter(|v| !v.is_empty()) {
        None => default.to_string(),
        Some(v) if valid.contains(&v) => v.to_string(),
        Some(v) => {
            warnings.push(format!("{key}={v} is not valid (valid: {}); using {default}", valid.join(", ")));
            default.to_string()
        }
    }
}

/// `(plan, warnings)` for one feature. `plan` always has every field.
pub fn resolve(fields: &Fields) -> (Value, Vec<String>) {
    let mut warnings = vec![];
    let split = choice(fields, "split", &SPLITS, "none", &mut warnings);
    let gate = choice(fields, "gate", &GATES, "default", &mut warnings);
    let quality = choice(fields, "quality", &QUALITY, "on", &mut warnings);
    let mut models = Map::new();
    for pair in list(fields.text("models")) {
        match pair.split_once(':') {
            Some((role, model)) if !role.is_empty() && !model.is_empty() => {
                models.insert(role.to_string(), json!(model));
            }
            _ => warnings.push(format!("models entry {pair:?} must be role:model; ignored")),
        }
    }
    let plan = json!({
        "split": split, "gate": gate, "quality": quality,
        "models": models, "reads": list(fields.text("reads")), "tools": list(fields.text("tools")),
    });
    (plan, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_state::parse_roadmap;

    fn fields(line: &str) -> Fields {
        parse_roadmap(line).0 .0.remove(0).1
    }

    #[test]
    fn no_field_means_todays_behavior() {
        let (plan, warnings) = resolve(&fields("- [a] status=pending paths=src"));
        assert!(warnings.is_empty());
        assert_eq!(plan["split"], "none");
        assert_eq!(plan["gate"], "default");
        assert_eq!(plan["quality"], "on");
        assert_eq!(plan["models"], json!({}));
        assert_eq!(plan["reads"], json!([]));
    }

    #[test]
    fn valid_fields_are_read() {
        let (plan, warnings) = resolve(&fields("- [a] status=pending paths=src split=group gate=both quality=off models=mapper:haiku,executor:sonnet reads=docs/codebase/CONVENTIONS.md,docs/codebase/TESTING.md tools=postgres"));
        assert!(warnings.is_empty());
        assert_eq!(plan["split"], "group");
        assert_eq!(plan["gate"], "both");
        assert_eq!(plan["quality"], "off");
        assert_eq!(plan["models"]["mapper"], "haiku");
        assert_eq!(plan["reads"].as_array().unwrap().len(), 2);
        assert_eq!(plan["tools"], json!(["postgres"]));
    }

    #[test]
    fn a_bad_value_is_a_warning_and_the_default_never_a_stop() {
        let (plan, warnings) = resolve(&fields("- [a] status=pending paths=src split=many quality=maybe models=oops"));
        assert_eq!(plan["split"], "none");
        assert_eq!(plan["quality"], "on");
        assert_eq!(warnings.len(), 3);
    }
}
