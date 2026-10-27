// SPDX-License-Identifier: MIT
//! Automatic worktree preparation of a repo, pure: what to install and which local files to copy. Nothing runs while detecting.

use pycompat::fnmatch_nt;
use serde_json::{json, Value};
use std::path::Path;

pub const HEAVY_FOLDER_NAMES: [&str; 4] = ["node_modules", "bin", "obj", "dist"];
const SUGGESTION_PATTERNS: [&str; 5] = [".env", ".env.*", "appsettings.*.json", "*.local.*", "*.local"];
const SECRET_PATTERNS: [&str; 4] = [".env", ".env.*", "appsettings.json", "appsettings.*.json"];
const EXAMPLE_FILE_SUFFIXES: [&str; 3] = [".example", ".sample", ".template"];
const SUGGESTION_LIMIT: usize = 100;
pub const IGNORED_FILES_ARGUMENTS: [&str; 5] = ["ls-files", "--others", "--ignored", "--exclude-standard", "--directory"];

const SETUP_BY_LOCKFILE: [(&str, &str); 4] = [
    ("pnpm-lock.yaml", "pnpm install --frozen-lockfile"),
    ("yarn.lock", "yarn install --frozen-lockfile"),
    ("package-lock.json", "npm ci"),
    ("package.json", "npm install"),
];
const SETUP_BY_REQUIREMENTS: [(&str, &str); 3] =
    [("requirements.txt", "python -m pip install -r requirements.txt"), ("poetry.lock", "poetry install"), ("go.mod", "go mod download")];

fn has_project_file(folder: &Path) -> bool {
    std::fs::read_dir(folder).map(|d| d.flatten().any(|e| is_project_name(&e.file_name().to_string_lossy()))).unwrap_or(false)
}

fn is_project_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    !name.starts_with('.') && (lower.ends_with(".sln") || lower.ends_with(".csproj"))
}

/// `*.sln`, `*.csproj`, `*/*.sln`, `*/*.csproj` under the folder (hidden names are not matched by a glob `*`).
pub fn has_dotnet_project(folder: &str) -> bool {
    let root = Path::new(folder);
    if has_project_file(root) {
        return true;
    }
    std::fs::read_dir(root)
        .map(|d| d.flatten().any(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().is_dir() && has_project_file(&e.path())))
        .unwrap_or(false)
}

/// The one-line setup command for the repo at `folder` (first match only), or `None`. Looks only at root files.
pub fn detect_setup(folder: &str) -> Option<String> {
    if folder.is_empty() || !Path::new(folder).is_dir() {
        return None;
    }
    let root = Path::new(folder);
    if let Some((_, command)) = SETUP_BY_LOCKFILE.iter().find(|(file, _)| root.join(file).is_file()) {
        return Some(command.to_string());
    }
    if has_dotnet_project(folder) {
        return Some("dotnet restore".into());
    }
    SETUP_BY_REQUIREMENTS.iter().find(|(file, _)| root.join(file).is_file()).map(|(_, c)| c.to_string())
}

fn matches_any(name: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| fnmatch_nt(name, p))
}

/// Local-config files among the git-ignored paths (`git ls-files --others --ignored ...` output), as sorted
/// `[{"path", "size", "secret"}]`. Heavy folders and example/sample/template files are never suggested.
pub fn suggest_copy_files(folder: &str, ignored_output: &str) -> Vec<Value> {
    let mut suggestions: Vec<(String, u64, bool)> = Vec::new();
    for line in pycompat::splitlines(ignored_output) {
        let relative = line.replace('\\', "/").trim().trim_matches('"').to_string();
        let parts: Vec<&str> = relative.split('/').collect();
        if relative.is_empty() || relative.ends_with('/') || parts.iter().any(|p| HEAVY_FOLDER_NAMES.contains(p)) {
            continue;
        }
        let file_name = parts[parts.len() - 1];
        let lower = file_name.to_lowercase();
        if EXAMPLE_FILE_SUFFIXES.iter().any(|s| lower.ends_with(s)) || !matches_any(file_name, &SUGGESTION_PATTERNS) {
            continue;
        }
        let absolute = parts.iter().fold(Path::new(folder).to_path_buf(), |acc, p| acc.join(p));
        let Ok(meta) = std::fs::metadata(&absolute) else { continue };
        if !meta.is_file() {
            continue;
        }
        suggestions.push((relative.clone(), meta.len(), matches_any(file_name, &SECRET_PATTERNS)));
        if suggestions.len() >= SUGGESTION_LIMIT {
            break;
        }
    }
    suggestions.sort();
    suggestions.into_iter().map(|(path, size, secret)| json!({"path": path, "size": size, "secret": secret})).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_first_match() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        assert_eq!(detect_setup(&root), None);
        std::fs::write(dir.path().join("go.mod"), "").unwrap();
        assert_eq!(detect_setup(&root).as_deref(), Some("go mod download"));
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("App.csproj"), "").unwrap();
        assert_eq!(detect_setup(&root).as_deref(), Some("dotnet restore"));
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(detect_setup(&root).as_deref(), Some("npm install"));
        std::fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(detect_setup(&root).as_deref(), Some("pnpm install --frozen-lockfile"));
        assert_eq!(detect_setup(""), None);
    }

    #[test]
    fn suggests_local_config_not_examples_or_heavy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        for (name, body) in [(".env", "A=1"), (".env.example", "x"), ("appsettings.Local.json", "{}"), ("node_modules/x/.env", "z"), ("notes.txt", "n")] {
            let p = dir.path().join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let out = suggest_copy_files(&root, ".env\n.env.example\nappsettings.Local.json\nnode_modules/x/.env\nnotes.txt\nsome-dir/\nmissing.local\n");
        assert_eq!(out, vec![json!({"path": ".env", "size": 3, "secret": true}), json!({"path": "appsettings.Local.json", "size": 2, "secret": true})]);
    }
}
