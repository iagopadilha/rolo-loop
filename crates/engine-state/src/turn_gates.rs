// SPDX-License-Identifier: MIT
//! Checks and pauses the loop applies between turns: scheduler flags, forgotten dependencies, quarantine and critical-decision pauses,
//! spend cap, E2E marks and the contract merge of verified features.

use crate::auto_answers::CRITICAL_DECISION_PREFIX;
use crate::loop_io::{git, read, save_json, save_meta};
use crate::loop_state::{feature_folder_name, loop_path, next_work, parse_roadmap, state_file, Fields, JSON_FOLDER_NAME, LOOP_FOLDER_NAME};
use crate::result_vocabulary::E2eResult;
use crate::system_map::{self, SYSTEM_DOCUMENT};
use crate::worktree_state::roadmap_score;
use crate::{audit_deps, decisions, subprojects};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;

pub const COST_CAP_REASON_PREFIX: &str = "cost cap reached: US$ ";
pub const COST_WARNING_RATIO: f64 = 0.8;

fn text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => pycompat::json::py_json_dumps(other, true),
    }
}

fn roadmap(dir: &str) -> String {
    read(&Path::new(dir).join("ROADMAP.md"))
}

// ---------------------------------------------------------------- one turn

/// Deterministic scheduler: the script computes the turn's lanes (`next_work`); the orchestrator executes them. A lane is ONE feature at
/// ITS OWN step (`--lanes slug:step,...`). With no readable ROADMAP or nothing to dispatch, returns empty.
pub fn plan_flags(work: &str, wave: i64, primary_name: &str) -> String {
    let (features, _) = parse_roadmap(&read(&Path::new(work).join(".specs").join("project").join("ROADMAP.md")));
    if features.is_empty() {
        return String::new();
    }
    let plan = next_work(&features, wave, true, primary_name);
    let list = |key: &str| -> Vec<String> { plan.get(key).and_then(Value::as_array).map(|a| a.iter().map(|v| text(Some(v))).collect()).unwrap_or_default() };
    let mut flags = format!(" --phase {}", text(plan.get("phase")));
    let lanes = plan.get("lanes").and_then(Value::as_array).cloned().unwrap_or_default();
    if !lanes.is_empty() {
        flags.push_str(&format!(" --lanes {}", lanes.iter().map(|l| format!("{}:{}", text(l.get("slug")), text(l.get("step")))).collect::<Vec<_>>().join(",")));
        flags.push_str(&format!(" --ready {}", list("slugs").join(",")));
    }
    let stuck = list("stuck_by_blocked");
    if !stuck.is_empty() {
        flags.push_str(&format!(" --stuck {}", stuck.join(",")));
    }
    flags
}

/// Before choosing the wave: writes forgotten `deps=` into the ROADMAP (audit --apply). It only increases serialization. Cycle or
/// invalid ROADMAP: nothing is written. Returns `["feature deps+=other", ...]`.
pub fn audit_before_turn(work: &str) -> Vec<String> {
    let (_, out) = audit_deps::run_cli(&[work.to_string(), "--apply".to_string()]);
    out.lines().filter_map(|l| l.strip_prefix("APPLIED: ").map(String::from)).collect()
}

/// With --e2e, `done` requires the final phase result on the line of EACH verified feature: e2e=ok, or e2e=not-covered when there is
/// no surface to test. `e2e=failed` with `done` is a contradiction. Returns the list of problems.
/// `e2e=pending` (the verifier's UNVERIFIED) is the same as no mark: work is left.
pub fn e2e_unmarked(specs: &str) -> Vec<String> {
    let features = parse_roadmap(&roadmap(specs)).0;
    let mut problems = Vec::new();
    for (slug, f) in features.iter() {
        let result = f.get("e2e").and_then(E2eResult::parse);
        if f.status() == "verified" && result.is_none_or(E2eResult::is_pending) {
            problems.push(if f.has("e2e") { format!("{slug} with e2e=pending (unverified)") } else { format!("{slug} without e2e=") });
        } else if result == Some(E2eResult::Failed) && f.status() != "blocked" {
            problems.push(format!("{slug} with e2e=failed"));
        }
    }
    problems
}

/// Verified features still without a final E2E result (`e2e=` absent or pending), in ROADMAP order. These are work left even when every
/// feature is verified.
pub fn e2e_pending(specs: &str) -> Vec<String> {
    let features = parse_roadmap(&roadmap(specs)).0;
    features
        .iter()
        .filter(|(_, f)| f.status() == "verified" && f.get("e2e").and_then(E2eResult::parse).is_none_or(E2eResult::is_pending))
        .map(|(slug, _)| slug.clone())
        .collect()
}

