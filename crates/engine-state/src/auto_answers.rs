// SPDX-License-Identifier: MIT
//! Automatic answers of the headless loop: a question that does not need the user is answered by the script with the recommended
//! option, and a spec stuck on wording at the gate gets a tiebreak.
//!
//! It stops (and leaves the question to the user) when the text talks about authorization, production, something irreversible,
//! push/merge/deploy, a secret, a credential, billing or payment, when the only options abandon the feature, and when the automatic
//! answer limits are reached.

use crate::decisions;
use crate::loop_io::{load_json, now_iso, read, save_json};
use crate::loop_state::{parse_roadmap, state_file};
use pycompat::Re;
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

/// LOOP.json reason of a critical decision stop; the panel and the notice key on it.
pub const CRITICAL_DECISION_PREFIX: &str = "critical decision: ";

static HARD_STOP: LazyLock<Re> = LazyLock::new(|| {
    Re::ci(r"authoriz|production|\bprod\b|irreversib|delete|\bdrop\b|truncate|\bpush\b|\bmerge\b|deploy|secret|password|credential|billing|payment|access|business rules?|discrepan|inconsisten|real data")
});
static ABANDON: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\bend\b|finish without|stop|skip|give up|abandon|accept .{0,40}blocked|do not do|cancel"));
static RECOMMENDED: LazyLock<Re> = LazyLock::new(|| Re::ci(r"recommended"));
static NO_TIEBREAK: LazyLock<Re> = LazyLock::new(|| Re::ci(r"business|discrep|inconsisten|access|credential|secret|password"));
static SPACES: LazyLock<Re> = LazyLock::new(|| Re::new(r"\s+"));

/// The options the headless loop was started with that matter here.
#[derive(Clone, Debug)]
pub struct AutoOpts {
    pub auto_answer: bool,
    pub max_auto_answers: i64,
    pub auto_answers_per_feature: i64,
    pub gate_tiebreaks: i64,
}

/// Counters of automatic answers of one run.
#[derive(Clone, Debug, Default)]
pub struct AutoState {
    pub total: i64,
    pub per_feature: Vec<(String, i64)>,
    pub tiebreaks: Vec<(String, i64)>,
}

fn counter(list: &[(String, i64)], key: &str) -> i64 {
    list.iter().find(|(k, _)| k == key).map(|(_, v)| *v).unwrap_or(0)
}

fn bump(list: &mut Vec<(String, i64)>, key: &str) {
    match list.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 += 1,
        None => list.push((key.to_string(), 1)),
    }
}

/// `str(value or "")`.
fn text_of(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(false)) => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, true),
    }
}

fn clip(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// Records the user's answer in ANSWERS.md. The orchestrator's next turn reads it first and applies it.
/// `Err(message)` is what the script prints before exiting with 1.
pub fn write_answer(work: &str, text: &str) -> Result<String, String> {
    let state = load_json(&state_file(work, "LOOP.json"));
    if state.get("status").and_then(Value::as_str) != Some("awaiting_user") {
        let current = text_of(state.get("status"));
        return Err(format!("--answer only applies when the loop stopped at awaiting_user (current status: {})", if current.is_empty() { "no LOOP.json".to_string() } else { current }));
    }
    let path = state_file(work, "ANSWERS.md");
    let block = format!("\n## Answer {} [pending]\nQuestion: {}\nUser answer: {}\n", now_iso(), clip(text_of(state.get("reason")).trim(), 1500), text.trim());
    let _ = std::fs::create_dir_all(path.parent().expect("has parent"));
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(block.as_bytes());
    }
    Ok(format!("[loop] answer recorded in {} (the next turn applies it and carries on)", path.display()))
}

/// Picks the answer to a question that does not need the user: `(number, text, why)` or `None` when it must stop.
pub fn pick_auto_answer(state: &Value) -> Option<(usize, String, &'static str)> {
    let options: Vec<String> = state.get("options").and_then(Value::as_array).map(|a| a.iter().map(|o| text_of(Some(o))).collect()).unwrap_or_default();
    let reason = text_of(state.get("reason"));
    if options.len() < 2 || reason.starts_with(CRITICAL_DECISION_PREFIX) {
        return None; // a critical decision needs the user: it is never answered by the script
    }
    if HARD_STOP.search(&format!("{reason} {}", options.join(" "))).is_some() {
        return None;
    }
    for (index, option) in options.iter().enumerate() {
        if RECOMMENDED.search(option).is_some() && ABANDON.search(option).is_none() {
            return Some((index + 1, option.clone(), "recommended"));
        }
    }
    for (index, option) in options.iter().enumerate() {
        if ABANDON.search(option).is_none() {
            return Some((index + 1, option.clone(), "first option that does not abandon the feature"));
        }
    }
    None
}

