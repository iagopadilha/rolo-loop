// SPDX-License-Identifier: MIT
//! Parsing and validation of the `Follow-up` stages of the ROADMAP (pure text rules, no I/O).
//!
//! The stage id is the first word of the heading, so the heading is `## Stage Follow-up-<n> - Follow-up <n>`; `Follow-up <n>` with a space is
//! accepted too. The ROADMAP grammar itself comes from the engine (`parse_roadmap`).

use engine_state::loop_state::parse_roadmap;
use pycompat::{splitlines, Re};
use serde_json::{json, Value};
use std::sync::LazyLock;

static STAGE_HEADING: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^#{1,6}\s*Stage\s+(Follow-up(?:[-\s]+(\d+))?)"));
static ANY_STAGE_HEADING: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^#{1,6}\s*Stage\s+\S+"));
static FEATURE_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^-\s*\[([^\]]+)\]"));
pub const OPEN_FEATURE_STATUSES: [&str; 5] = ["pending", "speccing", "specced", "approved", "building"];

pub fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// The `n` of a `Follow-up <n>` heading line, when it has one.
fn heading_number(line: &str) -> Option<Option<i64>> {
    STAGE_HEADING.match_start(line).map(|h| h.group(2).and_then(|n| n.parse().ok()))
}

/// `[{"id", "n", "features": [{slug, status, title, deps, paths, scope, kind}], "lines": {slug: feature line text}}]` of the `Follow-up` stages, in
/// file order. `lines` lets [`describe_stage_change`] see an edit of any field of a feature line.
pub fn list_follow_up_stages(roadmap_text: &str) -> Vec<Value> {
    let (features, _) = parse_roadmap(roadmap_text);
    let mut stages: Vec<Value> = Vec::new();
    let mut current: Option<usize> = None;
    for raw in splitlines(&normalize_newlines(roadmap_text)) {
        let line = raw.trim();
        if ANY_STAGE_HEADING.match_start(line).is_some() {
            current = None;
            if let Some(h) = STAGE_HEADING.match_start(line) {
                let id = h.group(1).unwrap_or("").replace(' ', "-");
                let n = h.group(2).and_then(|n| n.parse::<i64>().ok());
                stages.push(json!({"id": id, "n": n, "features": [], "lines": {}}));
                current = Some(stages.len() - 1);
            }
            continue;
        }
        let Some(found) = FEATURE_LINE.match_start(line) else { continue };
        let slug = found.group(1).unwrap_or("").to_string();
        if let (Some(index), Some(fields)) = (current, features.get(&slug)) {
            let stage = &mut stages[index];
            stage["features"].as_array_mut().expect("array").push(json!({
                "slug": slug, "status": fields.get("status"), "title": fields.text("title"), "deps": fields.text("deps"), "paths": fields.text("paths"),
                "scope": fields.text("scope"), "kind": fields.text("kind"),
            }));
            stage["lines"][slug.as_str()] = json!(line);
        }
    }
    stages
}

fn errors_count(text: &str) -> usize {
    parse_roadmap(text).1.len()
}

/// Why the two files are NOT "the old text + one appended Follow-up stage (+ appended FLOWS text)"; `None` when they are.
pub fn validate_appended_stage(roadmap_before: &str, roadmap_after: &str, flows_before: &str, flows_after: &str, expected_number: i64) -> Option<String> {
    let (before, after) = (normalize_newlines(roadmap_before).trim_end().to_string(), normalize_newlines(roadmap_after).trim_end().to_string());
    if !after.starts_with(&before) || after == before {
        return Some("an earlier part of the ROADMAP was changed (or nothing was appended); only a new Follow-up stage may be added".into());
    }
    let appended: Vec<String> = splitlines(&after[before.len()..]).into_iter().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    let heading_ok = appended.first().and_then(|l| heading_number(l)).flatten().is_some_and(|n| n == expected_number);
    if !heading_ok {
        return Some(format!("the appended stage must be headed `## Stage Follow-up-{expected_number} - Follow-up {expected_number}`"));
    }
    if appended[1..].iter().any(|l| ANY_STAGE_HEADING.match_start(l).is_some()) {
        return Some("only ONE stage may be appended".into());
    }
    let appended_slugs: Vec<String> = appended.iter().filter_map(|l| FEATURE_LINE.match_start(l).and_then(|f| f.group(1).map(String::from))).collect();
    if appended_slugs.is_empty() {
        return Some("the appended stage has no feature".into());
    }
    let (features_before, _) = parse_roadmap(&before);
    let (features_after, errors_after) = parse_roadmap(&after);
    let unique: std::collections::BTreeSet<&String> = appended_slugs.iter().collect();
    if unique.len() != appended_slugs.len() || appended_slugs.iter().any(|s| features_before.contains(s)) {
        return Some("a new feature reuses a slug that already exists".into());
    }
    if features_after.len() != features_before.len() + appended_slugs.len() {
        return Some("the appended features do not parse as the ROADMAP grammar".into());
    }
    if !errors_after.is_empty() && errors_after.len() > errors_count(&before) {
        return Some(format!("the appended stage breaks the ROADMAP grammar: {}", errors_after.iter().take(3).cloned().collect::<Vec<_>>().join("; ")));
    }
    if !normalize_newlines(flows_after).trim_end().starts_with(normalize_newlines(flows_before).trim_end()) {
        return Some("an earlier part of FLOWS.md was changed; only new entries may be appended".into());
    }
    None
}

