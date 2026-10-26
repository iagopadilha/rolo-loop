// SPDX-License-Identifier: MIT
//! Path spelling (port of `guard_paths.py` and the path half of `guard.py`): ONE canonical form
//! (`normalize_path`) every path rule compares - protected files, credentials, scope, copied secrets.

use crate::pyre::{nt_abspath, nt_dirname, nt_isabs, nt_join, nt_normpath, posix_normpath, Re};
use cfg::Env;
use std::path::Path;
use std::sync::LazyLock;

/// Everything the guard reads from its surroundings, injectable for tests.
#[derive(Clone, Debug)]
pub struct G {
    pub env: Env,
    /// Paths are compared case-insensitively only where the filesystem is (Windows).
    pub case_insensitive: bool,
}

impl G {
    pub fn from_process() -> G {
        G {
            env: Env::process(),
            case_insensitive: cfg!(windows),
        }
    }

    pub fn new(env: Env) -> G {
        G {
            env,
            case_insensitive: cfg!(windows),
        }
    }

    pub fn fold(&self, text: &str) -> String {
        if self.case_insensitive {
            text.to_lowercase()
        } else {
            text.to_string()
        }
    }

    /// `norm`: `normpath` with forward slashes, no trailing slash, folded.
    pub fn norm(&self, path: &str) -> String {
        let normalized = if cfg!(windows) {
            nt_normpath(path)
        } else {
            posix_normpath(path)
        };
        self.fold(normalized.replace('\\', "/").trim_end_matches('/'))
    }

    pub fn home(&self) -> String {
        self.env.home_dir().display().to_string()
    }

    /// `os.path.expanduser` (Windows semantics).
    pub fn expanduser(&self, path: &str) -> String {
        if !path.starts_with('~') {
            return path.to_string();
        }
        let n = path.len();
        let mut i = 1;
        let bytes = path.as_bytes();
        while i < n && bytes[i] != b'\\' && bytes[i] != b'/' {
            i += 1;
        }
        let mut userhome = self.home();
        if i != 1 {
            let target = &path[1..i];
            let current = self.env.text("USERNAME");
            if target != current {
                userhome = nt_join(&nt_dirname(&userhome), target);
            }
        }
        format!("{userhome}{}", &path[i..])
    }

    /// `os.path.expandvars` (Windows: `$var`, `${var}`, `%var%`).
    pub fn expandvars(&self, path: &str) -> String {
        if !path.contains('$') && !path.contains('%') {
            return path.to_string();
        }
        let chars: Vec<char> = path.chars().collect();
        let n = chars.len();
        let mut res = String::new();
        let mut index = 0;
        let varchars = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
        while index < n {
            let c = chars[index];
            if c == '\'' {
                match chars[index + 1..].iter().position(|x| *x == '\'') {
                    Some(p) => {
                        res.extend(&chars[index..=index + 1 + p]);
                        index += p + 1;
                    }
                    None => {
                        res.extend(&chars[index..]);
                        index = n - 1;
                    }
                }
            } else if c == '%' {
                if index + 1 < n && chars[index + 1] == '%' {
                    res.push('%');
                    index += 1;
                } else {
                    match chars[index + 1..].iter().position(|x| *x == '%') {
                        None => {
                            res.extend(&chars[index..]);
                            index = n - 1;
                        }
                        Some(p) => {
                            let var: String = chars[index + 1..index + 1 + p].iter().collect();
                            index += p + 1;
                            match self.env.get(&var) {
                                Some(v) => res.push_str(&v),
                                None => {
                                    res.push('%');
                                    res.push_str(&var);
                                    res.push('%');
                                }
                            }
                        }
                    }
                }
            } else if c == '$' {
                if index + 1 < n && chars[index + 1] == '$' {
                    res.push('$');
                    index += 1;
                } else if index + 1 < n && chars[index + 1] == '{' {
                    match chars[index + 2..].iter().position(|x| *x == '}') {
                        None => {
                            res.extend(&chars[index..]);
                            index = n - 1;
                        }
                        Some(p) => {
                            let var: String = chars[index + 2..index + 2 + p].iter().collect();
                            index += p + 2;
                            match self.env.get(&var) {
                                Some(v) => res.push_str(&v),
                                None => {
                                    res.push_str("${");
                                    res.push_str(&var);
                                    res.push('}');
                                }
                            }
                        }
                    }
                } else {
                    let mut var = String::new();
                    index += 1;
                    while index < n && varchars(chars[index]) {
                        var.push(chars[index]);
                        index += 1;
                    }
                    match self.env.get(&var).filter(|_| !var.is_empty()) {
                        Some(v) => res.push_str(&v),
                        None => {
                            res.push('$');
                            res.push_str(&var);
                        }
                    }
                    if index < n {
                        index -= 1;
                    } else {
                        break;
                    }
                }
            } else {
                res.push(c);
            }
            index += 1;
        }
        res
    }

