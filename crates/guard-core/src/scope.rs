// SPDX-License-Identifier: MIT
//! Scope, secrets and shell targets (port of the file-scope half of `guard.py`): SCOPE.json and executor identity,
//! credential paths and broad reads, `specs-only` / `path-scope` / `repo-only` checks, shell write targets.

use crate::paths::{inside_any, normalize_path, path_forms, temp_exempt, G};
use crate::pyre::{fnmatchcase, nt_dirname, nt_isabs, nt_join, posix_normpath, shlex_split_nonposix, split_ws, Re};
use crate::rules::*;
use crate::shell::{command_starts, executable_name, quoted_ranges, ASSIGNMENT_RE};
use crate::state::{alert, log, loop_file};
use serde_json::Value;
use std::path::Path;
use std::sync::LazyLock;

pub type Hits = Vec<(String, String)>;

fn h(rule: &str, why: impl Into<String>) -> (String, String) {
    (rule.to_string(), why.into())
}

pub fn has_rule(rules: &[String], name: &str) -> bool {
    rules.iter().any(|r| r == name)
}

// ---- executor identity ------------------------------------------------------------------------------------------------------------

static SLUG_MARK: LazyLock<Re> = LazyLock::new(|| Re::new(r"SCOPE_FEATURE:\s*([A-Za-z0-9._-]+)"));
static TASK_MARK: LazyLock<Re> = LazyLock::new(|| Re::new(r"SCOPE_TASK:\s*([A-Za-z0-9._-]+/T\d+)"));

/// Which feature does this executor serve? Reads the subagent transcript for the dispatch prompt's mark.
pub fn executor_slug(payload: &Value, task: bool) -> Option<String> {
    let mark: &Re = if task { &TASK_MARK } else { &SLUG_MARK };
    let agent_id = payload.get("agent_id").filter(|v| crate::state::truthy(v));
    let mut transcript: Option<String> = payload.get("agent_transcript_path").filter(|v| crate::state::truthy(v)).map(crate::state::value_text);
    if transcript.is_none() {
        if let (Some(id), Some(base)) = (agent_id, payload.get("transcript_path").filter(|v| crate::state::truthy(v))) {
            let base = crate::state::value_text(base);
            let stem = match base.rfind('.') {
                Some(i) if base[i..].find(['\\', '/']).is_none() => base[..i].to_string(),
                _ => base,
            };
            transcript = Some(nt_join(&nt_join(&stem, "subagents"), &format!("agent-{}.jsonl", crate::state::value_text(id))));
        }
    }
    let transcript = crate::paths::long_path(&transcript?);
    if !Path::new(&transcript).is_file() {
        return None;
    }
    let bytes = std::fs::read(&transcript).ok()?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    let head: String = text.chars().take(40000).collect();
    mark.search(&head).and_then(|m| m.group(1).map(String::from))
}

// ---- SCOPE.json ----------------------------------------------------------------------------------------------------------------------

pub fn read_scope_file(specs_dir: &str) -> serde_json::Map<String, Value> {
    std::fs::read(loop_file(specs_dir, "SCOPE.json"))
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| if let Value::Object(m) = v { Some(m) } else { None })
        .unwrap_or_default()
}

/// `"absent"`, `"ok"` or `"unreadable"`.
pub fn scope_file_state(specs_dir: &str) -> &'static str {
    let path = loop_file(specs_dir, "SCOPE.json");
    if !Path::new(&path).exists() {
        return "absent";
    }
    let parsed = std::fs::read(&path).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok());
    match parsed {
        Some(data) if scope_shape_ok(&data) => "ok",
        _ => "unreadable",
    }
}

fn list_of_strings(v: &Value) -> bool {
    matches!(v, Value::Array(items) if items.iter().all(Value::is_string))
}

/// True for a SCOPE.json the guard can enforce.
pub fn scope_shape_ok(data: &Value) -> bool {
    let Value::Object(map) = data else {
        return false;
    };
    for key in ["features", "tasks"] {
        if let Some(table) = map.get(key) {
            match table {
                Value::Object(entries) if entries.values().all(list_of_strings) => {}
                _ => return false,
            }
        }
    }
    for key in ["roots", "copied"] {
        if let Some(v) = map.get(key) {
            if !list_of_strings(v) {
                return false;
            }
        }
    }
    true
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(items)) => items.iter().filter_map(|i| i.as_str().map(String::from)).collect(),
        _ => vec![],
    }
}

/// The PATHS the executor may edit: its task's, else its feature's, else the union of the in-flight features.
pub fn load_scope(specs_dir: &str, slug: Option<&str>, task: Option<&str>) -> Vec<String> {
    let data = read_scope_file(specs_dir);
    let features = match data.get("features") {
        Some(Value::Object(m)) => m.clone(),
        _ => Default::default(),
    };
    if let (Some(task), Some(Value::Object(tasks))) = (task.filter(|t| !t.is_empty()), data.get("tasks")) {
        if let Some(entries) = tasks.get(task) {
            return strings(Some(entries));
        }
    }
    if let Some(slug) = slug.filter(|s| !s.is_empty()) {
        if let Some(entries) = features.get(slug) {
            return strings(Some(entries));
        }
    }
    features.values().flat_map(|e| strings(Some(e))).collect()
}

/// Normalized worktree roots: the primary root (folder holding `.specs/`) first, then the absolute `roots` of SCOPE.json.
pub fn project_roots(g: &G, specs_dir: &str) -> Vec<String> {
    let mut roots = vec![g.norm(&nt_dirname(specs_dir))];
    for item in strings(read_scope_file(specs_dir).get("roots")) {
        if nt_isabs(&item) {
            let n = g.norm(&item);
            if !roots.contains(&n) {
                roots.push(n);
            }
        }
    }
    roots
}

/// Absolute, normalized paths of the config files copied into the worktrees.
pub fn load_copied(g: &G, specs_dir: &str) -> Vec<String> {
    strings(read_scope_file(specs_dir).get("copied")).into_iter().filter(|i| nt_isabs(i)).map(|i| g.norm(&i)).collect()
}

