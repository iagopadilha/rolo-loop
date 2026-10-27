// SPDX-License-Identifier: MIT
//! Small emulations of the Python standard-library pieces the guard's behaviour depends on:
//! `re` (through fancy-regex, so lookaround works), `ntpath`/`posixpath` normalisation, `shlex.split(posix=False)`,
//! `fnmatch`, `urllib.parse.unquote` and `urlsplit`.

pub mod json;
pub mod prefilter;
pub mod pyregex;
pub use pyregex::py_regex_error;

use fancy_regex::{Regex as FRegex, RegexBuilder};

/// The matching engine behind a [`Re`]: the plain `regex` engine when the pattern needs nothing fancy (it compiles about three times faster, which
/// matters for the guard, started once per tool call), else `fancy-regex` (lookaround, back-references).
enum Engine {
    Plain(regex::Regex),
    Fancy(FRegex),
}

impl Engine {
    fn compile(pattern: &str, ci: bool) -> Result<Engine, String> {
        let source = if ci { format!("(?i){pattern}") } else { pattern.to_string() };
        if let Ok(plain) = regex::Regex::new(&source) {
            return Ok(Engine::Plain(plain));
        }
        let mut builder = RegexBuilder::new(&source);
        builder.backtrack_limit(2_000_000);
        builder.build().map(Engine::Fancy).map_err(|e| e.to_string())
    }
    fn is_match(&self, text: &str) -> bool {
        match self {
            Engine::Plain(re) => re.is_match(text),
            Engine::Fancy(re) => re.is_match(text).unwrap_or(false),
        }
    }
    fn captures_from_pos(&self, text: &str, from: usize) -> Option<M> {
        match self {
            Engine::Plain(re) => re.captures_at(text, from).map(|caps| {
                let whole = caps.get(0).expect("group 0 always exists");
                M { start: whole.start(), end: whole.end(), groups: (0..caps.len()).map(|i| caps.get(i).map(|g| g.as_str().to_string())).collect() }
            }),
            Engine::Fancy(re) => re.captures_from_pos(text, from).ok().flatten().map(|caps| to_m(&caps)),
        }
    }
}

/// A compiled pattern with Python-like helpers. Matching errors (backtrack limit) count as "no match".
pub struct Re {
    re: std::sync::OnceLock<Engine>,
    prefilter: Option<prefilter::Prefilter>,
    src: String,
    ci: bool,
    start: std::sync::OnceLock<Engine>,
    full: std::sync::OnceLock<Engine>,
}

/// One match: byte offsets and the captured groups as owned text.
#[derive(Clone, Debug)]
pub struct M {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<String>>,
}

impl M {
    pub fn text(&self) -> &str {
        self.groups[0].as_deref().unwrap_or("")
    }
    pub fn group(&self, index: usize) -> Option<&str> {
        self.groups.get(index).and_then(|g| g.as_deref())
    }
}

impl std::ops::Index<usize> for M {
    type Output = str;
    /// Group `index` as text; a group that did not take part in the match reads as ''.
    fn index(&self, index: usize) -> &str {
        self.group(index).unwrap_or("")
    }
}

static CACHE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<(String, bool), std::sync::Arc<Re>>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

