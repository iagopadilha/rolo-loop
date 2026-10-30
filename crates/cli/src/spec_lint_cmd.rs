// SPDX-License-Identifier: MIT
//! `harness spec-lint <root> <slug>`: the code half of the spec gate (no model). Checks what a program can decide: the spec exists,
//! opens with the metadata block (SCOPE, KIND, PATHS, DEPS) with valid values, and a Medium or larger spec carries requirement IDs.
//! Business rules and ambiguity stay with the `spec-reviewer` agent (`gate=agent|both`). Exit: 0 PASS, 1 FAIL, 2 bad arguments.

use clap::Args as ClapArgs;
use regex::Regex;
use std::path::PathBuf;

#[derive(ClapArgs)]
pub struct Args {
    /// Project root (the loop's worktree)
    root: PathBuf,
    /// The feature slug
    slug: String,
}

const SCOPES: [&str; 4] = ["small", "medium", "large", "complex"];
const KINDS: [&str; 4] = ["back", "front", "both", "generic"];

fn metadata(text: &str, key: &str) -> Option<String> {
    let pattern = Regex::new(&format!(r"(?im)^[\s>*\-]*\**{key}\**\s*[:=]\s*\**\s*(.+?)\s*$")).expect("metadata regex");
    pattern.captures(text).map(|c| c[1].trim_matches(|ch| ch == '*' || ch == '`' || ch == ' ').to_string())
}

/// One line per problem; empty = the spec passes the lint.
pub fn lint(text: &str) -> Vec<String> {
    let mut problems = vec![];
    if text.trim().is_empty() {
        return vec!["spec.md is empty".into()];
    }
    let scope = metadata(text, "SCOPE");
    match &scope {
        None => problems.push("no SCOPE (Small|Medium|Large|Complex) in the metadata block".into()),
        Some(v) if !SCOPES.contains(&v.to_lowercase().split_whitespace().next().unwrap_or("")) => {
            problems.push(format!("SCOPE {v:?} is not Small, Medium, Large or Complex"))
        }
        _ => {}
    }
    match metadata(text, "KIND") {
        None => problems.push("no KIND (back|front|both|generic) in the metadata block".into()),
        Some(v) if !KINDS.contains(&v.to_lowercase().split_whitespace().next().unwrap_or("")) => problems.push(format!("KIND {v:?} is not back, front, both or generic")),
        _ => {}
    }
    match metadata(text, "PATHS") {
        None => problems.push("no PATHS in the metadata block (routing, collisions and the guard's scope depend on it)".into()),
        Some(v) if v.is_empty() => problems.push("PATHS is empty".into()),
        _ => {}
    }
    if metadata(text, "DEPS").is_none() {
        problems.push("no DEPS in the metadata block (write `DEPS: none` when nothing is needed)".into());
    }
    let medium_or_more = scope.as_deref().map(str::to_lowercase).is_some_and(|s| !s.starts_with("small"));
    let requirement_id = Regex::new(r"\b[A-Z][A-Z0-9]{1,9}-\d{1,3}\b").expect("id regex");
    if medium_or_more && !requirement_id.is_match(text) {
        problems.push("a Medium or larger spec needs requirement IDs (for example ORD-1) so the gate and the verifier can trace them".into());
    }
    problems
}

pub fn run(args: Args) -> i32 {
    let path = args.root.join(".specs").join("features").join(&args.slug).join("spec.md");
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("FEATURE: {}\nVERDICT: FAIL\nFAIL:\n- {} does not exist", args.slug, path.display());
        return 1;
    };
    let problems = lint(&text);
    println!("FEATURE: {}\nVERDICT: {}\nFAIL:{}", args.slug, if problems.is_empty() { "PASS" } else { "FAIL" }, if problems.is_empty() { " none" } else { "" });
    for problem in &problems {
        println!("- {problem}");
    }
    i32::from(!problems.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "# Orders\n\nSCOPE: Medium\nKIND: back\nPATHS: src/orders\nDEPS: none\n\n| ID | Requirement |\n| ORD-1 | creates an order |\n";

    #[test]
    fn a_complete_spec_passes() {
        assert!(lint(GOOD).is_empty(), "{:?}", lint(GOOD));
    }

    #[test]
    fn bold_metadata_and_list_markers_are_read() {
        let text = "- **SCOPE:** Large\n- **KIND:** front\n- **PATHS:** `web/orders`\n- **DEPS:** none\nWEB-1 does it\n";
        assert!(lint(text).is_empty(), "{:?}", lint(text));
    }

    #[test]
    fn missing_pieces_are_listed_one_by_one() {
        let problems = lint("# x\nSCOPE: Huge\nKIND: back\n");
        assert!(problems.iter().any(|p| p.contains("SCOPE")));
        assert!(problems.iter().any(|p| p.contains("PATHS")));
        assert!(problems.iter().any(|p| p.contains("DEPS")));
    }

    #[test]
    fn a_small_spec_does_not_need_requirement_ids() {
        assert!(lint("SCOPE: Small\nKIND: generic\nPATHS: docs\nDEPS: none\n").is_empty());
    }

    #[test]
    fn a_medium_spec_without_ids_fails() {
        let problems = lint("SCOPE: Medium\nKIND: back\nPATHS: a\nDEPS: none\n");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("requirement IDs"));
    }
}
