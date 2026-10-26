// SPDX-License-Identifier: MIT
//! Draws the loop progress in 3 formats, from the ROADMAP (skill orchestrator-show-progress-graph).
//!
//! `harness graph <root> [--ascii | --short | --mermaid | --write]`.
//! Legend: `[x]` verified, `[>]` building/speccing, `[.]` pending/specced/approved, `[!]` blocked.

use crate::loop_state::{parse_roadmap, split_deps, state_file, stuck_by_blocked, Features};
use pycompat::{nt_abspath, Re};
use std::path::Path;
use std::sync::LazyLock;

static NON_WORD: LazyLock<Re> = LazyLock::new(|| Re::new(r"\W"));

fn symbol(status: &str) -> &'static str {
    match status {
        "verified" => "[x]",
        "building" | "speccing" => "[>]",
        "pending" | "specced" | "approved" => "[.]",
        "blocked" => "[!]",
        _ => "[?]",
    }
}

fn word(status: &str) -> String {
    match status {
        "verified" => "done",
        "building" => "building",
        "speccing" => "spec",
        "pending" => "pending",
        "specced" => "spec ok",
        "approved" => "approved",
        "blocked" => "BLOCKED",
        other => other,
    }
    .to_string()
}

fn word_or_q(status: Option<&str>) -> String {
    match status {
        Some(s) if ["verified", "building", "speccing", "pending", "specced", "approved", "blocked"].contains(&s) => word(s),
        _ => "?".into(),
    }
}

pub fn load(root: &str) -> Features {
    let path = Path::new(&nt_abspath(root)).join(".specs").join("project").join("ROADMAP.md");
    match std::fs::read(path) {
        Ok(bytes) => parse_roadmap(&String::from_utf8_lossy(&bytes)).0,
        Err(_) => Features::default(),
    }
}

/// `[(stage, [slugs])]` in order of first appearance.
fn stages(features: &Features) -> Vec<(String, Vec<String>)> {
    let mut order: Vec<(String, Vec<String>)> = Vec::new();
    for (slug, f) in features.iter() {
        let stage = f.text("stage").to_string();
        match order.iter_mut().find(|(s, _)| *s == stage) {
            Some((_, list)) => list.push(slug.clone()),
            None => order.push((stage, vec![slug.clone()])),
        }
    }
    order
}

fn waiting_on(features: &Features, slug: &str) -> Vec<String> {
    let (slugs, folders) = split_deps(features.get(slug).expect("exists"));
    let mut parts: Vec<String> = slugs.iter().map(|d| format!("{d}({})", word_or_q(features.get(d).map(|f| f.status())))).collect();
    parts.extend(folders.iter().map(|f| format!("dir:{f}")));
    parts
}

pub fn ascii_graph(features: &Features) -> String {
    if features.is_empty() {
        return "(ROADMAP has no features)".into();
    }
    let width = features.iter().map(|(s, _)| s.chars().count()).max().unwrap_or(0) + 1;
    let mut out: Vec<String> = Vec::new();
    for (stage, slugs) in stages(features) {
        let closed = slugs.iter().filter(|s| ["verified", "blocked"].contains(&features.get(s).expect("exists").status())).count();
        out.push(format!("Stage {}  ({closed}/{} closed)", if stage.is_empty() { "?" } else { &stage }, slugs.len()));
        for slug in &slugs {
            let f = features.get(slug).expect("exists");
            let status = f.status();
            let mut line = format!("  {} {:<width$} {}", symbol(status), slug, if ["verified", "building", "speccing", "pending", "specced", "approved", "blocked"].contains(&status) { word(status) } else { status.to_string() });
            let deps = waiting_on(features, slug);
            if status != "verified" && !deps.is_empty() {
                line.push_str(&format!("   <- {}", deps.join(" ")));
            }
            let stuck = stuck_by_blocked(features, slug);
            if !stuck.is_empty() && status != "blocked" {
                line.push_str(&format!("   STUCK on: {}", stuck.join(", ")));
            }
            if status == "blocked" && !f.text("tries").is_empty() {
                line.push_str(&format!("   ({} failures)", f.text("tries")));
            }
            out.push(line);
        }
    }
    out.push(String::new());
    out.push("Legend: [x] done  [>] in progress  [.] to do  [!] blocked  (<- waits for)".into());
    out.join("\n")
}