fn meta_list<'a>(meta: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
    let map = meta.as_object_mut().expect("meta is an object");
    let slot = map.entry(key.to_string()).or_insert_with(|| Value::Array(vec![]));
    if !slot.is_array() {
        *slot = Value::Array(vec![]);
    }
    slot.as_array_mut().expect("array")
}

/// Answers the pending LOOP.json question on its own, when it can. `true` = answered (the loop carries on).
pub fn try_auto_answer(work: &str, turn: i64, opts: &AutoOpts, meta: &mut Value, auto_state: &mut AutoState) -> bool {
    if !opts.auto_answer {
        return false;
    }
    let state = load_json(&state_file(work, "LOOP.json"));
    if state.get("status").and_then(Value::as_str) != Some("awaiting_user") {
        return false;
    }
    let Some((number, text, why)) = pick_auto_answer(&state) else { return false };
    let key = { let f = text_of(state.get("feature")); if f.is_empty() { "general".to_string() } else { f } };
    let used_here = counter(&auto_state.per_feature, &key);
    if auto_state.total >= opts.max_auto_answers || used_here >= opts.auto_answers_per_feature {
        println!("[loop] automatic answer limit reached ({} in total, {used_here} on {key}): stopping for you to decide", auto_state.total);
        return false;
    }
    let question = SPACES.replace_lit(&text_of(state.get("reason")), " ").trim().to_string();
    let _ = write_answer(work, &format!("{text} [automatic answer from the loop: option {number}, {why}]"));
    auto_state.total += 1;
    bump(&mut auto_state.per_feature, &key);
    meta_list(meta, "auto_answers").push(json!({"turn": turn, "feature": key, "option": number}));
    decisions::append(
        work,
        &key,
        Some(turn),
        "important",
        &format!("question answered automatically: \"{}\"", clip(&question, 160)),
        &format!("option {number} ({why}): {}", clip(&text, 140)),
        "--answer with another option",
    );
    println!("[loop] warning: question answered automatically (option {number}, {why}); noted in .specs/project/loop/VALIDATE.md");
    true
}

/// Features quarantined because they failed the gate (note=gate-...): only the spec is on paper, nothing was implemented yet.
pub fn gate_blocked_features(specs: &str) -> Vec<String> {
    parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md"))).0.iter().filter(|(_, f)| f.status() == "blocked" && f.text("note").contains("gate") && NO_TIEBREAK.search(f.text("note")).is_none()).map(|(s, _)| s.clone()).collect()
}