    /// `~`, `$HOME`, `${HOME}`, `%USERPROFILE%` and PowerShell `$env:USERPROFILE` expanded with the guard's own environment.
    pub fn expand_user_path(&self, token: &str) -> String {
        static PS_ENV: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\$env:(\w+)"));
        static PCT: LazyLock<Re> = LazyLock::new(|| Re::new(r"%(\w+)%"));
        let text = token
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .trim()
            .to_string();
        let text = PS_ENV.replace_all(&text, |m| {
            self.env
                .get(m.group(1).unwrap_or(""))
                .unwrap_or_else(|| m.text().to_string())
        });
        let text = PCT.replace_all(&text, |m| {
            self.env
                .get(m.group(1).unwrap_or(""))
                .unwrap_or_else(|| m.text().to_string())
        });
        self.expanduser(&self.expandvars(&text))
    }
}

// ---- Windows spellings ------------------------------------------------------------------------------------------------------

static PREFIX_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^//[?.]/"));
static DRIVE_COMPONENT: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[a-zA-Z]:\z"));
static SHORT_NAME: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"^[^/.~ ]{1,6}~\d{1,6}(?:\.[^/.]{0,3})?\n?\z"));
static GIT_BASH_DRIVE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"^/(?:mnt/|cygdrive/)?([a-zA-Z])(?=/|\n?\z)"));
static DRIVE_PREFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[a-zA-Z]:/"));
static MULTI_SLASH: LazyLock<Re> = LazyLock::new(|| Re::new(r"/{2,}"));

/// `//?/C:/x` and `//./C:/x` lose the device prefix, `//?/UNC/host/share` becomes `//host/share`, and the admin share of
/// this machine (`//localhost/C$/x`, `//127.0.0.1/C$/x`, `//[::1]/C$/x`, `//<this computer>/C$/x`) becomes `C:/x`.
pub fn strip_windows_prefixes(g: &G, text: &str) -> String {
    let mut text = text.to_string();
    if PREFIX_RE.is_match(&text) {
        text = text[4..].to_string();
        if text
            .get(..4)
            .is_some_and(|h| h.eq_ignore_ascii_case("UNC/"))
        {
            text = format!("//{}", &text[4..]);
        }
    }
    let computer = g
        .env
        .get("COMPUTERNAME")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "\0".to_string())
        .to_lowercase();
    let pattern = format!(
        r"^//(?:localhost|127\.0\.0\.1|\[?::1\]?|{})/([a-zA-Z])\$(?=/|\n?\z)",
        crate::pyre::escape(&computer)
    );
    Re::ci(&pattern).replace_all(&text, |m| format!("{}:", m.group(1).unwrap_or("")))
}

/// `/c/Users/x`, `/mnt/c/x`, `/cygdrive/c/x` -> `c:/Users/x` (Windows only).
pub fn git_bash_drive(text: &str) -> String {
    if !cfg!(windows) {
        return text.to_string();
    }
    GIT_BASH_DRIVE.replace_all(text, |m| format!("{}:", m.group(1).unwrap_or("")))
}