impl Re {
    /// A pattern built at run time (it depends on data), compiled once per process: the same text gives the same shared pattern. Compiling costs
    /// milliseconds, and a hook that starts once per tool call cannot afford to do it again for every path it looks at.
    pub fn cached(pattern: &str, ci: bool) -> std::sync::Arc<Re> {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.entry((pattern.to_string(), ci)).or_insert_with(|| std::sync::Arc::new(Re::build(pattern, ci))).clone()
    }
    /// Case-sensitive pattern.
    pub fn new(pattern: &str) -> Re {
        Re::build(pattern, false)
    }
    /// `re.I`.
    pub fn ci(pattern: &str) -> Re {
        Re::build(pattern, true)
    }
    fn compile(pattern: &str, ci: bool) -> Result<Engine, String> {
        Engine::compile(pattern, ci)
    }
    /// A pattern written in this codebase (static): compiled by the Rust engine's own grammar, no Python-grammar check.
    fn build(pattern: &str, ci: bool) -> Re {
        let prefilter = prefilter::Prefilter::new(pattern, ci);
        if prefilter.is_none() {
            // nothing to skip compiling for: compile now, so an invalid static pattern fails where it is written
            return match Re::compile(pattern, ci) {
                Ok(engine) => Re::from_parts(pattern, ci, None, Some(engine)),
                Err(error) => panic!("invalid pattern {pattern:?}: {error}"),
            };
        }
        Re::from_parts(pattern, ci, prefilter, None)
    }
    fn from_parts(pattern: &str, ci: bool, prefilter: Option<prefilter::Prefilter>, engine: Option<Engine>) -> Re {
        let re = std::sync::OnceLock::new();
        if let Some(engine) = engine {
            let _ = re.set(engine);
        }
        Re { re, prefilter, src: pattern.to_string(), ci, start: std::sync::OnceLock::new(), full: std::sync::OnceLock::new() }
    }
    /// The compiled engine, built on first use.
    fn engine(&self) -> &Engine {
        self.re.get_or_init(|| Re::compile(&self.src, self.ci).unwrap_or_else(|error| panic!("invalid pattern {:?}: {error}", self.src)))
    }
    /// False only when the text certainly holds no match from `from` on.
    fn may_match(&self, text: &str, from: usize) -> bool {
        match &self.prefilter {
            Some(filter) => text.get(from..).is_none_or(|rest| filter.possible(rest)),
            None => true,
        }
    }
    /// Fallible compile: a pattern that only looks like a regex (user data) must never crash the guard.
    pub fn try_build(pattern: &str, ci: bool) -> Option<Re> {
        Re::try_build_msg(pattern, ci).ok()
    }
    /// Like [`Re::try_build`] with the message Python's `re.compile` would raise. The pattern is first judged by Python's grammar
    /// (`(?<n>x)` compiles in Rust and is an error in Python), then compiled.
    pub fn try_build_msg(pattern: &str, ci: bool) -> Result<Re, String> {
        if let Some(message) = pyregex::py_regex_error(pattern) {
            return Err(message);
        }
        Re::compile(pattern, ci)
            .map(|engine| Re::from_parts(pattern, ci, prefilter::Prefilter::new(pattern, ci), Some(engine)))
            .map_err(|_| "invalid pattern".to_string())
    }
    pub fn is_match(&self, text: &str) -> bool {
        self.may_match(text, 0) && self.engine().is_match(text)
    }
    fn twin<'a>(&self, cell: &'a std::sync::OnceLock<Engine>, tail: &str) -> &'a Engine {
        cell.get_or_init(|| Re::compile(&format!(r"\A(?:{}){}", self.src, tail), self.ci).expect("anchored twin compiles"))
    }
    /// Python `re.match`: anchored at the start.
    pub fn match_start(&self, text: &str) -> Option<M> {
        if !self.may_match(text, 0) {
            return None;
        }
        self.twin(&self.start, "").captures_from_pos(text, 0)
    }
    pub fn search(&self, text: &str) -> Option<M> {
        self.find_at(text, 0)
    }
    /// Same as [`Re::search`]: the first match with its groups.
    pub fn captures(&self, text: &str) -> Option<M> {
        self.search(text)
    }
    fn find_at(&self, text: &str, from: usize) -> Option<M> {
        if !self.may_match(text, from) {
            return None;
        }
        self.engine().captures_from_pos(text, from)
    }
    /// `finditer`.
    pub fn find_all(&self, text: &str) -> Vec<M> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos <= text.len() {
            let Some(m) = self.find_at(text, pos) else {
                break;
            };
            pos = if m.end == m.start { next_boundary(text, m.end) } else { m.end };
            out.push(m);
        }
        out
    }
    /// `re.sub` with a closure.
    pub fn replace_all(&self, text: &str, f: impl Fn(&M) -> String) -> String {
        let mut out = String::new();
        let mut last = 0;
        for m in self.find_all(text) {
            out.push_str(&text[last..m.start]);
            out.push_str(&f(&m));
            last = m.end;
        }
        out.push_str(&text[last..]);
        out
    }
    /// `re.sub` with a literal replacement.
    pub fn replace_lit(&self, text: &str, with: &str) -> String {
        self.replace_all(text, |_| with.to_string())
    }
    /// `re.split` (without capture groups).
    pub fn split(&self, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut last = 0;
        for m in self.find_all(text) {
            if m.end == m.start {
                continue;
            }
            out.push(text[last..m.start].to_string());
            last = m.end;
        }
        out.push(text[last..].to_string());
        out
    }
    /// `re.fullmatch`.
    pub fn full_match(&self, text: &str) -> bool {
        self.may_match(text, 0) && self.twin(&self.full, r"\z").is_match(text)
    }
}