/// Index of the first `Follow-up <round_number>` heading line.
fn heading_index(lines: &[String], round_number: i64) -> Option<usize> {
    lines.iter().position(|raw| heading_number(raw.trim()).flatten() == Some(round_number))
}

/// Why the round's edit is NOT "only the Follow-up <n> stage changed (+ appended FLOWS text)"; `None` when it is.
pub fn validate_round_stage(
    baseline_roadmap: &str,
    after_roadmap: &str,
    baseline_flows: &str,
    after_flows: &str,
    round_number: i64,
    executed_slugs: &[String],
) -> Option<String> {
    let before_lines: Vec<String> = normalize_newlines(baseline_roadmap).trim_end().split('\n').map(String::from).collect();
    let after_lines: Vec<String> = normalize_newlines(after_roadmap).trim_end().split('\n').map(String::from).collect();
    let cut = heading_index(&before_lines, round_number);
    let mut frozen: Vec<String> = match cut {
        None => before_lines.clone(),
        Some(c) => before_lines[..c].to_vec(),
    };
    while frozen.last().is_some_and(|l| l.trim().is_empty()) {
        frozen.pop();
    }
    let rstrip = |l: &String| l.trim_end().to_string();
    let head: Vec<String> = after_lines.iter().take(frozen.len()).map(rstrip).collect();
    if head != frozen.iter().map(rstrip).collect::<Vec<_>>() {
        return Some(format!("a line above the Follow-up {round_number} stage was changed; only that stage may be edited"));
    }
    let headings: Vec<String> = after_lines.iter().map(|r| r.trim().to_string()).filter(|r| ANY_STAGE_HEADING.match_start(r).is_some()).collect();
    let own: Vec<&String> = headings.iter().filter(|h| heading_number(h).flatten() == Some(round_number)).collect();
    if own.is_empty() {
        return Some(format!("the Follow-up {round_number} stage is missing"));
    }
    if own.len() > 1 {
        return Some(format!("the Follow-up {round_number} stage appears more than once"));
    }
    if headings.last() != Some(own[0]) {
        return Some(format!("the Follow-up {round_number} stage must be the last stage of the ROADMAP"));
    }
    let stages = list_follow_up_stages(after_roadmap);
    if let Some(stage) = stages.iter().find(|s| s["n"].as_i64() == Some(round_number)) {
        for feature in stage["features"].as_array().into_iter().flatten() {
            let slug = feature["slug"].as_str().unwrap_or("");
            if executed_slugs.iter().any(|e| e == slug) {
                return Some(format!("feature slug `{slug}` of Follow-up {round_number} collides with an already executed feature"));
            }
        }
    }
    if !normalize_newlines(after_flows).trim_end().starts_with(normalize_newlines(baseline_flows).trim_end()) {
        return Some("an earlier part of FLOWS.md was changed; only new entries may be appended".into());
    }
    None
}

