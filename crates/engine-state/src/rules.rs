// SPDX-License-Identifier: MIT
//! Creates a project block rule and PROVES it works in the real guard (skill orchestrator-create-block-rule).
//!
//! `harness rules propose|write|test|list <root> ...`. `propose` only tests. `write` tests and, ONLY if everything passes (and the rule does
//! not break any older rule), writes `PERMISSIONS.json` (deny_*_regex), `rules.tests.json` (the examples) and a line in `RULES.md`.
//! Each example runs through the real guard (executor profile) in a temp folder: no regex guesswork.

use crate::loop_state::loop_path;
use guard_core::paths::G;
use pycompat::json::py_json_dumps_indent;
use pycompat::{nt_abspath, nt_join, splitlines, Re};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::LazyLock;

const NEUTRAL: [(&str, &str); 6] = [("bash", "ls"), ("bash", "git status"), ("bash", "dotnet build"), ("path", "src/app.ts"), ("path", "README.md"), ("path", "docs/guide.md")];
static ID_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^R-\d+\n?\z"));

fn project_dir(root: &str) -> std::path::PathBuf {
    Path::new(&nt_abspath(root)).join(".specs").join("project")
}

/// PERMISSIONS.json and rules.tests.json live in project/loop/json/; an old layout keeps being read from project/.
fn loop_file(root: &str, name: &str) -> std::path::PathBuf {
    let current = loop_path(&nt_abspath(root), name);
    if current.exists() {
        return current;
    }
    let legacy = project_dir(root).join(name);
    if legacy.exists() { legacy } else { current }
}

fn load_object(path: &Path) -> Map<String, Value> {
    match std::fs::read(path).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

fn save_json(path: &Path, data: &Value) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut text = py_json_dumps_indent(data, false, 2);
    text.push('\n');
    let _ = std::fs::write(path, text);
}

/// `(kind, value)` of `bash:<command>` or `path:<path>`.
fn parse_example(text: &str) -> Result<(String, String), String> {
    let (kind, _, value) = pycompat::partition(text, ":");
    if (kind != "bash" && kind != "path") || value.trim().is_empty() {
        return Err(format!("invalid example {}: use bash:<command> or path:<path>", pycompat::py_repr_str(text)));
    }
    Ok((kind.to_string(), value.trim().to_string()))
}

#[derive(Clone, Debug)]
pub struct CaseResult {
    pub example: String,
    pub expected: &'static str,
    pub ok: bool,
    pub detail: String,
}

fn run_guard(payload: &Value) -> (i32, String) {
    let g = G::from_process();
    let outcome = guard_core::run::run(&g, "executor", &pycompat::json::py_json_dumps(payload, true));
    (outcome.code, outcome.stderr)
}

/// Runs each example in the real guard.
fn evaluate(permissions: &Value, must_block: &[String], must_pass: &[String]) -> Vec<CaseResult> {
    let tmp = tempfile::Builder::new().prefix("p").tempdir().expect("temp dir");
    let tmp_path = tmp.path().display().to_string();
    save_json(&loop_path(&tmp_path, "PERMISSIONS.json"), permissions);
    save_json(&Path::new(&tmp_path).join(".specs").join("project").join("PERMISSIONS.json"), permissions);
    let mut results = Vec::new();
    for (expected, examples) in [("block", must_block), ("pass", must_pass)] {
        for example in examples {
            let Ok((kind, value)) = parse_example(example) else { continue };
            let payload = if kind == "bash" {
                json!({"tool_name": "Bash", "tool_input": {"command": value}, "cwd": tmp_path})
            } else {
                json!({"tool_name": "Edit", "tool_input": {"file_path": nt_join(&tmp_path, &value)}, "cwd": tmp_path})
            };
            let (code, stderr) = run_guard(&payload);
            let (ok, detail);
            if expected == "block" {
                ok = code == 2 && stderr.contains("project-deny");
                detail = if ok { "blocked by the rule".to_string() } else if code == 2 { "blocked by ANOTHER rule, not this one".into() } else { "was NOT blocked".into() };
            } else {
                ok = code == 0;
                detail = if ok { "passed".to_string() } else { format!("was blocked (rule too wide or another rule): {}", stderr.trim().chars().take(120).collect::<String>()) };
            }
            results.push(CaseResult { example: example.clone(), expected: if expected == "block" { "block" } else { "pass" }, ok, detail });
        }
    }
    results
}

