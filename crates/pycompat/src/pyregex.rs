// SPDX-License-Identifier: MIT
//! A syntax checker for Python `re` patterns that reproduces the `re.error` messages (and their positions) the reference prints when a
//! user-written regular expression is invalid. User data (a project's PERMISSIONS.json, a rule being created) must be judged by Python's
//! grammar, not by the grammar of the Rust engine that evaluates it afterwards: `(?<n>x)` compiles in Rust and is an error in Python.
//!
//! Covered: unbalanced / unterminated groups, character sets and ranges, repeats with nothing (or an already repeated item) before them,
//! `{m,n}` bounds, escapes (bad, incomplete, group references), group names and inline flags. What it does not look at (look-behind width)
//! is left to the Rust engine.

const FLAGS: &str = "aiLmsux";

struct P<'a> {
    c: &'a [char],
    i: usize,
    groups: usize,
    names: Vec<(String, usize)>,
}

type R = Result<(), String>;

fn err(message: &str, position: usize) -> R {
    Err(format!("{message} at position {position}"))
}

/// The message Python's `re.compile` would raise for `pattern`, or `None` when the pattern is valid (as far as this checker looks).
pub fn py_regex_error(pattern: &str) -> Option<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut p = P { c: &chars, i: 0, groups: 1, names: Vec::new() };
    let result = p.sequence(0, 0);
    match result {
        Err(e) => Some(e),
        Ok(()) if p.i < chars.len() => Some(format!("unbalanced parenthesis at position {}", p.i)),
        Ok(()) => None,
    }
}

