// SPDX-License-Identifier: MIT
//! Shell command analysis (port of `guard_git.py`): a lexer for Bash/PowerShell command text, and the finders for
//! `git` and `gh` invocations anywhere in a command.
//!
//! It is text analysis, not a shell: a command built from a variable, or a script that runs git itself, is not seen.

use crate::pyre::{split_ws, Re};
use std::sync::LazyLock;

pub static ASSIGNMENT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[A-Za-z_]\w*\+?="));
static EXEC_SUFFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"\.(?:exe|cmd|bat|com)\z"));
static DANGEROUS_CONFIG: LazyLock<Re> = LazyLock::new(|| {
    Re::ci(
        r"^(?:core\.(?:fsmonitor|pager|editor|sshcommand|hookspath|askpass|gitproxy)|credential\.helper|diff\.external|gpg\.program|sequence\.editor|uploadpack\.packobjectshook)\n?$",
    )
});
static CMD_CONTINUATION: LazyLock<Re> = LazyLock::new(|| Re::new(r"\^\r?\n"));

/// Words after which another command (or an argument that is itself a command) follows.
const WRAPPERS: &[&str] = &[
    "env",
    "command",
    "exec",
    "sudo",
    "doas",
    "time",
    "nohup",
    "nice",
    "ionice",
    "setsid",
    "stdbuf",
    "timeout",
    "xargs",
    "builtin",
    "watch",
    "unbuffer",
    "start",
    "call",
    "cmd",
    "bash",
    "sh",
    "zsh",
    "dash",
    "pwsh",
    "powershell",
    "wsl",
    "start-process",
    "saps",
    "invoke-expression",
    "iex",
    "invoke-command",
    "icm",
    "&",
    "!",
    "if",
    "then",
    "else",
    "elif",
    "do",
    "while",
    "until",
    "{",
    "}",
    "foreach-object",
    "%",
    "foreach",
];
const ESCAPABLE: &str = " \t\"'&;|()<>$`";
const MAX_DEPTH: usize = 3;
const GIT_OPTIONS_WITH_VALUE: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--super-prefix",
    "--config-env",
    "--attr-source",
    "--exec-path",
];
const PUSH_OPTIONS_WITH_VALUE: &[&str] = &[
    "--repo",
    "-o",
    "--push-option",
    "--receive-pack",
    "--exec",
    "--recurse-submodules",
];
const GH_OPTIONS_WITH_VALUE: &[&str] = &["-R", "--repo", "--hostname", "-H", "--header"];

/// `C:\Program Files\Git\bin\git.exe` -> `git`, `\rm` -> `rm`, `/usr/bin/gh` -> `gh`.
pub fn executable_name(word: &str) -> String {
    let trimmed = word.trim_matches(|c| c == '"' || c == '\'');
    let name = trimmed
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .to_lowercase();
    EXEC_SUFFIX.replace_lit(&name, "")
}

/// One event of the lexer.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Cmd(Vec<String>),
    Open,
    Close,
}

struct Lexer<'a> {
    text: Vec<char>,
    depth: usize,
    events: &'a mut Vec<Event>,
    powershell: bool,
    words: Vec<String>,
    nested: Vec<String>,
    buffer: String,
    in_word: bool,
    quoted: bool,
    skip_word: bool,
}

impl<'a> Lexer<'a> {
    fn new(text: &str, depth: usize, events: &'a mut Vec<Event>, powershell: bool) -> Self {
        Lexer {
            text: text.chars().collect(),
            depth,
            events,
            powershell,
            words: vec![],
            nested: vec![],
            buffer: String::new(),
            in_word: false,
            quoted: false,
            skip_word: false,
        }
    }

    fn finish_word(&mut self) {
        if !self.in_word {
            return;
        }
        let word = std::mem::take(&mut self.buffer);
        let quoted = self.quoted;
        self.in_word = false;
        self.quoted = false;
        if self.skip_word {
            self.skip_word = false;
            return;
        }
        self.words.push(word.clone());
        if quoted && self.depth < MAX_DEPTH && word.trim().chars().any(char::is_whitespace) {
            self.nested.push(word);
        }
    }

    fn end_command(&mut self) {
        self.finish_word();
        if !self.words.is_empty() {
            self.events
                .push(Event::Cmd(std::mem::take(&mut self.words)));
        }
        self.words = vec![];
        for word in std::mem::take(&mut self.nested) {
            self.events.push(Event::Open);
            let mut inner = Lexer::new(&word, self.depth + 1, self.events, self.powershell);
            inner.run();
            self.events.push(Event::Close);
        }
    }