fn merged_permissions(root: &str, bash: &[String], path: &[String]) -> Value {
    let mut perms = load_object(&loop_file(root, "PERMISSIONS.json"));
    perms.entry("allow").or_insert(json!([]));
    for (key, new) in [("deny_bash_regex", bash), ("deny_path_regex", path)] {
        let entry = perms.entry(key).or_insert(json!([]));
        if let Value::Array(existing) = entry {
            let add: Vec<Value> = new.iter().filter(|item| !existing.contains(&json!(item))).map(|i| json!(i)).collect();
            existing.extend(add);
        }
    }
    Value::Object(perms)
}

fn regex_problems(bash: &[String], path: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    for (label, patterns) in [("--bash", bash), ("--path", path)] {
        for pattern in patterns {
            let compiled = match Re::try_build_msg(pattern, true) {
                Ok(compiled) => compiled,
                Err(message) => {
                    problems.push(format!("{label} {} is not a valid regex: {message}", pycompat::py_repr_str(pattern)));
                    continue;
                }
            };
            if compiled.is_match("") {
                problems.push(format!("{label} {} matches empty text: it would block everything", pycompat::py_repr_str(pattern)));
            }
            let wanted = if label == "--bash" { "bash" } else { "path" };
            let wide: Vec<String> = NEUTRAL.iter().filter(|(k, v)| *k == wanted && compiled.is_match(v)).map(|(_, v)| pycompat::py_repr_str(v)).collect();
            if !wide.is_empty() {
                problems.push(format!("{label} {} is too wide: it matches {}", pycompat::py_repr_str(pattern), wide.join(", ")));
            }
        }
    }
    problems
}

fn show(results: &[CaseResult], out: &mut String) {
    for item in results {
        let example: String = item.example.chars().take(48).collect();
        out.push_str(&format!("  {:<4} {:<8} {:<48} {}\n", if item.ok { "ok" } else { "FAIL" }, item.expected, example, item.detail));
    }
}

fn append_rule_row(path: &Path, row: &str) {
    let template = "# Limiting rules\n\n> Always read. Beats spec/design/skill. Conflict = ask. Owner: orchestrator (or the main agent without a loop).\n\n| ID | Rule | Why | How to enforce | Origin |\n|----|------|-----|----------------|--------|\n";
    let current = std::fs::read(path).ok().and_then(|b| String::from_utf8(b).ok());
    let mut lines: Vec<String> = match &current {
        Some(t) => splitlines(t).into_iter().map(String::from).collect(),
        None => splitlines(template).into_iter().map(String::from).collect(),
    };
    let sep_re = Re::new(r"^\|[-\s|]+\|\z");
    let separator = lines.iter().position(|l| sep_re.match_start(l.trim()).is_some() && l.contains('-'));
    let separator = match separator {
        Some(s) => s,
        None => {
            lines.push(String::new());
            lines.extend(splitlines(template).into_iter().skip(4).map(String::from));
            lines.len() - 1
        }
    };
    let mut end = separator;
    while end + 1 < lines.len() && lines[end + 1].trim().starts_with('|') {
        end += 1;
    }
    lines.insert(end + 1, row.to_string());
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{}\n", lines.join("\n")));
}

#[derive(Default)]
struct Args {
    command: String,
    root: String,
    id: String,
    phrase: String,
    rule: String,
    reason: String,
    how: String,
    bash: Vec<String>,
    path: Vec<String>,
    block: Vec<String>,
    pass: Vec<String>,
    quiet: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut positional: Vec<String> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let word = args[index].as_str();
        let next = args.get(index + 1).cloned();
        match (word, next) {
            ("--quiet", _) => {
                a.quiet = true;
                index += 1;
            }
            ("--id", Some(v)) => {
                a.id = v;
                index += 2;
            }
            ("--phrase", Some(v)) => {
                a.phrase = v;
                index += 2;
            }
            ("--rule", Some(v)) => {
                a.rule = v;
                index += 2;
            }
            ("--reason", Some(v)) => {
                a.reason = v;
                index += 2;
            }
            ("--how", Some(v)) => {
                a.how = v;
                index += 2;
            }
            ("--bash", Some(v)) => {
                a.bash.push(v);
                index += 2;
            }
            ("--path", Some(v)) => {
                a.path.push(v);
                index += 2;
            }
            ("--block", Some(v)) => {
                a.block.push(v);
                index += 2;
            }
            ("--pass", Some(v)) => {
                a.pass.push(v);
                index += 2;
            }
            (other, _) => {
                positional.push(other.to_string());
                index += 1;
            }
        }
    }
    if positional.len() < 2 {
        return Err("the following arguments are required: command, root".into());
    }
    if !["propose", "write", "test", "list"].contains(&positional[0].as_str()) {
        return Err(format!("argument command: invalid choice: {} (choose from propose, write, test, list)", pycompat::py_repr_str(&positional[0])));
    }
    a.command = positional[0].clone();
    a.root = positional[1].clone();
    Ok(a)
}

