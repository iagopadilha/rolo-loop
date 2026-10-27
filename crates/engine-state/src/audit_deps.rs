// SPDX-License-Identifier: MIT
//! Audits forgotten dependencies between ROADMAP features (skill orchestrator-audit-dependencies). No AI.
//!
//! `harness audit <root> [--write] [--apply] [--json]`. Looks for evidence that feature B needs feature A while B does not declare
//! `deps=A`: IMPORT (code that exists), TEXT (B's spec mentions A's slug or a path of A), and REVERSE (a declared dep with no evidence ->
//! "review"). Only suggests an A that is not yet `verified`; a suggestion that would close a cycle becomes `cycle` (not applied).

use crate::loop_state::{check_fields, feature_dir, feature_folder_name, find_cycle, parse_roadmap, split_deps, state_file, Features};
use pycompat::json::py_json_dumps_indent;
use pycompat::{nt_abspath, nt_dirname, nt_join, nt_normpath, splitlines, Re};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;

const SKIP: [&str; 12] = ["node_modules", "bin", "obj", ".git", ".specs", "dist", "build", ".next", ".vs", "packages", "__pycache__", "coverage"];
const EXTS: [&str; 8] = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".py", ".cs", ".csproj"];
const MAX_FILES: usize = 400;

static MARK: LazyLock<Re> = LazyLock::new(|| Re::new(r"^- \[(pending|applied|refused|cycle|review)\] (\S+) -> (\S+) \|"));
static JS_IMPORT: LazyLock<Re> = LazyLock::new(|| Re::new(r#"(?:\bimport\s[^'";]*?from\s*|\bimport\s*|\brequire\(\s*|\bexport\s[^'";]*?from\s*)['"]([^'"]+)['"]"#));
static PY_FROM: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*from\s+(\.*[\w.]*)\s+import\b"));
static PY_IMPORT: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*import\s+([\w., ]+)"));
static CS_USING: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*using\s+(?:static\s+)?([\w.]+)\s*;"));
static CS_NAMESPACE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*namespace\s+([\w.]+)"));
static CS_PROJREF: LazyLock<Re> = LazyLock::new(|| Re::ci(r#"<ProjectReference\s+Include\s*=\s*["']([^"']+)["']"#));
static BLOCK_COMMENT: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?s)/\*.*?\*/|(?<!:)//[^\n]*"));
static TRAILING_COMMA: LazyLock<Re> = LazyLock::new(|| Re::new(r",(\s*[}\]])"));
static HEADING: LazyLock<Re> = LazyLock::new(|| Re::new(r"^#{1,6}\s*(.+)\z"));

fn read(path: &Path) -> String {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

fn norm(path: &str) -> String {
    nt_normpath(path).replace('\\', "/").trim_matches('/').to_string()
}

fn feature_paths(f: &crate::loop_state::Fields) -> Vec<String> {
    f.text("paths").split(',').filter(|p| !p.trim().is_empty()).map(norm).collect()
}

fn inside(target: &str, folder: &str) -> bool {
    let (t, f) = (target.to_lowercase(), folder.to_lowercase());
    t == f || t.starts_with(&format!("{f}/"))
}

fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i..].to_lowercase(),
        _ => String::new(),
    }
}

fn walk_files(folder: &Path, root: &Path, out: &mut Vec<String>) -> bool {
    let Ok(entries) = std::fs::read_dir(folder) else { return false };
    let mut dirs = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !SKIP.contains(&name.as_str()) {
                dirs.push(path);
            }
        } else if EXTS.contains(&ext_of(&name).as_str()) {
            out.push(norm(&path.strip_prefix(root).map(|p| p.display().to_string()).unwrap_or(name)));
            if out.len() >= MAX_FILES {
                return true;
            }
        }
    }
    for d in dirs {
        if walk_files(&d, root, out) {
            return true;
        }
    }
    false
}

