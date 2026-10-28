// SPDX-License-Identifier: MIT
//! Findings of the validation, sorted by what to do with them, and the intended behaviour that decides a fix (backlog Z2, Z4).
//!
//! A feature may keep `.specs/features/<slug>/FINDINGS.md`, one finding per bullet, fields separated by `|`:
//!
//! ```text
//! - fix-now: the picker lists a company whose lookups fail | wrong: lookup
//! - decide: should archived items show in the list?
//! - manual: confirm the e-mail arrives in a real inbox
//! ```
//!
//! * Z2: `fix-now` (clear expected result, low risk) is fixed by the loop in the same run; `decide` (a product choice) and `manual` (needs a
//!   person) are only reported. A class the engine does not know is read as `decide`, the safe side.
//! * Z4: a `fix-now` is accepted only when the screen's intended behaviour is written (`BEHAVIOUR.md` beside the spec: who operates it,
//!   which data it must reach) AND the finding says which side is wrong (`wrong:` the list, the lookup, the screen, the API...). Without
//!   them the finding becomes `decide` with a question, so a fix is never written in the wrong direction.

use crate::loop_io::read;
use crate::loop_state::{feature_dir, parse_roadmap};
use serde_json::{json, Value};
use std::path::Path;

const FINDINGS_FILE: &str = "FINDINGS.md";
const BEHAVIOUR_FILE: &str = "BEHAVIOUR.md";
/// The least words of a `BEHAVIOUR.md` that count as a written intent.
const MIN_BEHAVIOUR_WORDS: usize = 5;
/// `wrong:` values that say nothing.
const NO_SIDE: [&str; 5] = ["", "unknown", "?", "unsure", "n/a"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    FixNow,
    Decide,
    Manual,
}

impl Class {
    pub fn word(self) -> &'static str {
        match self {
            Class::FixNow => "fix-now",
            Class::Decide => "decide",
            Class::Manual => "manual",
        }
    }
    fn parse(word: &str) -> Option<Class> {
        match word.trim().to_ascii_lowercase().replace([' ', '_'], "-").as_str() {
            "fix-now" | "fix" => Some(Class::FixNow),
            "decide" | "decision" => Some(Class::Decide),
            "manual" => Some(Class::Manual),
            _ => None,
        }
    }
}

/// One line of FINDINGS.md as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub declared: String,
    pub text: String,
    pub wrong: String,
}

pub fn parse_findings(text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(body) = line.trim().strip_prefix("- ").or_else(|| line.trim().strip_prefix("* ")) else { continue };
        let mut parts = body.split('|');
        let Some((class, what)) = parts.next().and_then(|head| head.split_once(':')) else { continue };
        let mut finding = Finding { declared: class.trim().to_ascii_lowercase(), text: what.trim().to_string(), wrong: String::new() };
        for part in parts {
            if let Some((key, value)) = part.split_once(':') {
                if key.trim().eq_ignore_ascii_case("wrong") {
                    finding.wrong = value.trim().to_string();
                }
            }
        }
        if !finding.text.is_empty() {
            out.push(finding);
        }
    }
    out
}

/// True when `BEHAVIOUR.md` of the feature says enough to judge a screen against it.
pub fn behaviour_written(work: &str, slug: &str) -> bool {
    read(&Path::new(&feature_dir(work, slug)).join(BEHAVIOUR_FILE)).split_whitespace().count() >= MIN_BEHAVIOUR_WORDS
}

/// The class the engine acts on, and the question to ask when it moved a `fix-now` to `decide`.
pub fn effective(finding: &Finding, behaviour: bool) -> (Class, Option<String>) {
    let declared = Class::parse(&finding.declared).unwrap_or(Class::Decide);
    if declared != Class::FixNow {
        return (declared, None);
    }
    if !behaviour {
        return (
            Class::Decide,
            Some("The intended behaviour of this screen is not written. Which side is wrong: the list that shows the item, or the lookup that fails? Write BEHAVIOUR.md (who operates it, which data it must reach) or answer here.".into()),
        );
    }
    if NO_SIDE.contains(&finding.wrong.to_ascii_lowercase().as_str()) {
        return (
            Class::Decide,
            Some("The finding does not say which side is wrong (`wrong:`). Say whether the list or the lookup must change before a fix is written.".into()),
        );
    }
    (Class::FixNow, None)
}

