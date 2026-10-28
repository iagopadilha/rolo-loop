// SPDX-License-Identifier: MIT
//! Cost and number of attempts per feature (backlog VA5).
//!
//! After every turn the engine splits the turn's cost evenly over the features the turn worked on (the `feature` of LOOP.json, else the
//! features being specced or built) and counts one attempt for each. The totals live in `loop/json/FEATURE-COSTS.json`; [`report`] adds a
//! rough estimate of what is left (the average cost of a finished feature times the features not yet closed). Numbers only: no text from
//! the project is stored.

use crate::loop_io::{load_json, read, save_json};
use crate::loop_state::{loop_path, parse_roadmap, state_file};
use serde_json::{json, Value};
use std::path::Path;

const STORE: &str = "FEATURE-COSTS.json";
/// Statuses of a feature a turn is working on when LOOP.json names none.
const ACTIVE: [&str; 2] = ["speccing", "building"];

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// The slugs LOOP.json names as the turn's work (text with commas, or a list), only those the ROADMAP knows.
pub fn named_features(work: &str, specs: &str) -> Vec<String> {
    let state = load_json(&state_file(work, "LOOP.json"));
    let named: Vec<String> = match state.get("feature") {
        Some(Value::String(text)) => text.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect(),
        Some(Value::Array(list)) => list.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => vec![],
    };
    let features = parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md"))).0;
    named.into_iter().filter(|s| features.contains(s)).collect()
}

/// The features being specced or built right now. Read BEFORE a turn, so a feature the turn closes still gets its cost.
pub fn active_features(specs: &str) -> Vec<String> {
    let features = parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md"))).0;
    features.iter().filter(|(_, f)| ACTIVE.contains(&f.status())).map(|(s, _)| s.clone()).collect()
}

/// The slugs a turn worked on: the ones LOOP.json names, else `active_before`.
pub fn features_of_turn(work: &str, specs: &str, active_before: &[String]) -> Vec<String> {
    let named = named_features(work, specs);
    if named.is_empty() {
        active_before.to_vec()
    } else {
        named
    }
}

/// Adds one turn: `cost` split over `slugs`, one attempt each. A turn with no cost or no feature changes nothing. Returns false then.
pub fn record_turn(work: &str, turn: i64, cost: f64, slugs: &[String]) -> bool {
    if slugs.is_empty() || !cost.is_finite() || cost <= 0.0 {
        return false;
    }
    let path = loop_path(work, STORE);
    let mut store = load_json(&path);
    if !store.is_object() {
        store = json!({});
    }
    let share = cost / slugs.len() as f64;
    for slug in slugs {
        let entry = store.as_object_mut().expect("object").entry(slug.clone()).or_insert_with(|| json!({"cost_usd": 0.0, "attempts": 0, "last_turn": 0}));
        if entry.get("last_turn").and_then(Value::as_i64) == Some(turn) {
            continue; // the same turn is never counted twice
        }
        let total = entry.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0) + share;
        entry["cost_usd"] = json!(round4(total));
        entry["attempts"] = json!(entry.get("attempts").and_then(Value::as_i64).unwrap_or(0) + 1);
        entry["last_turn"] = json!(turn);
    }
    save_json(&path, &store).is_ok()
}