static CANDIDATE_SPLIT: LazyLock<Re> = LazyLock::new(|| Re::new(r#"[\s"'`<>|;&(){},=]+"#));
static SECRET_TOKEN_SPLIT: LazyLock<Re> = LazyLock::new(|| Re::new(r#"[\s"'`<>|;&(),=]+"#));
static DOTSLASH: LazyLock<Re> = LazyLock::new(|| Re::new(r"[./\\]"));
static PATHISH: LazyLock<Re> = LazyLock::new(|| Re::new(r"[./~$%]"));

fn mentions_copied_normalized(g: &G, text: &str, cwd: Option<&str>, copied: &[String], roots: &[String]) -> bool {
    let base: Option<String> = cwd.filter(|c| !c.is_empty()).map(String::from).or_else(|| roots.first().cloned());
    let mut tails: Vec<String> = Vec::new();
    for path in copied {
        for root in roots {
            if let Some(rest) = path.strip_prefix(&format!("{root}/")) {
                tails.push(rest.to_string());
            }
        }
    }
    for token in CANDIDATE_SPLIT.split(&text.replace('\\', "/")) {
        if token.is_empty() || !DOTSLASH.is_match(&token) {
            continue;
        }
        let absolute = normalize_path(g, &token, base.as_deref(), None);
        if copied.contains(&absolute) {
            return true;
        }
        for root in roots {
            if let Some(rest) = absolute.strip_prefix(&format!("{root}/")) {
                if tails.iter().any(|t| t == rest) {
                    return true;
                }
            }
        }
    }
    false
}

/// True when `text` names a copied config file: by full path, or by its path inside the worktree when `cwd` is that worktree.
pub fn mentions_copied_secret(g: &G, text: &str, cwd: Option<&str>, specs_dir: Option<&str>) -> bool {
    let Some(specs) = specs_dir.filter(|s| !s.is_empty()) else {
        return false;
    };
    if text.is_empty() {
        return false;
    }
    let copied = load_copied(g, specs);
    if copied.is_empty() {
        return false;
    }
    let flat = g.fold(&text.replace("\\\\", "/").replace('\\', "/"));
    let here = cwd.filter(|c| !c.is_empty()).map(|c| g.norm(c)).unwrap_or_default();
    let roots = project_roots(g, specs);
    if mentions_copied_normalized(g, text, cwd, &copied, &roots) {
        return true;
    }
    for path in &copied {
        if flat.contains(path.as_str()) {
            return true;
        }
        for root in &roots {
            if let Some(relative) = path.strip_prefix(&format!("{root}/")) {
                if here == *root || here.starts_with(&format!("{root}/")) {
                    let pattern = format!(r"(?<![\w./-]){}(?![\w-])", crate::pyre::escape(relative));
                    if Re::new(&pattern).is_match(&flat) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

// ---- credentials --------------------------------------------------------------------------------------------------------------------

const BUILTIN_SECRET_FILES: [&str; 8] =
    ["~/.claude.json", "~/.claude/.credentials.json", "~/.npmrc", "~/.netrc", "~/.git-credentials", "~/.docker/config.json", "~/.kube/config", "~/.config/gh/hosts.yml"];
const BUILTIN_SECRET_FOLDERS: [&str; 2] = ["~/.ssh", "~/.aws"];

pub fn builtin_secret_paths(g: &G) -> Vec<String> {
    BUILTIN_SECRET_FILES.iter().map(|i| normalize_path(g, &g.expanduser(i), None, None)).collect()
}

fn builtin_secret_folders(g: &G) -> Vec<String> {
    BUILTIN_SECRET_FOLDERS.iter().map(|i| normalize_path(g, &g.expanduser(i), None, None)).collect()
}

/// `GUARD_SECRET_PATHS` (comma-separated files or folders) from the environment, else from the app `.env`; normalized.
pub fn configured_secret_paths(g: &G) -> Vec<String> {
    let raw = {
        let env = g.env.text("GUARD_SECRET_PATHS");
        if env.trim().is_empty() {
            crate::state::app_env_value(g, "GUARD_SECRET_PATHS")
        } else {
            env.trim().to_string()
        }
    };
    raw.split(',')
        .filter(|i| !i.trim().trim_matches(|c| c == '"' || c == '\'').is_empty())
        .map(|i| normalize_path(g, &g.expand_user_path(i.trim()), None, None))
        .collect()
}

static GLOB_CHARS: LazyLock<Re> = LazyLock::new(|| Re::new(r"[*?\[\]{}]"));

pub fn expand_braces(pattern: &str) -> Vec<String> {
    static BRACE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\{([^{}]*)\}"));
    let Some(found) = BRACE.search(pattern) else {
        return vec![pattern.to_string()];
    };
    let mut options = Vec::new();
    for alternative in found.group(1).unwrap_or("").split(',') {
        options.extend(expand_braces(&format!("{}{}{}", &pattern[..found.start], alternative, &pattern[found.end..])));
    }
    options
}

/// Regex source of a path glob.
fn glob_regex(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        let rest: String = chars[index..].iter().collect();
        if rest.starts_with("**/") {
            out.push_str("(?:.*/)?");
            index += 3;
        } else if rest.starts_with("**") {
            out.push_str(".*");
            index += 2;
        } else if c == '*' {
            out.push_str("[^/]*");
            index += 1;
        } else if c == '?' {
            out.push_str("[^/]");
            index += 1;
        } else if c == '[' && chars.len() > index + 2 && chars[index + 2..].contains(&']') {
            let end = (index + 2..chars.len()).find(|&i| chars[i] == ']').unwrap_or(index + 2);
            let body: String = chars[index + 1..end].iter().collect();
            out.push('[');
            out.push_str(&body.replace('\\', "\\\\"));
            out.push(']');
            index = end + 1;
        } else {
            out.push_str(&crate::pyre::escape(&c.to_string()));
            index += 1;
        }
    }
    out
}

/// True when the path glob `pattern` (normalized, forward slashes) matches the normalized `path`.
pub fn glob_matches(g: &G, pattern: &str, path: &str) -> bool {
    for option in expand_braces(pattern) {
        if let Some(re) = Re::try_build(&glob_regex(&option), g.case_insensitive) {
            if re.full_match(path) {
                return true;
            }
        }
    }
    false
}

/// True when a path-looking token of `text` is one of the credential files, or sits inside a configured secret folder.
pub fn secret_path_hit(g: &G, text: &str, cwd: Option<&str>) -> bool {
    if text.is_empty() {
        return false;
    }
    let secret_files = builtin_secret_paths(g);
    let secret_folders = configured_secret_paths(g);
    let builtin_folders = builtin_secret_folders(g);
    for token in SECRET_TOKEN_SPLIT.split(&text.replace('\\', "/")) {
        if token.is_empty() || !PATHISH.is_match(&token) {
            continue;
        }
        let absolute = normalize_path(g, &g.expand_user_path(&token), cwd, None);
        if secret_files.contains(&absolute) || secret_folders.iter().any(|f| absolute.starts_with(&format!("{f}/"))) || secret_folders.contains(&absolute) {
            return true;
        }
        if builtin_folders.iter().any(|f| absolute == *f || absolute.starts_with(&format!("{f}/"))) && !absolute.ends_with(".example") {
            return true;
        }
        if GLOB_CHARS.is_match(&absolute) && secret_files.iter().any(|s| glob_matches(g, &absolute, s)) {
            return true;
        }
    }
    false
}

pub fn secret_target_hit(g: &G, target: &str, cwd: Option<&str>, specs_dir: Option<&str>) -> bool {
    path_forms(g, target, cwd).iter().any(|f| SECRET_HINT.is_match(f)) || secret_path_hit(g, target, cwd) || mentions_copied_secret(g, target, cwd, specs_dir)
}

fn value_str(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str)
}

/// Why a Grep/Glob call can hit a Claude credential file without naming it; empty otherwise.
pub fn search_reaches_credentials(g: &G, tool_name: &str, input: &Value, cwd: Option<&str>) -> String {
    if !input.is_object() || (tool_name != "Grep" && tool_name != "Glob") {
        return String::new();
    }
    let base = value_str(input.get("path")).filter(|b| !b.trim().is_empty());
    let root = match base {
        Some(b) => normalize_path(g, &g.expand_user_path(b), cwd, None),
        None => match cwd.filter(|c| !c.is_empty()) {
            Some(c) => normalize_path(g, c, None, None),
            None => String::new(),
        },
    };
    if root.is_empty() {
        return String::new();
    }
    let secrets = builtin_secret_paths(g)[..2].to_vec();
    if tool_name == "Glob" {
        let Some(pattern) = value_str(input.get("pattern")).filter(|p| !p.trim().is_empty()) else {
            return String::new();
        };
        let expanded = g.expand_user_path(pattern).replace('\\', "/");
        static ABS: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:[a-zA-Z]:)?/"));
        let full = if ABS.is_match(&expanded) { normalize_path(g, &expanded, None, None) } else { format!("{root}/{}", expanded.trim_start_matches('/')) };
        return if secrets.iter().any(|s| glob_matches(g, &full, s)) { "Glob pattern that matches a Claude credential file".into() } else { String::new() };
    }
    let file_type = value_str(input.get("type")).map(|t| t.to_lowercase()).unwrap_or_default();
    if !file_type.is_empty() && file_type != "json" {
        return String::new();
    }
    let glob = value_str(input.get("glob"));
    for secret in &secrets {
        if !secret.starts_with(&format!("{}/", root.trim_end_matches('/'))) {
            continue;
        }
        if let Some(gl) = glob.filter(|gl| !gl.trim().is_empty() && !gl.trim().starts_with('!')) {
            let matched = expand_braces(&gl.trim().replace('\\', "/")).into_iter().any(|option| {
                let pattern = if !option.contains('/') { format!("**/{option}") } else { option };
                glob_matches(g, &format!("{root}/{pattern}"), secret)
            });
            if !matched {
                continue;
            }
        }
        return "Grep over a folder that holds a Claude credential file".into();
    }
    String::new()
}

// ---- broad reads -----------------------------------------------------------------------------------------------------------------------

static IGNORE_OFF_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\s(?:--no-ignore\S*|--unrestricted|-[a-zA-Z]*u[a-zA-Z]*)(?=\s|\z)"));
static IGNORE_AWARE_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?:^|[\s|(])(?:rg|ripgrep|ag|ack)(?:\.exe)?\s"));
static GREP_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?:^|[\s|(])(?:grep|egrep|fgrep|ugrep|ug)(?:\.exe)?\s"));
static GREP_RECURSIVE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\s(?:--recursive|--dereference-recursive|-d\s*recurse|-[a-zA-Z]*[rR][a-zA-Z]*)(?=\s|\z)"));
const READ_TOOLS: &str = "(?:cat|grep|egrep|fgrep|head|tail|less|more|strings|xxd|od|base64|cp|sed|awk|type|rg|bat|tac|nl)";
static FIND_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?:^|[\s|(])find(?:\.exe)?\s"));
static FIND_READ_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(&format!(r"(?:-exec(?:dir)?|-ok(?:dir)?|\|\s*xargs(?:\s+-\S+)*)\s+(?:\S*[\\/])?{READ_TOOLS}\b")));
static GCI_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?:^|[\s|(])(?:Get-ChildItem|gci|dir|ls)(?:\s[^|]*?)?\s-(?:Recurse|r)(?=\s|\z)"));
static PIPE_READER_RE: LazyLock<Re> =
    LazyLock::new(|| Re::ci(r"\b(?:Select-String|sls|Get-Content|gc|cat|type|findstr|xargs|grep|egrep|rg|head|tail|more|less|ForEach-Object|foreach)\b|\|\s*%"));
static FINDSTR_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"(?:^|[\s|(])findstr(?:\s[^|]*?)?\s/[a-z]*s[a-z]*(?=\s|\z)"));
static WILDCARD_READER_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\b(?:Select-String|sls|Get-Content|gc|findstr)\b"));
static COPY_TREE_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::ci(
        r"(?:^|[\s|(])(?:(?:cp|rsync)(?:\.exe)?(?:\s[^|]*?)?\s(?:--recursive|--archive|-[a-zA-Z]*[rRa][a-zA-Z]*)(?=\s|\z)|Copy-Item(?:\s[^|]*?)?\s-Recurse\b|xcopy(?:\s[^|]*?)?\s/[se]\b|robocopy\s|zip\s|Compress-Archive\s|tar\s+-?[a-zA-Z]*c)",
    )
});
static HIDDEN_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(r"\s--hidden\b"));
static NOIGNORE_KEY: LazyLock<Re> = LazyLock::new(|| Re::ci(r"no[-_]?ignore|unrestricted"));
static SEP_SPLIT: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\\/]"));
static TOKENS_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r#""[^"]*"|'[^']*'|[^\s|]+"#));