    /// Length of the newline pair (`\` or a backtick, then LF or CRLF) that starts at `index`; 0 when it is not one.
    fn continuation(&self, index: usize) -> usize {
        let t = &self.text;
        let next = t.get(index + 1).copied();
        let next2 = t.get(index + 2).copied();
        if next == Some('\n') {
            2
        } else if next == Some('\r') && next2 == Some('\n') {
            3
        } else {
            0
        }
    }

    fn starts_with(&self, index: usize, a: char, b: char) -> bool {
        self.text.get(index) == Some(&a) && self.text.get(index + 1) == Some(&b)
    }

    fn run(&mut self) {
        let size = self.text.len();
        let mut index = 0;
        while index < size {
            let ch = self.text[index];
            let skipped = if ch == '\\' || ch == '`' {
                self.continuation(index)
            } else {
                0
            };
            if skipped > 0 {
                index += skipped;
            } else if ch == '\'' {
                let end = self.text[index + 1..]
                    .iter()
                    .position(|c| *c == '\'')
                    .map(|p| p + index + 1)
                    .unwrap_or(size);
                let chunk: String = self.text[index + 1..end.min(size)].iter().collect();
                self.buffer.push_str(&chunk);
                self.in_word = true;
                self.quoted = true;
                index = end + 1;
            } else if ch == '"' {
                self.in_word = true;
                self.quoted = true;
                index += 1;
                let mut chunk = String::new();
                while index < size && self.text[index] != '"' {
                    let pair = if self.text[index] == '\\' {
                        self.continuation(index)
                    } else {
                        0
                    };
                    if pair > 0 {
                        index += pair;
                    } else if self.text[index] == '\\'
                        && index + 1 < size
                        && "\"\\$`".contains(self.text[index + 1])
                        && !self.powershell
                    {
                        chunk.push(self.text[index + 1]);
                        index += 2;
                    } else {
                        chunk.push(self.text[index]);
                        index += 1;
                    }
                }
                self.buffer.push_str(&chunk);
                index += 1;
            } else if ch == '\\' && index + 1 < size && ESCAPABLE.contains(self.text[index + 1]) {
                self.buffer.push(self.text[index + 1]);
                self.in_word = true;
                index += 2;
            } else if ch == ' ' || ch == '\t' || ch == '\r' {
                self.finish_word();
                index += 1;
            } else if ch == '\n'
                || ch == ';'
                || ch == '`'
                || self.starts_with(index, '&', '&')
                || self.starts_with(index, '|', '|')
            {
                self.end_command();
                index += if self.starts_with(index, '&', '&') || self.starts_with(index, '|', '|') {
                    2
                } else {
                    1
                };
            } else if ch == '<'
                || ch == '>'
                || (ch == '&' && self.text.get(index + 1) == Some(&'>'))
            {
                if self.in_word
                    && !self.buffer.is_empty()
                    && self.buffer.chars().all(|c| c.is_ascii_digit())
                {
                    self.buffer.clear();
                    self.in_word = false;
                    self.quoted = false;
                } else {
                    self.finish_word();
                }
                while index < size && "<>&|".contains(self.text[index]) {
                    if self.text[index] == '|' && (index == 0 || self.text[index - 1] != '>') {
                        break;
                    }
                    index += 1;
                }
                self.skip_word = true;
            } else if ch == '|' || ch == '&' {
                self.end_command();
                index += 1;
            } else if ch == '(' {
                if self.buffer.ends_with('$') {
                    self.buffer.pop();
                    self.in_word = !self.buffer.is_empty();
                }
                self.skip_word = false;
                self.end_command();
                self.events.push(Event::Open);
                index += 1;
            } else if ch == ')' {
                self.end_command();
                self.events.push(Event::Close);
                index += 1;
            } else if (ch == '{' || ch == '}')
                && !self.in_word
                && (index + 1 >= size || " \t\r\n".contains(self.text[index + 1]))
            {
                self.end_command();
                index += 1;
            } else {
                self.buffer.push(ch);
                self.in_word = true;
                index += 1;
            }
        }
        self.end_command();
    }
}

