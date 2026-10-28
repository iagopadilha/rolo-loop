// SPDX-License-Identifier: MIT
//! One view of everything the validation produced, for the panel (`validation_report` in `GET state`).
//!
//! Joins the pieces that each own their rules: cause groups (VA2), plan versions (VA3), cost per feature (VA5), the review of the checks
//! (VA6), the findings by class and the missing intended behaviour (Z2, Z4) and the test data left behind (Z3).

use crate::{e2e_evidence, env_cleanup, feature_cost, findings, validation_groups, validation_plan};
use serde_json::{json, Value};

pub fn build(work: &str, specs: &str) -> Value {
    let rows = e2e_evidence::validations(work, specs);
    json!({
        "groups": validation_groups::group_by_cause(work, specs, &rows),
        "plans": validation_plan::plan_report(work, specs),
        "review": validation_plan::review(work, specs),
        "cost": feature_cost::report(work, specs),
        "findings": findings::report(work, specs),
        "behaviour_missing": findings::behaviour_missing(work, specs),
        "cleanup": env_cleanup::report(work),
    })
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
    fn an_empty_project_gives_every_section_empty() {
        let (_dir, work, specs) = project("## S\n- [a] status=pending kind=back\n");
        let report = build(&work, &specs);
        for key in ["groups", "plans", "review", "findings", "behaviour_missing"] {
            assert_eq!(report[key], json!([]), "{key}");
        }
        assert_eq!(report["cost"]["total_usd"], 0.0);
        assert_eq!(report["cleanup"]["open"], 0);
    }

    #[test]
    fn the_sections_follow_the_files_of_the_project() {
        let (_dir, work, specs) = project("## S\n- [a] status=verified kind=back e2e=pending\n");
        env_cleanup::record(&work, "test order", "delete it", "a");
        let report = build(&work, &specs);
        assert_eq!(report["groups"][0]["kind"], "pending");
        assert_eq!(report["behaviour_missing"], json!(["a"]));
        assert_eq!(report["cleanup"]["open"], 1);
    }
}