/// `(checked, total)` of the final E2E phase over the verified features: "E2E 3 of 8 done".
pub fn e2e_progress(specs: &str) -> (usize, usize) {
    let features = parse_roadmap(&roadmap(specs)).0;
    let verified: Vec<_> = features.iter().filter(|(_, f)| f.status() == "verified").collect();
    let pending = verified.iter().filter(|(_, f)| f.get("e2e").and_then(E2eResult::parse).is_none_or(E2eResult::is_pending)).count();
    (verified.len() - pending, verified.len())
}

/// True for a back-end (kind back/both) feature whose paths fall in a subproject of a confirmed monorepo and that has no contract.md.
pub fn missing_backend_contract(work: &str, slug: &str, feature: &Fields) -> bool {
    let kind = feature.text("kind").to_lowercase();
    if kind != "back" && kind != "both" {
        return false;
    }
    if system_map::contract_path(work, slug).is_file() {
        return false;
    }
    feature.text("paths").split(',').any(|path| {
        let path = path.trim();
        !path.is_empty() && subprojects::owner_of(work, path, None).is_some_and(|owner| owner.get("skip") != Some(&Value::Bool(true)))
    })
}

pub fn append_validate_line(specs: &str, line: &str) {
    let path = Path::new(specs).join(LOOP_FOLDER_NAME).join("VALIDATE.md");
    let current = read(&path);
    let _ = std::fs::create_dir_all(path.parent().expect("has parent"));
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{}{line}", if current.is_empty() || current.ends_with('\n') { "" } else { "\n" });
    }
}

fn state_turn(state: &Value) -> Option<i64> {
    state.get("turn").and_then(Value::as_i64)
}

/// Contract merge hook. Only for a CONFIRMED monorepo; otherwise nothing at all.
///
/// Right after a turn is validated, for every feature the ROADMAP now shows as `verified` that was not handled yet, one feature at a
/// time in ROADMAP order. The engine merges between turns, records `contract_merge: {slug: {changed, warnings}}` in LOOP.json and
/// leaves SYSTEM.md for the NEXT checkpoint commit. When no next turn will happen the engine commits SYSTEM.md itself.
pub fn merge_verified_contracts(work: &str, meta: &mut Value, state: &mut Value, specs: &str) {
    if !subprojects::monorepo_confirmed(work, None) {
        return;
    }
    let (features, _) = parse_roadmap(&read(&Path::new(work).join(".specs").join("project").join("ROADMAP.md")));
    let mut handled: Vec<String> =
        meta.get("contract_merged_slugs").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let mut recorded = serde_json::Map::new();
    for (slug, feature) in features.iter() {
        if feature.status() != "verified" || handled.contains(slug) {
            continue;
        }
        let result = system_map::merge_contract_deltas(work, std::slice::from_ref(slug));
        handled.push(slug.clone());
        let mut warnings = result.warnings.clone();
        if missing_backend_contract(work, slug, feature) {
            let warning = format!("back-end feature verified without .specs/features/{}/contract.md: SYSTEM.md was not updated", feature_folder_name(slug));
            warnings.push(warning.clone());
            println!("[loop] WARNING (contract merge, {slug}): {warning}");
            decisions::append(
                work,
                slug,
                state_turn(state),
                "important",
                &format!("WARNING contract: {warning}"),
                "went on without the contract delta",
                "write the contract.md (routes, env, events) and merge it into SYSTEM.md",
            );
        }
        if result.changed {
            if let Some(m) = meta.as_object_mut() {
                m.insert("system_md_uncommitted".into(), json!(true));
            }
        }
        for warning in &result.warnings {
            println!("[loop] WARNING (contract merge, {slug}): {warning}");
            append_validate_line(specs, &format!("WARNING contract merge ({slug}): {warning}"));
            if warning.contains(system_map::NO_GRAMMAR_MARK) {
                decisions::append(
                    work,
                    slug,
                    state_turn(state),
                    "important",
                    &format!("WARNING contract: {warning}"),
                    "went on without merging the contract delta",
                    "rewrite contract.md as lines `- [add|change|remove] route|dto|auth|env <identity> — <purpose> (<file>)` (no table) and merge it into SYSTEM.md",
                );
            }
        }
        recorded.insert(slug.clone(), json!({"changed": result.changed, "warnings": warnings}));
    }
    if let Some(m) = meta.as_object_mut() {
        m.insert("contract_merged_slugs".into(), json!(handled));
    }
    if recorded.is_empty() {
        return;
    }
    if let Some(s) = state.as_object_mut() {
        s.insert("contract_merge".into(), Value::Object(recorded));
    }
    let _ = save_json(&state_file(work, "LOOP.json"), state);
    let (_, total, done) = roadmap_score(work);
    if crate::notify_truthy(meta.get("system_md_uncommitted")) && (state.get("status").and_then(Value::as_str) == Some("done") || (total != 0 && done == total)) {
        let (mut code, _, mut err) = git(work, &["add", "--", SYSTEM_DOCUMENT]);
        if code == 0 {
            (code, _, err) = git(work, &["commit", "-m", "chore(loop): merge feature contracts into docs/codebase/SYSTEM.md", "--no-verify"]);
        }
        if code == 0 {
            if let Some(m) = meta.as_object_mut() {
                m.insert("system_md_uncommitted".into(), json!(false));
            }
        } else {
            let chars: Vec<char> = err.chars().collect();
            println!("[loop] WARNING: SYSTEM.md contract merge could not be committed: {}", chars[chars.len().saturating_sub(200)..].iter().collect::<String>());
        }
    }
    let _ = save_meta(&loop_path(work, "meta.json"), meta);
}