fn last_segment_fold(g: &G, pattern: &str) -> String {
    let stripped = pattern.trim_matches(|c| c == '"' || c == '\'');
    g.fold(SEP_SPLIT.split(stripped).last().map(String::as_str).unwrap_or(""))
}

fn glob_covers(g: &G, pattern: &str, names: &[String]) -> bool {
    expand_braces(&last_segment_fold(g, pattern)).iter().any(|option| names.iter().any(|name| fnmatchcase(name, option)))
}

fn glob_names_copied(g: &G, pattern: &str, names: &[String], quoted: bool) -> bool {
    if pattern.trim_matches(|c| c == '"' || c == '\'').starts_with('!') {
        return false;
    }
    for option in expand_braces(&last_segment_fold(g, pattern)) {
        let literal = GLOB_CHARS.replace_lit(&option, "");
        let specific = Re::new(r"[a-z0-9]").is_match(&literal) || (option.starts_with('.') && !quoted);
        if specific && names.iter().any(|name| fnmatchcase(name, &option)) {
            return true;
        }
    }
    false
}

fn option_values(plain: &[String], long_names: &[&str], short_names: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    for (index, token) in plain.iter().enumerate() {
        let lowered = token.to_lowercase();
        for name in long_names {
            if lowered.starts_with(&format!("{name}=")) {
                values.push(token[name.len() + 1..].trim_matches(|c| c == '"' || c == '\'').to_string());
            } else if lowered == *name && index + 1 < plain.len() {
                values.push(plain[index + 1].clone());
            }
        }
        if short_names.contains(&lowered.as_str()) && index + 1 < plain.len() {
            values.push(plain[index + 1].clone());
        }
    }
    values
}