/// Events of a command text, in execution order. With `powershell` a backslash inside double quotes is not an escape.
pub fn shell_events(command: &str, powershell: bool) -> Vec<Event> {
    let mut events = Vec::new();
    Lexer::new(command, 0, &mut events, powershell).run();
    events
}

/// The event lists to check: the Bash reading, the PowerShell reading when the text has a backslash-quote, and the cmd
/// reading when the text has `^` + newline.
pub fn shell_event_variants(command: &str) -> Vec<Vec<Event>> {
    let mut variants = vec![shell_events(command, false)];
    if command.contains("\\\"") {
        variants.push(shell_events(command, true));
    }
    if CMD_CONTINUATION.is_match(command) {
        let joined = CMD_CONTINUATION.replace_lit(command, "");
        variants.push(shell_events(&joined, false));
        if joined.contains("\\\"") {
            variants.push(shell_events(&joined, true));
        }
    }
    variants
}

/// Indexes in `words` where a program may start.
pub fn command_starts(words: &[String]) -> Vec<usize> {
    let mut index = 0;
    while index < words.len() && ASSIGNMENT_RE.is_match(&words[index]) {
        index += 1;
    }
    if index >= words.len() {
        return vec![];
    }
    let mut starts = vec![index];
    if WRAPPERS.contains(&executable_name(&words[index]).as_str())
        || WRAPPERS.contains(&words[index].as_str())
    {
        starts.extend(index + 1..words.len());
    }
    starts
}

/// One `git ...` invocation.
#[derive(Clone, Debug)]
pub struct GitCall {
    pub sub: String,
    pub args: Vec<String>,
    pub dirs_c: Vec<String>,
    pub extra_dirs: Vec<String>,
    pub alias_push: bool,
}

impl GitCall {
    pub fn is_push(&self) -> bool {
        self.sub == "push" || self.alias_push
    }

    /// `(force, mirror_or_all, positional)` of a push's arguments.
    pub fn push_facts(&self) -> (bool, bool, Vec<String>) {
        static FORCE_SHORT: LazyLock<Re> = LazyLock::new(|| Re::new(r"^-[A-Za-z]*f[A-Za-z]*\z"));
        let (mut force, mut mirror_all) = (false, false);
        let mut positional = Vec::new();
        let mut skip = false;
        for arg in &self.args {
            if skip {
                skip = false;
                continue;
            }
            if PUSH_OPTIONS_WITH_VALUE.contains(&arg.as_str()) {
                skip = true;
            } else if arg.starts_with('-') {
                if arg == "--force"
                    || arg.starts_with("--force-with-lease")
                    || FORCE_SHORT.is_match(arg)
                {
                    force = true;
                } else if arg == "--mirror"
                    || arg == "--all"
                    || arg.starts_with("--mirror=")
                    || arg.starts_with("--all=")
                {
                    mirror_all = true;
                }
            } else if arg.starts_with('+') && arg.chars().count() > 1 {
                force = true;
                positional.push(arg.clone());
            } else {
                positional.push(arg.clone());
            }
        }
        (force, mirror_all, positional)
    }
}