/// A feature that just entered quarantine (2 failures) PAUSES the loop and notifies the user, instead of carrying on alone. Returns the
/// pause text (and rewrites LOOP.json as awaiting_user, so --answer works) or "" if there is nothing to pause. Only acts on a turn the
/// orchestrator left as `continue`.
pub fn pause_on_quarantine(specs: &str, prev: Option<&serde_json::Map<String, Value>>, state: &mut Value, continue_on_blocked: bool) -> String {
    let Some(prev) = prev else { return String::new() };
    if continue_on_blocked || state.get("status").and_then(Value::as_str) != Some("continue") {
        return String::new();
    }
    let old = parse_roadmap(&roadmap(prev.get("dir").and_then(Value::as_str).unwrap_or(""))).0;
    let new = parse_roadmap(&roadmap(specs)).0;
    let fresh: Vec<String> =
        new.iter().filter(|(slug, f)| f.status() == "blocked" && old.get(slug).map(|o| o.status()) != Some("blocked")).map(|(s, _)| s.clone()).collect();
    if fresh.is_empty() {
        return String::new();
    }
    let why = text(state.get("reason")).trim().to_string();
    let joined = fresh.join(", ");
    let mut reason =
        format!("Quarantine after 2 failures: {joined}. The loop paused for you to decide (try again with guidance from you, or skip and carry on with the others).");
    if !why.is_empty() {
        reason.push_str(&format!(" Orchestrator detail: {}", why.chars().take(600).collect::<String>()));
    }
    if let Some(s) = state.as_object_mut() {
        s.insert("status".into(), json!("awaiting_user"));
        s.insert("reason".into(), json!(reason));
        s.insert("options".into(), json!(["try the feature again", "skip the feature and carry on with the others"]));
        s.insert("next".into(), json!(format!("apply the user's decision about: {joined}")));
    }
    let _ = save_json(&Path::new(specs).join(LOOP_FOLDER_NAME).join(JSON_FOLDER_NAME).join("LOOP.json"), state);
    reason
}

// ---------------------------------------------------------------- critical decisions

/// A turn that wrote a `level=critical` line in VALIDATE.md (after line `mark`) stops the loop like an open question: LOOP.json becomes
/// awaiting_user with the decision as the question. `important` lines never stop it. A turn that ended in `error` stays an error.
/// Returns the reason (and rewrites LOOP.json) or "" when there is nothing to stop for.
pub fn pause_on_critical_decision(work: &str, state: &mut Value, mark: i64) -> String {
    if state.get("status").and_then(Value::as_str) == Some("error") {
        return String::new();
    }
    let found = decisions::new_critical(work, mark);
    let Some(first) = found.first() else { return String::new() };
    let mut reason = format!("{CRITICAL_DECISION_PREFIX}{} | chose: {} | revert: {}", text(first.get("doubt")), text(first.get("chose")), text(first.get("revert")));
    if found.len() > 1 {
        reason.push_str(&format!(" | {} more critical decision(s) in VALIDATE.md", found.len() - 1));
    }
    if let Some(s) = state.as_object_mut() {
        s.insert("status".into(), json!("awaiting_user"));
        s.insert("reason".into(), json!(reason));
        s.insert("feature".into(), first.get("slug").cloned().unwrap_or(Value::Null));
        s.insert("options".into(), json!(["keep as decided", "change it: <write how>"]));
        s.insert("next".into(), json!("apply the user's answer about the critical decision, then continue"));
    }
    let _ = save_json(&state_file(work, "LOOP.json"), state);
    reason
}

// ---------------------------------------------------------------- spend cap

/// Cap of US$ of this run: the one raised from the panel (meta max_cost_usd) wins over --max-cost; 0 = no cap.
pub fn effective_cost_cap(meta: &Value, max_cost: f64) -> f64 {
    let raised = match meta.get("max_cost_usd") {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0),
        Some(Value::Bool(true)) => 1.0,
        _ => 0.0,
    };
    if raised > 0.0 {
        raised
    } else {
        max_cost
    }
}

