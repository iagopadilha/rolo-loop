// SPDX-License-Identifier: MIT
//! ONE vocabulary for E2E / verification results. The verifier says `PASS` / `FAIL` / `UNVERIFIED`, the orchestrator copies words into the
//! ROADMAP field `e2e=`, and the state check reads that field. The words are mapped here, once:
//!
//! | verifier | roadmap `e2e=` (canonical) | meaning |
//! |---|---|---|
//! | PASS | `ok` | the real-app check passed |
//! | FAIL | `failed` | the check ran and failed |
//! | UNVERIFIED | `pending` | the check did not run or proved nothing: still work left |
//! | (no surface) | `not-covered` | the feature has nothing to test |
//!
//! Aliases (`pass`, `fail`, `unverified`, ...) are accepted and rewritten to the canonical word instead of stopping the loop.

use std::path::Path;

/// The canonical words of the roadmap field.
pub const E2E_WORDS: [&str; 4] = ["ok", "failed", "not-covered", "pending"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum E2eResult {
    Ok,
    Failed,
    NotCovered,
    Pending,
}

impl E2eResult {
    /// Any spelling the verifier or the orchestrator may use, case-insensitive. `None` = not a result word.
    pub fn parse(word: &str) -> Option<E2eResult> {
        match word.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "ok" | "pass" | "passed" | "passing" => Some(E2eResult::Ok),
            "failed" | "fail" | "failure" | "failing" => Some(E2eResult::Failed),
            "not-covered" | "notcovered" | "n/a" | "na" | "none" => Some(E2eResult::NotCovered),
            "pending" | "unverified" | "unknown" | "skipped" => Some(E2eResult::Pending),
            _ => None,
        }
    }

    /// The word written in the ROADMAP.
    pub fn roadmap_word(self) -> &'static str {
        match self {
            E2eResult::Ok => "ok",
            E2eResult::Failed => "failed",
            E2eResult::NotCovered => "not-covered",
            E2eResult::Pending => "pending",
        }
    }

    /// The word of the verifier agents.
    pub fn verifier_word(self) -> &'static str {
        match self {
            E2eResult::Ok => "PASS",
            E2eResult::Failed => "FAIL",
            E2eResult::NotCovered | E2eResult::Pending => "UNVERIFIED",
        }
    }

    /// True when the feature still has E2E work left.
    pub fn is_pending(self) -> bool {
        self == E2eResult::Pending
    }
}

/// Canonical roadmap word of any accepted spelling.
pub fn canonical_e2e(word: &str) -> Option<&'static str> {
    E2eResult::parse(word).map(E2eResult::roadmap_word)
}

/// Rewrites every accepted alias of `e2e=` on the feature lines to its canonical word. Returns the new text and one note per change
/// (`slug: e2e=pass -> e2e=ok`). Unknown words are left alone (the field check reports them).
pub fn normalize_roadmap_e2e(text: &str) -> (String, Vec<String>) {
    let mut notes = Vec::new();
    let mut out = String::with_capacity(text.len());
    for segment in text.split_inclusive('\n') {
        let line = segment.trim_end_matches(['\n', '\r']);
        let tail = &segment[line.len()..];
        if line.trim_start().starts_with("- [") {
            let slug = line.split(']').next().and_then(|s| s.split('[').nth(1)).unwrap_or("").to_string();
            let mut words: Vec<String> = Vec::new();
            let mut in_note = false;
            for token in line.split(' ') {
                if token.starts_with("note=") {
                    in_note = true; // everything after note= is free text
                }
                match token.strip_prefix("e2e=").filter(|_| !in_note) {
                    Some(value) => match canonical_e2e(value) {
                        Some(canonical) if canonical != value => {
                            notes.push(format!("{slug}: e2e={value} -> e2e={canonical}"));
                            words.push(format!("e2e={canonical}"));
                        }
                        _ => words.push(token.to_string()),
                    },
                    None => words.push(token.to_string()),
                }
            }
            out.push_str(&words.join(" "));
        } else {
            out.push_str(line);
        }
        out.push_str(tail);
    }
    (out, notes)
}

/// Repairs `ROADMAP.md` in `specs` (the `.specs/project` folder) in place. Returns the notes; empty when nothing changed.
pub fn repair_roadmap_e2e(specs: &str) -> Vec<String> {
    let path = Path::new(specs).join("ROADMAP.md");
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let (fixed, notes) = normalize_roadmap_e2e(&text);
    if notes.is_empty() || std::fs::write(&path, fixed).is_err() {
        return Vec::new();
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_words_map_both_ways() {
        for (word, roadmap) in [("PASS", "ok"), ("FAIL", "failed"), ("UNVERIFIED", "pending"), ("pass", "ok"), ("fail", "failed"), ("unverified", "pending")] {
            assert_eq!(canonical_e2e(word), Some(roadmap), "{word}");
        }
        assert_eq!(E2eResult::Ok.verifier_word(), "PASS");
        assert_eq!(E2eResult::Failed.verifier_word(), "FAIL");
        assert_eq!(E2eResult::NotCovered.verifier_word(), "UNVERIFIED");
        assert_eq!(canonical_e2e("not_covered"), Some("not-covered"));
        assert_eq!(canonical_e2e("banana"), None);
        for word in E2E_WORDS {
            assert_eq!(canonical_e2e(word), Some(word));
        }
    }

    #[test]
    fn roadmap_aliases_are_rewritten() {
        let text = "## S\n- [a] status=verified e2e=pass\r\n- [b] status=verified e2e=UNVERIFIED note=e2e=pass\n- [c] status=verified e2e=ok\n- [d] e2e=banana\n";
        let (fixed, notes) = normalize_roadmap_e2e(text);
        assert_eq!(fixed, "## S\n- [a] status=verified e2e=ok\r\n- [b] status=verified e2e=pending note=e2e=pass\n- [c] status=verified e2e=ok\n- [d] e2e=banana\n");
        assert_eq!(notes, vec!["a: e2e=pass -> e2e=ok", "b: e2e=UNVERIFIED -> e2e=pending"]);
    }

    #[test]
    fn repair_writes_the_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let specs = dir.path().display().to_string();
        std::fs::write(dir.path().join("ROADMAP.md"), "- [a] status=verified e2e=pass\n").unwrap();
        assert_eq!(repair_roadmap_e2e(&specs).len(), 1);
        assert!(repair_roadmap_e2e(&specs).is_empty());
        assert_eq!(std::fs::read_to_string(dir.path().join("ROADMAP.md")).unwrap(), "- [a] status=verified e2e=ok\n");
    }
}