impl P<'_> {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn at_start_flags(&self, open: usize) -> bool {
        // global flags are accepted only before anything else, or after other global flags
        let mut k = 0;
        while k < open {
            if self.c[k] == '(' && self.c.get(k + 1) == Some(&'?') {
                let mut j = k + 2;
                while j < open && FLAGS.contains(self.c[j]) {
                    j += 1;
                }
                if j < open && self.c[j] == ')' && j > k + 2 {
                    k = j + 1;
                    continue;
                }
            }
            return false;
        }
        true
    }

    /// Parses alternatives until `)` (left unconsumed) or the end. `depth` > 0 inside a group opened at `open`.
    fn sequence(&mut self, depth: usize, open: usize) -> R {
        // `last`: 0 = nothing to repeat, 1 = repeatable item, 2 = anchor (not repeatable), 3 = a repeat already applied
        let mut last = 0u8;
        while let Some(ch) = self.peek() {
            match ch {
                '|' => {
                    self.i += 1;
                    last = 0;
                }
                ')' => {
                    if depth == 0 {
                        return err("unbalanced parenthesis", self.i);
                    }
                    return Ok(());
                }
                '(' => {
                    self.group()?;
                    last = 1;
                }
                '[' => {
                    self.set()?;
                    last = 1;
                }
                '*' | '+' | '?' => {
                    let at = self.i;
                    self.repeat_check(last, at)?;
                    self.i += 1;
                    if matches!(self.peek(), Some('?') | Some('+')) {
                        self.i += 1;
                    }
                    last = 3;
                }
                '{' => {
                    let at = self.i;
                    if let Some((min, max, end)) = self.brace(at) {
                        self.repeat_check(last, at)?;
                        if let Some(max) = max {
                            if max < min {
                                return err("min repeat greater than max repeat", at + 1);
                            }
                        }
                        self.i = end;
                        if matches!(self.peek(), Some('?') | Some('+')) {
                            self.i += 1;
                        }
                        last = 3;
                    } else {
                        self.i += 1;
                        last = 1;
                    }
                }
                '\\' => {
                    let anchor = self.escape(false)?;
                    last = if anchor { 2 } else { 1 };
                }
                '^' | '$' => {
                    self.i += 1;
                    last = 2;
                }
                _ => {
                    self.i += 1;
                    last = 1;
                }
            }
        }
        if depth > 0 {
            return err("missing ), unterminated subpattern", open);
        }
        Ok(())
    }

    fn repeat_check(&self, last: u8, at: usize) -> R {
        match last {
            0 | 2 => err("nothing to repeat", at),
            3 => err("multiple repeat", at),
            _ => Ok(()),
        }
    }

    /// `{m}`, `{m,}`, `{,n}`, `{m,n}` at `at`: `(min, max, end index)`; `None` when the brace is a literal.
    fn brace(&self, at: usize) -> Option<(u64, Option<u64>, usize)> {
        let mut j = at + 1;
        if self.c.get(j) == Some(&'}') {
            return None;
        }
        let digits = |j: &mut usize| -> String {
            let mut s = String::new();
            while let Some(d) = self.c.get(*j).filter(|d| d.is_ascii_digit()) {
                s.push(*d);
                *j += 1;
            }
            s
        };
        let lo = digits(&mut j);

        let hi = if self.c.get(j) == Some(&',') {
            j += 1;
            digits(&mut j)
        } else {
            lo.clone()
        };
        if self.c.get(j) != Some(&'}') {
            return None;
        }
        let min = if lo.is_empty() { 0 } else { lo.parse().unwrap_or(u64::MAX) };
        let max = if hi.is_empty() { None } else { Some(hi.parse().unwrap_or(u64::MAX)) };
        Some((min, max, j + 1))
    }

    /// A backslash escape at `self.i`; returns true for an anchor (`\A \b \B \Z \z`). `in_set` changes what is legal.
    fn escape(&mut self, in_set: bool) -> Result<bool, String> {
        let at = self.i;
        let Some(&next) = self.c.get(at + 1) else { return Err(format!("bad escape (end of pattern) at position {at}")) };
        self.i += 2;
        match next {
            'd' | 'D' | 's' | 'S' | 'w' | 'W' | 'a' | 'f' | 'n' | 'r' | 't' | 'v' => Ok(false),
            'b' => Ok(!in_set),
            'A' | 'B' | 'Z' | 'z' if !in_set => Ok(true),
            'x' | 'u' | 'U' => {
                let need = match next {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let digits: String = self.c[self.i..].iter().take(need).take_while(|d| d.is_ascii_hexdigit()).collect();
                if digits.len() != need {
                    let seen: String = self.c[at..(self.i + digits.len()).min(self.c.len())].iter().collect();
                    return Err(format!("incomplete escape {seen} at position {at}"));
                }
                self.i += need;
                Ok(false)
            }
            'N' => {
                if self.c.get(self.i) == Some(&'{') {
                    match self.c[self.i..].iter().position(|c| *c == '}') {
                        Some(end) => self.i += end + 1,
                        None => return Err(format!("missing }}, unterminated name at position {}", self.i + 1)),
                    }
                    Ok(false)
                } else {
                    Err(format!("missing {{ at position {}", self.i))
                }
            }
            '0' => {
                while self.c.get(self.i).is_some_and(|d| ('0'..='7').contains(d)) && self.i < at + 3 {
                    self.i += 1;
                }
                Ok(false)
            }
            '1'..='9' => {
                if in_set {
                    return Ok(false);
                }
                // up to two digits are a group reference (three octal digits are an octal escape)
                let mut number = next.to_digit(10).unwrap_or(0) as usize;
                if let Some(d) = self.c.get(self.i).and_then(|d| d.to_digit(10)) {
                    let octal3 = next <= '3'
                        && ('0'..='7').contains(&self.c[at + 1])
                        && ('0'..='7').contains(&self.c[self.i])
                        && self.c.get(self.i + 1).is_some_and(|x| ('0'..='7').contains(x));
                    if octal3 {
                        self.i += 2;
                        return Ok(false);
                    }
                    number = number * 10 + d as usize;
                    self.i += 1;
                }
                if number >= self.groups {
                    return Err(format!("invalid group reference {number} at position {}", at + 1));
                }
                Ok(false)
            }
            c if c.is_ascii_alphabetic() => Err(format!("bad escape \\{c} at position {at}")),
            _ => Ok(false),
        }
    }

    fn set(&mut self) -> R {
        let start = self.i;
        self.i += 1;
        if self.peek() == Some('^') {
            self.i += 1;
        }
        let mut first = true;
        loop {
            let Some(ch) = self.peek() else { return err("unterminated character set", start) };
            if ch == ']' && !first {
                self.i += 1;
                return Ok(());
            }
            first = false;
            let item_start = self.i;
            let (lo, lo_class) = self.set_item()?;
            if self.peek() == Some('-') && self.c.get(self.i + 1).is_some_and(|c| *c != ']') {
                self.i += 1;
                let (hi, hi_class) = self.set_item()?;
                let text: String = self.c[item_start..self.i].iter().collect();
                if lo_class || hi_class {
                    return err(&format!("bad character range {text}"), item_start);
                }
                if hi < lo {
                    return err(&format!("bad character range {text}"), item_start);
                }
            }
        }
    }

    /// One set member: `(code point, is a class like \d)`.
    fn set_item(&mut self) -> Result<(u32, bool), String> {
        let Some(ch) = self.peek() else { return Err(format!("unterminated character set at position {}", self.i)) };
        if ch == '\\' {
            let at = self.i;
            let class = matches!(self.c.get(at + 1), Some('d' | 'D' | 's' | 'S' | 'w' | 'W'));
            self.escape(true)?;
            let code = match self.c.get(at + 1) {
                Some('n') => 10,
                Some('t') => 9,
                Some('r') => 13,
                Some(c) => *c as u32,
                None => 0,
            };
            return Ok((code, class));
        }
        self.i += 1;
        Ok((ch as u32, false))
    }

    fn name_end(&self, from: usize, terminator: char) -> Option<usize> {
        self.c[from..].iter().position(|c| *c == terminator).map(|n| from + n)
    }

    fn group(&mut self) -> R {
        let open = self.i;
        self.i += 1;
        if self.peek() != Some('?') {
            self.groups += 1;
            return self.close_group(open);
        }
        self.i += 1;
        let Some(ch) = self.peek() else { return err("unexpected end of pattern", self.i) };
        match ch {
            'P' => {
                self.i += 1;
                match self.peek() {
                    Some('<') => {
                        self.i += 1;
                        let start = self.i;
                        let Some(end) = self.name_end(start, '>') else { return err("missing >, unterminated name", start) };
                        let name: String = self.c[start..end].iter().collect();
                        if name.is_empty() {
                            return err("missing group name", start);
                        }
                        if !is_identifier(&name) {
                            return err(&format!("bad character in group name {}", crate::py_repr_str(&name)), start);
                        }
                        if let Some((_, number)) = self.names.iter().find(|(n, _)| *n == name) {
                            return err(&format!("redefinition of group name {} as group {}; was group {}", crate::py_repr_str(&name), self.groups, number), start);
                        }
                        self.names.push((name, self.groups));
                        self.groups += 1;
                        self.i = end + 1;
                        self.close_group(open)
                    }
                    Some('=') => {
                        self.i += 1;
                        let start = self.i;
                        let Some(end) = self.name_end(start, ')') else { return err("missing ), unterminated name", start) };
                        let name: String = self.c[start..end].iter().collect();
                        if name.is_empty() {
                            return err("missing group name", start);
                        }
                        if !is_identifier(&name) {
                            return err(&format!("bad character in group name {}", crate::py_repr_str(&name)), start);
                        }
                        if !self.names.iter().any(|(n, _)| *n == name) {
                            return err(&format!("unknown group name {}", crate::py_repr_str(&name)), start);
                        }
                        self.i = end + 1;
                        Ok(())
                    }
                    other => {
                        let shown: String = std::iter::once('?').chain(std::iter::once('P')).chain(other).collect();
                        err(&format!("unknown extension {shown}"), open + 1)
                    }
                }
            }
            ':' | '=' | '!' => {
                self.i += 1;
                self.close_group(open)
            }
            '<' => {
                self.i += 1;
                match self.peek() {
                    Some('=') | Some('!') => {
                        self.i += 1;
                        self.close_group(open)
                    }
                    other => {
                        let shown: String = std::iter::once('?').chain(std::iter::once('<')).chain(other).collect();
                        err(&format!("unknown extension {shown}"), open + 1)
                    }
                }
            }
            '#' => match self.name_end(self.i, ')') {
                Some(end) => {
                    self.i = end + 1;
                    Ok(())
                }
                None => err("missing ), unterminated comment", open),
            },
            '(' => {
                // conditional on a group: (?(id)yes|no)
                let Some(end) = self.name_end(self.i + 1, ')') else { return err("missing ), unterminated name", self.i + 1) };
                let id: String = self.c[self.i + 1..end].iter().collect();
                if let Ok(n) = id.parse::<usize>() {
                    if n == 0 || n >= self.groups + 1000 {
                        return err("bad group number", self.i + 1);
                    }
                } else if !self.names.iter().any(|(n, _)| *n == id) {
                    return err(&format!("unknown group name {}", crate::py_repr_str(&id)), self.i + 1);
                }
                self.i = end + 1;
                self.close_group(open)
            }
            c if FLAGS.contains(c) || c == '-' => self.flags(open),
            c => err(&format!("unknown extension ?{c}"), open + 1),
        }
    }

    fn flags(&mut self, open: usize) -> R {
        let mut removing = false;
        let mut seen_any = false;
        loop {
            let Some(ch) = self.peek() else { return err("missing -, : or )", self.i) };
            match ch {
                ')' => {
                    if removing {
                        return err("missing :", self.i);
                    }
                    if !self.at_start_flags(open) {
                        return err("global flags not at the start of the expression", open);
                    }
                    self.i += 1;
                    return Ok(());
                }
                ':' => {
                    if removing && !seen_any {
                        return err("missing flag", self.i);
                    }
                    self.i += 1;
                    return self.close_group(open);
                }
                '-' if !removing => {
                    removing = true;
                    seen_any = false;
                    self.i += 1;
                }
                c if FLAGS.contains(c) => {
                    if removing && !"imsx".contains(c) {
                        return err("bad inline flags: cannot turn off flags 'a', 'u' and 'L'", self.i);
                    }
                    seen_any = true;
                    self.i += 1;
                }
                c if c.is_alphabetic() => return err("unknown flag", self.i),
                _ => return err(if removing { "missing :" } else { "missing -, : or )" }, self.i),
            }
        }
    }

    fn close_group(&mut self, open: usize) -> R {
        self.sequence(1, open)?;
        if self.peek() != Some(')') {
            return err("missing ), unterminated subpattern", open);
        }
        self.i += 1;
        Ok(())
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic()) && chars.all(|c| c == '_' || c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Messages printed by Python 3.14 `re.compile` for these patterns (see the calibration script in the change notes).
    #[test]
    fn messages_match_python() {
        let cases: &[(&str, Option<&str>)] = &[
            ("(unclosed", Some("missing ), unterminated subpattern at position 0")),
            ("a)", Some("unbalanced parenthesis at position 1")),
            ("[abc", Some("unterminated character set at position 0")),
            ("[a-", Some("unterminated character set at position 0")),
            ("*a", Some("nothing to repeat at position 0")),
            ("a**", Some("multiple repeat at position 2")),
            ("a{2}{3}", Some("multiple repeat at position 4")),
            ("\\q", Some("bad escape \\q at position 0")),
            ("\\", Some("bad escape (end of pattern) at position 0")),
            ("a(?P<n", Some("missing >, unterminated name at position 5")),
            ("(?P<n>a)(?P<n>b)", Some("redefinition of group name 'n' as group 2; was group 1 at position 12")),
            ("[z-a]", Some("bad character range z-a at position 1")),
            ("a{3,2}", Some("min repeat greater than max repeat at position 2")),
            ("(?x", Some("missing -, : or ) at position 3")),
            ("x{1,", None),
            ("(", Some("missing ), unterminated subpattern at position 0")),
            ("[", Some("unterminated character set at position 0")),
            ("?", Some("nothing to repeat at position 0")),
            ("+x", Some("nothing to repeat at position 0")),
            ("a|*", Some("nothing to repeat at position 2")),
            ("(?:", Some("missing ), unterminated subpattern at position 0")),
            ("\\1", Some("invalid group reference 1 at position 1")),
            ("(a)\\2", Some("invalid group reference 2 at position 4")),
            ("(?P=x)", Some("unknown group name 'x' at position 4")),
            ("(?P<1a>x)", Some("bad character in group name '1a' at position 4")),
            ("\\x4", Some("incomplete escape \\x4 at position 0")),
            ("\\u12", Some("incomplete escape \\u12 at position 0")),
            ("(?i", Some("missing -, : or ) at position 3")),
            ("[\\d-z]", Some("bad character range \\d-z at position 1")),
            ("a++", None),
            ("a*+", None),
            ("(?<n>x)", Some("unknown extension ?<n at position 1")),
            ("(?#c", Some("missing ), unterminated comment at position 0")),
            ("\\8", Some("invalid group reference 8 at position 1")),
            ("(?z)", Some("unknown extension ?z at position 1")),
            ("(?i:", Some("missing ), unterminated subpattern at position 0")),
            ("a{,}", None),
            ("\\k<x>", Some("bad escape \\k at position 0")),
            ("(?P>x)", Some("unknown extension ?P> at position 1")),
            ("[]", Some("unterminated character set at position 0")),
            ("[^]", Some("unterminated character set at position 0")),
            ("(*)", Some("nothing to repeat at position 1")),
            ("a{1", None),
            ("\\p{L}", Some("bad escape \\p at position 0")),
            ("ab(?=c", Some("missing ), unterminated subpattern at position 2")),
            ("\\b+", Some("nothing to repeat at position 2")),
            ("^*", Some("nothing to repeat at position 1")),
            ("$+", Some("nothing to repeat at position 1")),
            ("(?i)(?-i)a", Some("missing : at position 8")),
            ("a(?!b+)c", None),
            ("(a|b)+\\d{2,4}[a-z0-9_]\\.x", None),
            ("\\.lock$", None),
        ];
        for (pattern, expected) in cases {
            assert_eq!(py_regex_error(pattern).as_deref(), *expected, "pattern {pattern:?}");
        }
    }
}