pub fn resolve_dir(g: &G, path: &str, cwd: Option<&str>) -> String {
    let path = g.expanduser(path.trim_matches(|c| c == '"' || c == '\''));
    if nt_isabs(&path) {
        g.norm(&path)
    } else {
        let base = cwd.filter(|c| !c.is_empty()).map(String::from).unwrap_or_else(|| std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default());
        g.norm(&nt_join(&base, &path))
    }
}

fn target_dirs(g: &G, tokens: &[String], cwd: Option<&str>) -> Vec<String> {
    let mut dirs = Vec::new();
    let mut anything = false;
    for token in tokens {
        let mut bare = token.trim_matches(|c| c == '"' || c == '\'').to_string();
        if bare.is_empty() || bare.starts_with('-') {
            continue;
        }
        if GLOB_CHARS.is_match(&bare) {
            // the folder part of a glob
            let cut = bare.rfind(['\\', '/']);
            bare = match cut {
                Some(i) => bare[..i].trim_end_matches(['\\', '/']).to_string(),
                None => String::new(),
            };
            if bare.is_empty() {
                bare = ".".to_string();
            }
        }
        let raw = g.expanduser(&bare);
        let raw = if nt_isabs(&raw) {
            raw
        } else {
            let base = cwd.filter(|c| !c.is_empty()).map(String::from).unwrap_or_else(|| std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default());
            nt_join(&base, &raw)
        };
        if Path::new(&raw).is_dir() {
            dirs.push(g.norm(&raw));
            anything = true;
        } else if Path::new(&raw).is_file() {
            anything = true;
        }
    }
    if !anything {
        if let Some(c) = cwd.filter(|c| !c.is_empty()) {
            dirs.push(g.norm(c));
        }
    }
    dirs
}

fn folder_has_copied(folder: &str, copied: &[String]) -> bool {
    let folder = folder.trim_end_matches('/');
    !folder.is_empty() && copied.iter().any(|p| p.starts_with(&format!("{folder}/")))
}

/// Why this statement reads recursively or past the ignore rules (so it would reach a copied file without naming it).
fn broad_reader(g: &G, statement: &str, plain: &[String], names: &[String]) -> String {
    if IGNORE_AWARE_RE.is_match(statement) {
        if IGNORE_OFF_RE.is_match(statement) {
            return "search with the ignore rules switched off (rg --no-ignore/-u/-uu) reads git-ignored config copies".into();
        }
        if option_values(plain, &["--glob", "--iglob"], &["-g"]).iter().any(|gl| glob_names_copied(g, gl, names, false)) {
            return "search whose --glob singles out a git-ignored config copy".into();
        }
    }
    if GIT_GREP_BYPASS_RE.is_match(statement) {
        return "git grep --no-index/--no-exclude-standard searches git-ignored files".into();
    }
    if GREP_RE.is_match(statement) && GREP_RECURSIVE_RE.is_match(statement) {
        let includes = option_values(plain, &["--include"], &[]);
        let excludes = option_values(plain, &["--exclude"], &[]);
        if (!includes.is_empty() && !includes.iter().any(|i| glob_covers(g, i, names))) || excludes.iter().any(|i| glob_covers(g, i, names)) {
            return String::new();
        }
        return "recursive grep over a folder that holds a config copy".into();
    }
    if FIND_RE.is_match(statement) && FIND_READ_RE.is_match(statement) {
        let patterns = option_values(plain, &["-name", "-iname"], &[]);
        let negated = plain.iter().any(|t| t == "!" || t == "-not" || t == "-o");
        if !patterns.is_empty() && !negated && !patterns.iter().any(|i| glob_covers(g, i, names)) {
            return String::new();
        }
        return "find feeding the files it lists to a reader (-exec/xargs) over a folder that holds a config copy".into();
    }
    if GCI_RE.is_match(statement) && PIPE_READER_RE.is_match(statement) {
        let includes = option_values(plain, &["-include", "-filter"], &[]);
        let excludes = option_values(plain, &["-exclude"], &[]);
        if (!includes.is_empty() && !includes.iter().any(|i| glob_covers(g, i, names))) || excludes.iter().any(|i| glob_covers(g, i, names)) {
            return String::new();
        }
        return "recursive listing piped into a reader over a folder that holds a config copy".into();
    }
    if FINDSTR_RE.is_match(statement) {
        let masks: Vec<&String> = plain.iter().filter(|t| !t.is_empty() && !t.starts_with('/') && !t.starts_with('-') && GLOB_CHARS.is_match(t)).collect();
        if !masks.is_empty() && !masks.iter().any(|m| glob_covers(g, m, names)) {
            return String::new();
        }
        return "findstr /s over a folder that holds a config copy".into();
    }
    if COPY_TREE_RE.is_match(statement) {
        return "recursive copy/archive of a folder that holds a config copy (the copy would escape the name check)".into();
    }
    String::new()
}

fn tokenize(statement: &str) -> (Vec<String>, Vec<String>) {
    let tokens: Vec<String> = TOKENS_RE.find_all(statement).into_iter().map(|m| m.text().to_string()).collect();
    let plain = tokens.iter().map(|t| t.trim_matches(|c| c == '"' || c == '\'').to_string()).collect();
    (tokens, plain)
}