fn to_m(caps: &fancy_regex::Captures<'_>) -> M {
    let whole = caps.get(0).expect("group 0 always exists");
    M { start: whole.start(), end: whole.end(), groups: (0..caps.len()).map(|i| caps.get(i).map(|g| g.as_str().to_string())).collect() }
}

fn next_boundary(text: &str, index: usize) -> usize {
    let mut i = index + 1;
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Python `repr` of a `str`.
pub fn py_repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `repr` of a decoded JSON value.
pub fn py_repr_json(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if n.is_f64() {
                let f = n.as_f64().unwrap_or(0.0);
                if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e16 {
                    format!("{f:.1}")
                } else {
                    format!("{f}")
                }
            } else {
                n.to_string()
            }
        }
        Value::String(s) => py_repr_str(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr_json).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!("{{{}}}", map.iter().map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr_json(v))).collect::<Vec<_>>().join(", ")),
    }
}

/// `re.escape`.
pub fn escape(text: &str) -> String {
    fancy_regex::escape(text).into_owned()
}

// ---- Python str helpers ------------------------------------------------------------------------------------------------------

/// `str.split()` with no arguments (any whitespace run).
pub fn split_ws(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// `str.strip(chars)`.
pub fn strip_chars<'a>(text: &'a str, chars: &str) -> &'a str {
    text.trim_matches(|c| chars.contains(c))
}

/// `str.partition(sep)` -> (head, sep found, tail).
pub fn partition<'a>(text: &'a str, sep: &str) -> (&'a str, bool, &'a str) {
    match text.find(sep) {
        Some(i) => (&text[..i], true, &text[i + sep.len()..]),
        None => (text, false, ""),
    }
}

/// `str.rpartition(sep)` -> (head, sep found, tail); when absent head is "" and tail the whole text, as Python does.
pub fn rpartition<'a>(text: &'a str, sep: &str) -> (&'a str, bool, &'a str) {
    match text.rfind(sep) {
        Some(i) => (&text[..i], true, &text[i + sep.len()..]),
        None => ("", false, text),
    }
}

/// `str.splitlines()`: splits at `\n`, `\r`, `\r\n`, `\v`, `\f`, `\x1c`-`\x1e`, `\x85`, ` `, ` `.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    while i < bytes.len() {
        let (pos, c) = bytes[i];
        if matches!(c, '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            out.push(&text[start..pos]);
            let mut next = pos + c.len_utf8();
            if c == '\r' && bytes.get(i + 1).is_some_and(|(_, n)| *n == '\n') {
                next += 1;
                i += 1;
            }
            start = next;
        }
        i += 1;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// `str.splitlines(keepends=True)`.