fn today() -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc()).format(format).unwrap_or_default()
}

fn cmd_check(a: &Args, write: bool) -> (i32, String) {
    let root = nt_abspath(&a.root);
    let mut out = String::new();
    if a.bash.is_empty() && a.path.is_empty() {
        return (2, "PROBLEM: give at least one --bash or --path\n".into());
    }
    if a.block.is_empty() || a.pass.is_empty() {
        return (2, "PROBLEM: without examples there is no proof. Give at least one --block and one --pass\n".into());
    }
    let mut problems = regex_problems(&a.bash, &a.path);
    if !ID_RE.match_start(&a.id).is_some() {
        problems.push("--id must be R-<number> (e.g. R-04)".into());
    }
    if write && a.phrase.trim().is_empty() {
        problems.push("--phrase is empty: only write a rule with the user's EXPLICIT phrase".into());
    }
    let tests_path = loop_file(&root, "rules.tests.json");
    let mut tests = load_object(&tests_path);
    let rules_md = project_dir(&root).join("RULES.md");
    let rules_text = std::fs::read(&rules_md).ok().and_then(|b| String::from_utf8(b).ok()).unwrap_or_default();
    let id_in_table = Re::new(&format!(r"\|\s*{}\s*\|", pycompat::escape(if a.id.is_empty() { "?" } else { &a.id }))).is_match(&rules_text);
    if tests.contains_key(&a.id) || id_in_table {
        problems.push(format!("{} already exists in RULES.md/rules.tests.json: use another ID", a.id));
    }
    for example in a.block.iter().chain(a.pass.iter()) {
        if let Err(error) = parse_example(example) {
            problems.push(error);
            break;
        }
    }
    if !problems.is_empty() {
        for item in problems {
            out.push_str(&format!("PROBLEM: {item}\n"));
        }
        return (2, out);
    }
    let perms = merged_permissions(&root, &a.bash, &a.path);
    out.push_str(&format!("{}: testing in the real guard\n", a.id));
    let results = evaluate(&perms, &a.block, &a.pass);
    show(&results, &mut out);
    let mut broken: Vec<CaseResult> = Vec::new();
    for (rule_id, data) in &tests {
        let list = |k: &str| -> Vec<String> { data.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default() };
        for mut r in evaluate(&perms, &list("block"), &list("pass")) {
            r.example = format!("[{rule_id}] {}", r.example);
            if !r.ok {
                broken.push(r);
            }
        }
    }
    if !broken.is_empty() {
        out.push_str("The new rule BREAKS older rules:\n");
        show(&broken, &mut out);
    }
    if !results.iter().all(|r| r.ok) || !broken.is_empty() {
        out.push_str("REJECTED: nothing written. Adjust the regex or the examples and run again.\n");
        return (1, out);
    }
    if !write {
        out.push_str("PASSED the test. Run `write` (with the user's --phrase) to make it count.\n");
        return (0, out);
    }
    let day = today();
    save_json(&loop_file(&root, "PERMISSIONS.json"), &perms);
    tests.insert(
        a.id.clone(),
        json!({"phrase": a.phrase.trim(), "bash": a.bash, "path": a.path, "block": a.block, "pass": a.pass, "created": day}),
    );
    save_json(&tests_path, &Value::Object(tests));
    let how = if a.how.is_empty() { "Blocked by the hook (project-deny); the subagent returns NEEDS_CONFIRM" } else { a.how.as_str() };
    let rule_text = if a.rule.is_empty() { &a.phrase } else { &a.rule };
    let reason = if a.reason.is_empty() { "-" } else { a.reason.as_str() };
    let row = format!("| {} | {} | {} | {} | user, {}: \"{}\" |", a.id, rule_text.trim(), reason.trim(), how.trim(), day, a.phrase.trim());
    append_rule_row(&rules_md, &row.replace('\n', " "));
    out.push_str(&format!("WRITTEN: {} in PERMISSIONS.json, rules.tests.json and RULES.md\n", a.id));
    (0, out)
}

