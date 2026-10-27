// SPDX-License-Identifier: MIT
//! Classifies a project's structure as STANDARD or LEGACY and (once confirmed) writes it to CLAUDE.md.
//!
//! The suggestion is only a suggestion: the user decides. STANDARD = the skills' structure (back end Api/Data/Domain: .NET
//! projects or NestJS folders under src/; front end with src/pages + src/components). LEGACY = anything else.

use pycompat::{nt_abspath, Re};
use std::path::Path;

pub const BEGIN: &str = "<!-- CONFORMANCE:BEGIN -->";
pub const END: &str = "<!-- CONFORMANCE:END -->";
/// The one shared list of folder names no scan descends into.
pub const SKIP_DIR_NAMES: [&str; 10] = ["node_modules", "bin", "obj", ".git", "dist", "build", ".next", ".specs", ".vs", "coverage"];

pub fn is_skipped(name: &str) -> bool {
    SKIP_DIR_NAMES.contains(&name) || name == "__pycache__"
}

fn subdirs(path: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(path) else { return vec![] };
    entries.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| !is_skipped(n)).collect()
}

fn files_with_extension(root: &Path, extension: &str) -> bool {
    std::fs::read_dir(root).map(|r| r.flatten().any(|e| e.path().is_file() && e.file_name().to_string_lossy().to_lowercase().ends_with(extension))).unwrap_or(false)
}