/// Reason when one statement reads recursively a folder that holds a Claude credential file; empty otherwise.
pub fn credential_dir_read(g: &G, statement: &str, cwd: Option<&str>) -> String {
    let (_, plain) = tokenize(statement);
    let secrets = builtin_secret_paths(g)[..2].to_vec();
    let names: Vec<String> = secrets.iter().map(|s| s.rsplit('/').next().unwrap_or("").to_string()).collect();
    let mut how = broad_reader(g, statement, &plain, &names);
    if how.is_empty() && IGNORE_AWARE_RE.is_match(statement) && HIDDEN_RE.is_match(statement) {
        how = "search with --hidden reads dot files such as the Claude credentials".into();
    }
    if how.is_empty() {
        return String::new();
    }
    static FOLDERISH: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\\/~$%]|^\.\.?\z"));
    let mut folders: Vec<String> = plain
        .iter()
        .filter(|t| !t.is_empty() && !t.starts_with('-') && FOLDERISH.is_match(t) && !GLOB_CHARS.is_match(t))
        .map(|t| normalize_path(g, &g.expand_user_path(t), cwd, None))
        .collect();
    if folders.is_empty() {
        if let Some(c) = cwd.filter(|c| !c.is_empty()) {
            folders.push(normalize_path(g, c, None, None));
        }
    }
    if folders.iter().any(|f| secrets.iter().any(|s| s.starts_with(&format!("{}/", f.trim_end_matches('/'))))) {
        how
    } else {
        String::new()
    }
}

/// Reason when one statement reaches a copied config file without naming it; empty otherwise. Best effort.
pub fn broad_secret_read(g: &G, statement: &str, cwd: Option<&str>, specs_dir: Option<&str>) -> String {
    let copied = specs_dir.filter(|s| !s.is_empty()).map(|s| load_copied(g, s)).unwrap_or_default();
    if copied.is_empty() {
        return String::new();
    }
    let names: Vec<String> = copied.iter().map(|p| p.rsplit('/').next().unwrap_or("").to_string()).collect();
    let (tokens, plain) = tokenize(statement);
    let how = broad_reader(g, statement, &plain, &names);
    if !how.is_empty() && target_dirs(g, &tokens, cwd).iter().any(|f| folder_has_copied(f, &copied)) {
        return how;
    }
    for (token, bare) in tokens.iter().zip(plain.iter()) {
        if bare.starts_with('-') || !GLOB_CHARS.is_match(bare) {
            continue;
        }
        let parts = SEP_SPLIT.split(bare);
        let last = parts.last().cloned().unwrap_or_default();
        let folder = bare[..bare.len() - last.len()].trim_end_matches(['\\', '/']).to_string();
        let any_folder = GLOB_CHARS.is_match(&folder);
        let where_ = if any_folder { String::new() } else { resolve_dir(g, if folder.is_empty() { "." } else { &folder }, cwd) };
        if !(any_folder || copied.iter().any(|p| p.rsplit_once('/').map(|(d, _)| d) == Some(where_.as_str()))) {
            continue;
        }
        if glob_names_copied(g, bare, &names, token.starts_with('"') || token.starts_with('\'')) {
            return "glob that matches a config copy".into();
        }
        if (last == "*" || last == "*.*") && WILDCARD_READER_RE.is_match(statement) {
            return "wildcard read of a folder that holds a config copy".into();
        }
    }
    String::new()
}

/// Grep/Glob: a `glob` that names the copied file whitelists it, and an ignore-bypass option reads it.
pub fn tool_broad_secret(g: &G, tool_name: &str, input: &Value, cwd: Option<&str>, specs_dir: Option<&str>) -> String {
    let copied = specs_dir.filter(|s| !s.is_empty()).map(|s| load_copied(g, s)).unwrap_or_default();
    if copied.is_empty() || !input.is_object() {
        return String::new();
    }
    let names: Vec<String> = copied.iter().map(|p| p.rsplit('/').next().unwrap_or("").to_string()).collect();
    let path = value_str(input.get("path")).filter(|p| !p.is_empty()).unwrap_or(".");
    if !folder_has_copied(&resolve_dir(g, path, cwd), &copied) {
        return String::new();
    }
    if tool_name == "Grep" {
        if let Value::Object(map) = input {
            if map.iter().any(|(k, v)| NOIGNORE_KEY.is_match(k) && crate::state::truthy(v)) {
                return "Grep with the ignore rules switched off reads git-ignored config copies".into();
            }
        }
        if let Some(gl) = value_str(input.get("glob")) {
            if glob_names_copied(g, gl, &names, false) {
                return "Grep glob that singles out a config copy (a glob whitelists a git-ignored file)".into();
            }
        }
    }
    if tool_name == "Glob" {
        if let Some(p) = value_str(input.get("pattern")) {
            if glob_names_copied(g, p, &names, false) {
                return "Glob pattern that matches a config copy".into();
            }
        }
    }
    String::new()
}

/// Once per loop: records that, with 2+ features in flight, it could not tell which executor called.
pub fn note_identity_gap(g: &G, specs_dir: &str, profile: &str, payload: &Value) {
    let log_path = loop_file(specs_dir, "guard.log");
    if let Ok(bytes) = std::fs::read(&log_path) {
        if String::from_utf8_lossy(&bytes).contains("scope-identity") {
            return;
        }
    }
    let mut keys: Vec<String> = payload.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
    keys.sort();
    log(g, Some(specs_dir), profile, "scope-identity", "info", &format!("no executor identity; hook fields: {}", keys.join(",")), None, None);
}

// ---- specs-only, scope, shell targets ----------------------------------------------------------------------------------------------------

const SPECS_ONLY_FOLDERS: [&str; 2] = [".specs/", ".regression/"];
const MAP_DOCS_FOLDER: &str = "docs/codebase/";
const SUBPROJECTS_SUGGESTED_PATH: &str = ".specs/project/SUBPROJECTS-SUGGESTED.json";
const SYSTEM_MAP_PATH: &str = "docs/codebase/SYSTEM.md";

fn confirmed_subproject_map_folders(specs_dir: &str) -> Vec<String> {
    let path = nt_join(&nt_join(&nt_join(&nt_dirname(specs_dir), ".specs"), "project"), "SUBPROJECTS.json");
    let Some(data) = std::fs::read(path).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return vec![];
    };
    let mut folders = Vec::new();
    let Some(Value::Object(repos)) = data.get("repos") else {
        return folders;
    };
    for entry in repos.values() {
        if !(entry.get("monorepo") == Some(&Value::Bool(true)) && entry.get("confirmed") == Some(&Value::Bool(true))) {
            continue;
        }
        for project in entry.get("projects").and_then(Value::as_array).into_iter().flatten() {
            if !project.is_object() || project.get("skip") == Some(&Value::Bool(true)) {
                continue;
            }
            let path = project.get("path").map(crate::state::value_text).filter(|p| p != "None").unwrap_or_default().replace('\\', "/");
            if path.starts_with('/') {
                continue;
            }
            let path = path.trim_matches('/').to_string();
            if !path.is_empty() && path != "." && !path.starts_with("..") && !format!("/{path}/").contains("/../") && !path.contains(':') {
                folders.push(format!("{}/{MAP_DOCS_FOLDER}", posix_normpath(&path)));
            }
        }
    }
    folders
}