/// `{features: [{feature, cost_usd, attempts, status}], total_usd, left_estimate_usd, closed, open}`. `left_estimate_usd` is `null` until a
/// feature has been closed (nothing to average yet).
pub fn report(work: &str, specs: &str) -> Value {
    let store = load_json(&loop_path(work, STORE));
    let features = parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md"))).0;
    let mut rows = Vec::new();
    let (mut total, mut closed_cost, mut closed, mut open) = (0.0, 0.0, 0usize, 0usize);
    for (slug, fields) in features.iter() {
        let entry = store.get(slug.as_str());
        let cost = entry.and_then(|e| e.get("cost_usd")).and_then(Value::as_f64).unwrap_or(0.0);
        let attempts = entry.and_then(|e| e.get("attempts")).and_then(Value::as_i64).unwrap_or(0);
        total += cost;
        match fields.status() {
            "verified" => {
                closed += 1;
                closed_cost += cost;
            }
            "blocked" => {}
            _ => open += 1,
        }
        if cost > 0.0 || attempts > 0 {
            rows.push(json!({"feature": slug, "cost_usd": cost, "attempts": attempts, "status": fields.status()}));
        }
    }
    let left = (closed > 0).then(|| round4(closed_cost / closed as f64 * open as f64));
    json!({"features": rows, "total_usd": round4(total), "left_estimate_usd": left, "closed": closed, "open": open})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(roadmap: &str) -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let specs = dir.path().join(".specs").join("project");
        std::fs::create_dir_all(&specs).unwrap();
        std::fs::write(specs.join("ROADMAP.md"), roadmap).unwrap();
        let specs = specs.display().to_string();
        (dir, work, specs)
    }

    #[test]
    fn a_turn_splits_its_cost_and_counts_one_attempt_each() {
        let (_d, work, specs) = project("## S\n- [a] status=building kind=back\n- [b] status=building kind=back\n- [c] status=pending kind=back\n");
        let slugs = features_of_turn(&work, &specs, &active_features(&specs));
        assert_eq!(slugs, vec!["a", "b"]);
        assert!(record_turn(&work, 1, 1.0, &slugs));
        assert!(record_turn(&work, 2, 0.5, &["a".to_string()]));
        let report = report(&work, &specs);
        assert_eq!(report["features"][0]["feature"], "a");
        assert_eq!(report["features"][0]["cost_usd"], 1.0);
        assert_eq!(report["features"][0]["attempts"], 2);
        assert_eq!(report["features"][1]["cost_usd"], 0.5);
        assert_eq!(report["total_usd"], 1.5);
        assert_eq!(report["left_estimate_usd"], Value::Null);
    }

    #[test]
    fn the_same_turn_is_never_counted_twice() {
        let (_d, work, specs) = project("## S\n- [a] status=building kind=back\n");
        record_turn(&work, 3, 2.0, &["a".to_string()]);
        record_turn(&work, 3, 2.0, &["a".to_string()]);
        assert_eq!(report(&work, &specs)["features"][0]["attempts"], 1);
    }

    #[test]
    fn the_estimate_is_the_average_closed_feature_times_the_open_ones() {
        let (_d, work, specs) = project("## S\n- [a] status=verified kind=back\n- [b] status=verified kind=back\n- [c] status=building kind=back\n- [d] status=pending kind=back\n- [e] status=blocked kind=back\n");
        record_turn(&work, 1, 2.0, &["a".to_string()]);
        record_turn(&work, 2, 4.0, &["b".to_string()]);
        let report = report(&work, &specs);
        assert_eq!(report["left_estimate_usd"], 6.0);
        assert_eq!((report["closed"].clone(), report["open"].clone()), (json!(2), json!(2)));
    }

    #[test]
    fn nothing_is_recorded_for_a_free_turn_or_no_feature() {
        let (_d, work, _specs) = project("## S\n- [a] status=building kind=back\n");
        assert!(!record_turn(&work, 1, 0.0, &["a".to_string()]));
        assert!(!record_turn(&work, 1, 1.0, &[]));
        assert!(!record_turn(&work, 1, f64::NAN, &["a".to_string()]));
    }

    #[test]
    fn loop_json_names_the_features_of_the_turn() {
        let (_d, work, specs) = project("## S\n- [a] status=building kind=back\n- [b] status=pending kind=back\n");
        let path = state_file(&work, "LOOP.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"status":"continue","feature":"b, ghost"}"#).unwrap();
        assert_eq!(features_of_turn(&work, &specs, &["a".to_string()]), vec!["b"]);
        std::fs::write(&path, r#"{"status":"continue"}"#).unwrap();
        assert_eq!(features_of_turn(&work, &specs, &["a".to_string()]), vec!["a"]);
    }
}
