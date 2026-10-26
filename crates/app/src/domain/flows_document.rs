// SPDX-License-Identifier: MIT
//! `.specs/project/FLOWS.md`: one `## [slug] Title` section per ROADMAP feature with the numbered steps of the flow that proves the feature
//! works (the verifier-flow checks the `Expected:` steps).
//!
//! Parse rule: section heading `^##\s*\[(slug)\]\s*(title)?`; steps = lines matching `^\s*\d+[.)]\s+\S` inside the section. A feature "has a flow"
//! when its section exists with at least [`MIN_FLOW_STEPS`] steps. HTML comments (`<!-- ... -->`, where the template keeps its example) are ignored.

use pycompat::{splitlines, Re};
use serde_json::{json, Value};
use std::sync::LazyLock;

pub const MIN_FLOW_STEPS: usize = 2;
static FLOW_HEADING: LazyLock<Re> = LazyLock::new(|| Re::new(r"^##\s*\[([^\]]+)\]\s*(.*?)\s*\z"));
static OTHER_HEADING: LazyLock<Re> = LazyLock::new(|| Re::new(r"^#{1,6}\s"));
static FLOW_STEP: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*\d+[.)]\s+\S"));
static HTML_COMMENT: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?s)<!--.*?-->"));

/// `[(slug, title, steps)]` in file order (a repeated slug adds its steps to the first section).
pub fn parse_flows(text: &str) -> Vec<(String, String, Vec<String>)> {
    let mut flows: Vec<(String, String, Vec<String>)> = Vec::new();
    let mut current: Option<usize> = None;
    for raw in splitlines(&HTML_COMMENT.replace_lit(text, "")) {
        if let Some(h) = FLOW_HEADING.match_start(raw.trim()) {
            let slug = h.group(1).unwrap_or("").trim().to_string();
            let index = match flows.iter().position(|(s, _, _)| *s == slug) {
                Some(i) => i,
                None => {
                    flows.push((slug, h.group(2).unwrap_or("").trim().to_string(), vec![]));
                    flows.len() - 1
                }
            };
            current = Some(index);
            continue;
        }
        if OTHER_HEADING.match_start(raw.trim()).is_some() {
            current = None;
            continue;
        }
        if let Some(i) = current {
            if FLOW_STEP.match_start(raw).is_some() {
                flows[i].2.push(raw.trim().to_string());
            }
        }
    }
    flows
}

/// `{"missing": [ROADMAP slugs without a flow of >= MIN_FLOW_STEPS steps], "orphans": [FLOWS slugs not in the ROADMAP]}`.
pub fn check_flow_coverage(feature_slugs: &[String], flows_text: &str) -> Value {
    let flows = parse_flows(flows_text);
    let missing: Vec<&String> = feature_slugs.iter().filter(|s| flows.iter().find(|(f, _, _)| f == *s).map(|(_, _, steps)| steps.len()).unwrap_or(0) < MIN_FLOW_STEPS).collect();
    let orphans: Vec<&String> = flows.iter().map(|(s, _, _)| s).filter(|s| !feature_slugs.contains(s)).collect();
    json!({"missing": missing, "orphans": orphans})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flows_and_coverage() {
        let text = "<!-- ## [ghost] Example\n1. x\n2. y -->\n## [a] Login\n1. open\n2. sign in\n3) done\nnot a step\n## [b] Empty\n1. only one\n# Other\n1. ignored\n## [a] again\n4. more\n";
        let flows = parse_flows(text);
        assert_eq!(flows.iter().map(|(s, t, n)| (s.as_str(), t.as_str(), n.len())).collect::<Vec<_>>(), vec![("a", "Login", 4), ("b", "Empty", 1)]);
        let slugs = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(check_flow_coverage(&slugs, text), json!({"missing": ["b", "c"], "orphans": []}));
        assert_eq!(check_flow_coverage(&["a".to_string()], text)["orphans"], json!(["b"]));
    }
}