/// Windows only. Per path component: drop an alternate data stream, trailing dots and spaces; a component made only of
/// dots and spaces is the folder itself. The drive (`C:`) and `.` / `..` are left alone.
pub fn clean_components(text: &str) -> String {
    if !cfg!(windows) {
        return text.to_string();
    }
    let mut cleaned: Vec<String> = Vec::new();
    for (position, part) in text.split('/').enumerate() {
        if part == "." || part == ".." || (position == 0 && DRIVE_COMPONENT.is_match(part)) {
            cleaned.push(part.to_string());
            continue;
        }
        let mut part = part.to_string();
        if let Some(i) = part.find(':') {
            part.truncate(i);
        }
        let right_space = part.trim_end_matches(' ');
        if right_space == "." || right_space == ".." {
            part = right_space.to_string();
        } else if !part.trim_matches(|c| c == ' ' || c == '.').is_empty() {
            part = part.trim_end_matches([' ', '.']).to_string();
        } else if part.is_empty() {
            part = String::new();
        } else {
            part = ".".to_string();
        }
        cleaned.push(part);
    }
    cleaned.join("/")
}

pub fn has_short_name(text: &str) -> bool {
    cfg!(windows) && text.split('/').any(|part| SHORT_NAME.is_match(part))
}

fn long_path_name(path: &str) -> String {
    match std::fs::canonicalize(path) {
        Ok(p) => {
            let s = p.display().to_string();
            if let Some(rest) = s.strip_prefix("\\\\?\\UNC\\") {
                format!("\\\\{rest}")
            } else {
                s.strip_prefix("\\\\?\\").map(String::from).unwrap_or(s)
            }
        }
        Err(_) => String::new(),
    }
}

/// Windows only: the long form of the 8.3 short names in an absolute `text` (forward slashes).
pub fn expand_short_names(text: &str) -> String {
    if !has_short_name(text) || !DRIVE_PREFIX.is_match(text) {
        return text.to_string();
    }
    let parts: Vec<&str> = text.split('/').collect();
    for length in (2..=parts.len()).rev() {
        let prefix = parts[..length].join("/").replace('/', "\\");
        if !Path::new(&prefix).exists() {
            continue;
        }
        let long_prefix = long_path_name(&prefix);
        if !long_prefix.is_empty() {
            let mut out = vec![long_prefix.replace('\\', "/")];
            out.extend(parts[length..].iter().map(|s| s.to_string()));
            return out.join("/");
        }
        break;
    }
    text.to_string()
}

/// Canonical, forward-slash absolute form of a path as a tool or a shell may spell it. `fold_case` overrides the
/// platform default (`None` = the platform default).
pub fn normalize_path(g: &G, path: &str, base: Option<&str>, fold_case: Option<bool>) -> String {
    let mut text = path
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .replace('\\', "/");
    text = strip_windows_prefixes(g, &text);
    text = MULTI_SLASH.replace_lit(&text, "/");
    text = git_bash_drive(&text);
    text = clean_components(&text);
    if !(text.starts_with('/') || DRIVE_PREFIX.is_match(&text)) {
        if let Some(base) = base.filter(|b| !b.is_empty()) {
            text = format!("{}/{}", normalize_path(g, base, None, Some(false)), text);
        }
    }
    text = posix_normpath(&text).trim_end_matches('/').to_string();
    if cfg!(windows) && text.contains('~') {
        text = expand_short_names(&text);
    }
    if fold_case.unwrap_or(g.case_insensitive) {
        text.to_lowercase()
    } else {
        text
    }
}