fn source_files(root: &str, paths: &[String]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for rel in paths {
        let absolute = Path::new(root).join(rel);
        if absolute.is_file() {
            found.push(rel.clone());
            continue;
        }
        if walk_files(&absolute, Path::new(root), &mut found) {
            return found;
        }
    }
    found
}

struct Resolver {
    root: String,
    namespaces: Option<Vec<(String, BTreeSet<String>)>>,
    tsconfigs: Vec<(String, Vec<(String, Vec<String>)>)>,
}

impl Resolver {
    fn new(root: &str) -> Resolver {
        Resolver { root: root.to_string(), namespaces: None, tsconfigs: vec![] }
    }

    fn namespaces(&mut self) -> &Vec<(String, BTreeSet<String>)> {
        if self.namespaces.is_none() {
            let mut map: Vec<(String, BTreeSet<String>)> = Vec::new();
            let mut count = 0;
            fn walk(folder: &Path, root: &Path, map: &mut Vec<(String, BTreeSet<String>)>, count: &mut usize) -> bool {
                let Ok(entries) = std::fs::read_dir(folder) else { return false };
                let mut dirs = Vec::new();
                for e in entries.flatten() {
                    let path = e.path();
                    let name = e.file_name().to_string_lossy().into_owned();
                    if path.is_dir() {
                        if !SKIP.contains(&name.as_str()) {
                            dirs.push(path);
                        }
                    } else if name.ends_with(".cs") {
                        *count += 1;
                        if *count > 6000 {
                            return true;
                        }
                        let text = read(&path);
                        for line in splitlines(&text).into_iter().take(40) {
                            if let Some(m) = CS_NAMESPACE.match_start(line) {
                                let ns = m.group(1).unwrap_or("").to_string();
                                let rel = norm(&relpath(folder, root));
                                match map.iter_mut().find(|(n, _)| *n == ns) {
                                    Some((_, set)) => {
                                        set.insert(rel);
                                    }
                                    None => map.push((ns, BTreeSet::from([rel]))),
                                }
                                break;
                            }
                        }
                    }
                }
                for d in dirs {
                    if walk(&d, root, map, count) {
                        return true;
                    }
                }
                false
            }
            walk(Path::new(&self.root), Path::new(&self.root), &mut map, &mut count);
            self.namespaces = Some(map);
        }
        self.namespaces.as_ref().expect("set above")
    }