fn parse_git(words: &[String], start: usize, assignments: &[String]) -> Option<GitCall> {
    static INLINE_DIR: LazyLock<Re> = LazyLock::new(|| Re::new(r"^--(?:git-dir|work-tree)=(.+)"));
    static ALIAS_KEY: LazyLock<Re> = LazyLock::new(|| Re::ci(r"^alias\."));
    let mut index = start + 1;
    let mut dirs_c: Vec<String> = vec![];
    let mut extra_dirs: Vec<String> = vec![];
    let mut config: Vec<(String, String)> = vec![];
    for assignment in assignments {
        let (name, _, value) = crate::pyre::partition(assignment, "=");
        let upper = name.to_uppercase();
        if (upper == "GIT_DIR" || upper == "GIT_WORK_TREE") && !value.is_empty() {
            extra_dirs.push(value.to_string());
        }
    }
    while index < words.len() && words[index].starts_with('-') {
        let token = words[index].as_str();
        let has_value = index + 1 < words.len();
        if token == "-C" && has_value {
            dirs_c.push(words[index + 1].clone());
            index += 2;
        } else if token == "-c" && has_value {
            let (key, _, value) = crate::pyre::partition(&words[index + 1], "=");
            let key = key.to_lowercase();
            if let Some(slot) = config.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = value.to_string();
            } else {
                config.push((key, value.to_string()));
            }
            index += 2;
        } else if GIT_OPTIONS_WITH_VALUE.contains(&token) && has_value {
            if token == "--git-dir" || token == "--work-tree" {
                extra_dirs.push(words[index + 1].clone());
            }
            index += 2;
        } else {
            if let Some(m) = INLINE_DIR.match_start(token) {
                extra_dirs.push(m.group(1).unwrap_or("").to_string());
            }
            index += 1;
        }
    }
    if index >= words.len() {
        return None;
    }
    if words[index].trim().chars().any(char::is_whitespace) {
        // `Start-Process git -ArgumentList "push origin main"`: subcommand and arguments came as one word
        let mut rebuilt: Vec<String> = words[..index].to_vec();
        rebuilt.extend(split_ws(&words[index]).into_iter().map(String::from));
        rebuilt.extend(words[index + 1..].iter().cloned());
        return parse_git(&rebuilt, start, assignments);
    }
    let raw_sub = words[index].clone();
    let mut sub = raw_sub.to_lowercase();
    let args: Vec<String> = words[index + 1..].to_vec();
    let mut alias_push = false;
    if let Some((_, alias_value)) = config.iter().find(|(k, _)| *k == format!("alias.{sub}")) {
        let stripped = alias_value.trim();
        let first = split_ws(stripped)
            .first()
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        if stripped.starts_with('!') || first == "push" {
            alias_push = true;
        }
        if !first.is_empty() {
            sub = first;
        }
    }
    if sub == "config" && args.iter().any(|a| ALIAS_KEY.is_match(a)) {
        let value = args
            .iter()
            .filter(|a| !ALIAS_KEY.is_match(a) && !a.starts_with('-'))
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let value = value.trim();
        let lowered = value.to_lowercase();
        let first_word = split_ws(&lowered).first().copied().unwrap_or("");
        if value.starts_with('!') || first_word == "push" || split_ws(&lowered).contains(&"push") {
            alias_push = true;
        }
    }
    if sub != "push" && config.iter().any(|(k, _)| DANGEROUS_CONFIG.is_match(k)) {
        sub = "config-exec".to_string();
    }
    Some(GitCall {
        sub,
        args,
        dirs_c,
        extra_dirs,
        alias_push,
    })
}

/// A `GitCall` for every git invocation in one command's words (after assignments and wrappers).
pub fn git_calls_in(words: &[String]) -> Vec<GitCall> {
    let mut calls = Vec::new();
    for start in command_starts(words) {
        if executable_name(&words[start]) == "git" {
            let assignments: Vec<String> = words[..start]
                .iter()
                .filter(|w| ASSIGNMENT_RE.is_match(w))
                .cloned()
                .collect();
            if let Some(call) = parse_git(words, start, &assignments) {
                calls.push(call);
            }
        }
    }
    calls
}

#[derive(Clone, Debug)]
pub struct GhCall {
    pub sub: String,
    pub sub2: String,
    pub args: Vec<String>,
}

/// A `GhCall` for every gh invocation in one command's words; flags before the subcommand are skipped.
pub fn gh_calls_in(words: &[String]) -> Vec<GhCall> {
    let mut calls = Vec::new();
    for start in command_starts(words) {
        if executable_name(&words[start]) != "gh" {
            continue;
        }
        let mut index = start + 1;
        let mut positional: Vec<String> = vec![];
        while index < words.len() && positional.len() < 2 {
            let word = &words[index];
            if word.starts_with('-') {
                index += if GH_OPTIONS_WITH_VALUE.contains(&word.as_str()) {
                    2
                } else {
                    1
                };
                continue;
            }
            positional.push(word.to_lowercase());
            index += 1;
        }
        calls.push(GhCall {
            sub: positional.first().cloned().unwrap_or_default(),
            sub2: positional.get(1).cloned().unwrap_or_default(),
            args: words[start + 1..].to_vec(),
        });
    }
    calls
}