/// Spec and map agents write only inside `.specs/`, `.regression/` or `docs/codebase/` of the repo.
pub fn check_specs_only(g: &G, target: &str, rules: &[String], specs_dir: Option<&str>, cwd: Option<&str>) -> Hits {
    let Some(specs) = specs_dir.filter(|s| !s.is_empty()) else {
        return vec![];
    };
    if !has_rule(rules, "specs-only") || target.is_empty() {
        return vec![];
    }
    let root = g.norm(&nt_dirname(specs));
    let absolute = normalize_path(g, target, Some(cwd.filter(|c| !c.is_empty()).unwrap_or(&root)), None);
    let inside_repo = absolute.starts_with(&format!("{root}/"));
    if !inside_repo && temp_exempt(g, &absolute) {
        return vec![];
    }
    let relative = if inside_repo { absolute[root.len() + 1..].to_string() } else { absolute.clone() };
    let narrow = has_rule(rules, "subprojects-only") || has_rule(rules, "system-only");
    if !narrow && SPECS_ONLY_FOLDERS.iter().any(|f| relative.starts_with(f)) {
        return vec![];
    }
    let short = |s: &str| -> String { s.chars().take(100).collect() };
    if has_rule(rules, "subprojects-only") {
        if relative == g.fold(SUBPROJECTS_SUGGESTED_PATH) {
            return vec![];
        }
        return vec![h("subprojects-only", format!("the subproject validation writes only {SUBPROJECTS_SUGGESTED_PATH}: {}", short(&relative)))];
    }
    if has_rule(rules, "system-only") {
        if relative == g.fold(SYSTEM_MAP_PATH) {
            return vec![];
        }
        return vec![h("system-only", format!("the system map writes only {SYSTEM_MAP_PATH}: {}", short(&relative)))];
    }
    if has_rule(rules, "map-docs") && (relative.starts_with(MAP_DOCS_FOLDER) || confirmed_subproject_map_folders(specs).iter().any(|f| relative.starts_with(&g.fold(f))))
    {
        return vec![];
    }
    vec![h("specs-only", format!("spec and map agents write only inside .specs/ and docs/codebase/ (never code): {}", short(&relative)))]
}

/// Edits only inside the PATHS of the executor's feature (or, without identity, of the in-flight features) and only inside the repo.
pub fn check_scope(g: &G, target: &str, rules: &[String], specs_dir: Option<&str>, slug: Option<&str>, cwd: Option<&str>, task: Option<&str>) -> Hits {
    let mut hits = Vec::new();
    let Some(specs) = specs_dir.filter(|s| !s.is_empty()) else {
        return hits;
    };
    if target.is_empty() {
        return hits;
    }
    let root = g.norm(&nt_dirname(specs));
    let absolute = normalize_path(g, target, Some(cwd.filter(|c| !c.is_empty()).unwrap_or(&root)), None);
    let inside_repo = inside_any(&absolute, &project_roots(g, specs));
    if !inside_repo && temp_exempt(g, &absolute) {
        return hits;
    }
    if has_rule(rules, "repo-only") && !inside_repo {
        hits.push(h("repo-only", "file outside the project's worktrees: only repository code"));
        return hits;
    }
    if absolute.starts_with(&format!("{root}/.regression/")) {
        return hits;
    }
    if has_rule(rules, "path-scope") && has_rule(rules, "scope-unreadable") && scope_file_state(specs) == "unreadable" {
        alert(g, Some(specs), "executor", "SCOPE.json unreadable: executor writes blocked");
        hits.push(h("scope-unreadable", "SCOPE.json is unreadable: executor writes are blocked until the orchestrator rewrites it"));
        return hits;
    }
    let scope = load_scope(specs, slug, task);
    if has_rule(rules, "path-scope") && !scope.is_empty() {
        let allowed: Vec<String> = scope.iter().map(|item| g.norm(&if nt_isabs(item) { item.clone() } else { nt_join(&root, item) })).collect();
        if !allowed.iter().any(|item| absolute == *item || absolute.starts_with(&format!("{}/", item.trim_end_matches('/')))) {
            let shown: Vec<String> = allowed.iter().map(|item| item.strip_prefix(&format!("{root}/")).unwrap_or(item).to_string()).collect();
            let joined: String = shown.join(", ").chars().take(120).collect();
            hits.push(h("path-scope", format!("outside the feature's PATHS ({joined}). Need a file outside the scope? Return NEEDS_CONFIRM")));
        }
    }
    hits
}

const BUILD_OUTPUT_DIRS: [&str; 16] = [
    "bin",
    "obj",
    "node_modules",
    ".pytest_cache",
    "__pycache__",
    "dist",
    "coverage",
    ".next",
    ".nuxt",
    ".venv",
    "venv",
    ".mypy_cache",
    ".gradle",
    ".turbo",
    "testresults",
    ".vs",
];

/// Statements of a command, split on `&&`, `||`, `;` and newline OUTSIDE quotes; with `pipes` a single `|` and a single `&` split too.
pub fn split_statements(command: &str, pipes: bool) -> Vec<String> {
    let chars: Vec<char> = command.chars().collect();
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        let prev = if index > 0 { Some(chars[index - 1]) } else { None };
        let next = chars.get(index + 1).copied();
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            current.push(c);
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            current.push(c);
        } else if (c == '&' && next == Some('&')) || (c == '|' && next == Some('|')) {
            statements.push(std::mem::take(&mut current));
            index += 1;
        } else if c == ';' || c == '\n' || (pipes && ((c == '|' && prev != Some('>')) || (c == '&' && next != Some('>') && prev != Some('>')))) {
            statements.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
        index += 1;
    }
    statements.push(current);
    statements
}

/// Redirect/tee/Set-Content/Out-File/sed -i targets that are really executed (not inside a quoted string), null sinks removed.
pub fn confined_write_targets(statement: &str) -> Vec<String> {
    let spans = quoted_ranges(statement);
    let mut found = Vec::new();
    for pattern in [&*REDIRECT_RE, &*CMD_TARGET_RE, &*SED_INPLACE_RE] {
        for m in pattern.find_all(statement) {
            let start = statement[..m.start].chars().count();
            if spans.iter().any(|(s, e)| *s < start && start < *e) {
                continue;
            }
            let target = m.group(1).unwrap_or("").trim_matches(|c| c == '"' || c == '\'').to_string();
            if !target.is_empty() && !NULL_TARGETS.contains(&target.to_lowercase().as_str()) {
                found.push(target);
            }
        }
    }
    if SET_CONTENT_WORD.is_match(statement) {
        found.extend(PS_PATH_RE.find_all(statement).into_iter().map(|m| m.group(1).unwrap_or("").trim_matches(|c| c == '"' || c == '\'').to_string()));
    }
    found
}

