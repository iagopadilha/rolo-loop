// SPDX-License-Identifier: MIT
//! Generates the loop's final summary in Markdown, WITHOUT Claude: it only reads files and the git log.
//!
//! `harness summary <project-or-worktree-root> [--out <folder>] [--base <ref>] [--branch <name>]`. Output:
//! `<out>/loop-summary-<project>-<YYYY-MM-DD>.md` (default: `output_dir` of config.json, `~/Downloads` when unset).

use crate::graph_view;
use crate::loop_io::{git, load_json, read};
use crate::loop_state;
use cfg::Env;
use pycompat::{nt_abspath, nt_join, splitlines, Re};
use serde_json::Value;
use std::path::Path;
use std::sync::LazyLock;

static LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^- \[([^\]]+)\]\s+(.*)\z"));
static KEY_VALUE: LazyLock<Re> = LazyLock::new(|| Re::new(r"(\w+)=(\S+)"));
const SECTIONS: [&str; 6] = ["Delivered", "User decisions", "Lessons", "Unverified", "Blocked", "For the next stage"];

/// The app's output folder (`output_dir` in config.json, default `~/Downloads`).
pub fn default_output_dir(env: &Env) -> String {
    let config = cfg::locator::read_user_config(&env.user_data_dir());
    let configured = config.get("output_dir").and_then(Value::as_str).unwrap_or("");
    if configured.is_empty() {
        nt_join(&env.home_dir().display().to_string(), "Downloads")
    } else {
        configured.to_string()
    }
}

struct Stage {
    title: String,
    features: Vec<Vec<(String, String)>>,
}

fn parse_roadmap(text: &str) -> Vec<Stage> {
    let mut stages: Vec<Stage> = Vec::new();
    for raw in splitlines(text) {
        if let Some(title) = raw.strip_prefix("## ") {
            stages.push(Stage { title: title.trim().to_string(), features: vec![] });
            continue;
        }
        if let (Some(m), Some(stage)) = (LINE.match_start(raw.trim()), stages.last_mut()) {
            let mut fields: Vec<(String, String)> = Vec::new();
            for kv in KEY_VALUE.find_all(m.group(2).unwrap_or("")) {
                set(&mut fields, kv.group(1).unwrap_or(""), kv.group(2).unwrap_or(""));
            }
            set(&mut fields, "slug", m.group(1).unwrap_or(""));
            stage.features.push(fields);
        }
    }
    stages
}

/// `[(slug, status or "?")]` of every feature of the ROADMAP text, in file order.
pub fn feature_statuses(text: &str) -> Vec<(String, String)> {
    parse_roadmap(text)
        .iter()
        .flat_map(|stage| stage.features.iter())
        .map(|f| (field(f, "slug").unwrap_or("").to_string(), field(f, "status").unwrap_or("?").to_string()))
        .collect()
}

fn set(fields: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some(slot) = fields.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value.to_string();
    } else {
        fields.push((key.to_string(), value.to_string()));
    }
}

fn field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// `## name` sections of a document, name -> stripped body.
fn sections(text: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<usize> = None;
    for raw in splitlines(text) {
        if let Some(name) = raw.strip_prefix("## ") {
            let name = name.trim().to_string();
            match found.iter().position(|(n, _)| *n == name) {
                Some(i) => {
                    found[i].1.clear();
                    current = Some(i);
                }
                None => {
                    found.push((name, vec![]));
                    current = Some(found.len() - 1);
                }
            }
        } else if let Some(i) = current {
            found[i].1.push(raw.to_string());
        }
    }
    found.into_iter().map(|(k, v)| (k, v.join("\n").trim().to_string())).collect()
}

