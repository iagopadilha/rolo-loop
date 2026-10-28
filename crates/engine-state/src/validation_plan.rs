// SPDX-License-Identifier: MIT
//! The validation plan of a feature as a versioned file, and the review of its quality (backlog VA3, VA6).
//!
//! The plan lives next to the spec: `.specs/features/<slug>/VALIDATION.md`. One case per bullet, fields separated by `|`:
//!
//! ```text
//! - login-ok: a registered user signs in | covers: AUTH-01, AUTH-02 | call: POST /session | expect: 201 and a session cookie | needs: test user
//! ```
//!
//! * VA3: the plan is hashed every turn ([`record_plans`]); a different hash is a new version, kept with its turn in
//!   `loop/json/VALIDATION-PLANS.json`, so the panel shows a change in the plan as a change ([`plan_report`]).
//! * VA6: [`review`] lists the requirement ids of the spec that no case covers, and the cases that cannot prove anything (no expected result,
//!   or an expected result that is only "200" or "opens").
//!
//! Pure file reads and one JSON write; no AI, no network. A requirement id is a token such as `AUTH-01` (capitals, dash, digits).

use crate::loop_io::{load_json, read, save_json};
use crate::loop_state::{feature_dir, loop_path, parse_roadmap};
use pycompat::Re;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::LazyLock;

const PLAN_FILE: &str = "VALIDATION.md";
const STORE: &str = "VALIDATION-PLANS.json";
const MAX_HISTORY: usize = 20;

static REQUIREMENT: LazyLock<Re> = LazyLock::new(|| Re::new(r"\b[A-Z][A-Z0-9]*-\d+\b"));
/// An expected result that proves nothing by itself.
static WEAK_EXPECT: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^\W*(200|ok|200 ok|success|opens?|loads?|renders?|works?|no error)\W*\z"));

/// One case of the plan.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Case {
    pub id: String,
    pub what: String,
    pub covers: Vec<String>,
    pub call: String,
    pub expect: String,
    pub needs: String,
}

/// Parses the plan text. Lines that are not bullets with an `id: text` head are ignored.
pub fn parse_plan(text: &str) -> Vec<Case> {
    let mut cases = Vec::new();
    for line in text.lines() {
        let Some(body) = line.trim().strip_prefix("- ").or_else(|| line.trim().strip_prefix("* ")) else { continue };
        let mut parts = body.split('|');
        let Some((id, what)) = parts.next().and_then(|head| head.split_once(':')) else { continue };
        let id = id.trim().trim_matches('`');
        if id.is_empty() || id.contains(' ') {
            continue;
        }
        let mut case = Case { id: id.to_string(), what: what.trim().to_string(), ..Case::default() };
        for part in parts {
            let Some((key, value)) = part.split_once(':') else { continue };
            let value = value.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "covers" => case.covers = value.split(',').map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect(),
                "call" => case.call = value.to_string(),
                "expect" => case.expect = value.to_string(),
                "needs" => case.needs = value.to_string(),
                _ => {}
            }
        }
        cases.push(case);
    }
    cases
}