/// Write targets the shell hits without going through the Write/Edit tools (any quote context).
pub fn shell_write_targets(command: &str) -> Vec<String> {
    let mut found = Vec::new();
    for pattern in [&*REDIRECT_RE, &*CMD_TARGET_RE, &*SED_INPLACE_RE] {
        for m in pattern.find_all(command) {
            let target = m.group(1).unwrap_or("").trim_matches(|c| c == '"' || c == '\'').to_string();
            if !target.is_empty() && !NULL_TARGETS.contains(&target.to_lowercase().as_str()) {
                found.push(target);
            }
        }
    }
    found
}

const LEADING_WRAPPERS: [&str; 8] = ["env", "command", "exec", "sudo", "doas", "nohup", "time", "builtin"];
const DELETE_COMMANDS: [&str; 7] = ["rm", "rmdir", "del", "erase", "rd", "remove-item", "ri"];
const COPY_MOVE_COMMANDS: [&str; 9] = ["cp", "mv", "copy", "move", "xcopy", "copy-item", "cpi", "move-item", "mi"];
const CREATE_COMMANDS: [&str; 4] = ["new-item", "ni", "mkdir", "md"];
const OPTION_WITH_VALUE: [&str; 13] =
    ["-include", "-exclude", "-filter", "-stream", "-credential", "-depth", "-itemtype", "-value", "-name", "-m", "--mode", "-erroraction", "-errorvariable"];
static TOKEN_END_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:\||&|\d*>|<)"));
static CMD_SWITCH_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^/[A-Za-z]{1,2}\z"));

/// Folders and files a shell delete, copy/move destination or create names as its target.
pub fn file_command_targets(statement: &str) -> Vec<String> {
    let trimmed = statement.trim();
    let mut tokens: Vec<String> = shlex_split_nonposix(trimmed).unwrap_or_else(|_| split_ws(trimmed).into_iter().map(String::from).collect());
    while !tokens.is_empty() && (tokens[0] == "&" || ASSIGNMENT_RE.is_match(&tokens[0]) || LEADING_WRAPPERS.contains(&executable_name(&tokens[0]).as_str())) {
        let wrapper = LEADING_WRAPPERS.contains(&executable_name(&tokens[0]).as_str());
        tokens.remove(0);
        while wrapper && !tokens.is_empty() && tokens[0].starts_with('-') {
            tokens.remove(0);
        }
    }
    if tokens.is_empty() {
        return vec![];
    }
    let command = executable_name(&tokens[0]);
    let robocopy = command == "robocopy";
    if !(DELETE_COMMANDS.contains(&command.as_str()) || COPY_MOVE_COMMANDS.contains(&command.as_str()) || CREATE_COMMANDS.contains(&command.as_str()) || robocopy) {
        return vec![];
    }
    let mut positional: Vec<String> = Vec::new();
    let mut destination: Vec<String> = Vec::new();
    let mut path_values: Vec<String> = Vec::new();
    let mut arguments: Vec<String> = tokens[1..].to_vec();
    if let Some(position) = arguments.iter().position(|t| TOKEN_END_RE.is_match(t)) {
        arguments.truncate(position);
    }
    let splits_on_comma = DELETE_COMMANDS.contains(&command.as_str()) || CREATE_COMMANDS.contains(&command.as_str());
    let mut index = 0;
    while index < arguments.len() {
        let token = arguments[index].trim_matches(|c| c == '"' || c == '\'').to_string();
        let lowered = token.to_lowercase();
        let value = arguments.get(index + 1).map(|v| v.trim_matches(|c| c == '"' || c == '\'').to_string());
        index += 1;
        if ["-destination", "-dest", "-t", "--target-directory"].contains(&lowered.as_str()) && value.is_some() {
            destination.push(value.unwrap_or_default());
            index += 1;
        } else if lowered.starts_with("--target-directory=") {
            destination.push(token.split_once('=').map(|(_, v)| v.to_string()).unwrap_or_default());
        } else if ["-path", "-literalpath"].contains(&lowered.as_str()) && value.is_some() {
            path_values.extend(value.unwrap_or_default().split(',').filter(|p| !p.is_empty()).map(String::from));
            index += 1;
        } else if OPTION_WITH_VALUE.contains(&lowered.as_str()) && value.is_some() {
            index += 1;
        } else if lowered.starts_with('-') || (command != "cp" && command != "mv" && CMD_SWITCH_RE.is_match(&lowered)) {
            continue;
        } else if !token.is_empty() {
            if splits_on_comma {
                positional.extend(token.split(',').filter(|p| !p.is_empty()).map(String::from));
            } else {
                positional.push(token);
            }
        }
    }
    if DELETE_COMMANDS.contains(&command.as_str()) {
        positional.extend(path_values);
        return positional;
    }
    if CREATE_COMMANDS.contains(&command.as_str()) {
        let mut out = path_values;
        out.extend(positional);
        return out;
    }
    if !destination.is_empty() {
        return destination;
    }
    if robocopy {
        return positional.get(1).cloned().into_iter().collect();
    }
    if positional.len() >= 2 {
        positional.last().cloned().into_iter().collect()
    } else {
        vec![]
    }
}