/// Python `str(x)` of a JSON value (for a `%s` in the summary).
fn pystr(v: Option<&Value>, default: &str) -> String {
    match v {
        None | Some(Value::Null) => default.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn number(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn add_repositories_section(add: &mut Vec<String>, meta: &Value, primary_root: &str) {
    let Some(Value::Object(repos)) = meta.get("repos") else { return };
    if repos.is_empty() {
        return;
    }
    add.push(String::new());
    add.push("## Repositories".into());
    let mut names: Vec<&String> = repos.keys().collect();
    names.sort_by_key(|k| repos[*k].get("primary").is_none_or(|p| !truthy(Some(p))));
    for name in names {
        let entry = &repos[name];
        let primary = truthy(entry.get("primary"));
        let worktree = if primary { primary_root.to_string() } else { pystr(entry.get("worktree"), "") };
        let delivery =
            if worktree.is_empty() { Value::Object(Default::default()) } else { load_json(&Path::new(&worktree).join(".specs/project/loop/json/delivery.json")) };
        add.push(String::new());
        add.push(format!("### {name}{}", if primary { " (primary)" } else { "" }));
        add.push(String::new());
        let branch = if truthy(entry.get("branch")) { pystr(entry.get("branch"), "") } else { "not reported".into() };
        let forked = if truthy(entry.get("base")) { format!(" (forked from `{}`)", pystr(entry.get("base"), "")) } else { String::new() };
        add.push(format!("- Branch: `{branch}`{forked}"));
        add.push(format!("- Work folder: `{}`", if worktree.is_empty() { "not reported".to_string() } else { worktree.clone() }));
        add.push(format!("- Branch push: {}", if truthy(delivery.get("push")) { pystr(delivery.get("push"), "") } else { "not done".into() }));
        if truthy(delivery.get("pr")) {
            add.push(format!("- **PR: {}** ({})", pystr(delivery.get("pr"), ""), pystr(delivery.get("pr_status"), "")));
        }
        let base = if truthy(delivery.get("base")) { pystr(delivery.get("base"), "") } else { pystr(entry.get("base"), "") };
        let commits = if !worktree.is_empty() && Path::new(&worktree).is_dir() {
            let range = if !base.is_empty() {
                format!("{base}..{}", if truthy(entry.get("branch")) { pystr(entry.get("branch"), "") } else { "HEAD".into() })
            } else {
                "-30".into()
            };
            let (code, out, _) = git(&worktree, &["log", "--oneline", "--no-decorate", &range]);
            if code == 0 {
                out
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        add.push(String::new());
        add.push(format!("```\n{}\n```", if commits.is_empty() { "no commits found" } else { &commits }));
    }
}

fn git_out(root: &str, args: &[&str]) -> String {
    let (code, out, _) = git(root, args);
    if code == 0 {
        out
    } else {
        String::new()
    }
}

pub const USAGE: &str = include_str!("loop_summary_usage.txt");

/// `harness summary ...`; returns `(exit_code, stdout)`.
pub fn run_cli(args: &[String], env: &Env) -> (i32, String) {
    if args.is_empty() {
        return (2, format!("{USAGE}\n"));
    }
    let root = nt_abspath(&args[0]);
    let opt = |flag: &str, default: &str| -> String { args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| default.to_string()) };
    let trimmed = root.trim_end_matches(['/', '\\']);
    let project_raw = trimmed.rsplit(['\\', '/']).next().unwrap_or("");
    let project = Re::new(r"-(worktree-)?loop\z").replace_lit(project_raw, "");
    let specs = Path::new(&root).join(".specs");
    let meta = load_json(&specs.join("project/loop/json/meta.json"));
    let loop_json = load_json(&specs.join("project/loop/json/LOOP.json"));
    let branch_default = if truthy(meta.get("branch")) { pystr(meta.get("branch"), "") } else { git_out(&root, &["branch", "--show-current"]) };
    let branch = opt("--branch", &branch_default);
    let base = opt("--base", &pystr(meta.get("base"), ""));

    let (mut cost, mut duration, mut turns) = (0.0f64, 0.0f64, 0usize);
    let turns_dir = specs.join("project/loop/internal/turns");
    let mut files: Vec<String> = std::fs::read_dir(&turns_dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".json") && n.chars().next().is_some_and(|c| c.is_ascii_digit()))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    for name in files {
        let data = load_json(&turns_dir.join(name));
        turns += 1;
        cost += number(data.get("total_cost_usd"));
        duration += number(data.get("duration_ms"));
    }

    let roadmap_text = read(&specs.join("project/ROADMAP.md"));
    let stages = parse_roadmap(&roadmap_text);
    let mut counts: Vec<(String, usize)> = Vec::new();
    for stage in &stages {
        for feature in &stage.features {
            let status = field(feature, "status").unwrap_or("?").to_string();
            match counts.iter_mut().find(|(k, _)| *k == status) {
                Some((_, n)) => *n += 1,
                None => counts.push((status, 1)),
            }
        }
    }
    counts.sort();
    let range = if base.is_empty() { "-30".to_string() } else { format!("{base}..{branch}") };
    let commits = git_out(&root, &["log", "--oneline", "--no-decorate", &range]);

    let turns_shown = if turns > 0 { turns.to_string() } else { pystr(meta.get("turns"), "?") };
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let stamp = now.format(time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]")).unwrap_or_default();
    let day = now.format(time::macros::format_description!("[year]-[month]-[day]")).unwrap_or_default();

    let mut out: Vec<String> = Vec::new();
    out.push(format!("# Loop summary: {project}"));
    out.push(String::new());
    out.push(format!("- Generated: {stamp} (by script, no Claude)"));
    out.push(format!(
        "- Loop branch: `{}`{}",
        if branch.is_empty() { "not reported" } else { &branch },
        if base.is_empty() { String::new() } else { format!(" (base `{base}`)") }
    ));
    out.push(format!("- Work folder: `{root}`"));
    out.push(format!("- Turns run: {turns_shown} | total cost: US$ {cost:.2} | time in turns: {:.0} min", duration / 60000.0));
    let max_cost_flag = opt("--max-cost", "0");
    let cap = {
        let from_meta = number(meta.get("max_cost_usd"));
        if from_meta != 0.0 {
            from_meta
        } else {
            max_cost_flag.parse::<f64>().unwrap_or(0.0)
        }
    };
    let spent = if meta.get("cost_usd").is_some() && truthy(meta.get("cost_usd")) {
        number(meta.get("cost_usd"))
    } else if meta.get("cost_usd").is_some() {
        0.0
    } else {
        cost
    };
    out.push(format!("- Spend: US$ {spent:.2} of the US$ {} cap | turns: {turns_shown}", if cap != 0.0 { format!("{cap:.2}") } else { "no".into() }));
    let stopped = if truthy(meta.get("stopped")) {
        pystr(meta.get("stopped"), "")
    } else if truthy(loop_json.get("status")) {
        pystr(loop_json.get("status"), "")
    } else {
        "not reported".into()
    };
    let reason = if truthy(meta.get("reason")) { pystr(meta.get("reason"), "") } else { pystr(loop_json.get("reason").filter(|v| truthy(Some(v))), "") };
    out.push(format!("- Stop reason: **{stopped}**{}", if reason.is_empty() { String::new() } else { format!(" - {reason}") }));
    out.push(format!("- Branch push: {}", if meta.get("push").is_some() { pystr(meta.get("push"), "") } else { "not done".into() }));
    if truthy(meta.get("pr")) {
        out.push(format!("- **PR: {}** ({})", pystr(meta.get("pr"), ""), pystr(meta.get("pr_status"), "")));
    }
    out.push(format!(
        "- Features: {}",
        if counts.is_empty() { "none found in the ROADMAP".to_string() } else { counts.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", ") }
    ));
    out.push(String::new());
    out.push("## Features by stage".into());
    if stages.is_empty() {
        out.push("ROADMAP.md not found.".into());
    }
    for stage in &stages {
        out.push(String::new());
        out.push(format!("### {}", stage.title));
        out.push(String::new());
        out.push("| Feature | Status | Scope | Kind | Paths | Notes |".into());
        out.push("|---|---|---|---|---|---|".into());
        for f in &stage.features {
            let note = field(f, "note").unwrap_or("");
            let origin = field(f, "origin").map(|o| format!("origin {o}")).unwrap_or_default();
            let obs: Vec<&str> = [note, origin.as_str()].into_iter().filter(|s| !s.is_empty()).collect();
            let obs = obs.join(" ");
            out.push(format!(
                "| {} | {} | {} | {} | {} | {} |",
                field(f, "slug").unwrap_or(""),
                field(f, "status").unwrap_or("?"),
                field(f, "scope").unwrap_or("-"),
                field(f, "kind").unwrap_or("-"),
                field(f, "paths").unwrap_or("-"),
                if obs.is_empty() { "-" } else { &obs }
            ));
        }
    }
    let features_now = graph_view::load(&root);
    out.push(String::new());
    out.push("## Progress by feature".into());
    out.push(String::new());
    out.push(format!("```\n{}\n```", graph_view::ascii_graph(&features_now)));
    out.push(String::new());
    out.push(format!("```mermaid\n{}\n```", graph_view::mermaid(&features_now)));
    out.push(String::new());
    out.push("## State graph".into());
    out.push(String::new());
    out.push(format!("```mermaid\n{}\n```", loop_state::graph(&roadmap_text)));
    out.push(String::new());
    out.push("## Loop commits".into());
    out.push(String::new());
    out.push(format!("```\n{}\n```", if commits.is_empty() { "no commits found" } else { &commits }));
    add_repositories_section(&mut out, &meta, &root);

    let phases_dir = specs.join("project/phases");
    let mut phases: Vec<String> = std::fs::read_dir(&phases_dir)
        .map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("stage-") && n.ends_with(".md")).collect())
        .unwrap_or_default();
    phases.sort();
    for name in &phases {
        let parts = sections(&read(&phases_dir.join(name)));
        out.push(String::new());
        out.push(format!("## Closing: {}", name.trim_end_matches(".md")));
        for section in SECTIONS {
            let body = parts.iter().find(|(k, _)| k == section).map(|(_, v)| v.trim().to_string()).unwrap_or_default();
            if !body.is_empty() {
                out.push(String::new());
                out.push(format!("**{section}**"));
                out.push(String::new());
                out.push(body);
            }
        }
    }
    if phases.is_empty() {
        out.push(String::new());
        out.push("## Stage closing".into());
        out.push(String::new());
        out.push("No `phases/stage-NN.md` found (the loop stopped before closing a stage).".into());
    }
    let state_text = read(&specs.join("project/STATE.md"));
    let block_re = Re::ci(r"^#+\s*(STOP|Questions|Quarantine)");
    let pending: Vec<String> = Re::new(r"\n(?=#+ )").split(&state_text).into_iter().filter(|b| block_re.match_start(b).is_some()).collect();
    if !pending.is_empty() {
        out.push(String::new());
        out.push("## For you to decide (from STATE.md)".into());
        for block in pending {
            out.push(String::new());
            out.push(block.trim().to_string());
        }
    }
    let validate_text = read(&specs.join("project/loop/VALIDATE.md"));
    let to_validate: Vec<String> = splitlines(&validate_text).into_iter().filter(|l| l.trim().starts_with("- ")).map(|l| l.trim()[2..].to_string()).collect();
    if !to_validate.is_empty() {
        out.push(String::new());
        out.push("## For you to validate (decided without stopping the loop)".into());
        out.push(String::new());
        out.extend(to_validate.into_iter().map(|i| format!("- {i}")));
    }
    if truthy(loop_json.get("next")) {
        out.push(String::new());
        out.push("## Next step".into());
        out.push(String::new());
        out.push(pystr(loop_json.get("next"), ""));
    }
    let dest = {
        let d = opt("--out", "");
        if d.is_empty() {
            default_output_dir(env)
        } else {
            d
        }
    };
    let _ = std::fs::create_dir_all(&dest);
    let target = nt_join(&dest, &format!("loop-summary-{project}-{day}.md"));
    let _ = std::fs::write(&target, format!("{}\n", out.join("\n")));
    (0, format!("{target}\n"))
}