/// sha256 (hex, first 16 chars) of the plan without blank lines and trailing spaces, so a reformat is not a change.
pub fn plan_hash(text: &str) -> String {
    let normal: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    let digest = Sha256::digest(normal.join("\n").as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn plan_text(work: &str, slug: &str) -> String {
    read(&Path::new(&feature_dir(work, slug)).join(PLAN_FILE))
}

fn slugs_of(specs: &str) -> Vec<String> {
    parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md"))).0.slugs()
}

/// Remembers the version of every feature's plan. A new hash is appended with the turn that first saw it. Returns the slugs whose plan changed
/// (not the first sight of a plan). Never fails into the caller.
pub fn record_plans(work: &str, specs: &str, turn: i64) -> Vec<String> {
    let path = loop_path(work, STORE);
    let mut store = load_json(&path);
    let mut changed = Vec::new();
    let mut dirty = false;
    if !store.is_object() {
        store = json!({});
    }
    for slug in slugs_of(specs) {
        let text = plan_text(work, &slug);
        let entry = store.as_object_mut().expect("object").entry(slug.clone()).or_insert_with(|| json!({"history": []}));
        let history = entry.get_mut("history").and_then(Value::as_array_mut);
        let Some(history) = history else { continue };
        let last = history.last().and_then(|h| h.get("hash")).and_then(Value::as_str).unwrap_or("").to_string();
        let hash = if text.trim().is_empty() { String::new() } else { plan_hash(&text) };
        if hash == last || (hash.is_empty() && history.is_empty()) {
            continue;
        }
        if !history.is_empty() {
            changed.push(slug.clone());
        }
        history.push(json!({"hash": hash, "turn": turn}));
        if history.len() > MAX_HISTORY {
            history.remove(0);
        }
        dirty = true;
    }
    if dirty {
        let _ = save_json(&path, &store);
    }
    changed
}

/// `[{feature, cases, version, changed_in_turns}]` for the features that have a plan (or had one). `version` counts the distinct plans seen.
pub fn plan_report(work: &str, specs: &str) -> Vec<Value> {
    let store = load_json(&loop_path(work, STORE));
    let mut out = Vec::new();
    for slug in slugs_of(specs) {
        let text = plan_text(work, &slug);
        let history: Vec<Value> = store.get(&slug).and_then(|e| e.get("history")).and_then(Value::as_array).cloned().unwrap_or_default();
        if text.trim().is_empty() && history.is_empty() {
            continue;
        }
        let turns: Vec<Value> = history.iter().skip(1).filter_map(|h| h.get("turn").cloned()).collect();
        let current = if text.trim().is_empty() { String::new() } else { plan_hash(&text) };
        let recorded = history.last().and_then(|h| h.get("hash")).and_then(Value::as_str).unwrap_or("");
        out.push(json!({
            "feature": slug, "cases": parse_plan(&text).len(), "version": history.len().max(1),
            "changed_in_turns": turns,
            "changed_since_last_turn": !current.is_empty() && !recorded.is_empty() && current != recorded,
            "removed": text.trim().is_empty(),
        }));
    }
    out
}

/// The ids of requirements named in the spec, in order of first appearance.
pub fn spec_requirements(spec_text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for m in REQUIREMENT.find_all(spec_text) {
        let id = m.group(0).unwrap_or("").to_string();
        if !id.is_empty() && !found.contains(&id) {
            found.push(id);
        }
    }
    found
}

/// A case that cannot prove its requirement, and why.
fn weakness(case: &Case) -> Option<&'static str> {
    if case.expect.trim().is_empty() {
        Some("no expected result")
    } else if WEAK_EXPECT.match_start(case.expect.trim()).is_some() {
        Some("the expected result is only a status or \"opens\": say what must be in the answer or on the screen")
    } else if case.covers.is_empty() {
        Some("covers no requirement")
    } else {
        None
    }
}

/// `[{feature, requirements_without_case, weak_cases: [{case, problem}]}]` for the features that have a spec with requirement ids or a plan.
/// A feature with a spec but no plan lists every requirement as uncovered.
pub fn review(work: &str, specs: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for slug in slugs_of(specs) {
        let dir = feature_dir(work, &slug);
        let requirements = spec_requirements(&read(&Path::new(&dir).join("spec.md")));
        let cases = parse_plan(&plan_text(work, &slug));
        if requirements.is_empty() && cases.is_empty() {
            continue;
        }
        let covered: Vec<&String> = cases.iter().flat_map(|c| c.covers.iter()).collect();
        let uncovered: Vec<&String> = requirements.iter().filter(|r| !covered.contains(r)).collect();
        let weak: Vec<Value> = cases.iter().filter_map(|c| weakness(c).map(|problem| json!({"case": c.id, "problem": problem}))).collect();
        if uncovered.is_empty() && weak.is_empty() {
            continue;
        }
        out.push(json!({"feature": slug, "requirements_without_case": uncovered, "weak_cases": weak}));
    }
    out
}

/// Lists the plan's data needs (`needs:`) of every feature, without repeats, so test data is asked for once.
pub fn needs_of(work: &str, specs: &str) -> Map<String, Value> {
    let mut map = Map::new();
    for slug in slugs_of(specs) {
        let needs: Vec<String> = parse_plan(&plan_text(work, &slug)).into_iter().map(|c| c.needs).filter(|n| !n.is_empty()).collect();
        if !needs.is_empty() {
            let mut unique: Vec<String> = Vec::new();
            for n in needs {
                if !unique.contains(&n) {
                    unique.push(n);
                }
            }
            map.insert(slug, json!(unique));
        }
    }
    map
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

    fn write_feature(work: &str, slug: &str, file: &str, text: &str) {
        let dir = Path::new(&feature_dir(work, slug)).to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), text).unwrap();
    }

    const PLAN: &str = "# Plan\n- login-ok: a user signs in | covers: AUTH-01, AUTH-02 | call: POST /session | expect: 201 and a session cookie | needs: test user\n- page-opens: the page | covers: AUTH-03 | expect: 200\nnot a case\n";

    #[test]
    fn parses_cases_and_fields() {
        let cases = parse_plan(PLAN);
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].id, "login-ok");
        assert_eq!(cases[0].covers, vec!["AUTH-01", "AUTH-02"]);
        assert_eq!(cases[0].call, "POST /session");
        assert_eq!(cases[0].needs, "test user");
    }

    #[test]
    fn a_reformat_keeps_the_hash_and_a_change_does_not() {
        assert_eq!(plan_hash("- a: x\n\n- b: y  \n"), plan_hash("- a: x\n- b: y\n"));
        assert_ne!(plan_hash("- a: x\n"), plan_hash("- a: z\n"));
    }

    #[test]
    fn a_changed_plan_is_recorded_as_a_new_version() {
        let (_d, work, specs) = project("## S\n- [f] status=building kind=back\n");
        write_feature(&work, "f", PLAN_FILE, PLAN);
        assert!(record_plans(&work, &specs, 1).is_empty(), "first sight is not a change");
        assert!(record_plans(&work, &specs, 2).is_empty());
        write_feature(&work, "f", PLAN_FILE, &format!("{PLAN}- extra: more | covers: AUTH-04 | expect: error body names the field\n"));
        assert_eq!(record_plans(&work, &specs, 3), vec!["f".to_string()]);
        let report = plan_report(&work, &specs);
        assert_eq!(report[0]["version"], 2);
        assert_eq!(report[0]["changed_in_turns"], json!([3]));
        assert_eq!(report[0]["cases"], 3);
        assert_eq!(report[0]["changed_since_last_turn"], false);
    }

    #[test]
    fn a_plan_edited_after_the_last_record_shows_as_pending_change() {
        let (_d, work, specs) = project("## S\n- [f] status=building kind=back\n");
        write_feature(&work, "f", PLAN_FILE, PLAN);
        record_plans(&work, &specs, 1);
        write_feature(&work, "f", PLAN_FILE, "- only: one | covers: AUTH-01 | expect: a body\n");
        assert_eq!(plan_report(&work, &specs)[0]["changed_since_last_turn"], true);
    }

    #[test]
    fn review_lists_requirements_without_a_case_and_weak_cases() {
        let (_d, work, specs) = project("## S\n- [f] status=building kind=back\n- [g] status=building kind=back\n");
        write_feature(&work, "f", "spec.md", "AUTH-01 sign in\nAUTH-02 sign out\nAUTH-03 page\nAUTH-09 lock\n");
        write_feature(&work, "f", PLAN_FILE, PLAN);
        write_feature(&work, "g", "spec.md", "no ids here");
        let found = review(&work, &specs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["feature"], "f");
        assert_eq!(found[0]["requirements_without_case"], json!(["AUTH-09"]));
        assert_eq!(found[0]["weak_cases"][0]["case"], "page-opens");
    }

    #[test]
    fn a_spec_without_a_plan_has_every_requirement_uncovered() {
        let (_d, work, specs) = project("## S\n- [f] status=building kind=back\n");
        write_feature(&work, "f", "spec.md", "FEAT-1 and FEAT-2");
        assert_eq!(review(&work, &specs)[0]["requirements_without_case"], json!(["FEAT-1", "FEAT-2"]));
    }

    #[test]
    fn needs_are_listed_once_per_feature() {
        let (_d, work, specs) = project("## S\n- [f] status=building kind=back\n");
        write_feature(&work, "f", PLAN_FILE, "- a: x | needs: test user\n- b: y | needs: test user\n- c: z | needs: seeded order\n");
        assert_eq!(needs_of(&work, &specs)["f"], json!(["test user", "seeded order"]));
    }
}