pub fn splitlines_keepends(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let (pos, c) = chars[i];
        if matches!(c, '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            let mut end = pos + c.len_utf8();
            if c == '\r' && chars.get(i + 1).is_some_and(|(_, n)| *n == '\n') {
                end += 1;
                i += 1;
            }
            out.push(&text[start..end]);
            start = end;
        }
        i += 1;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// `text[:n]` by characters.
pub fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// `os.path.basename`-like split of a Windows/posix path on both separators.
pub fn last_segment(text: &str) -> &str {
    text.rsplit(['\\', '/']).next().unwrap_or("")
}

// ---- ntpath / posixpath --------------------------------------------------------------------------------------------------------

fn split_drive_nt(path: &str) -> (String, String) {
    let p = path.replace('/', "\\");
    let b = p.as_bytes();
    if p.starts_with("\\\\") && !p.starts_with("\\\\\\") {
        // UNC: \\server\share
        let rest = &p[2..];
        if let Some(i) = rest.find('\\') {
            let after = &rest[i + 1..];
            let j = after.find('\\').unwrap_or(after.len());
            let end = 2 + i + 1 + j;
            return (path[..end].to_string(), path[end..].to_string());
        }
        return (path.to_string(), String::new());
    }
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return (path[..2].to_string(), path[2..].to_string());
    }
    (String::new(), path.to_string())
}

/// `os.path.normpath` on Windows (separators become backslashes).
pub fn nt_normpath(path: &str) -> String {
    let (drive, rest) = split_drive_nt(path);
    let rest = rest.replace('/', "\\");
    let drive = drive.replace('/', "\\");
    let rooted = rest.starts_with('\\');
    let mut parts: Vec<&str> = Vec::new();
    for c in rest.split('\\') {
        match c {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else if !rooted && !drive.starts_with("\\\\") {
                    parts.push("..");
                }
            }
            c => parts.push(c),
        }
    }
    let mut out = drive;
    if rooted {
        out.push('\\');
    }
    out.push_str(&parts.join("\\"));
    if out.is_empty() {
        ".".to_string()
    } else {
        out
    }
}

/// `posixpath.normpath`.
pub fn posix_normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let initial = if path.starts_with('/') {
        if path.starts_with("//") && !path.starts_with("///") {
            2
        } else {
            1
        }
    } else {
        0
    };
    let mut comps: Vec<&str> = Vec::new();
    for c in path.split('/') {
        if c.is_empty() || c == "." {
            continue;
        }
        if c != ".." || (initial == 0 && comps.is_empty()) || comps.last().is_some_and(|l| *l == "..") {
            comps.push(c);
        } else if !comps.is_empty() {
            comps.pop();
        }
    }
    let joined = comps.join("/");
    let mut out = "/".repeat(initial);
    out.push_str(&joined);
    if out.is_empty() {
        ".".to_string()
    } else {
        out
    }
}

/// `os.path.isabs` on Windows (Python 3.13+: a lone leading slash is not absolute).
pub fn nt_isabs(path: &str) -> bool {
    let p = path.replace('/', "\\");
    let b = p.as_bytes();
    (b.len() >= 3 && b[1] == b':' && b[0].is_ascii_alphabetic() && b[2] == b'\\') || (p.starts_with("\\\\") && p.len() > 2)
}

/// `os.path.join(a, b)` on Windows.
pub fn nt_join(a: &str, b: &str) -> String {
    if b.is_empty() {
        return if a.is_empty() {
            String::new()
        } else if a.ends_with(['\\', '/']) {
            a.to_string()
        } else {
            format!("{a}\\")
        };
    }
    let (bd, br) = split_drive_nt(b);
    if nt_isabs(b) || (!bd.is_empty() && !bd.eq_ignore_ascii_case(&split_drive_nt(a).0)) {
        return b.to_string();
    }
    let (ad, ar) = split_drive_nt(a);
    if br.starts_with(['\\', '/']) {
        return format!("{}{}", if bd.is_empty() { ad } else { bd }, br);
    }
    if a.is_empty() {
        return b.to_string();
    }
    let base = if ar.is_empty() || ar.ends_with('\\') || a.ends_with(['\\', '/']) { a.to_string() } else { format!("{a}\\") };
    format!("{base}{}", if bd.is_empty() { b.to_string() } else { br })
}

