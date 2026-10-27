// SPDX-License-Identifier: MIT
//! Setup files of a repository: what the app generated while planning and what may be uncommitted.
//!
//! Setup files = `docs/codebase/*.md` of the repo (or of every non-skipped subproject of a confirmed monorepo) plus the root `SYSTEM.md`
//! map, the root `.gitignore` when the only change is the line the app adds for `.env.test`, the `CLAUDE.md` files that carry an app
//! block, `.specs/project/**` and `.specs/features/**` (never the loop's runtime state under `.specs/project/loop/`, never an `.env*` file
//! (those may hold secrets) and never `NETWORK.json` (the engine copies it, see [`refresh_loop_inputs`])). Everything else that is changed
//! in the folder is an "other change".
//!
//! This module only READS the user's folder (carry copies out of it into the worktree); it never stages, commits or stashes.

use crate::context_files::{self, build_pathspecs, confirmed_project_folders, kind_of, KIND_CLAUDE};
use crate::loop_io::{atomic_write_text, copy2, run_timeout_env, run_timeout_raw};
use crate::loop_state::internal_dir;
use crate::project_repos::RepoRef;
use crate::subprojects;
use pycompat::{nt_abspath, nt_normcase};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const GIT_TIMEOUT_SECONDS: u64 = 30;
const SPECS_PATHSPECS: [&str; 2] = [":(glob).specs/project/**", ":(glob).specs/features/**"];
const EXCLUDED_PATHSPECS: [&str; 4] = [
    ":(exclude,glob).specs/project/loop/**",
    ":(exclude,glob).specs/project/.env*",
    ":(exclude,glob).specs/project/**/.env*",
    ":(exclude,glob).specs/project/NETWORK.json",
];
const LOOP_STATE_PREFIX: &str = ".specs/project/loop/";
const SPECS_PREFIX: &str = ".specs/";
const APP_BLOCK_MARKERS: [&str; 4] = ["<!-- maestro:context -->", "<!-- CODEBASE:BEGIN -->", "Conformance:", "@.specs/project/RULES.md"];
const GITIGNORE_PATHSPEC: &str = ":(glob).gitignore";
const MONOREPO_ROOT_PATHSPECS: [&str; 2] = [":(glob)CLAUDE.md", ":(glob)docs/codebase/*.md"];
const ROOT_CLAUDE_FILE: &str = "CLAUDE.md";
const LIST_LIMIT: usize = 12;
pub const COMMIT_MESSAGE: &str = "chore(setup): codebase maps and agent context";
const GITIGNORE_FILE: &str = ".gitignore";
/// The repo holds the master; copied over the worktree's.
const LOOP_INPUT_FILES: [&str; 2] = [".specs/project/.env.test", ".specs/project/NETWORK.json"];
pub const NETWORK_FILTER_FILE: &str = "network-filter.js";
pub const NETWORK_ALLOW_FILE: &str = "network-allow.json";
/// The toolkit's binary that writes the filter: `craft testenv network-filter --project <work>`.
const TOOLKIT_BINARY: [&str; 2] = ["bin", "craft.exe"];
const TESTENV_TIMEOUT_SECONDS: u64 = 30;
/// The only lines the app adds to the user's .gitignore (secret files, `.specs/` and its trash).
const APP_IGNORE_LINES: [&str; 7] = [".env", ".env.*", "!.env.example", ".specs/project/.env.test", ".specs/project/ACCESS.json", ".specs/", ".specs/.trash/"];
const GITATTRIBUTES_FILE: &str = ".gitattributes";
const GITATTRIBUTES_PATHSPEC: &str = ":(glob).gitattributes";
/// The line the app adds to `.gitattributes`: text files are LF in the repository, so a CRLF checkout is not a change.
pub const APP_ATTRIBUTE_LINE: &str = "* text=auto eol=lf";

