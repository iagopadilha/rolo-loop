// SPDX-License-Identifier: MIT
//! A cheap necessary condition for a pattern: literal texts of which every match must hold at least one.
//!
//! Compiling a pattern costs far more than scanning a short text for a word. The guard starts once per tool call and holds a hundred patterns, almost
//! all of which do not match the command at hand; with this condition they are never compiled. The literals are only ever used to say "this text
//! cannot match", so a pattern whose literals cannot be proved gets no prefilter and is compiled as before.

use regex_syntax::hir::{Hir, HirKind};

const MAX_LITERALS: usize = 64;
const MIN_LITERAL_CHARS: usize = 2;

/// Removes `(?=..)`, `(?!..)`, `(?<=..)` and `(?<!..)` groups. A look-around only restricts where a match may be, so the pattern without it
/// matches at least everything the original does, and what is necessary for the wider pattern is necessary for the original.
fn strip_look_arounds(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len());
    let mut index = 0;
    let mut in_class = false;
    while index < chars.len() {
        let c = chars[index];
        if c == '\\' {
            out.push(c);
            if let Some(next) = chars.get(index + 1) {
                out.push(*next);
            }
            index += 2;
            continue;
        }
        if in_class {
            if c == ']' {
                in_class = false;
            }
            out.push(c);
            index += 1;
            continue;
        }
        if c == '[' {
            in_class = true;
            out.push(c);
            index += 1;
            // a `]` right after `[` or `[^` is a literal member of the class
            if chars.get(index) == Some(&'^') {
                out.push('^');
                index += 1;
            }
            if chars.get(index) == Some(&']') {
                out.push(']');
                index += 1;
            }
            continue;
        }
        let rest: String = chars[index..chars.len().min(index + 4)].iter().collect();
        let opener = ["(?<!", "(?<=", "(?=", "(?!"].iter().find(|o| rest.starts_with(**o));
        if let Some(opener) = opener {
            let mut depth = 1;
            let mut cursor = index + opener.chars().count();
            let mut class = false;
            while cursor < chars.len() && depth > 0 {
                match chars[cursor] {
                    '\\' => cursor += 1,
                    '[' if !class => class = true,
                    ']' if class => class = false,
                    '(' if !class => depth += 1,
                    ')' if !class => depth -= 1,
                    _ => {}
                }
                cursor += 1;
            }
            index = cursor;
            continue;
        }
        out.push(c);
        index += 1;
    }
    out
}

/// The literals of which every match holds at least one (as a substring): `None` when the node can match without any fixed text. A concatenation
/// needs only one of its parts, so the part with the longest shortest literal is taken; an alternation needs every branch to have its own.
fn required(hir: &Hir) -> Option<Vec<Vec<u8>>> {
    match hir.kind() {
        HirKind::Literal(literal) => Some(vec![literal.0.to_vec()]),
        HirKind::Capture(capture) => required(&capture.sub),
        HirKind::Repetition(repetition) if repetition.min >= 1 => required(&repetition.sub),
        HirKind::Concat(parts) => parts.iter().filter_map(required).max_by_key(|set| set.iter().map(Vec::len).min().unwrap_or(0)),
        HirKind::Alternation(branches) => {
            let mut all: Vec<Vec<u8>> = vec![];
            for branch in branches {
                all.extend(required(branch)?);
            }
            Some(all)
        }
        _ => None,
    }
}

/// The literals one of which every match must hold, as written (not case-folded), or `None` when none can be proved.
pub fn required_prefixes(pattern: &str, case_insensitive: bool) -> Option<Vec<String>> {
    let stripped = strip_look_arounds(pattern);
    let hir = regex_syntax::ParserBuilder::new().build().parse(&stripped).ok()?;
    let literals = required(&hir)?;
    if literals.is_empty() || literals.len() > MAX_LITERALS {
        return None;
    }
    let mut out: Vec<String> = Vec::with_capacity(literals.len());
    for literal in literals {
        let text = String::from_utf8(literal).ok()?;
        if text.chars().count() < MIN_LITERAL_CHARS {
            return None;
        }
        // folding a non-ASCII letter is not a simple lower-casing: no proof, no prefilter
        if case_insensitive && !text.is_ascii() {
            return None;
        }
        out.push(if case_insensitive { text.to_ascii_lowercase() } else { text });
    }
    out.sort();
    out.dedup();
    Some(out)
}

/// The prefilter of one pattern.
pub struct Prefilter {
    literals: Vec<String>,
    case_insensitive: bool,
}

impl Prefilter {
    pub fn new(pattern: &str, case_insensitive: bool) -> Option<Prefilter> {
        required_prefixes(pattern, case_insensitive).map(|literals| Prefilter { literals, case_insensitive })
    }

    /// False only when the text certainly holds no match. A case-insensitive pattern is judged only on ASCII text (the rest may fold in ways
    /// plain lower-casing does not follow), so anything else is "possible".
    pub fn possible(&self, text: &str) -> bool {
        if self.case_insensitive {
            if !text.is_ascii() {
                return true;
            }
            let lower = text.to_ascii_lowercase();
            return self.literals.iter().any(|l| lower.contains(l.as_str()));
        }
        self.literals.iter().any(|l| text.contains(l.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_come_from_the_parts_every_match_needs() {
        assert_eq!(required_prefixes(r"\bgit(?:\.exe)?\s+push\b", true), Some(vec!["push".to_string()]));
        assert_eq!(required_prefixes(r"foo|bar\d", false), Some(vec!["bar".to_string(), "foo".to_string()]));
        assert_eq!(required_prefixes(r"[\w.-]+:\d+/p/[^/]+/api/", false), Some(vec!["/api/".to_string()]));
        assert_eq!(required_prefixes(r"(?:^|[\s|(])(?:rg|ripgrep)(?:\.exe)?\s", true), Some(vec!["rg".to_string(), "ripgrep".to_string()]));
    }

    #[test]
    fn look_arounds_are_removed_before_extraction() {
        assert_eq!(strip_look_arounds(r"(?<![\w.-])(?:production|dev)(?![\w-])"), "(?:production|dev)");
        assert_eq!(required_prefixes(r"(?<![\w.-])(?:production|developer)(?![\w-])", true), Some(vec!["developer".to_string(), "production".to_string()]));
        assert_eq!(strip_look_arounds(r"\(?=x[)(]"), r"\(?=x[)(]");
    }

    #[test]
    fn a_pattern_that_may_match_without_fixed_text_has_no_prefilter() {
        assert!(required_prefixes(r"\s+x", false).is_none(), "one letter is not worth a filter");
        assert!(required_prefixes(r"a*", false).is_none());
        assert!(required_prefixes(r"(?:ab|\d+)cd?", false).is_none(), "an alternation branch with no fixed text means no proof");
    }

    #[test]
    fn the_filter_says_no_only_when_no_literal_is_there() {
        let filter = Prefilter::new(r"\bgit\b", true).unwrap();
        assert!(filter.possible("run GIT status"));
        assert!(!filter.possible("ls -la"));
        assert!(filter.possible("caf\u{e9} ls"), "non-ASCII text is never rejected for a case-insensitive pattern");
        let exact = Prefilter::new("Foo", false).unwrap();
        assert!(!exact.possible("foo") && exact.possible("a Foo b"));
    }
}