/// `(stack, verdict, evidence)`.
pub fn analyze(root: &str) -> (String, String, Vec<String>) {
    let mut evidence = Vec::new();
    let mut stack = "unknown".to_string();
    let mut verdict = "LEGACY".to_string();
    let rootp = Path::new(root);
    let top = subdirs(rootp);
    let csproj_here = files_with_extension(rootp, ".csproj");
    let solutions = files_with_extension(rootp, ".sln");
    let package = rootp.join("package.json");
    if solutions || csproj_here {
        stack = ".NET".into();
        let mut all_found = true;
        for layer in ["api", "data", "domain"] {
            let pattern = Re::ci(&format!(r"(^|\.){layer}\z"));
            let found: Vec<&String> = top.iter().filter(|n| pattern.search(n).is_some()).collect();
            let mut label = layer.to_string();
            label[..1].make_ascii_uppercase();
            evidence.push(format!(
                "project/folder {label}: {}",
                if found.is_empty() { "NOT FOUND".to_string() } else { found.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ") }
            ));
            all_found &= !found.is_empty();
        }
        if all_found {
            verdict = "STANDARD".into();
        }
        if csproj_here && !all_found {
            evidence.push(".csproj in the root (single project): flat structure".into());
        }
        let flat: Vec<&String> = top.iter().filter(|n| ["Controllers", "Services", "Models", "Utils", "Interfaces"].contains(&n.as_str())).collect();
        if !flat.is_empty() {
            evidence.push(format!("flat folders in the root: {}", flat.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
        }
    } else if package.exists() {
        let src_dirs: Vec<String> = subdirs(&rootp.join("src")).iter().map(|n| n.to_lowercase()).collect();
        let text = String::from_utf8_lossy(&std::fs::read(&package).unwrap_or_default()).into_owned();
        if text.contains("@nestjs") {
            stack = "NestJS".into();
            for layer in ["api", "data", "domain"] {
                evidence.push(format!("src/{layer}: {}", if src_dirs.iter().any(|d| d == layer) { "ok" } else { "NOT FOUND" }));
            }
            if ["api", "data", "domain"].iter().all(|l| src_dirs.iter().any(|d| d == l)) {
                verdict = "STANDARD".into();
            }
        } else {
            let app_dir = rootp.join("app").is_dir();
            stack = if text.contains("react") && !app_dir { "React/Vite/TS".into() } else { "React/Next".into() };
            for name in ["pages", "components", "services", "hooks"] {
                evidence.push(format!("src/{name}: {}", if src_dirs.iter().any(|d| d == name) { "ok" } else { "NOT FOUND" }));
            }
            if app_dir && !src_dirs.iter().any(|d| d == "pages") {
                evidence.push("app/ folder in the root (Next app router structure, no src/pages)".into());
            }
            if src_dirs.iter().any(|d| d == "pages") && src_dirs.iter().any(|d| d == "components") {
                verdict = "STANDARD".into();
            }
        }
    } else {
        evidence.push("neither .sln/.csproj nor package.json in the root: confirm the project root".into());
    }
    (stack, verdict, evidence)
}

fn today() -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc()).format(format).unwrap_or_default()
}

pub fn section(classification: &str, stack: &str, suggestion: &str, note: &str) -> String {
    let mut lines = vec![
        BEGIN.to_string(),
        "## Conformance".to_string(),
        format!("Conformance: {classification}"),
        format!("Stack: {stack}"),
        format!("Defined by: user on {} (script suggestion: {suggestion})", today()),
    ];
    if !note.is_empty() {
        lines.push(format!("Note: {note}"));
    }
    if classification == "STANDARD" {
        lines.push("Rule: new code follows the skills' standard (back end Api/Data/Domain; front end pages/components). A file the feature touches may be edited and the touched class refactored to the standard (only it), with a regression check offered.".into());
    } else {
        lines.push("Rule: LEGACY. New code follows the pattern the project already has (no new layer). Old files only get minimal edits, no renaming or splitting; refactoring becomes a Point of attention. Never touch anything outside the feature's PATHS.".into());
    }
    lines.push("Scope (always): only the feature's PATHS and only repo code. A new file is fine if it stays in scope; a large deviation = stop and ask.".into());
    lines.push(END.to_string());
    lines.push(String::new());
    lines.join("\n")
}

pub fn write_claude_md(root: &str, classification: &str, stack: &str, suggestion: &str, note: &str) -> String {
    let path = Path::new(root).join("CLAUDE.md");
    let block = section(classification, stack, suggestion, note);
    let mut text = if path.exists() {
        String::from_utf8_lossy(&std::fs::read(&path).unwrap_or_default()).into_owned()
    } else {
        let name = nt_abspath(root);
        format!("# {}\n\n", name.rsplit('\\').next().unwrap_or(""))
    };
    if text.contains(BEGIN) {
        let pattern = Re::new(&format!(r"(?s){}.*?{}\n?", pycompat::escape(BEGIN), pycompat::escape(END)));
        if let Some(m) = pattern.search(&text) {
            text = format!("{}{}{}", &text[..m.start], block, &text[m.end..]);
        }
    } else {
        text = format!("{}\n\n{}", text.trim_end_matches('\n'), block);
    }
    let _ = std::fs::write(&path, &text);
    path.display().to_string()
}

pub const USAGE: &str = include_str!("classify_structure_usage.txt");

/// `harness classify <root> [--write STANDARD|LEGACY] [--note TEXT]`; returns `(exit_code, stdout)`.
pub fn run_cli(args: &[String]) -> (i32, String) {
    if args.is_empty() {
        return (2, format!("{USAGE}\n"));
    }
    let root = nt_abspath(&args[0]);
    let (stack, suggestion, evidence) = analyze(&root);
    if let Some(i) = args.iter().position(|a| a == "--write") {
        let classification = args.get(i + 1).map(|s| s.to_uppercase()).unwrap_or_default();
        if classification != "STANDARD" && classification != "LEGACY" {
            return (2, "use STANDARD or LEGACY\n".into());
        }
        let note = args.iter().position(|a| a == "--note").and_then(|j| args.get(j + 1)).cloned().unwrap_or_default();
        return (0, format!("written to {}\n", write_claude_md(&root, &classification, &stack, &suggestion, &note)));
    }
    let mut out = format!("ROOT: {root}\nSTACK: {stack}\nSUGGESTION: {suggestion}\n");
    for line in evidence {
        out.push_str(&format!(" - {line}\n"));
    }
    out.push_str("QUESTION FOR THE USER: is this project already in the skills' new standard (STANDARD) or is it legacy (LEGACY)?\n");
    out.push_str("WHY ASK: in LEGACY the agent only makes minimal edits to old files (without tests, refactoring there causes silent regressions); in STANDARD it may refactor the class it touches.\n");
    (0, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotnet_layers_make_it_standard() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["App.Api", "App.Data", "App.Domain"] {
            std::fs::create_dir(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("App.sln"), "").unwrap();
        let (stack, verdict, evidence) = analyze(&dir.path().display().to_string());
        assert_eq!((stack.as_str(), verdict.as_str()), (".NET", "STANDARD"));
        assert_eq!(evidence[0], "project/folder Api: App.Api");
    }

    #[test]
    fn write_is_idempotent_and_replaces_the_block() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        write_claude_md(&root, "LEGACY", "unknown", "LEGACY", "n1");
        let first = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        write_claude_md(&root, "LEGACY", "unknown", "LEGACY", "n1");
        assert_eq!(first, std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap());
        write_claude_md(&root, "STANDARD", "NestJS", "STANDARD", "");
        let text = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert_eq!(text.matches(BEGIN).count(), 1);
        assert!(text.contains("Conformance: STANDARD") && !text.contains("n1"));
    }
}