    /// Alias map of the nearest tsconfig above `directory`: `[(prefix, [repo-relative folder])]`.
    fn tsconfig(&mut self, directory: &str) -> Vec<(String, Vec<String>)> {
        let mut here = directory.to_string();
        loop {
            if let Some((_, a)) = self.tsconfigs.iter().find(|(h, _)| *h == here) {
                return a.clone();
            }
            let candidate = Path::new(&self.root).join(&here).join("tsconfig.json");
            if candidate.is_file() {
                let text = BLOCK_COMMENT.replace_lit(&read(&candidate), "");
                let text = TRAILING_COMMA.replace_all(&text, |m| m.group(1).unwrap_or("").to_string());
                let mut aliases: Vec<(String, Vec<String>)> = Vec::new();
                if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
                    let options = map.get("compilerOptions").and_then(Value::as_object).cloned().unwrap_or_default();
                    let base_url = options.get("baseUrl").and_then(Value::as_str).unwrap_or(".");
                    let base = norm(&nt_join(&here, base_url));
                    if let Some(Value::Object(paths)) = options.get("paths") {
                        for (pattern, targets) in paths {
                            let list: Vec<String> = targets
                                .as_array()
                                .map(|a| a.iter().filter_map(Value::as_str).map(|t| norm(&nt_join(&base, t.trim_end_matches('*')))).collect())
                                .unwrap_or_default();
                            aliases.push((pattern.trim_end_matches('*').to_string(), list));
                        }
                    }
                }
                self.tsconfigs.push((here.clone(), aliases.clone()));
                return aliases;
            }
            if here.is_empty() || here == "." {
                return vec![];
            }
            let parent = nt_dirname(&here);
            here = if parent.is_empty() { String::new() } else { norm(&parent) };
        }
    }

    /// `[(repo-relative target, line number, text)]` of the file's imports that point inside the repo.
    fn imports(&mut self, rel: &str) -> Vec<(String, usize, String)> {
        let mut out = Vec::new();
        let extension = ext_of(rel);
        let directory = norm(&nt_dirname(rel));
        let text = read(&Path::new(&self.root).join(rel));
        if extension == ".csproj" {
            for (number, line) in splitlines(&text).into_iter().enumerate() {
                if let Some(m) = CS_PROJREF.search(line) {
                    let include = m.group(1).unwrap_or("").to_string();
                    let target = norm(&nt_join(&directory, &nt_dirname(&include.replace('\\', "/"))));
                    out.push((target, number + 1, format!("ProjectReference {include}")));
                }
            }
            return out;
        }
        for (index, line) in splitlines(&text).into_iter().enumerate() {
            let number = index + 1;
            let snippet: String = line.trim().chars().take(100).collect();
            if [".ts", ".tsx", ".js", ".jsx", ".mjs"].contains(&extension.as_str()) {
                for m in JS_IMPORT.find_all(line) {
                    let spec = m.group(1).unwrap_or("").to_string();
                    if spec.starts_with('.') {
                        out.push((norm(&nt_join(&directory, &spec)), number, snippet.clone()));
                    } else {
                        for (prefix, targets) in self.tsconfig(&directory) {
                            if !prefix.is_empty() && spec.starts_with(&prefix) {
                                for t in &targets {
                                    out.push((norm(&nt_join(t, &spec[prefix.len()..])), number, snippet.clone()));
                                }
                                break;
                            }
                        }
                    }
                }
            } else if extension == ".py" {
                let mut modules: Vec<String> = Vec::new();
                if let Some(m) = PY_FROM.match_start(line) {
                    modules.push(m.group(1).unwrap_or("").to_string());
                }
                if let Some(m) = PY_IMPORT.match_start(line) {
                    modules.extend(m.group(1).unwrap_or("").split(',').map(|x| x.trim().split(" as ").next().unwrap_or("").to_string()));
                }
                for module in modules {
                    let dots = module.len() - module.trim_start_matches('.').len();
                    let parts: Vec<&str> = if module.trim_matches('.').is_empty() { vec![] } else { module.trim_start_matches('.').split('.').collect() };
                    let mut base = directory.clone();
                    for _ in 0..dots.saturating_sub(1) {
                        base = norm(&nt_dirname(&base));
                    }
                    let origins: Vec<String> = if dots > 0 { vec![base.clone()] } else { vec![String::new(), directory.clone()] };
                    for origin in origins {
                        let candidate = if parts.is_empty() {
                            base.clone()
                        } else {
                            let mut p = origin.clone();
                            for part in &parts {
                                p = nt_join(&p, part);
                            }
                            norm(&p)
                        };
                        let root = Path::new(&self.root);
                        if root.join(&candidate).is_dir() || root.join(format!("{candidate}.py")).is_file() {
                            out.push((candidate, number, snippet.clone()));
                            break;
                        }
                    }
                }
            } else if extension == ".cs" {
                if let Some(m) = CS_USING.match_start(line) {
                    let name = m.group(1).unwrap_or("").to_string();
                    let folders: Vec<String> = self.namespaces().iter().find(|(n, _)| *n == name).map(|(_, s)| s.iter().cloned().collect()).unwrap_or_default();
                    for folder in folders {
                        out.push((folder, number, snippet.clone()));
                    }
                }
            }
        }
        out
    }
}

fn relpath(path: &Path, start: &Path) -> String {
    let (p, s) = (path.display().to_string(), start.display().to_string());
    match p.strip_prefix(&s) {
        Some(rest) => {
            let rest = rest.trim_start_matches(['\\', '/']);
            if rest.is_empty() {
                ".".into()
            } else {
                rest.to_string()
            }
        }
        None => p,
    }
}