/// True when `absolute` still holds an 8.3 short name the system could not expand AND what comes before it is a
/// protected folder (the project's `.specs/`, the user's home, or `~/.claude/`).
pub fn unresolved_short_name(g: &G, absolute: &str, specs_dir: Option<&str>) -> bool {
    if !cfg!(windows) || !has_short_name(absolute) {
        return false;
    }
    let parts: Vec<&str> = absolute.split('/').collect();
    let first = parts
        .iter()
        .position(|p| SHORT_NAME.is_match(p))
        .unwrap_or(0);
    let before = parts[..first].join("/");
    let home = normalize_path(g, &g.home(), None, Some(false));
    let claude = format!("{home}/.claude");
    let fold_before = before.to_lowercase();
    if fold_before == home.to_lowercase()
        || fold_before == claude.to_lowercase()
        || fold_before.starts_with(&format!("{}/", claude.to_lowercase()))
    {
        return true;
    }
    if let Some(specs) = specs_dir.filter(|s| !s.is_empty()) {
        let specs = g.norm(specs).to_lowercase();
        return fold_before == specs || fold_before.starts_with(&format!("{specs}/"));
    }
    false
}

/// The spellings of a tool or shell target the protected-file regexes are matched against: as written, canonical, and
/// (for an existing symlink/junction) the real target.
pub fn path_forms(g: &G, target: &str, cwd: Option<&str>) -> Vec<String> {
    let text = target
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();
    let mut forms = vec![text.clone()];
    forms.push(normalize_path(g, &text, cwd, None));
    if !text.is_empty() && !text.contains('\0') {
        let candidate = g.expanduser(&text);
        let candidate = match cwd.filter(|c| !c.is_empty()) {
            Some(cwd) if !nt_isabs(&candidate) => nt_join(cwd, &candidate),
            _ => candidate,
        };
        if std::fs::symlink_metadata(&candidate).is_ok() {
            let real = long_path_name(&nt_abspath(&candidate));
            let real = if real.is_empty() {
                nt_abspath(&candidate)
            } else {
                real
            };
            let real = normalize_path(g, &real, None, None);
            if !forms.contains(&real) {
                forms.push(real);
            }
        }
    }
    forms
}

/// A protected-file pattern tested against every spelling of `target`.
pub fn path_matches(g: &G, pattern: &Re, target: &str, cwd: Option<&str>) -> bool {
    path_forms(g, target, cwd)
        .iter()
        .any(|form| pattern.is_match(form))
}

/// True when a folder above `absolute` holds a `.git` entry (the path is inside a git working tree).
pub fn has_git_ancestor(absolute: &str) -> bool {
    let mut here = nt_dirname(absolute.trim_end_matches('/'));
    while !here.is_empty() {
        if Path::new(&here).join(".git").exists() {
            return true;
        }
        let parent = nt_dirname(&here);
        if parent == here {
            break;
        }
        here = parent;
    }
    false
}

/// The real temp folders of this machine, normalized.
pub fn temp_roots(g: &G) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    for name in ["TMPDIR", "TEMP", "TMP"] {
        if let Some(v) = g.env.get(name).filter(|v| !v.is_empty()) {
            if Path::new(&v).is_dir() && !candidates.contains(&v) {
                candidates.push(v);
            }
        }
    }
    if candidates.is_empty() {
        candidates.push(nt_join(&g.home(), "AppData\\Local\\Temp"));
    }
    for name in ["TMPDIR", "TMP", "TEMP"] {
        if let Some(v) = g.env.get(name).filter(|v| !v.is_empty()) {
            if !candidates.contains(&v) {
                candidates.push(v);
            }
        }
    }
    let mut roots: Vec<String> = Vec::new();
    for item in candidates {
        if item.is_empty() || !nt_isabs(&item) {
            continue;
        }
        let real = long_path_name(&item);
        for form in [item.clone(), real] {
            if form.is_empty() {
                continue;
            }
            let root = normalize_path(g, &form, None, None);
            if !root.is_empty()
                && root != "/"
                && root != "c:"
                && !roots.contains(&root)
                && !Re::ci(r"^[a-z]:\z").is_match(&root)
            {
                roots.push(root);
            }
        }
    }
    roots
}