/// `git status --porcelain -z <arguments>` -> `[(code, path)]` in the order git printed them; `[]` on any failure.
pub fn run_status(repo: &str, arguments: &[&str]) -> Vec<(String, String)> {
    let mut args: Vec<&str> = vec!["-C", repo, "status", "--porcelain", "-z"];
    args.extend_from_slice(arguments);
    let (code, out, _) = run_timeout_raw("git", &args, None, GIT_TIMEOUT_SECONDS);
    if code != 0 {
        return vec![];
    }
    let (mut rows, records) = (Vec::new(), out.split('\0').collect::<Vec<_>>());
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        index += 1;
        if record.chars().count() < 4 {
            continue;
        }
        let code: String = record.chars().take(2).collect();
        let path = subprojects::normalize(&record.chars().skip(3).collect::<String>());
        if code.contains('R') || code.contains('C') {
            index += 1; // the origin path
        }
        rows.push((code, path));
    }
    rows
}

/// CLAUDE.md files only count as setup files when they carry something the app wrote (context block, codebase block, Conformance).
pub fn has_app_block(repo: &str, path: &str) -> bool {
    let full = path.split('/').fold(PathBuf::from(repo), |acc, p| acc.join(p));
    match std::fs::read(full) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            APP_BLOCK_MARKERS.iter().any(|m| text.contains(m))
        }
        Err(_) => false,
    }
}

fn state_of(code: &str) -> &'static str {
    let letter = if code == "??" { '?' } else { code.chars().find(|c| *c != ' ').unwrap_or('M') };
    context_files::state_by_code(letter)
}

/// `(pathspecs, monorepo)` of the setup files. A confirmed monorepo also lists the root CLAUDE.md (the RULES import) and root maps.
fn setup_pathspecs(entry: Option<&Map<String, Value>>) -> (Vec<String>, bool) {
    let monorepo = confirmed_project_folders(entry).is_some();
    let mut specs = build_pathspecs(entry);
    specs.extend(SPECS_PATHSPECS.iter().map(|s| s.to_string()));
    specs.push(GITIGNORE_PATHSPEC.into());
    specs.push(GITATTRIBUTES_PATHSPEC.into());
    if monorepo {
        specs.extend(MONOREPO_ROOT_PATHSPECS.iter().map(|s| s.to_string()));
    }
    specs.extend(EXCLUDED_PATHSPECS.iter().map(|s| s.to_string()));
    (specs, monorepo)
}

/// True when the uncommitted change of the root `.gitignore` is ONLY the line(s) the app added.
pub fn is_app_only_gitignore_change(repo: &str, path: &str) -> bool {
    path == GITIGNORE_FILE && only_app_lines_added(repo, path, &APP_IGNORE_LINES)
}

/// True when the uncommitted change of the root `.gitattributes` is ONLY the line-ending rule the app adds.
pub fn is_app_only_gitattributes_change(repo: &str, path: &str) -> bool {
    path == GITATTRIBUTES_FILE && only_app_lines_added(repo, path, &[APP_ATTRIBUTE_LINE])
}

/// Appends the LF rule to the repo's `.gitattributes` (created when missing) unless a catch-all `*` text rule is already there, so a
/// CRLF/LF difference stops counting as a changed file. Returns the names written (`[".gitattributes"]`) or `[]`. The file is then a
/// setup file: it is carried and committed with the others. Whether the user's git also sets `core.autocrlf=input` is advice, see
/// `preflight_checks::line_ending_warnings`.
pub fn ensure_line_endings(repo: &str) -> Vec<String> {
    let path = Path::new(repo).join(GITATTRIBUTES_FILE);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let has_rule = pycompat::splitlines(&text).iter().any(|l| {
        let l = l.trim();
        l.starts_with("* ") && (l.contains("text") || l.contains("eol="))
    });
    if has_rule {
        return vec![];
    }
    let separator = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    match atomic_write_text(&path, &format!("{text}{separator}{APP_ATTRIBUTE_LINE}\n")) {
        Ok(()) => vec![GITATTRIBUTES_FILE.into()],
        Err(_) => vec![],
    }
}