fn spec_text(root: &str, slug: &str) -> Vec<(String, usize, String)> {
    let mut lines = Vec::new();
    for name in ["spec.md", "design.md", "tasks.md"] {
        let path = Path::new(&feature_dir(root, slug)).join(name);
        for (index, line) in splitlines(&read(&path)).into_iter().enumerate() {
            lines.push((format!(".specs/features/{}/{name}", feature_folder_name(slug)), index + 1, line.to_string()));
        }
    }
    lines
}

fn mentions(text_lines: &[(String, usize, String)], slug: &str, paths: &[String]) -> Vec<String> {
    let slug_re = if slug.chars().count() >= 4 || slug.contains('-') {
        Re::ci(&format!(r"(?<![\w-]){}(?![\w-])", pycompat::escape(slug)))
    } else {
        Re::ci(&format!(r"[`\[]{}[`\]]", pycompat::escape(slug)))
    };
    let mut found = Vec::new();
    let mut section = String::new();
    for (file, number, line) in text_lines {
        if let Some(h) = HEADING.match_start(line) {
            section = h.group(1).unwrap_or("").trim().to_lowercase();
        }
        let lowered = line.to_lowercase();
        let hit = slug_re.search(line).is_some() || paths.iter().any(|p| p.matches('/').count() >= 1 && lowered.contains(&p.to_lowercase()));
        if hit && !section.contains("out of scope") {
            let snippet: String = line.trim().chars().take(100).collect();
            found.push(format!("{file}:{number} {snippet}"));
        }
    }
    found
}

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub feature: String,
    pub depends_on: String,
    pub kind: String,
    pub evidence: Vec<String>,
    pub cycle: Vec<String>,
}

impl Suggestion {
    fn to_json(&self) -> Value {
        json!({"feature": self.feature, "depends_on": self.depends_on, "type": self.kind, "evidence": self.evidence, "cycle": self.cycle})
    }
}

pub struct AuditResult {
    pub features: Features,
    pub suggestions: Vec<Suggestion>,
    pub review: Vec<(String, String)>,
    pub roadmap_errors: Vec<String>,
}

pub fn audit(root: &str) -> AuditResult {
    let root = nt_abspath(root);
    let (features, errors) = parse_roadmap(&read(&Path::new(&root).join(".specs").join("project").join("ROADMAP.md")));
    let mut resolver = Resolver::new(&root);
    let mut suggestions: Vec<Suggestion> = Vec::new();
    let mut review: Vec<(String, String)> = Vec::new();
    for (b, fb) in features.iter() {
        if ["verified", "blocked"].contains(&fb.status()) {
            continue;
        }
        let (declared, _folders) = split_deps(fb);
        let own = feature_paths(fb);
        // a -> (import evidence, text evidence), in order of discovery
        let mut evidence: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
        for rel in source_files(&root, &own) {
            for (target, number, snippet) in resolver.imports(&rel) {
                if own.iter().any(|mine| inside(&target, mine)) {
                    continue;
                }
                for (a, fa) in features.iter() {
                    if a == b || fa.status() == "verified" {
                        continue;
                    }
                    if feature_paths(fa).iter().any(|theirs| inside(&target, theirs) || (inside(theirs, &target) && target.matches('/').count() >= 1)) {
                        let line = format!("{rel}:{number} {snippet}");
                        match evidence.iter_mut().find(|(k, _, _)| k == a) {
                            Some((_, imp, _)) => imp.push(line),
                            None => evidence.push((a.clone(), vec![line], vec![])),
                        }
                    }
                }
            }
        }
        let lines = spec_text(&root, b);
        for (a, fa) in features.iter() {
            if a == b || fa.status() == "verified" {
                continue;
            }
            let found = mentions(&lines, a, &feature_paths(fa));
            if !found.is_empty() {
                match evidence.iter_mut().find(|(k, _, _)| k == a) {
                    Some((_, _, txt)) => txt.extend(found),
                    None => evidence.push((a.clone(), vec![], found)),
                }
            }
        }
        for (a, imp, txt) in &evidence {
            if declared.contains(a) {
                continue;
            }
            let mut trial = features.clone();
            if let Some((_, f)) = trial.0.iter_mut().find(|(s, _)| s == b) {
                let joined: Vec<String> = [fb.text("deps").to_string(), a.clone()].into_iter().filter(|s| !s.is_empty()).collect();
                f.set("deps", &joined.join(","));
            }
            let cycle = find_cycle(&trial);
            let mut kinds: Vec<&str> = Vec::new();
            if !imp.is_empty() {
                kinds.push("import");
            }
            if !txt.is_empty() {
                kinds.push("text");
            }
            let all: Vec<String> = imp.iter().chain(txt.iter()).take(3).cloned().collect();
            let suggestion = Suggestion { feature: b.clone(), depends_on: a.clone(), kind: kinds.join("+"), evidence: all, cycle };
            match suggestions.iter_mut().find(|s| s.feature == *b && s.depends_on == *a) {
                Some(slot) => *slot = suggestion,
                None => suggestions.push(suggestion),
            }
        }
        for a in &declared {
            if features.contains(a) && !evidence.iter().any(|(k, _, _)| k == a) && features.get(a).expect("exists").status() != "verified" {
                review.push((b.clone(), a.clone()));
            }
        }
    }
    AuditResult { features, suggestions, review, roadmap_errors: errors }
}