pub fn inside_any(absolute: &str, roots: &[String]) -> bool {
    roots
        .iter()
        .any(|root| absolute == root || absolute.starts_with(&format!("{root}/")))
}

/// Genuine temp files only: inside the OS temp folder AND not inside any git working tree.
pub fn temp_exempt(g: &G, absolute: &str) -> bool {
    inside_any(absolute, &temp_roots(g)) && !has_git_ancestor(absolute)
}

/// Windows: a path past MAX_PATH needs the `\\?\` prefix to be opened.
pub fn long_path(path: &str) -> String {
    if !cfg!(windows) || path.starts_with("\\\\?\\") {
        return path.to_string();
    }
    let absolute = nt_abspath(path);
    if absolute.chars().count() >= 248 {
        format!("\\\\?\\{absolute}")
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g() -> G {
        G::new(Env::from_pairs([
            ("USERPROFILE", r"C:\Users\tester"),
            ("COMPUTERNAME", "BOX1"),
            ("USERNAME", "tester"),
        ]))
    }

    #[test]
    fn spellings_collapse_to_one_form() {
        let g = g();
        let canon = "c:/users/tester/.claude/.credentials.json";
        for spelling in [
            r"C:\Users\tester\.claude\.credentials.json",
            r"\\?\C:\Users\tester\.claude\.credentials.json",
            "C://Users//tester//.claude//.credentials.json",
            "/c/Users/tester/.claude/.credentials.json",
            "/mnt/c/Users/tester/.claude/.credentials.json",
            r"C:\Users\tester\.claude\.credentials.json.",
            r"C:\Users\tester\.claude\.credentials.json::$DATA",
            r"C:\Users\tester\.claude\..\.claude\.credentials.json",
            r"\\localhost\C$\Users\tester\.claude\.credentials.json",
            r"\\BOX1\C$\Users\tester\.claude\.credentials.json",
        ] {
            assert_eq!(
                normalize_path(&g, spelling, None, None),
                canon,
                "{spelling}"
            );
        }
    }

    #[test]
    fn local_share_prefix() {
        let g = g();
        assert_eq!(
            strip_windows_prefixes(&g, "//BOX1/C$/Users/x"),
            "C:/Users/x"
        );
        assert_eq!(
            strip_windows_prefixes(&g, "//localhost/C$/Users/x"),
            "C:/Users/x"
        );
        assert_eq!(
            strip_windows_prefixes(&g, "//other/C$/Users/x"),
            "//other/C$/Users/x"
        );
    }

    #[test]
    fn relative_paths_use_the_base() {
        let g = g();
        assert_eq!(
            normalize_path(&g, "../x/./y", Some(r"C:\work\proj"), None),
            "c:/work/x/y"
        );
    }

    #[test]
    fn expansion_rules() {
        let g = g();
        assert_eq!(g.expand_user_path("~/.ssh/id"), r"C:\Users\tester/.ssh/id");
        assert_eq!(
            g.expand_user_path("$env:USERPROFILE\\x"),
            r"C:\Users\tester\x"
        );
        assert_eq!(g.expand_user_path("%USERPROFILE%/x"), r"C:\Users\tester/x");
        assert_eq!(g.expand_user_path("${USERPROFILE}/x"), r"C:\Users\tester/x");
        assert_eq!(g.expand_user_path("$NOPE/x"), "$NOPE/x");
    }

    #[test]
    fn short_name_detection() {
        assert!(has_short_name("c:/users/claude~1.jso"));
        assert!(!has_short_name("c:/users/normal"));
    }

    #[test]
    fn inside_any_checks_boundaries() {
        let roots = vec!["c:/proj".to_string()];
        assert!(inside_any("c:/proj", &roots));
        assert!(inside_any("c:/proj/a", &roots));
        assert!(!inside_any("c:/proj2/a", &roots));
    }

    #[test]
    fn norm_folds_and_strips() {
        let g = g();
        assert_eq!(g.norm(r"C:\Proj\A\"), "c:/proj/a");
    }
}