/// `gh api ...` is not a plain read: an explicit method other than GET, else any field/input flag.
pub fn gh_api_writes(args: &[String]) -> bool {
    static METHOD_SHORT: LazyLock<Re> = LazyLock::new(|| Re::new(r"^-X\w+\z"));
    static FIELD_EQ: LazyLock<Re> = LazyLock::new(|| Re::new(r"^--(?:field|raw-field|input)="));
    static FIELD_SHORT: LazyLock<Re> = LazyLock::new(|| Re::new(r"^-[fF]."));
    let mut method: Option<String> = None;
    let mut fields = false;
    for (position, arg) in args.iter().enumerate() {
        let lowered = arg.to_lowercase();
        if (lowered == "-x" || lowered == "--method") && position + 1 < args.len() {
            method = Some(args[position + 1].to_uppercase());
        } else if lowered.starts_with("--method=") {
            method = Some(crate::pyre::partition(arg, "=").2.to_uppercase());
        } else if METHOD_SHORT.is_match(arg) {
            method = Some(arg[2..].to_uppercase());
        } else if ["-f", "-F", "--field", "--raw-field", "--input"].contains(&arg.as_str())
            || FIELD_EQ.is_match(arg)
            || FIELD_SHORT.is_match(arg)
        {
            fields = true;
        }
    }
    match method {
        Some(m) if !m.is_empty() => m != "GET",
        _ => fields,
    }
}

/// `(start, end)` (character offsets) of every CLOSED quoted string.
pub fn quoted_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (index, ch) in text.chars().enumerate() {
        match quote {
            Some(q) => {
                if ch == q {
                    ranges.push((start, index));
                    quote = None;
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                    start = index;
                }
            }
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<Vec<String>> {
        shell_events(text, false)
            .into_iter()
            .filter_map(|e| if let Event::Cmd(w) = e { Some(w) } else { None })
            .collect()
    }

    fn calls(text: &str) -> Vec<GitCall> {
        words(text).iter().flat_map(|w| git_calls_in(w)).collect()
    }

    #[test]
    fn plain_push_is_found() {
        let c = calls("git push origin main");
        assert_eq!(c.len(), 1);
        assert!(c[0].is_push());
        assert_eq!(c[0].push_facts().2, vec!["origin", "main"]);
    }

    #[test]
    fn options_wrappers_and_subshell() {
        assert!(calls("git -C /x -c a=b push")
            .first()
            .is_some_and(GitCall::is_push));
        assert!(calls("VAR=1 env git push")
            .first()
            .is_some_and(GitCall::is_push));
        assert!(calls("echo $(git push)")
            .first()
            .is_some_and(GitCall::is_push));
        assert!(calls("bash -c \"cd x && git push\"")
            .first()
            .is_some_and(GitCall::is_push));
    }

    #[test]
    fn alias_push_counts() {
        assert!(calls("git -c alias.p=push p")
            .first()
            .is_some_and(GitCall::is_push));
        assert!(calls("git -c alias.x=!sh x")
            .first()
            .is_some_and(GitCall::is_push));
    }

    #[test]
    fn quoted_word_with_spaces_is_lexed_again_like_the_reference() {
        // a quoted word that holds blanks may be a command line (`bash -c "..."`), so the push inside is seen
        assert!(calls("echo 'git push'")
            .first()
            .is_some_and(GitCall::is_push));
        assert!(calls("echo 'git'").is_empty());
    }

    #[test]
    fn force_facts() {
        let c = calls("git push -f origin +feature");
        let (force, mirror, pos) = c[0].push_facts();
        assert!(force && !mirror);
        assert_eq!(pos, vec!["origin", "+feature"]);
        assert!(calls("git push --mirror")[0].push_facts().1);
    }

    #[test]
    fn dangerous_config_changes_subcommand() {
        assert_eq!(calls("git -c core.pager=evil log")[0].sub, "config-exec");
    }

    #[test]
    fn gh_calls() {
        let w = words("gh -R x/y pr create");
        let g = gh_calls_in(&w[0]);
        assert_eq!((g[0].sub.as_str(), g[0].sub2.as_str()), ("pr", "create"));
        assert!(gh_api_writes(&[
            "repos/x".into(),
            "-f".into(),
            "a=b".into()
        ]));
        assert!(!gh_api_writes(&["repos/x".into()]));
        assert!(!gh_api_writes(&[
            "-X".into(),
            "GET".into(),
            "repos/x".into()
        ]));
    }

    #[test]
    fn executable_names() {
        assert_eq!(
            executable_name("C:\\Program Files\\Git\\bin\\git.exe"),
            "git"
        );
        assert_eq!(executable_name("\\rm"), "rm");
        assert_eq!(executable_name("/usr/bin/gh"), "gh");
    }

    #[test]
    fn continuation_joins_lines() {
        assert!(calls("git \\\npush").first().is_some_and(GitCall::is_push));
    }
}