/// `os.path.dirname` on Windows.
pub fn nt_dirname(path: &str) -> String {
    let (drive, rest) = split_drive_nt(path);
    let cut = rest.rfind(['\\', '/']).map(|i| i + 1).unwrap_or(0);
    let head = &rest[..cut];
    let trimmed = head.trim_end_matches(['\\', '/']);
    format!("{drive}{}", if trimmed.is_empty() { head } else { trimmed })
}

/// `os.path.abspath` (Windows): join with the cwd when relative, normalise.
pub fn nt_abspath(path: &str) -> String {
    let full = if nt_isabs(path) {
        path.to_string()
    } else {
        let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
        nt_join(&cwd, path)
    };
    nt_normpath(&full)
}

// ---- shlex --------------------------------------------------------------------------------------------------------------------

/// `shlex.split(text, posix=False)`; `Err` where Python raises ValueError (no closing quotation).
pub fn shlex_split_nonposix(text: &str) -> Result<Vec<String>, ()> {
    let ws = |c: char| c == ' ' || c == '\t' || c == '\r' || c == '\n';
    let quote = |c: char| c == '"' || c == '\'';
    let mut tokens = Vec::new();
    let mut token = String::new();
    // state: None = whitespace, Some(' ') word, Some(q) in quote
    let mut state: Option<char> = None;
    let mut word = false;
    for c in text.chars() {
        match state {
            None => {
                if ws(c) {
                    continue;
                } else if quote(c) {
                    token.push(c);
                    state = Some(c);
                } else {
                    token.push(c);
                    state = Some('a');
                    word = true;
                }
            }
            Some('a') => {
                if ws(c) {
                    tokens.push(std::mem::take(&mut token));
                    state = None;
                    word = false;
                } else {
                    token.push(c);
                }
            }
            Some(q) => {
                token.push(c);
                if c == q {
                    tokens.push(std::mem::take(&mut token));
                    state = None;
                }
            }
        }
    }
    let _ = word;
    match state {
        Some('a') => tokens.push(token),
        Some(_) => return Err(()),
        None => {}
    }
    Ok(tokens)
}

// ---- fnmatch ------------------------------------------------------------------------------------------------------------------

/// `fnmatch.fnmatchcase(name, pattern)`.
pub fn fnmatchcase(name: &str, pattern: &str) -> bool {
    let mut re = String::from("^(?s:");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '[' => {
                let mut j = i;
                if j < chars.len() && chars[j] == '!' {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ']' {
                    j += 1;
                }
                while j < chars.len() && chars[j] != ']' {
                    j += 1;
                }
                if j >= chars.len() {
                    re.push_str("\\[");
                } else {
                    let mut stuff: String = chars[i..j].iter().collect();
                    stuff = stuff.replace('\\', "\\\\");
                    i = j + 1;
                    if let Some(rest) = stuff.strip_prefix('!') {
                        re.push_str(&format!("[^{rest}]"));
                    } else if stuff.starts_with('^') {
                        re.push_str(&format!("[\\{stuff}]"));
                    } else {
                        re.push_str(&format!("[{stuff}]"));
                    }
                }
            }
            c => re.push_str(&fancy_regex::escape(&c.to_string())),
        }
    }
    re.push_str(")$");
    match FRegex::new(&re) {
        Ok(r) => r.is_match(name).unwrap_or(false),
        Err(_) => false,
    }
}

// ---- urllib -------------------------------------------------------------------------------------------------------------------