/// `{(feature, dependency): (mark, line)}` read from DEPENDENCIES.md.
pub fn load_marks(path: &Path) -> Vec<((String, String), (String, String))> {
    let mut marks: Vec<((String, String), (String, String))> = Vec::new();
    for line in splitlines(&read(path)) {
        if let Some(m) = MARK.match_start(line.trim()) {
            let key = (m.group(2).unwrap_or("").to_string(), m.group(3).unwrap_or("").to_string());
            let value = (m.group(1).unwrap_or("").to_string(), line.trim().to_string());
            match marks.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => marks.push((key, value)),
            }
        }
    }
    marks
}

fn today() -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc()).format(format).unwrap_or_default()
}

pub fn render(result: &AuditResult, previous: &[((String, String), (String, String))]) -> String {
    let mut out: Vec<String> = vec![
        "# Dependencies between features (audit)".into(),
        String::new(),
        format!("> Generated by `audit_deps.py` on {}. A suggestion NEVER enters the ROADMAP by itself: the orchestrator shows it to the user (or, in the unsupervised loop, applies it) and swaps `[pending]` for `[applied]` or `[refused]`.", today()),
        "> Evidence `import` = existing code; `text` = the spec mentions it. Erring serial costs time; erring parallel corrupts code.".into(),
        String::new(),
        "## Suggestions".into(),
        String::new(),
    ];
    let kept: Vec<&((String, String), (String, String))> = previous.iter().filter(|(_, (mark, _))| mark == "applied" || mark == "refused").collect();
    let mut seen: Vec<(String, String)> = Vec::new();
    for item in &result.suggestions {
        let key = (item.feature.clone(), item.depends_on.clone());
        seen.push(key.clone());
        if let Some((_, (_, line))) = kept.iter().find(|(k, _)| *k == key) {
            out.push(line.clone());
            continue;
        }
        let mark = if item.cycle.is_empty() { "pending" } else { "cycle" };
        let note = if item.cycle.is_empty() { format!(" deps={}", item.depends_on) } else { format!(" would close a cycle: {}", item.cycle.join(" -> ")) };
        out.push(format!("- [{mark}] {} -> {} | {} |{note} | {}", item.feature, item.depends_on, item.kind, item.evidence.join(" ; ").replace('\n', " ")));
    }
    for (key, (_, line)) in kept {
        if !seen.contains(key) {
            out.push(line.clone());
        }
    }
    out.extend([
        String::new(),
        "## Review (deps declared without evidence)".into(),
        String::new(),
        "May be a legitimate ordering dependency (A creates data/a route that B uses). Nothing is removed on its own.".into(),
        String::new(),
    ]);
    if result.review.is_empty() {
        out.push("- (none)".into());
    } else {
        out.extend(result.review.iter().map(|(f, d)| format!("- [review] {f} -> {d} | no import nor mention in the spec")));
    }
    format!("{}\n", out.join("\n"))
}