fn only_app_lines_added(repo: &str, path: &str, allowed: &[&str]) -> bool {
    let (code, out, _) = run_timeout_raw("git", &["-C", repo, "diff", "HEAD", "--unified=0", "--ignore-cr-at-eol", "--", path], None, GIT_TIMEOUT_SECONDS);
    if code != 0 {
        return false;
    }
    // Python's `line[:1] in "+-"` is also true for an empty line
    let changed: Vec<&str> =
        pycompat::splitlines(&out).into_iter().filter(|l| (l.is_empty() || l.starts_with(['+', '-'])) && !l.starts_with("+++") && !l.starts_with("---")).collect();
    if changed.is_empty() {
        // a file git does not know yet has no diff: it is the app's when every line in it is one the app writes
        let (_, tracked, _) = run_timeout_raw("git", &["-C", repo, "ls-files", "--cached", "--", path], None, GIT_TIMEOUT_SECONDS);
        let content = std::fs::read_to_string(Path::new(repo).join(path)).unwrap_or_default();
        let lines: Vec<&str> = content.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        return tracked.trim().is_empty() && !lines.is_empty() && lines.iter().all(|l| allowed.contains(l));
    }
    changed.iter().all(|l| l.starts_with('+') && allowed.contains(&l[1..].trim()))
}

/// True when `path` (found by status or ls-files) is a setup file the app generated.
fn is_setup_path(repo: &str, path: &str, monorepo: bool) -> bool {
    if path == GITIGNORE_FILE {
        return is_app_only_gitignore_change(repo, path);
    }
    if path == GITATTRIBUTES_FILE {
        return is_app_only_gitattributes_change(repo, path);
    }
    if path.starts_with(SPECS_PREFIX) {
        return !path.starts_with(LOOP_STATE_PREFIX);
    }
    let mut kind = kind_of(path, monorepo);
    if kind.is_none() && monorepo && path == ROOT_CLAUDE_FILE {
        kind = Some(KIND_CLAUDE);
    }
    kind.is_some() && !(kind == Some(KIND_CLAUDE) && !has_app_block(repo, path))
}

fn entry_of(repo: &str, entry: Option<&Map<String, Value>>) -> Option<Map<String, Value>> {
    entry.cloned().or_else(|| subprojects::load(repo, None).0)
}