pub fn short(features: &Features, limit: usize) -> String {
    if features.is_empty() {
        return "ROADMAP empty".into();
    }
    let parts: Vec<String> = stages(features)
        .iter()
        .map(|(stage, slugs)| {
            let items = slugs.iter().map(|s| format!("{s} {}", word(features.get(s).expect("exists").status()))).collect::<Vec<_>>().join(", ");
            format!("S{}: {items}", if stage.is_empty() { "?" } else { stage })
        })
        .collect();
    let text = parts.join(" | ");
    if text.chars().count() <= limit {
        return text;
    }
    let mut counts: Vec<(String, usize)> = Vec::new();
    for (_, f) in features.iter() {
        match counts.iter_mut().find(|(k, _)| k == f.status()) {
            Some((_, n)) => *n += 1,
            None => counts.push((f.status().to_string(), 1)),
        }
    }
    counts.sort_by(|a, b| a.0.cmp(&b.0));
    let line = format!("{} features: {}", features.len(), counts.iter().map(|(k, n)| format!("{n} {}", word(k))).collect::<Vec<_>>().join(", "));
    line.chars().take(limit).collect()
}

fn node_id(slug: &str) -> String {
    format!("n_{}", NON_WORD.replace_lit(slug, "_"))
}

pub fn mermaid(features: &Features) -> String {
    let mut lines = vec!["flowchart LR".to_string()];
    for (stage, slugs) in stages(features) {
        let id = NON_WORD.replace_lit(if stage.is_empty() { "x" } else { &stage }, "_");
        lines.push(format!("  subgraph S{id}[\"Stage {}\"]", if stage.is_empty() { "?" } else { &stage }));
        for slug in &slugs {
            let status = features.get(slug).expect("exists").status();
            lines.push(format!("    {}[\"{slug}<br/>{}\"]:::{status}", node_id(slug), word_or_q(Some(status))));
        }
        lines.push("  end".into());
    }
    for (slug, f) in features.iter() {
        for dep in split_deps(f).0 {
            if features.contains(&dep) {
                lines.push(format!("  {} --> {}", node_id(&dep), node_id(slug)));
            }
        }
    }
    lines.extend(
        [
            "  classDef verified fill:#d1fae5,stroke:#047857",
            "  classDef building fill:#dbeafe,stroke:#1d4ed8",
            "  classDef speccing fill:#dbeafe,stroke:#1d4ed8",
            "  classDef blocked fill:#fee2e2,stroke:#b91c1c",
            "  classDef pending fill:#f3f4f6,stroke:#6b7280",
            "  classDef specced fill:#f3f4f6,stroke:#6b7280",
            "  classDef approved fill:#fef3c7,stroke:#b45309",
        ]
        .map(String::from),
    );
    lines.join("\n")
}

/// Writes `.specs/project/loop/GRAPH.md` and returns its path.
pub fn write(root: &str) -> String {
    let features = load(root);
    let target = state_file(&nt_abspath(root), "GRAPH.md");
    let _ = std::fs::create_dir_all(target.parent().expect("has parent"));
    let format = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    let stamp = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc()).format(format).unwrap_or_default();
    let text = format!(
        "# Progress (generated by graph_view.py on {stamp}; do not edit)\n\n```\n{}\n```\n\n```mermaid\n{}\n```\n",
        ascii_graph(&features),
        mermaid(&features)
    );
    let _ = std::fs::write(&target, text);
    target.display().to_string()
}

pub const USAGE: &str = include_str!("graph_view_usage.txt");

pub fn run_cli(args: &[String]) -> (i32, String) {
    if args.is_empty() {
        return (2, format!("{USAGE}\n"));
    }
    let root = &args[0];
    if args.iter().any(|a| a == "--write") {
        return (0, format!("{}\n", write(root)));
    }
    let features = load(root);
    let text = if args.iter().any(|a| a == "--mermaid") {
        mermaid(&features)
    } else if args.iter().any(|a| a == "--short") {
        short(&features, 300)
    } else {
        ascii_graph(&features)
    };
    (0, format!("{text}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROADMAP: &str = "## Stage 1\n- [api] status=verified paths=a\n- [ui] status=building paths=b deps=api,dir:shared\n## Stage 2\n- [late] status=blocked tries=2 paths=c deps=ui\n- [after] status=pending deps=late\n";

    #[test]
    fn ascii_shows_deps_stuck_and_failures() {
        let (f, _) = parse_roadmap(ROADMAP);
        let text = ascii_graph(&f);
        assert!(text.starts_with("Stage 1  (1/2 closed)\n"));
        assert!(text.contains("   <- api(done) dir:shared"));
        assert!(text.contains("BLOCKED   <- ui(building)   (2 failures)"));
        assert!(text.contains("pending   <- late(BLOCKED)   STUCK on: late"));
        assert!(text.ends_with("Legend: [x] done  [>] in progress  [.] to do  [!] blocked  (<- waits for)"));
    }

    #[test]
    fn short_and_mermaid() {
        let (f, _) = parse_roadmap(ROADMAP);
        assert_eq!(short(&f, 300), "S1: api done, ui building | S2: late BLOCKED, after pending");
        assert!(short(&f, 20).len() <= 20);
        let m = mermaid(&f);
        assert!(m.contains("  subgraph S1[\"Stage 1\"]") && m.contains("n_api --> n_ui") && m.contains("n_late[\"late<br/>BLOCKED\"]:::blocked"));
    }
}