type Applied = Vec<(String, String)>;

/// Writes `deps=` into the ROADMAP for pending suggestions without a cycle. Validates the grammar; undoes if it breaks.
pub fn apply_to_roadmap(root: &str, result: &AuditResult, marks: &[((String, String), (String, String))]) -> (Applied, Vec<String>) {
    let path = Path::new(root).join(".specs").join("project").join("ROADMAP.md");
    let original = read(&path);
    let mut text = original;
    let mut applied: Applied = Vec::new();
    for item in &result.suggestions {
        let key = (item.feature.clone(), item.depends_on.clone());
        let mark = marks.iter().find(|(k, _)| *k == key).map(|(_, (m, _))| m.as_str()).unwrap_or("pending");
        if !item.cycle.is_empty() || mark == "refused" || mark == "applied" {
            continue;
        }
        let (b, a) = (&key.0, &key.1);
        let pattern = Re::new(&format!(r"(?m)^(- \[{}\]\s.*)$", pycompat::escape(b)));
        let Some(m) = pattern.search(&text) else { continue };
        let line = m.group(1).unwrap_or("").to_string();
        let deps_re = Re::new(r"\bdeps=(\S+)");
        let new_line = if let Some(d) = deps_re.search(&line) {
            format!("{}deps={},{}{}", &line[..d.start], d.group(1).unwrap_or(""), a, &line[d.end..])
        } else {
            format!("{} deps={}", line.trim_end(), a)
        };
        if let Some(pos) = text.find(&line) {
            text = format!("{}{}{}", &text[..pos], new_line, &text[pos + line.len()..]);
        }
        applied.push(key);
    }
    if !applied.is_empty() {
        let (features, mut errors) = parse_roadmap(&text);
        errors.extend(check_fields(&features, None));
        if !errors.is_empty() {
            return (vec![], errors);
        }
        let _ = std::fs::write(&path, &text);
    }
    (applied, vec![])
}

pub fn mark_applied(markdown: &str, applied: &Applied) -> String {
    let mut out = markdown.to_string();
    for (b, a) in applied {
        let pattern = Re::new(&format!(r"(?m)^- \[pending\] {} -> {} \|", pycompat::escape(b), pycompat::escape(a)));
        out = pattern.replace_all(&out, |_| format!("- [applied] {b} -> {a} |"));
    }
    out
}

pub const USAGE: &str = include_str!("audit_deps_usage.txt");