/// The uncommitted setup files of `repo`, sorted by path: `[{"path", "state"}]`.
pub fn list_setup_files(repo: &str, entry: Option<&Map<String, Value>>) -> Vec<Value> {
    let entry = entry_of(repo, entry);
    let (specs, monorepo) = setup_pathspecs(entry.as_ref());
    let mut args: Vec<&str> = vec!["-uall", "--"];
    args.extend(specs.iter().map(String::as_str));
    let mut items: Vec<(String, &'static str)> = Vec::new();
    for (code, path) in run_status(repo, &args) {
        if code.contains('D') {
            continue; // a deleted file has nothing to carry or commit
        }
        if is_setup_path(repo, &path, monorepo) {
            items.push((path, state_of(&code)));
        }
    }
    items.sort();
    items.into_iter().map(|(path, state)| json!({"path": path, "state": state})).collect()
}

/// Every setup file that exists in the user's folder, committed or not (tracked + untracked, `.gitignore` honoured). `[]` on any git failure.
pub fn list_present_setup_files(repo: &str, entry: Option<&Map<String, Value>>) -> Vec<String> {
    let entry = entry_of(repo, entry);
    let (specs, monorepo) = setup_pathspecs(entry.as_ref());
    let mut args: Vec<&str> = vec!["-C", repo, "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--"];
    args.extend(specs.iter().map(String::as_str));
    let (code, out, _) = run_timeout_raw("git", &args, None, GIT_TIMEOUT_SECONDS);
    if code != 0 {
        return vec![];
    }
    let paths: BTreeSet<String> = out.split('\0').filter(|r| !r.is_empty()).map(subprojects::normalize).collect();
    paths.into_iter().filter(|p| is_setup_path(repo, p, monorepo)).collect()
}

/// Tracked files changed in the folder that are not setup files (untracked files are not walked: node_modules and the like).
pub fn list_other_changes(repo: &str, setup_paths: &[String]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for (code, path) in run_status(repo, &["--untracked-files=no"]) {
        // a file whose only difference is CRLF vs LF is not a change (S3)
        if code.trim() == "M" && crate::loop_io::line_ending_only_change(repo, &path) {
            continue;
        }
        if !setup_paths.contains(&path) && !path.starts_with(LOOP_STATE_PREFIX) && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths.sort();
    paths
}

/// Copies the setup files of `repo` that are MISSING in the new worktree `work`, committed on the user's branch or not (the worktree
/// starts from another branch, so files committed on the current one would never arrive). A file that already exists there is never
/// overwritten. `include_specs=false` for a repo whose `.specs` is not used (a non-primary repo). Returns the relative paths copied.
pub fn carry_setup_files(repo: &str, work: &str, entry: Option<&Map<String, Value>>, include_specs: bool) -> Vec<String> {
    let mut copied = Vec::new();
    for path in list_present_setup_files(repo, entry) {
        if !include_specs && path.starts_with(SPECS_PREFIX) {
            continue;
        }
        let join = |base: &str| path.split('/').fold(PathBuf::from(base), |acc, p| acc.join(p));
        let (source, target) = (join(repo), join(work));
        if !source.is_file() || target.exists() {
            continue;
        }
        let _ = std::fs::create_dir_all(target.parent().expect("has parent"));
        if copy2(&source, &target).is_ok() {
            copied.push(path);
        }
    }
    copied
}

/// One block per repo with something uncommitted and no loop worktree yet: `{"repo", "setup_files", "other_changes"}`.
pub fn uncommitted_blocks(repos: &[RepoRef], primary_repo: &str) -> Vec<Value> {
    let mut blocks = Vec::new();
    for item in repos {
        if Path::new(&item.loop_worktree_path()).is_dir() || item.role == "reference" {
            continue;
        }
        let source = if primary_repo.is_empty() { item.path.as_str() } else { primary_repo };
        let repo_name = if item.primary || primary_repo.is_empty() { None } else { Some(item.name.as_str()) };
        let entry = subprojects::load(source, repo_name).0;
        let setup: Vec<String> = list_setup_files(&item.path, entry.as_ref()).iter().filter_map(|r| r["path"].as_str().map(String::from)).collect();
        let other = list_other_changes(&item.path, &setup);
        if !setup.is_empty() || !other.is_empty() {
            blocks.push(json!({"repo": item.name, "setup_files": setup, "other_changes": other}));
        }
    }
    blocks
}

pub fn shortened(paths: &[String]) -> String {
    let shown = paths.iter().take(LIST_LIMIT).cloned().collect::<Vec<_>>().join(", ");
    if paths.len() > LIST_LIMIT {
        format!("{shown} and {} more", paths.len() - LIST_LIMIT)
    } else {
        shown
    }
}

fn string_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// Readiness text of one block: the setup files that are carried, then the other changes that are not.
pub fn block_text(block: &Value) -> String {
    let (setup, other) = (string_list(block, "setup_files"), string_list(block, "other_changes"));
    let mut parts: Vec<String> = Vec::new();
    if !setup.is_empty() {
        parts.push(format!(
            "{} setup file{} not committed: they will be carried into the worktree (or commit them from Workspace): {}",
            setup.len(),
            if setup.len() == 1 { "" } else { "s" },
            shortened(&setup)
        ));
    }
    if !other.is_empty() {
        parts.push(format!(
            "{} other changed file{} will NOT be carried (commit them on your branch first if the loop needs them): {}",
            other.len(),
            if other.len() == 1 { "" } else { "s" },
            shortened(&other)
        ));
    }
    format!("uncommitted changes in {}: {}", block["repo"].as_str().unwrap_or(""), parts.join("; "))
}

// ---- loop inputs: credentials, network list and the files derived from them ---------------------------------------------------------------

pub(crate) fn same_folder(first: &str, second: &str) -> bool {
    nt_normcase(&nt_abspath(first)) == nt_normcase(&nt_abspath(second))
}

fn same_bytes(first: &Path, second: &Path) -> bool {
    match (std::fs::read(first), std::fs::read(second)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// `.env.test` and `NETWORK.json` of `repo`, when they exist, copied over the worktree's (also when `.specs/project` is versioned and
/// `carry_setup_files` leaves `.env*` out). The repo holds the master: a file deleted there is deleted from the worktree too, so a stale
/// network list never outlives the user's change. Returns the relative paths whose content changed. Nothing when `repo` is `work`.
pub fn copy_loop_inputs(repo: &str, work: &str) -> Vec<String> {
    let mut changed = Vec::new();
    if same_folder(repo, work) {
        return changed;
    }
    for relative in LOOP_INPUT_FILES {
        let join = |base: &str| relative.split('/').fold(PathBuf::from(base), |acc, p| acc.join(p));
        let (source, target) = (join(repo), join(work));
        if !source.is_file() {
            if target.is_file() {
                let _ = std::fs::remove_file(&target);
                changed.push(relative.to_string());
            }
            continue;
        }
        if same_bytes(&source, &target) {
            continue;
        }
        let _ = std::fs::create_dir_all(target.parent().expect("has parent"));
        if std::fs::copy(&source, &target).is_ok() {
            changed.push(relative.to_string());
        }
    }
    changed
}

/// `(javascript text, note)`: stdout of the toolkit's `craft testenv network-filter --project <work>`; `("", why)` when it cannot be produced.
pub fn network_filter_script(work: &str, toolkit_root: &str, data_dir: &str) -> (String, String) {
    if toolkit_root.is_empty() {
        return (String::new(), "dev toolkit not found: no network-filter.js (the guard then blocks every playwright run-code route)".into());
    }
    let binary = TOOLKIT_BINARY.iter().fold(PathBuf::from(toolkit_root), |acc, p| acc.join(p));
    if !binary.is_file() {
        return (String::new(), format!("toolkit has no {}: no network-filter.js", TOOLKIT_BINARY.join("/")));
    }
    let (code, out, _) =
        run_timeout_env(&binary.display().to_string(), &["testenv", "network-filter", "--project", work], None, &[("LOOP_DATA_DIR", data_dir)], TESTENV_TIMEOUT_SECONDS);
    if code != 0 || out.trim().is_empty() {
        return (String::new(), format!("craft testenv network-filter failed (exit {code}): no network-filter.js"));
    }
    (out, String::new())
}

/// Writes `loop/internal/network-allow.json` and `network-filter.js` of the worktree. Returns `(names written, notes)`.
pub fn write_network_internal_files(work: &str, toolkit_root: &str, data_dir: &str) -> (Vec<String>, Vec<String>) {
    let internal = internal_dir(work);
    let (mut written, mut notes) = (Vec::<String>::new(), Vec::<String>::new());
    let (allow_path, filter_path) = (internal.join(NETWORK_ALLOW_FILE), internal.join(NETWORK_FILTER_FILE));
    let effective = crate::network_policy::load(work, Path::new(data_dir), None);
    let text = format!("{}\n", pycompat::json::py_json_dumps_indent(&crate::network_policy::to_jsonable(&effective), false, 2));
    match atomic_write_text(&allow_path, &text) {
        Ok(()) => written.push(NETWORK_ALLOW_FILE.into()),
        Err(_) => notes.push(format!("could not write {NETWORK_ALLOW_FILE} (OSError)")),
    }
    let (script, note) = network_filter_script(work, toolkit_root, data_dir);
    if !script.is_empty() {
        match atomic_write_text(&filter_path, &script) {
            Ok(()) => written.push(NETWORK_FILTER_FILE.into()),
            Err(_) => notes.push(format!("could not update {NETWORK_FILTER_FILE} (OSError)")),
        }
    } else if filter_path.is_file() {
        let _ = std::fs::remove_file(&filter_path); // a stale filter must not outlive a failure: the guard then blocks every run-code route
    }
    if !note.is_empty() {
        notes.push(note);
    }
    (written, notes)
}

/// `{"copied", "written", "notes"}` of [`refresh_loop_inputs`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RefreshResult {
    pub copied: Vec<String>,
    pub written: Vec<String>,
    pub notes: Vec<String>,
}

/// Brings the worktree's loop inputs up to date: copies `<repo>/.specs/project/.env.test` and `NETWORK.json` over the worktree's (the
/// panel writes them in the repo), then writes `loop/internal/network-allow.json` and `network-filter.js` from them. Called at worktree
/// creation, loop start and resume and at the start of every turn. A missing `.env.test` copies nothing and is no error; a missing toolkit
/// means no filter file. Never fails.
pub fn refresh_loop_inputs(repo: &str, work: &str, data_dir: &str, toolkit_root: &str) -> RefreshResult {
    let copied = copy_loop_inputs(repo, work);
    let (written, notes) = write_network_internal_files(work, toolkit_root, data_dir);
    RefreshResult { copied, written, notes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(status.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&status.stderr));
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.name", "t"]);
        git(dir.path(), &["config", "user.email", "t@example.com"]);
        std::fs::write(dir.path().join("README.md"), "x").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn lists_carries_and_texts() {
        let repo = repo();
        let r = repo.path().display().to_string();
        write(repo.path(), "docs/codebase/STACK.md", "map");
        write(repo.path(), "CLAUDE.md", "no app block here");
        write(repo.path(), ".specs/project/PROJECT.md", "p");
        write(repo.path(), ".specs/project/loop/meta.json", "{}");
        write(repo.path(), ".specs/project/.env.test", "A=1");
        write(repo.path(), "README.md", "changed");
        let listed: Vec<String> = list_setup_files(&r, None).iter().map(|v| v["path"].as_str().unwrap().to_string()).collect();
        assert_eq!(listed, vec![".specs/project/PROJECT.md", "docs/codebase/STACK.md"]);
        assert_eq!(list_other_changes(&r, &listed), vec!["README.md"]);
        write(repo.path(), "CLAUDE.md", "<!-- maestro:context -->");
        assert!(list_setup_files(&r, None).iter().any(|v| v["path"] == "CLAUDE.md"));
        let work = tempfile::tempdir().unwrap();
        let w = work.path().display().to_string();
        write(work.path(), "docs/codebase/STACK.md", "already there");
        let carried = carry_setup_files(&r, &w, None, true);
        assert!(carried.contains(&".specs/project/PROJECT.md".to_string()) && !carried.contains(&"docs/codebase/STACK.md".to_string()));
        assert_eq!(std::fs::read_to_string(work.path().join("docs/codebase/STACK.md")).unwrap(), "already there");
        assert!(!work.path().join(".specs/project/.env.test").exists());
        let block = json!({"repo": "api", "setup_files": ["a", "b"], "other_changes": ["c"]});
        assert_eq!(block_text(&block), "uncommitted changes in api: 2 setup files not committed: they will be carried into the worktree (or commit them from Workspace): a, b; 1 other changed file will NOT be carried (commit them on your branch first if the loop needs them): c");
    }

    #[test]
    fn gitignore_counts_only_the_app_line() {
        let repo = repo();
        let r = repo.path().display().to_string();
        write(repo.path(), ".gitignore", "target\n");
        git(repo.path(), &["add", ".gitignore"]);
        git(repo.path(), &["commit", "-q", "-m", "ignore"]);
        write(repo.path(), ".gitignore", "target\n.specs/project/.env.test\n");
        assert!(is_app_only_gitignore_change(&r, ".gitignore"));
        write(repo.path(), ".gitignore", "target\n.specs/project/.env.test\nsecret\n");
        assert!(!is_app_only_gitignore_change(&r, ".gitignore"));
        assert!(!is_app_only_gitignore_change(&r, "other"));
    }

    #[test]
    fn all_the_lines_the_app_writes_keep_gitignore_a_setup_file() {
        let repo = repo();
        let r = repo.path().display().to_string();
        write(repo.path(), ".gitignore", "target\n");
        git(repo.path(), &["add", ".gitignore"]);
        git(repo.path(), &["commit", "-q", "-m", "ignore"]);
        write(repo.path(), ".gitignore", "target\n.env\n.env.*\n!.env.example\n.specs/project/.env.test\n.specs/project/ACCESS.json\n.specs/\n.specs/.trash/\n");
        assert!(is_app_only_gitignore_change(&r, ".gitignore"));
        assert!(list_setup_files(&r, None).iter().any(|v| v["path"] == ".gitignore"));
        write(repo.path(), ".gitignore", "target\n.env\nnode_modules/\n");
        assert!(!is_app_only_gitignore_change(&r, ".gitignore"), "a line the app did not write makes it the user's change");
    }

    #[test]
    fn line_ending_rule_is_a_setup_file_and_is_not_added_twice() {
        let repo = repo();
        let r = repo.path().display().to_string();
        assert_eq!(ensure_line_endings(&r), vec![".gitattributes"]);
        assert_eq!(std::fs::read_to_string(repo.path().join(".gitattributes")).unwrap(), "* text=auto eol=lf\n");
        assert!(ensure_line_endings(&r).is_empty(), "the rule is already there");
        assert!(list_setup_files(&r, None).iter().any(|v| v["path"] == ".gitattributes"), "the new file is offered as a setup file");
        let work = tempfile::tempdir().unwrap();
        let carried = carry_setup_files(&r, &work.path().display().to_string(), None, true);
        assert!(carried.contains(&".gitattributes".to_string()));
        // a user's own rules are never rewritten, and a user's own edit is not a setup file
        git(repo.path(), &["add", ".gitattributes"]);
        git(repo.path(), &["commit", "-q", "-m", "attrs"]);
        write(repo.path(), ".gitattributes", "* text=auto eol=lf\n*.png binary\n");
        assert!(!is_app_only_gitattributes_change(&r, ".gitattributes"));
        let other = tempfile::tempdir().unwrap();
        write(other.path(), ".gitattributes", "*.png binary\n");
        assert_eq!(ensure_line_endings(&other.path().display().to_string()), vec![".gitattributes"]);
        assert_eq!(std::fs::read_to_string(other.path().join(".gitattributes")).unwrap(), "*.png binary\n* text=auto eol=lf\n");
    }

    #[test]
    fn crlf_only_files_are_not_other_changes() {
        let repo = repo();
        let r = repo.path().display().to_string();
        git(repo.path(), &["config", "core.autocrlf", "false"]);
        write(repo.path(), "src/a.txt", "one\ntwo\n");
        write(repo.path(), "src/b.txt", "one\ntwo\n");
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "files"]);
        write(repo.path(), "src/a.txt", "one\r\ntwo\r\n");
        assert!(list_other_changes(&r, &[]).is_empty(), "CRLF only: nothing changed");
        write(repo.path(), "src/b.txt", "one\r\nTWO\r\n");
        assert_eq!(list_other_changes(&r, &[]), vec!["src/b.txt"], "a real edit still counts");
    }

    #[test]
    fn loop_inputs_follow_the_repo_master() {
        let repo = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let (r, w) = (repo.path().display().to_string(), work.path().display().to_string());
        write(repo.path(), ".specs/project/.env.test", "A=1");
        write(repo.path(), ".specs/project/NETWORK.json", r#"{"version": 1, "allow": ["x.example.com"]}"#);
        write(work.path(), ".specs/project/NETWORK.json", "stale");
        let data = tempfile::tempdir().unwrap();
        let result = refresh_loop_inputs(&r, &w, &data.path().display().to_string(), "");
        assert_eq!(result.copied, vec![".specs/project/.env.test", ".specs/project/NETWORK.json"]);
        assert_eq!(result.written, vec!["network-allow.json"]);
        assert_eq!(result.notes, vec!["dev toolkit not found: no network-filter.js (the guard then blocks every playwright run-code route)"]);
        let allow = std::fs::read_to_string(internal_dir(&w).join("network-allow.json")).unwrap();
        assert!(allow.ends_with("}\n") && allow.contains("\"text\": \"x.example.com\""));
        std::fs::remove_file(repo.path().join(".specs/project/NETWORK.json")).unwrap();
        assert_eq!(copy_loop_inputs(&r, &w), vec![".specs/project/NETWORK.json"]);
        assert!(copy_loop_inputs(&r, &r).is_empty());
    }
}