pub fn cost_cap_reason(cost: f64, cap: f64) -> String {
    format!("{COST_CAP_REASON_PREFIX}{cost:.2} of {cap:.2}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_io::load_json;

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
    fn cap_and_reason() {
        assert_eq!(effective_cost_cap(&json!({"max_cost_usd": 7.5}), 5.0), 7.5);
        assert_eq!(effective_cost_cap(&json!({"max_cost_usd": "x"}), 5.0), 5.0);
        assert_eq!(effective_cost_cap(&json!({}), 0.0), 0.0);
        assert_eq!(cost_cap_reason(5.0, 5.0), "cost cap reached: US$ 5.00 of 5.00");
    }

    #[test]
    fn e2e_marks() {
        let (_d, _w, specs) =
            project("## Stage 1\n- [a] status=verified\n- [b] status=verified e2e=ok\n- [c] status=building e2e=failed\n- [d] status=blocked e2e=failed\n");
        assert_eq!(e2e_unmarked(&specs), vec!["a without e2e=", "c with e2e=failed"]);
    }

    #[test]
    fn e2e_words_of_the_verifier_count_as_results() {
        let (_d, _w, specs) = project(
            "## S\n- [a] status=verified e2e=pass\n- [b] status=verified e2e=unverified\n- [c] status=verified e2e=not-covered\n- [d] status=verified\n- [e] status=blocked e2e=fail\n",
        );
        assert_eq!(e2e_unmarked(&specs), vec!["b with e2e=pending (unverified)", "d without e2e="]);
        assert_eq!(e2e_pending(&specs), vec!["b", "d"]);
        assert_eq!(e2e_progress(&specs), (2, 4));
    }

    #[test]
    fn plan_flags_lists_lanes() {
        let (_d, work, _s) = project("## Stage 1\n- [a] status=pending\n- [b] status=pending\n");
        let flags = plan_flags(&work, 1, "");
        assert!(flags.starts_with(" --phase ") && flags.contains(" --lanes ") && flags.contains(" --ready "), "{flags}");
        let (_d2, work2, _s2) = project("");
        assert_eq!(plan_flags(&work2, 1, ""), "");
    }

    #[test]
    fn critical_decision_pauses_unless_error() {
        let (_d, work, _s) = project("## Stage 1\n- [a] status=building\n");
        decisions::append(&work, "a", Some(2), "critical", "which DB?", "pg", "migrate");
        decisions::append(&work, "a", Some(2), "critical", "which cloud?", "aws", "move");
        let mut state = json!({"status": "continue"});
        let reason = pause_on_critical_decision(&work, &mut state, 0);
        assert_eq!(reason, "critical decision: which DB? | chose: pg | revert: migrate | 1 more critical decision(s) in VALIDATE.md");
        assert_eq!((state["status"].as_str(), state["feature"].as_str()), (Some("awaiting_user"), Some("a")));
        assert_eq!(load_json(&state_file(&work, "LOOP.json"))["status"], "awaiting_user");
        assert_eq!(pause_on_critical_decision(&work, &mut json!({"status": "error"}), 0), "");
        assert_eq!(pause_on_critical_decision(&work, &mut json!({"status": "continue"}), 99), "");
    }

    #[test]
    fn quarantine_pause() {
        let (dir, work, specs) = project("## Stage 1\n- [a] status=blocked\n- [b] status=building\n");
        let prev_dir = dir.path().join("prev");
        std::fs::create_dir_all(&prev_dir).unwrap();
        std::fs::write(prev_dir.join("ROADMAP.md"), "## Stage 1\n- [a] status=building\n- [b] status=building\n").unwrap();
        let prev = json!({"dir": prev_dir.display().to_string()});
        let mut state = json!({"status": "continue", "reason": " detail "});
        let reason = pause_on_quarantine(&specs, prev.as_object(), &mut state, false);
        assert!(reason.starts_with("Quarantine after 2 failures: a. The loop paused") && reason.ends_with("Orchestrator detail: detail"));
        assert_eq!(state["status"], "awaiting_user");
        assert_eq!(pause_on_quarantine(&specs, prev.as_object(), &mut json!({"status": "continue"}), true), "");
        assert_eq!(pause_on_quarantine(&specs, None, &mut json!({"status": "continue"}), false), "");
        assert!(!work.is_empty());
    }

    #[test]
    fn contract_merge_is_off_without_a_confirmed_monorepo() {
        let (_d, work, specs) = project("## Stage 1\n- [a] status=verified kind=back paths=api\n");
        let (mut meta, mut state) = (json!({}), json!({"status": "continue"}));
        merge_verified_contracts(&work, &mut meta, &mut state, &specs);
        assert_eq!(meta, json!({}));
        assert!(state.get("contract_merge").is_none());
    }
}