pub fn run_cli(args: &[String]) -> (i32, String) {
    if args.is_empty() {
        return (2, format!("{USAGE}\n"));
    }
    let root = nt_abspath(&args[0]);
    let mut result = audit(&root);
    let target = state_file(&root, "DEPENDENCIES.md");
    let _ = std::fs::create_dir_all(target.parent().expect("has parent"));
    let previous = load_marks(&target);
    for (key, (mark, _)) in &previous {
        if mark == "refused" {
            result.suggestions.retain(|s| (s.feature.clone(), s.depends_on.clone()) != *key);
        }
    }
    let mut out = String::new();
    let mut applied: Applied = Vec::new();
    if args.iter().any(|a| a == "--apply") {
        let (done, problems) = apply_to_roadmap(&root, &result, &previous);
        applied = done;
        for item in &problems {
            out.push_str(&format!("PROBLEM while applying (ROADMAP untouched): {item}\n"));
        }
        for (b, a) in &applied {
            out.push_str(&format!("APPLIED: {b} deps+={a}\n"));
        }
    }
    if args.iter().any(|a| a == "--write" || a == "--apply") {
        let markdown = mark_applied(&render(&result, &previous), &applied);
        let _ = std::fs::write(&target, markdown);
        out.push_str(&format!("written: {}\n", target.display()));
    }
    if args.iter().any(|a| a == "--json") {
        let review: Vec<Value> = result.review.iter().map(|(f, d)| json!({"feature": f, "depends_on": d})).collect();
        let doc =
            json!({"suggestions": result.suggestions.iter().map(Suggestion::to_json).collect::<Vec<_>>(), "review": review, "roadmap_errors": result.roadmap_errors});
        out.push_str(&format!("{}\n", py_json_dumps_indent(&doc, false, 2)));
    } else {
        let pending = result.suggestions.iter().filter(|s| s.cycle.is_empty() && !applied.contains(&(s.feature.clone(), s.depends_on.clone()))).count();
        out.push_str(&format!("{pending} deps suggestion(s), {} to review\n", result.review.len()));
        for item in &result.suggestions {
            let flag = if !item.cycle.is_empty() {
                "CYCLE"
            } else if applied.contains(&(item.feature.clone(), item.depends_on.clone())) {
                "applied"
            } else {
                "pending"
            };
            out.push_str(&format!("  [{flag}] {} -> {} ({})\n", item.feature, item.depends_on, item.kind));
            for line in &item.evidence {
                out.push_str(&format!("       {line}\n"));
            }
        }
        for (f, d) in &result.review {
            out.push_str(&format!("  [review] {f} -> {d} no evidence\n"));
        }
    }
    (0, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, text) in files {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        dir
    }

    #[test]
    fn import_evidence_produces_a_suggestion() {
        let dir = project(&[
            (".specs/project/ROADMAP.md", "- [users-api] status=approved paths=src/users\n- [orders-api] status=approved paths=src/orders\n"),
            ("src/orders/svc.ts", "import { User } from '../users/model';\n"),
            ("src/users/model.ts", "export class User {}\n"),
        ]);
        let r = audit(&dir.path().display().to_string());
        assert_eq!(r.suggestions.len(), 1);
        assert_eq!((r.suggestions[0].feature.as_str(), r.suggestions[0].depends_on.as_str(), r.suggestions[0].kind.as_str()), ("orders-api", "users-api", "import"));
        assert!(r.suggestions[0].evidence[0].starts_with("src/orders/svc.ts:1 import { User }"));
    }

    #[test]
    fn spec_mentions_and_cycles() {
        let dir = project(&[
            (".specs/project/ROADMAP.md", "- [alpha-feature] status=approved paths=src/a deps=beta-feature\n- [beta-feature] status=approved paths=src/b\n"),
            (".specs/features/beta-feature/spec.md", "Needs alpha-feature first.\n## Out of scope\nalpha-feature again\n"),
        ]);
        let r = audit(&dir.path().display().to_string());
        assert_eq!(r.suggestions.len(), 1);
        assert_eq!(r.suggestions[0].cycle, vec!["alpha-feature", "beta-feature", "alpha-feature"]);
        assert_eq!(r.review, vec![("alpha-feature".to_string(), "beta-feature".to_string())]);
    }

    #[test]
    fn apply_writes_deps_and_marks() {
        let dir = project(&[
            (".specs/project/ROADMAP.md", "- [users-api] status=approved paths=src/users\n- [orders-api] status=approved paths=src/orders deps=dir:shared\n"),
            ("src/orders/svc.ts", "import x from '../users/model';\n"),
        ]);
        let root = dir.path().display().to_string();
        let (code, out) = run_cli(&[root.clone(), "--apply".into()]);
        assert_eq!(code, 0);
        assert!(out.contains("APPLIED: orders-api deps+=users-api"));
        let text = std::fs::read_to_string(dir.path().join(".specs/project/ROADMAP.md")).unwrap();
        assert!(text.contains("deps=dir:shared,users-api"));
        let md = std::fs::read_to_string(dir.path().join(".specs/project/loop/DEPENDENCIES.md")).unwrap();
        assert!(md.contains("- [applied] orders-api -> users-api |"));
    }
}