/// `{"added", "removed", "edited"}` slug lists between two stages (as built by [`list_follow_up_stages`], or `None`). An optional `lines` key
/// makes the comparison exact on the line text.
pub fn describe_stage_change(before_stage: Option<&Value>, after_stage: Option<&Value>) -> Value {
    fn entries(stage: Option<&Value>) -> Vec<(String, String)> {
        let Some(stage) = stage else { return vec![] };
        if let Some(lines) = stage.get("lines").and_then(Value::as_object) {
            return lines.iter().map(|(s, t)| (s.clone(), t.as_str().map(String::from).unwrap_or_else(|| t.to_string()))).collect();
        }
        stage
            .get("features")
            .and_then(Value::as_array)
            .map(|fs| {
                fs.iter()
                    .map(|f| {
                        let mut pairs: Vec<(String, String)> = f
                            .as_object()
                            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))).collect())
                            .unwrap_or_default();
                        pairs.sort();
                        (f["slug"].as_str().unwrap_or("").to_string(), format!("{pairs:?}"))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    let (before, after) = (entries(before_stage), entries(after_stage));
    let added: Vec<&String> = after.iter().map(|(s, _)| s).filter(|s| !before.iter().any(|(b, _)| b == *s)).collect();
    let removed: Vec<&String> = before.iter().map(|(s, _)| s).filter(|s| !after.iter().any(|(a, _)| a == *s)).collect();
    let edited: Vec<&String> = after.iter().filter(|(s, t)| before.iter().any(|(b, bt)| b == s && bt != t)).map(|(s, _)| s).collect();
    json!({"added": added, "removed": removed, "edited": edited})
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "## Stage 1\n- [a] status=verified\n";

    #[test]
    fn lists_follow_up_stages() {
        let text = format!(
            "{BASE}## Stage Follow-up-1 - Follow-up 1\n- [b] status=pending paths=src/b\n## Stage Follow-up-2 - Follow-up 2\n- [c] status=pending paths=src/c deps=b\n"
        );
        let stages = list_follow_up_stages(&text);
        assert_eq!(stages.len(), 2);
        assert_eq!((stages[0]["id"].as_str(), stages[0]["n"].as_i64(), stages[0]["features"][0]["slug"].as_str()), (Some("Follow-up-1"), Some(1), Some("b")));
        assert_eq!(stages[1]["features"][0]["deps"], "b");
        assert!(stages[1]["lines"]["c"].as_str().unwrap().starts_with("- [c] status=pending"));
    }

    #[test]
    fn an_appended_stage_must_be_the_only_change() {
        let after = format!("{BASE}\n## Stage Follow-up-1 - Follow-up 1\n- [b] status=pending paths=src/b\n");
        assert_eq!(validate_appended_stage(BASE, &after, "", "", 1), None);
        assert!(validate_appended_stage(BASE, &after, "", "", 2).unwrap().starts_with("the appended stage must be headed"));
        assert!(validate_appended_stage(BASE, BASE, "", "", 1).unwrap().starts_with("an earlier part of the ROADMAP was changed"));
        let reuse = format!("{BASE}## Stage Follow-up-1 - Follow-up 1\n- [a] status=pending paths=src/x\n");
        assert_eq!(validate_appended_stage(BASE, &reuse, "", "", 1).as_deref(), Some("a new feature reuses a slug that already exists"));
        assert_eq!(validate_appended_stage(BASE, &after, "keep", "", 1).as_deref(), Some("an earlier part of FLOWS.md was changed; only new entries may be appended"));
    }

    #[test]
    fn a_round_may_only_edit_its_own_stage() {
        let baseline = format!("{BASE}## Stage Follow-up-1 - Follow-up 1\n- [b] status=pending paths=src/b\n");
        let edited = format!("{BASE}## Stage Follow-up-1 - Follow-up 1\n- [b] status=pending paths=src/b2\n- [c] status=pending paths=src/c\n");
        assert_eq!(validate_round_stage(&baseline, &edited, "", "", 1, &[]), None);
        let touched = edited.replace("[a] status=verified", "[a] status=pending");
        assert!(validate_round_stage(&baseline, &touched, "", "", 1, &[]).unwrap().starts_with("a line above the Follow-up 1 stage was changed"));
        assert!(validate_round_stage(&baseline, &edited, "", "", 1, &["c".to_string()]).unwrap().contains("collides"));
        assert_eq!(validate_round_stage(BASE, BASE, "", "", 1, &[]).as_deref(), Some("the Follow-up 1 stage is missing"));
        let before = &list_follow_up_stages(&baseline)[0];
        let after = &list_follow_up_stages(&edited)[0];
        assert_eq!(describe_stage_change(Some(before), Some(after)), json!({"added": ["c"], "removed": [], "edited": ["b"]}));
    }
}
