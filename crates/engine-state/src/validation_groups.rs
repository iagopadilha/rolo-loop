// SPDX-License-Identifier: MIT
//! Groups the validations that did not pass by their cause (backlog VA2).
//!
//! Several results depend on the same few causes (a blocked call, a missing credential, a check that was never run), but they are listed
//! feature by feature, so one problem looks like many. This module reads the rows of [`crate::e2e_evidence::validations`] and returns one
//! group per cause with the features it holds and one suggested fix. Pure: it reads the ROADMAP, the feature `E2E.md` files and the
//! project credential NAMES (never a value).

use crate::credential_check;
use crate::e2e_evidence::{FeatureInput, SKIP_LAYER_OPTION};
use crate::loop_io::read;
use crate::loop_state::parse_roadmap;
use serde_json::{json, Value};
use std::path::Path;

/// Statuses that need no action: nothing to group.
const SETTLED: [&str; 3] = ["ok", "not-covered", "skipped-by-user"];

#[derive(Default)]
struct Group {
    key: String,
    kind: &'static str,
    cause: String,
    suggestion: String,
    features: Vec<String>,
}

fn add(groups: &mut Vec<Group>, key: String, kind: &'static str, cause: String, suggestion: String, slug: &str) {
    match groups.iter_mut().find(|g| g.key == key) {
        Some(group) => {
            if !group.features.iter().any(|f| f == slug) {
                group.features.push(slug.to_string());
            }
        }
        None => groups.push(Group { key, kind, cause, suggestion, features: vec![slug.to_string()] }),
    }
}

/// `[{kind, cause, count, features, suggestion}]`, biggest group first. `rows` are the validations rows; `work` the loop worktree and
/// `specs` its `.specs/project` folder. A feature appears in one group only (the first cause that applies).
pub fn group_by_cause(work: &str, specs: &str, rows: &[Value]) -> Vec<Value> {
    let features = parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md"))).0;
    let mut groups: Vec<Group> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for row in rows {
        let status = row.get("status").and_then(Value::as_str).unwrap_or("");
        let slug = row.get("feature").and_then(Value::as_str).unwrap_or("");
        if slug.is_empty() || SETTLED.contains(&status) || seen.iter().any(|s| s == slug) {
            continue;
        }
        seen.push(slug.to_string());
        let Some(fields) = features.get(slug) else { continue };
        let input = FeatureInput::load(work, slug, fields);
        let blocked = input.blocked_call();
        if !blocked.is_empty() {
            add(
                &mut groups,
                format!("blocked:{blocked}"),
                "blocked_call",
                format!("the guard refused this call: {blocked}"),
                format!("Release that call with one permission rule, or choose \"{SKIP_LAYER_OPTION}\" for the group."),
                slug,
            );
            continue;
        }
        let values = credential_check::read_project_values(work);
        let missing: Vec<String> = input.credentials().into_iter().filter(|n| !values.get(n).is_some_and(|v| !v.trim().is_empty())).collect();
        if !missing.is_empty() && status != "ok" {
            let names = missing.join(", ");
            add(
                &mut groups,
                format!("credential:{names}"),
                "credential",
                format!("the test environment lacks {names} (names only)"),
                "Add the value to the project test environment file once; every feature in the group can then run.".to_string(),
                slug,
            );
            continue;
        }
        let (kind, cause, suggestion) = match status {
            "failed" => ("failed", "the check ran and failed", "Open the evidence of each feature; fix the code, not the check."),
            "unproven" => ("unproven", "marked ok but no evidence file backs it", "Run the check again and list its result file as evidence."),
            "rejected" => ("rejected", "marked not-covered without proof that the feature has no surface", "Give a stated reason or run the check."),
            _ => ("pending", "the check was not run yet", "Run the pending checks; nothing else is wrong."),
        };
        add(&mut groups, format!("status:{status}"), kind, cause.to_string(), suggestion.to_string(), slug);
    }
    let mut out: Vec<Value> =
        groups.into_iter().map(|g| json!({"kind": g.kind, "cause": g.cause, "count": g.features.len(), "features": g.features, "suggestion": g.suggestion})).collect();
    out.sort_by_key(|g| std::cmp::Reverse(g["count"].as_u64().unwrap_or(0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2e_evidence::validations;

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
    fn features_blocked_by_the_same_call_become_one_group() {
        let (_dir, work, specs) = project(
            "## S\n- [a] status=verified kind=back blocked-call=curl_with_url_in_variable\n- [b] status=verified kind=back blocked-call=curl_with_url_in_variable\n- [c] status=verified kind=back e2e=pending\n- [d] status=verified kind=back e2e=ok evidence=x.log\n",
        );
        std::fs::write(Path::new(&work).join("x.log"), "PASS").unwrap();
        let groups = group_by_cause(&work, &specs, &validations(&work, &specs));
        assert_eq!(groups.len(), 2, "{groups:?}");
        assert_eq!(groups[0]["kind"], "blocked_call");
        assert_eq!(groups[0]["count"], 2);
        assert_eq!(groups[0]["features"], json!(["a", "b"]));
        assert!(groups[0]["cause"].as_str().unwrap().contains("curl with url in variable"));
        assert_eq!(groups[1]["kind"], "pending");
    }

    #[test]
    fn a_missing_credential_groups_by_names_and_settled_rows_are_left_out() {
        let (_dir, work, specs) = project(
            "## S\n- [a] status=verified kind=back needs-credential=TEST_USER e2e=pending\n- [b] status=verified kind=front needs-credential=TEST_USER e2e=failed\n- [c] status=verified kind=generic paths=docs/guide e2e=not-covered no-surface=docs_only_change\n",
        );
        let groups = group_by_cause(&work, &specs, &validations(&work, &specs));
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(groups[0]["kind"], "credential");
        assert_eq!(groups[0]["count"], 2);
        assert!(!groups[0]["cause"].as_str().unwrap().contains('='));
    }

    #[test]
    fn nothing_to_group_gives_an_empty_list() {
        let (_dir, work, specs) = project("## S\n- [a] status=building kind=back\n");
        assert!(group_by_cause(&work, &specs, &validations(&work, &specs)).is_empty());
    }
}
