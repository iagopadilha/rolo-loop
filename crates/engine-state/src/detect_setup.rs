// SPDX-License-Identifier: MIT
//! Automatic worktree preparation of a repo, pure: what to install and which local files to copy. Nothing runs while detecting.
//!
//! Installs are planned PER SUBPROJECT ([`install_plan`]): the root and each folder one level below are looked at on their own, and a
//! command is only planned where its manifest sits beside its lockfile (`npm ci` needs a `package.json` next to `package-lock.json`).
//! The table of ecosystems ([`TOOLCHAINS`]) is data: a new stack is one more row, not new code.

use pycompat::fnmatch_nt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const HEAVY_FOLDER_NAMES: [&str; 4] = ["node_modules", "bin", "obj", "dist"];
const SUGGESTION_PATTERNS: [&str; 5] = [".env", ".env.*", "appsettings.*.json", "*.local.*", "*.local"];
const SECRET_PATTERNS: [&str; 4] = [".env", ".env.*", "appsettings.json", "appsettings.*.json"];
const EXAMPLE_FILE_SUFFIXES: [&str; 3] = [".example", ".sample", ".template"];
const SUGGESTION_LIMIT: usize = 100;
pub const IGNORED_FILES_ARGUMENTS: [&str; 5] = ["ls-files", "--others", "--ignored", "--exclude-standard", "--directory"];
/// Folders never scanned for subprojects (generated or vendored).
const SKIPPED_SUBFOLDERS: [&str; 7] = ["node_modules", "bin", "obj", "dist", "target", "vendor", "__pycache__"];

/// One ecosystem row. `trigger` is the file that declares what to install (the lockfile, or the manifest when the ecosystem has none),
/// `requires` a file that must sit beside it ("" = none). Within one `group` of one folder the FIRST matching row wins.
/// `routine` = restores what is ALREADY declared (the engine may run it without a decision); `artifact` = what a finished install leaves
/// in the folder ("" = unknown), so a deleted `node_modules` is installed again even when the lockfile did not change.
pub struct Toolchain {
    pub group: &'static str,
    pub trigger: &'static str,
    pub requires: &'static str,
    pub command: &'static str,
    pub artifact: &'static str,
    pub routine: bool,
    /// Files whose bytes decide whether an install is up to date.
    pub fingerprint: &'static [&'static str],
}

const fn row(
    group: &'static str,
    trigger: &'static str,
    requires: &'static str,
    command: &'static str,
    artifact: &'static str,
    routine: bool,
    fingerprint: &'static [&'static str],
) -> Toolchain {
    Toolchain { group, trigger, requires, command, artifact, routine, fingerprint }
}

pub const TOOLCHAINS: [Toolchain; 10] = [
    row("js", "pnpm-lock.yaml", "package.json", "pnpm install --frozen-lockfile", "node_modules", true, &["pnpm-lock.yaml", "package.json"]),
    row("js", "yarn.lock", "package.json", "yarn install --frozen-lockfile", "node_modules", true, &["yarn.lock", "package.json"]),
    row("js", "package-lock.json", "package.json", "npm ci", "node_modules", true, &["package-lock.json", "package.json"]),
    row("js", "package.json", "", "npm install", "node_modules", false, &["package.json"]),
    row("python", "requirements.txt", "", "python -m pip install -r requirements.txt", "", true, &["requirements.txt"]),
    row("python", "poetry.lock", "pyproject.toml", "poetry install", "", true, &["poetry.lock", "pyproject.toml"]),
    row("go", "go.mod", "", "go mod download", "", true, &["go.mod", "go.sum"]),
    row("rust", "Cargo.lock", "Cargo.toml", "cargo fetch --locked", "", true, &["Cargo.lock", "Cargo.toml"]),
    row("php", "composer.lock", "composer.json", "composer install --no-interaction", "vendor", true, &["composer.lock", "composer.json"]),
    row("ruby", "Gemfile.lock", "Gemfile", "bundle install", "", true, &["Gemfile.lock", "Gemfile"]),
];

/// Files that, with a `.sln`/`.csproj`, decide whether a restore is up to date.
const DOTNET_FINGERPRINT_EXTRAS: [&str; 3] = ["packages.lock.json", "Directory.Packages.props", "nuget.config"];

/// One install to run: `command` in `<worktree>/<dir>`.
#[derive(Clone, Debug, PartialEq)]
pub struct InstallStep {
    /// Folder relative to the repo root, `/` separated; "" = the root.
    pub dir: String,
    pub command: String,
    pub routine: bool,
    /// Relative to `dir`: what a finished install leaves behind ("" = unknown).
    pub artifact: String,
    /// Files (relative to `dir`) that decide whether the install is up to date.
    pub fingerprint: Vec<String>,
}