/// `[{feature, class, declared, text, wrong, question}]` for every finding of every feature, in ROADMAP order.
pub fn report(work: &str, specs: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for slug in parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md"))).0.slugs() {
        let findings = parse_findings(&read(&Path::new(&feature_dir(work, &slug)).join(FINDINGS_FILE)));
        if findings.is_empty() {
            continue;
        }
        let behaviour = behaviour_written(work, &slug);
        for finding in findings {
            let (class, question) = effective(&finding, behaviour);
            out.push(json!({
                "feature": slug, "class": class.word(), "declared": finding.declared, "text": finding.text,
                "wrong": finding.wrong, "question": question,
            }));
        }
    }
    out
}

/// Features with a screen or an API (anything but `kind=generic`) past the spec step that have no written intended behaviour.
pub fn behaviour_missing(work: &str, specs: &str) -> Vec<String> {
    parse_roadmap(&read(&Path::new(specs).join("ROADMAP.md")))
        .0
        .iter()
        .filter(|(_, f)| f.text("kind") != "generic" && ["specced", "approved", "building", "verified"].contains(&f.status()))
        .map(|(s, _)| s.clone())
        .filter(|s| !behaviour_written(work, s))
        .collect()
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

    #[test]
    fn parses_class_text_and_side() {
        let list = parse_findings("# x\n- fix-now: the picker lists a bad item | wrong: lookup\n- decide: show archived?\nplain line\n- bad line no colon\n");
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].declared.as_str(), list[0].wrong.as_str()), ("fix-now", "lookup"));
        assert_eq!(list[1].text, "show archived?");
    }

    #[test]
    fn fix_now_needs_the_written_behaviour_and_the_wrong_side() {
        let with_side = Finding { declared: "fix-now".into(), text: "t".into(), wrong: "lookup".into() };
        assert_eq!(effective(&with_side, true), (Class::FixNow, None));
        let (class, question) = effective(&with_side, false);
        assert_eq!(class, Class::Decide);
        assert!(question.unwrap().contains("BEHAVIOUR.md"));
        let no_side = Finding { wrong: "unknown".into(), ..with_side };
        let (class, question) = effective(&no_side, true);
        assert_eq!(class, Class::Decide);
        assert!(question.unwrap().contains("which side is wrong"));
    }

    #[test]
    fn other_classes_pass_through_and_an_unknown_one_is_a_decision() {
        let make = |declared: &str| Finding { declared: declared.into(), text: "t".into(), wrong: String::new() };
        assert_eq!(effective(&make("manual"), false), (Class::Manual, None));
        assert_eq!(effective(&make("decide"), true), (Class::Decide, None));
        assert_eq!(effective(&make("whatever"), true), (Class::Decide, None));
    }

    #[test]
    fn the_report_reads_each_features_file() {
        let (_d, work, specs) = project("## S\n- [a] status=verified kind=front\n- [b] status=verified kind=back\n");
        write_feature(&work, "a", FINDINGS_FILE, "- fix-now: list shows a company whose lookups fail | wrong: list\n- manual: check a real inbox\n");
        write_feature(&work, "a", BEHAVIOUR_FILE, "The operator sees only companies of their own domain in the picker.");
        write_feature(&work, "b", FINDINGS_FILE, "- fix-now: wrong total | wrong: api\n");
        let rows = report(&work, &specs);
        let class = |feature: &str, index: usize| rows.iter().filter(|r| r["feature"] == feature).nth(index).unwrap()["class"].clone();
        assert_eq!(rows.len(), 3);
        assert_eq!((class("a", 0), class("a", 1), class("b", 0)), (json!("fix-now"), json!("manual"), json!("decide")));
        assert!(rows[2]["question"].as_str().unwrap().contains("not written"));
    }

    #[test]
    fn features_with_a_surface_and_no_intent_are_listed() {
        let (_d, work, specs) =
            project("## S\n- [a] status=building kind=front\n- [b] status=building kind=generic\n- [c] status=pending kind=back\n- [d] status=verified kind=back\n");
        write_feature(&work, "d", BEHAVIOUR_FILE, "Operators of one domain read and edit its orders only.");
        assert_eq!(behaviour_missing(&work, &specs), vec!["a"]);
    }
}