/// Gate tiebreak: after the gate rounds the script tells the orchestrator to APPROVE the spec, treat the remaining findings as notes
/// for the executor and move on to execution. Once per feature (`gate_tiebreaks`). `true` = answered (the loop carries on).
pub fn try_gate_tiebreak(work: &str, turn: i64, opts: &AutoOpts, meta: &mut Value, auto_state: &mut AutoState) -> bool {
    if !opts.auto_answer {
        return false;
    }
    let specs = Path::new(work).join(".specs").join("project").display().to_string();
    let stuck: Vec<String> = gate_blocked_features(&specs).into_iter().filter(|s| counter(&auto_state.tiebreaks, s) < opts.gate_tiebreaks).collect();
    if stuck.is_empty() {
        return false;
    }
    let slugs = stuck.join(", ");
    let mut state = load_json(&state_file(work, "LOOP.json"));
    if let Value::Object(map) = &mut state {
        map.insert("status".into(), json!("awaiting_user"));
        map.insert("reason".into(), json!(format!("Gate tiebreak: {slugs} failed the gate several times on spec wording points.")));
        map.insert("options".into(), json!(["approve the spec and run it, remaining findings become notes for the executor", "finish without the feature"]));
    }
    let _ = save_json(&state_file(work, "LOOP.json"), &state);
    let _ = write_answer(
        work,
        &format!(
            "Gate tiebreak (automatic loop decision): after several rejections, APPROVE the spec of {slugs} (status=approved) without redoing the gate. \
             Treat the remaining findings as mandatory notes for the executor; a case that depends on data, a secret or a write that \
             TEST-ENVIRONMENT.md does not cover becomes not-covered in regression.md. Record it in VALIDATE.md and move on to execution with \
             baseline and regression: build and regression decide, not the spec wording."
        ),
    );
    for slug in &stuck {
        bump(&mut auto_state.tiebreaks, slug);
    }
    meta_list(meta, "gate_tiebreaks").push(json!({"turn": turn, "features": stuck}));
    decisions::append(work, &slugs, Some(turn), "important", "gate tiebreak: spec approved without a new gate after several rejections", "run with the remaining findings as notes; build and regression decide", "--answer asking to redo the gate");
    println!("[loop] warning: gate tiebreak for {slugs} (spec approved, findings become notes); noted in .specs/project/loop/VALIDATE.md");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> AutoOpts {
        AutoOpts { auto_answer: true, max_auto_answers: 5, auto_answers_per_feature: 2, gate_tiebreaks: 1 }
    }

    fn setup(state: Value) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        save_json(&state_file(&work, "LOOP.json"), &state).unwrap();
        (dir, work)
    }

    #[test]
    fn picks_recommended_then_first_non_abandoning() {
        let pick = |reason: &str, options: Value| pick_auto_answer(&json!({"reason": reason, "options": options}));
        assert_eq!(pick("Naming?", json!(["a", "b (Recommended)"])).map(|p| p.0), Some(2));
        assert_eq!(pick("Naming?", json!(["stop here", "b"])).map(|p| (p.0, p.2)), Some((2, "first option that does not abandon the feature")));
        assert!(pick("Deploy to prod?", json!(["a", "b"])).is_none());
        assert!(pick("critical decision: x", json!(["a", "b"])).is_none());
        assert!(pick("x", json!(["only"])).is_none());
        assert!(pick("x", json!(["skip", "cancel"])).is_none());
    }

    #[test]
    fn write_answer_needs_awaiting_user() {
        let (_d, work) = setup(json!({"status": "running"}));
        assert_eq!(write_answer(&work, "x").unwrap_err(), "--answer only applies when the loop stopped at awaiting_user (current status: running)");
        let (_d2, work2) = setup(json!({"status": "awaiting_user", "reason": " Which? "}));
        assert!(write_answer(&work2, " go ").unwrap().starts_with("[loop] answer recorded in "));
        let text = std::fs::read_to_string(state_file(&work2, "ANSWERS.md")).unwrap();
        assert!(text.contains(" [pending]\nQuestion: Which?\nUser answer: go\n"));
    }

    #[test]
    fn auto_answer_counts_and_logs() {
        let (_d, work) = setup(json!({"status": "awaiting_user", "reason": "Naming?", "options": ["a", "b"], "feature": "f1"}));
        let (mut meta, mut st) = (json!({}), AutoState::default());
        assert!(try_auto_answer(&work, 4, &opts(), &mut meta, &mut st));
        assert_eq!((st.total, counter(&st.per_feature, "f1")), (1, 1));
        assert_eq!(meta["auto_answers"], json!([{"turn": 4, "feature": "f1", "option": 1}]));
        assert!(decisions::read_file(&work).contains("- [f1] turn=4 level=important question answered automatically: \"Naming?\" | chose: option 1 (first option that does not abandon the feature): a | revert: --answer with another option"));
        let off = AutoOpts { auto_answer: false, ..opts() };
        assert!(!try_auto_answer(&work, 4, &off, &mut meta, &mut st));
    }

    #[test]
    fn gate_tiebreak_once_per_feature() {
        let (_d, work) = setup(json!({"status": "done"}));
        let road = Path::new(&work).join(".specs").join("project");
        std::fs::create_dir_all(&road).unwrap();
        std::fs::write(road.join("ROADMAP.md"), "## Stage 1\n- [a] status=blocked note=gate-failed\n- [b] status=blocked note=gate-access\n").unwrap();
        let (mut meta, mut st) = (json!({}), AutoState::default());
        assert!(try_gate_tiebreak(&work, 2, &opts(), &mut meta, &mut st));
        assert_eq!(meta["gate_tiebreaks"], json!([{"turn": 2, "features": ["a"]}]));
        let state = load_json(&state_file(&work, "LOOP.json"));
        assert_eq!(state["status"], "awaiting_user");
        assert!(!try_gate_tiebreak(&work, 3, &opts(), &mut meta, &mut st));
    }
}