impl InstallStep {
    /// One line that runs in a shell started at the repo root.
    pub fn display(&self) -> String {
        if self.dir.is_empty() {
            self.command.clone()
        } else {
            format!("pushd \"{}\" && {} && popd", self.dir, self.command)
        }
    }

    /// Key of this step in the install cache.
    pub fn key(&self) -> String {
        format!("{}|{}", self.dir, self.command)
    }

    /// `sha256:` of the fingerprint files that exist under `base` (names included, so a rename counts; line endings do not); "" when none.
    pub fn fingerprint_hash(&self, base: &Path) -> String {
        let folder = if self.dir.is_empty() { base.to_path_buf() } else { base.join(&self.dir) };
        let mut hasher = Sha256::new();
        let mut any = false;
        for name in &self.fingerprint {
            if let Ok(bytes) = std::fs::read(folder.join(name)) {
                let normalized: Vec<u8> = bytes.into_iter().filter(|b| *b != b'\r').collect();
                hasher.update(name.as_bytes());
                hasher.update(normalized);
                any = true;
            }
        }
        if any {
            format!("sha256:{}", hasher.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>())
        } else {
            String::new()
        }
    }
}

fn is_project_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    !name.starts_with('.') && (lower.ends_with(".sln") || lower.ends_with(".csproj"))
}