/// `urllib.parse.unquote` (UTF-8, errors replaced).
pub fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// The parts of `urlsplit(url)` the guard reads.
pub struct UrlParts {
    pub scheme: String,
    pub hostname: Option<String>,
    pub port: Option<u32>,
    pub path: String,
    pub query: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// `urlsplit` + `.hostname` + `.port`; `Err(message)` where Python raises ValueError.
pub fn urlsplit(url: &str) -> Result<UrlParts, String> {
    let mut rest = url.to_string();
    let mut scheme = String::new();
    if let Some(i) = rest.find(':') {
        let cand = &rest[..i];
        let valid =
            !cand.is_empty() && cand.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && cand.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        if valid {
            scheme = cand.to_ascii_lowercase();
            rest = rest[i + 1..].to_string();
        }
    }
    let mut netloc = String::new();
    if rest.starts_with("//") {
        let body = &rest[2..];
        let end = body.find(['/', '?', '#']).unwrap_or(body.len());
        netloc = body[..end].to_string();
        rest = body[end..].to_string();
        if (netloc.contains('[') && !netloc.contains(']')) || (netloc.contains(']') && !netloc.contains('[')) {
            return Err("Invalid IPv6 URL".into());
        }
    }
    if let Some(i) = rest.find('#') {
        rest.truncate(i);
    }
    let mut query = String::new();
    if let Some(i) = rest.find('?') {
        query = rest[i + 1..].to_string();
        rest.truncate(i);
    }
    let path = rest;
    let (userinfo, have_info, _) = rpartition(&netloc, "@");
    let (username, password) = if have_info {
        let (u, has, p) = partition(userinfo, ":");
        (Some(u.to_string()), has.then(|| p.to_string()))
    } else {
        (None, None)
    };
    // hostname and port, as SplitResult does
    let hostinfo = rpartition(&netloc, "@").2.to_string();
    let (mut hostname, mut port_text): (Option<String>, Option<String>) = (None, None);
    if netloc.is_empty() {
        // no authority
    } else if hostinfo.starts_with('[') && hostinfo.contains(']') {
        let close = hostinfo.find(']').unwrap_or(0);
        hostname = Some(hostinfo[1..close].to_ascii_lowercase());
        let after = &hostinfo[close + 1..];
        if let Some(p) = after.strip_prefix(':') {
            port_text = Some(p.to_string());
        }
    } else {
        let (h, has, p) = partition(&hostinfo, ":");
        hostname = Some(h.to_ascii_lowercase());
        if has {
            port_text = Some(p.to_string());
        }
    }
    if hostname.as_deref() == Some("") {
        hostname = None;
    }
    let mut port = None;
    if let Some(p) = port_text {
        if !p.is_empty() {
            if p.chars().all(|c| c.is_ascii_digit()) {
                let n: u32 = p.parse().map_err(|_| "Port out of range 0-65535".to_string())?;
                if n > 65535 {
                    return Err("Port out of range 0-65535".into());
                }
                port = Some(n);
            } else {
                return Err("Port could not be cast to integer value".into());
            }
        }
    }
    Ok(UrlParts { scheme, hostname, port, path, query, username, password })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nt_normpath_basics() {
        assert_eq!(nt_normpath("C:/a/./b/../c"), "C:\\a\\c");
        assert_eq!(nt_normpath("a/../../b"), "..\\b");
        assert_eq!(nt_normpath(""), ".");
        assert_eq!(nt_normpath("C:\\"), "C:\\");
        assert_eq!(nt_normpath("C:\\..\\x"), "C:\\x");
    }

    #[test]
    fn posix_normpath_basics() {
        assert_eq!(posix_normpath("c:/a/../b"), "c:/b");
        assert_eq!(posix_normpath("/a//b/./c/.."), "/a/b");
        assert_eq!(posix_normpath("a/.."), ".");
    }

    #[test]
    fn shlex_nonposix_keeps_quotes() {
        assert_eq!(shlex_split_nonposix("rm -rf \"a b\" c").unwrap(), vec!["rm", "-rf", "\"a b\"", "c"]);
        assert_eq!(shlex_split_nonposix("\"abc\"def").unwrap(), vec!["\"abc\"", "def"]);
        assert_eq!(shlex_split_nonposix("abc\"def ghi\"").unwrap(), vec!["abc\"def", "ghi\""]);
        assert!(shlex_split_nonposix("echo \"unclosed").is_err());
    }

    #[test]
    fn fnmatch_cases() {
        assert!(fnmatchcase(".env.local", ".env*"));
        assert!(fnmatchcase("a.txt", "*.txt"));
        assert!(!fnmatchcase("a.txt", "*.md"));
        assert!(fnmatchcase("a1", "a[0-9]"));
    }

    #[test]
    fn regex_wrapper() {
        let r = Re::ci(r"(?<![\w.-])main(?![\w.-])");
        assert!(r.is_match("push origin main"));
        assert!(!r.is_match("push origin domain"));
        assert_eq!(Re::new(r"\d+").find_all("a1b22").len(), 2);
        assert_eq!(Re::new(r"[,;]").split("a,b;c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn urlsplit_basics() {
        let u = urlsplit("https://User@Example.com:8443/a/b?x#y").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.hostname.as_deref(), Some("example.com"));
        assert_eq!(u.port, Some(8443));
        assert_eq!(u.path, "/a/b");
        assert!(urlsplit("http://h:abc/").is_err());
        assert_eq!(urlsplit("http://[::1]:80/").unwrap().hostname.as_deref(), Some("::1"));
    }

    #[test]
    fn unquote_works() {
        assert_eq!(unquote("a%20b%2Fc%zz"), "a b/c%zz");
    }
}

// ---- more ntpath / fnmatch ----------------------------------------------------------------------------------------------------

/// `os.path.normcase` on Windows: lower case, backslashes.
pub fn nt_normcase(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

/// `os.path.realpath` on Windows: symlinks and 8.3 names resolved for the part of the path that exists (without the verbatim prefix); the part
/// that does not exist is appended as it is.
pub fn nt_realpath(path: &str) -> String {
    const VERBATIM_UNC: &str = r"\\?\UNC\";
    const VERBATIM: &str = r"\\?\";
    let abs = nt_abspath(path);
    let mut existing = std::path::PathBuf::from(&abs);
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(&existing) {
            let text = real.display().to_string();
            let mut resolved =
                if let Some(tail) = text.strip_prefix(VERBATIM_UNC) { format!(r"\\{tail}") } else { text.strip_prefix(VERBATIM).unwrap_or(&text).to_string() };
            for part in rest.iter().rev() {
                if !resolved.ends_with('\\') {
                    resolved.push('\\');
                }
                resolved.push_str(&part.to_string_lossy());
            }
            return resolved;
        }
        match (existing.file_name().map(|n| n.to_os_string()), existing.parent().map(|p| p.to_path_buf())) {
            (Some(name), Some(parent)) if parent != existing => {
                rest.push(name);
                existing = parent;
            }
            _ => return abs,
        }
    }
}

/// `fnmatch.fnmatch` on Windows: both sides are case-folded first.
pub fn fnmatch_nt(name: &str, pattern: &str) -> bool {
    fnmatchcase(&name.to_lowercase(), &pattern.to_lowercase())
}

/// Python `%g` of a float (6 significant digits, trailing zeros removed).
pub fn format_g(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let exponent = value.abs().log10().floor() as i32;
    if !(-4..6).contains(&exponent) {
        let text = format!("{value:.5e}");
        let (mantissa, exp) = text.split_once('e').unwrap_or((&text, "0"));
        let mantissa = if mantissa.contains('.') { mantissa.trim_end_matches('0').trim_end_matches('.') } else { mantissa };
        let exp: i32 = exp.parse().unwrap_or(0);
        return format!("{mantissa}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    let decimals = (5 - exponent).max(0) as usize;
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}