fn cmd_test(a: &Args) -> (i32, String) {
    let root = nt_abspath(&a.root);
    let tests = load_object(&loop_file(&root, "rules.tests.json"));
    let perms = Value::Object(load_object(&loop_file(&root, "PERMISSIONS.json")));
    if tests.is_empty() {
        return (0, "no rules with a stored test (rules.tests.json empty or missing)\n".into());
    }
    let mut out = String::new();
    let mut failed = 0;
    for (rule_id, data) in &tests {
        let list = |k: &str| -> Vec<String> { data.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default() };
        let results = evaluate(&perms, &list("block"), &list("pass"));
        let bad: Vec<&CaseResult> = results.iter().filter(|r| !r.ok).collect();
        failed += bad.len();
        out.push_str(&format!("{rule_id}: {}/{} ok\n", results.len() - bad.len(), results.len()));
        if !bad.is_empty() && !a.quiet {
            let owned: Vec<CaseResult> = bad.iter().map(|r| (*r).clone()).collect();
            show(&owned, &mut out);
        } else {
            for item in bad {
                out.push_str(&format!("  FAIL {} {}\n", item.expected, item.example));
            }
        }
    }
    out.push_str(&if failed == 0 { "ALL rules pass\n".to_string() } else { format!("{failed} case(s) failed: PERMISSIONS.json changed or the guard changed\n") });
    (if failed > 0 { 1 } else { 0 }, out)
}

fn cmd_list(a: &Args) -> (i32, String) {
    let tests = load_object(&loop_file(&nt_abspath(&a.root), "rules.tests.json"));
    let mut out = String::new();
    for (rule_id, data) in &tests {
        let count = |k: &str| data.get(k).and_then(Value::as_array).map_or(0, Vec::len);
        out.push_str(&format!(
            "{rule_id}  {}  (bash:{} path:{}, {} block / {} pass)\n",
            data.get("phrase").and_then(Value::as_str).unwrap_or(""),
            count("bash"),
            count("path"),
            count("block"),
            count("pass")
        ));
    }
    if tests.is_empty() {
        out.push_str("no rule with a stored test\n");
    }
    (0, out)
}

/// `harness rules ...`; returns `(exit_code, stdout, stderr)`.
pub fn run_cli(args: &[String]) -> (i32, String, String) {
    let parsed = match parse_args(args) {
        Ok(a) => a,
        Err(message) => return (2, String::new(), format!("usage: harness.py rules [-h] ... {{propose,write,test,list}} root\nharness.py rules: error: {message}\n")),
    };
    let (code, out) = match parsed.command.as_str() {
        "test" => cmd_test(&parsed),
        "list" => cmd_list(&parsed),
        other => cmd_check(&parsed, other == "write"),
    };
    (code, out, String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_parse() {
        assert_eq!(parse_example("bash: rm -rf x ").unwrap(), ("bash".to_string(), "rm -rf x".to_string()));
        assert!(parse_example("sql:select").is_err());
        assert!(parse_example("path:").is_err());
    }

    #[test]
    fn too_wide_and_empty_regexes_are_reported() {
        let problems = regex_problems(&["".into(), "git".into()], &["src".into()]);
        assert!(problems[0].contains("matches empty text"));
        assert!(problems.iter().any(|p| p.contains("too wide: it matches 'git status'")));
        assert!(problems.iter().any(|p| p.contains("--path 'src' is too wide: it matches 'src/app.ts'")));
    }

    #[test]
    fn rows_are_appended_under_the_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("RULES.md");
        append_rule_row(&path, "| R-01 | a | b | c | d |");
        append_rule_row(&path, "| R-02 | a | b | c | d |");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("|----|------|-----|----------------|--------|\n| R-01 | a | b | c | d |\n| R-02 | a | b | c | d |\n"));
    }
}