/// Sorted `.sln` / `.csproj` file names directly in `folder`.
fn project_files(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| is_project_name(n) && folder.join(n).is_file()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

fn has_project_file(folder: &Path) -> bool {
    !project_files(folder).is_empty()
}

/// Names of the folders one level below `root` that are scanned for subprojects, sorted.
fn subfolders(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.') && !SKIPPED_SUBFOLDERS.contains(&n.as_str()))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// `*.sln`, `*.csproj`, `*/*.sln`, `*/*.csproj` under the folder (hidden names are not matched by a glob `*`).
pub fn has_dotnet_project(folder: &str) -> bool {
    let root = Path::new(folder);
    has_project_file(root) || subfolders(root).iter().any(|name| has_project_file(&root.join(name)))
}

fn dotnet_steps(folder: &Path, dir: &str) -> Vec<InstallStep> {
    let files = project_files(folder);
    let slns: Vec<&String> = files.iter().filter(|n| n.to_lowercase().ends_with(".sln")).collect();
    // a solution covers its projects; without one every project is restored on its own
    let targets: Vec<&String> = if slns.is_empty() { files.iter().collect() } else { vec![slns[0]] };
    let single = files.len() == 1;
    targets
        .into_iter()
        .map(|file| {
            let mut fingerprint = vec![file.clone()];
            fingerprint.extend(DOTNET_FINGERPRINT_EXTRAS.iter().filter(|extra| folder.join(extra).is_file()).map(|e| e.to_string()));
            InstallStep {
                dir: dir.to_string(),
                command: if single { "dotnet restore".into() } else { format!("dotnet restore \"{file}\"") },
                routine: true,
                artifact: String::new(),
                fingerprint,
            }
        })
        .collect()
}

fn steps_of_folder(folder: &Path, dir: &str, allow_dotnet: bool) -> Vec<InstallStep> {
    let mut steps = Vec::new();
    let mut seen_groups: Vec<&str> = Vec::new();
    for row in TOOLCHAINS.iter() {
        if seen_groups.contains(&row.group) || !folder.join(row.trigger).is_file() || (!row.requires.is_empty() && !folder.join(row.requires).is_file()) {
            continue;
        }
        seen_groups.push(row.group);
        steps.push(InstallStep {
            dir: dir.to_string(),
            command: row.command.to_string(),
            routine: row.routine,
            artifact: row.artifact.to_string(),
            fingerprint: row.fingerprint.iter().filter(|f| folder.join(f).is_file()).map(|f| f.to_string()).collect(),
        });
    }
    if allow_dotnet {
        steps.extend(dotnet_steps(folder, dir));
    }
    steps
}

/// The installs of the repo at `folder`, per subproject: the root, then every folder one level below, each on its own. A step exists only
/// where its manifest sits beside its lockfile. `.sln`/`.csproj` are restored where they are (the root wins over folders below).
pub fn install_plan(folder: &str) -> Vec<InstallStep> {
    let root = Path::new(folder);
    if folder.is_empty() || !root.is_dir() {
        return vec![];
    }
    let root_has_dotnet = has_project_file(root);
    let mut plan = steps_of_folder(root, "", true);
    for name in subfolders(root) {
        plan.extend(steps_of_folder(&root.join(&name), &name, !root_has_dotnet));
    }
    plan
}

/// The one-line setup command for the repo at `folder`, or `None`: every step of [`install_plan`], one after the other.
pub fn detect_setup(folder: &str) -> Option<String> {
    let plan = install_plan(folder);
    if plan.is_empty() {
        None
    } else {
        Some(plan.iter().map(InstallStep::display).collect::<Vec<_>>().join(" && "))
    }
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

    fn touch(root: &Path, relative: &str) {
        let p = root.join(relative);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "{}").unwrap();
    }

    #[test]
    fn detects_by_first_match() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        assert_eq!(detect_setup(&root), None);
        std::fs::write(dir.path().join("go.mod"), "").unwrap();
        assert_eq!(detect_setup(&root).as_deref(), Some("go mod download"));
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("App.csproj"), "").unwrap();
        assert_eq!(detect_setup(&root).as_deref(), Some("go mod download && pushd \"src\" && dotnet restore && popd"));
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(detect_setup(&root).as_deref(), Some("npm install && go mod download && pushd \"src\" && dotnet restore && popd"));
        std::fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        assert!(detect_setup(&root).unwrap().starts_with("pnpm install --frozen-lockfile && go mod download"));
        assert_eq!(detect_setup(""), None);
    }

    #[test]
    fn npm_ci_only_where_package_json_sits_beside_the_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        touch(dir.path(), "package-lock.json"); // a lockfile alone is not an install
        touch(dir.path(), "web/package.json");
        touch(dir.path(), "web/package-lock.json");
        touch(dir.path(), "docs/package.json"); // manifest without lockfile: not routine
        let plan = install_plan(&root);
        let routine: Vec<(String, String)> = plan.iter().filter(|s| s.routine).map(|s| (s.dir.clone(), s.command.clone())).collect();
        assert_eq!(routine, vec![("web".to_string(), "npm ci".to_string())]);
        assert!(plan.iter().any(|s| s.dir == "docs" && s.command == "npm install" && !s.routine));
        assert!(!plan.iter().any(|s| s.dir.is_empty()), "the root has no package.json: no install there");
        assert_eq!(detect_setup(&root).as_deref(), Some("pushd \"docs\" && npm install && popd && pushd \"web\" && npm ci && popd"));
    }

    #[test]
    fn dotnet_restore_runs_where_the_solution_or_project_is() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        touch(dir.path(), "backend/App.sln");
        touch(dir.path(), "backend/App/App.csproj"); // two levels down: covered by the solution, not scanned
        assert_eq!(install_plan(&root).iter().map(InstallStep::display).collect::<Vec<_>>(), vec!["pushd \"backend\" && dotnet restore && popd"]);
        touch(dir.path(), "backend/Tests.csproj");
        let plan = install_plan(&root);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].command, "dotnet restore \"App.sln\"");
        touch(dir.path(), "Root.csproj");
        let plan = install_plan(&root);
        assert_eq!(plan.len(), 1, "a project at the root wins over folders below");
        assert_eq!((plan[0].dir.as_str(), plan[0].command.as_str()), ("", "dotnet restore"));
    }

    #[test]
    fn other_stacks_come_from_the_table() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "svc/Cargo.toml");
        touch(dir.path(), "svc/Cargo.lock");
        touch(dir.path(), "tool/poetry.lock"); // no pyproject.toml beside it
        touch(dir.path(), "py/requirements.txt");
        touch(dir.path(), "node_modules/x/package-lock.json"); // generated folders are not scanned
        let plan = install_plan(&dir.path().display().to_string());
        let cmds: Vec<String> = plan.iter().map(|s| format!("{}:{}", s.dir, s.command)).collect();
        assert_eq!(cmds, vec!["py:python -m pip install -r requirements.txt", "svc:cargo fetch --locked"]);
    }

    #[test]
    fn fingerprint_follows_the_lockfile_not_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "package.json");
        std::fs::write(dir.path().join("package-lock.json"), "a\nb\n").unwrap();
        let step = install_plan(&dir.path().display().to_string()).remove(0);
        let first = step.fingerprint_hash(dir.path());
        assert!(first.starts_with("sha256:"));
        std::fs::write(dir.path().join("package-lock.json"), "a\r\nb\r\n").unwrap();
        assert_eq!(step.fingerprint_hash(dir.path()), first);
        std::fs::write(dir.path().join("package-lock.json"), "a\nc\n").unwrap();
        assert_ne!(step.fingerprint_hash(dir.path()), first);
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