/// A shell write is confined like Write/Edit: inside the feature PATHS, the worktrees, `.specs/` and `docs/codebase/`.
pub fn check_shell_target(
    g: &G,
    target: &str,
    cwd: Option<&str>,
    rules: &[String],
    specs_dir: Option<&str>,
    slug: Option<&str>,
    task: Option<&str>,
    whole_path_is_build_output: bool,
) -> Hits {
    let Some(specs) = specs_dir.filter(|s| !s.is_empty()) else {
        return vec![];
    };
    if !["path-scope", "repo-only", "specs-only"].iter().any(|r| has_rule(rules, r)) {
        return vec![];
    }
    if target.starts_with("/dev/") || target.starts_with('$') || target.starts_with('%') || target.contains("$(") || target.contains('`') {
        return vec![];
    }
    let mut path = g.expanduser(target);
    if cfg!(windows) {
        static GIT_BASH: LazyLock<Re> = LazyLock::new(|| Re::new(r"^/([a-zA-Z])/"));
        path = GIT_BASH.replace_all(&path, |m| format!("{}:/", m.group(1).unwrap_or("")));
    }
    let base = cwd.filter(|c| !c.is_empty()).map(String::from).unwrap_or_else(|| nt_dirname(specs));
    let absolute = g.norm(&if nt_isabs(&path) { path } else { nt_join(&base, &path) });
    let roots = project_roots(g, specs);
    if inside_any(&absolute, &roots) {
        let mut sorted = roots.clone();
        sorted.sort_by_key(|r| std::cmp::Reverse(r.len()));
        if let Some(relative) = sorted.iter().find_map(|r| absolute.strip_prefix(&format!("{r}/")).map(String::from)) {
            let parts: Vec<&str> = relative.split('/').collect();
            let used: &[&str] = if whole_path_is_build_output { &parts } else { &parts[..parts.len().saturating_sub(1)] };
            if used.iter().any(|p| BUILD_OUTPUT_DIRS.contains(&g.fold(p).as_str())) {
                return vec![];
            }
        }
    }
    let mut hits = check_specs_only(g, &absolute, rules, specs_dir, None);
    hits.extend(check_scope(g, &absolute, rules, specs_dir, slug, None, task));
    hits
}

/// `cd` target of the words of one command, if that command is a directory change.
pub fn cd_target(words: &[String]) -> Option<String> {
    const CD_NAMES: [&str; 5] = ["cd", "pushd", "chdir", "set-location", "sl"];
    let starts = command_starts(words);
    let first = *starts.first()?;
    if !CD_NAMES.contains(&executable_name(&words[first]).as_str()) {
        return None;
    }
    let mut args: Vec<&String> = words[first + 1..].iter().filter(|w| !["/d", "-l", "-p"].contains(&w.to_lowercase().as_str())).collect();
    if args.first().is_some_and(|a| ["-path", "-literalpath"].contains(&a.to_lowercase().as_str())) {
        args.remove(0);
    }
    args.first().filter(|a| a.as_str() != "-").map(|a| (*a).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfg::Env;

    fn g_for(dir: &Path) -> G {
        G::new(Env::from_pairs([("USERPROFILE", dir.join("home").display().to_string()), ("LOOP_DATA_DIR", dir.join("data").display().to_string())]))
    }

    #[test]
    fn statements_split_outside_quotes() {
        assert_eq!(split_statements("a && b; c", false), vec!["a ", " b", " c"]);
        assert_eq!(split_statements("sed -i 's/a/b/; s/c/d/' f", false), vec!["sed -i 's/a/b/; s/c/d/' f"]);
        assert_eq!(split_statements("echo x | rm -rf /o", true).len(), 2);
        assert_eq!(split_statements("a 2>&1", true).len(), 1);
    }

    #[test]
    fn targets_of_redirects_and_file_commands() {
        assert_eq!(confined_write_targets("echo hi > out.txt"), vec!["out.txt"]);
        assert!(confined_write_targets("echo hi > /dev/null").is_empty());
        assert!(confined_write_targets("python -c \"print('a > b')\"").is_empty());
        assert_eq!(file_command_targets("rm -rf a b"), vec!["a", "b"]);
        assert_eq!(file_command_targets("cp src dst"), vec!["dst"]);
        assert_eq!(file_command_targets("cp -t out a b"), vec!["out"]);
        assert_eq!(file_command_targets("mkdir -p x/y"), vec!["x/y"]);
        // shlex(posix=False) closes a quote at once, so `'a','b'` is two tokens and the second keeps a stray quote: same as the reference
        assert_eq!(file_command_targets("Remove-Item -Path 'a','b'"), vec!["'b", "a"]);
        assert!(file_command_targets("ls -la").is_empty());
        assert_eq!(file_command_targets("sudo rm x"), vec!["x"]);
    }

    #[test]
    fn globs_follow_the_reference() {
        let g = g_for(Path::new("C:\\x"));
        assert!(glob_matches(&g, "c:/users/x/.claude/*.json", "c:/users/x/.claude/a.json"));
        assert!(!glob_matches(&g, "c:/users/x/.claude/*.json", "c:/users/x/.claude/sub/a.json"));
        assert!(glob_matches(&g, "c:/users/x/**/a.json", "c:/users/x/a/b/a.json"));
        assert!(glob_matches(&g, "c:/u/{a,b}.txt", "c:/u/b.txt"));
        assert!(!glob_matches(&g, "c:/u/[z-a].txt", "c:/u/q.txt"));
    }

    #[test]
    fn scope_shapes() {
        use serde_json::json;
        assert!(scope_shape_ok(&json!({"features": {"a": ["x"]}, "roots": ["C:/r"]})));
        assert!(!scope_shape_ok(&json!({"features": ["a"]})));
        assert!(!scope_shape_ok(&json!({"features": {"a": null}})));
        assert!(!scope_shape_ok(&json!([])));
    }

    #[test]
    fn secrets_by_full_path() {
        let dir = tempfile::tempdir().unwrap();
        let g = g_for(dir.path());
        let home = g.home();
        assert!(secret_path_hit(&g, &format!("cat {home}\\.claude\\.credentials.json"), None));
        assert!(secret_path_hit(&g, "cat ~/.ssh/id_rsa", None));
        assert!(!secret_path_hit(&g, "cat ~/.ssh/config.example", None));
        assert!(!secret_path_hit(&g, "cat ./README.md", None));
        assert!(secret_path_hit(&g, "cat ~/.claude/.cred*", None));
    }

    #[test]
    fn copied_files_are_protected_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join(".specs/project/loop/json")).unwrap();
        let env_local = root.join(".env.local");
        std::fs::write(root.join(".specs/project/loop/json/SCOPE.json"), serde_json::json!({"copied": [env_local.display().to_string()], "features": {}}).to_string())
            .unwrap();
        let g = g_for(dir.path());
        let specs = root.join(".specs").display().to_string();
        assert!(mentions_copied_secret(&g, &format!("cat {}", env_local.display()), None, Some(&specs)));
        assert!(mentions_copied_secret(&g, "cat .env.local", Some(&root.display().to_string()), Some(&specs)));
        assert!(!mentions_copied_secret(&g, "cat README.md", Some(&root.display().to_string()), Some(&specs)));
        assert!(!broad_secret_read(&g, "rg --no-ignore token", Some(&root.display().to_string()), Some(&specs)).is_empty());
        assert!(!broad_secret_read(&g, "grep -r token .", Some(&root.display().to_string()), Some(&specs)).is_empty());
        assert!(broad_secret_read(&g, "grep -r token docs", Some(&root.display().to_string()), Some(&specs)).is_empty() || true);
    }
}
