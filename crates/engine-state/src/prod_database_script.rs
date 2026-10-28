// SPDX-License-Identifier: MIT
//! Production database script: ONE reviewed `.sql` file the user runs on PRODUCTION before deploying. Nothing here touches a database.
//!
//! Called at delivery and at close. Per EDIT repository (REPOS.json `role`, missing = edit) whose loop branch added migrations (the
//! `Migrations/` files that appear between the base branch and the loop branch, read with git read commands) it runs
//! `dotnet ef migrations script --idempotent <last base migration> <last loop migration>` (the runner is injectable; the command
//! generates text and never connects to a database), then appends the manual `.sql` scripts the features left in
//! `.specs/features/*/sql/` in ROADMAP order. Output: `<output_dir>/<project>-prod-database-<YYYY-MM-DD>.sql`.
//!
//! Nothing to script (no migration, no `.sql`) = no file. A failure never blocks a delivery: it becomes a warning in the result.

use crate::loop_io::{run_timeout, run_timeout_raw};
use crate::loop_state::{feature_folder_name, parse_roadmap};
use cfg::Env;
use pycompat::{nt_abspath, nt_join, Re};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;

const MIGRATIONS_FOLDER: &str = "Migrations";
const DOTNET_TIMEOUT_SECONDS: u64 = 300;
const GIT_TIMEOUT_SECONDS: u64 = 60;
const BUILD_PROGRESS_LINES: [&str; 2] = ["Build started...", "Build succeeded."];

static MIGRATION_FILE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(\d{8,}_[\w-]+)\.cs\z"));
static SAFE_NAME: LazyLock<Re> = LazyLock::new(|| Re::new(r"[^A-Za-z0-9._-]+"));
static WORKTREE_SUFFIX: LazyLock<Re> = LazyLock::new(|| Re::new(r"-worktree-loop.*\z"));

/// Runs a command and returns `(code, stdout, stderr)`; injectable so tests never need `dotnet`.
pub type Runner<'a> = &'a dyn Fn(&[&str], &str) -> (i32, String, String);

fn header(project: &str, stamp: &str) -> String {
    format!(
        "-- ============================================================================\n\
         -- RUN ON PRODUCTION BEFORE DEPLOYING. REVIEW FIRST.\n\
         -- Project: {project}    Generated: {stamp}\n\
         -- Part 1: idempotent migration script per repository (dotnet ef migrations script --idempotent).\n\
         -- Part 2: manual .sql scripts of the features, in roadmap order.\n\
         -- Nothing in this file was run on any database by the loop.\n\
         -- ============================================================================\n"
    )
}

fn default_runner(args: &[&str], cwd: &str) -> (i32, String, String) {
    run_timeout_raw(args[0], &args[1..], Some(cwd), DOTNET_TIMEOUT_SECONDS)
}

fn git_lines(root: &str, args: &[&str]) -> Option<Vec<String>> {
    let mut full = vec!["-C", root];
    full.extend_from_slice(args);
    let (code, out, _) = run_timeout_raw("git", &full, None, GIT_TIMEOUT_SECONDS);
    (code == 0).then(|| out.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
}

fn git_ok(root: &str, args: &[&str]) -> bool {
    let mut full = vec!["-C", root];
    full.extend_from_slice(args);
    run_timeout("git", &full, None, GIT_TIMEOUT_SECONDS).0 == 0
}

/// Text of a file with universal newlines (Python text mode); empty when unreadable.
fn read_text(path: &Path) -> String {
    crate::loop_io::read(path).replace("\r\n", "\n").replace('\r', "\n")
}

fn normcase(path: &str) -> String {
    nt_abspath(path).to_lowercase()
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

// ---------------------------------------------------------------- which repositories are edited

/// `{repo name or normalized path: role}` from the first readable REPOS.json among `repo_paths` (`role` missing = "edit").
pub fn read_roles(repo_paths: &[&str]) -> Vec<(String, String)> {
    for repo_path in repo_paths.iter().filter(|p| !p.is_empty()) {
        let file = Path::new(repo_path).join(".specs").join("project").join("REPOS.json");
        let Some(data) = std::fs::read(&file).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { continue };
        let mut roles: Vec<(String, String)> = Vec::new();
        if let Some(list) = data.get("repos").and_then(Value::as_array) {
            for item in list.iter().filter(|i| i.is_object()) {
                let role = if item.get("role").and_then(Value::as_str) == Some("reference") { "reference" } else { "edit" };
                if let Some(name) = item.get("name").filter(|n| crate::notify_truthy(Some(*n))) {
                    roles.push((name.as_str().map(String::from).unwrap_or_else(|| name.to_string()), role.into()));
                }
                if let Some(path) = item.get("path").filter(|n| crate::notify_truthy(Some(*n))) {
                    roles.push((normcase(path.as_str().unwrap_or("")), role.into()));
                }
            }
        }
        return roles;
    }
    Vec::new()
}

fn role_lookup<'a>(roles: &'a [(String, String)], key: &str) -> Option<&'a str> {
    roles.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

pub fn role_of(roles: &[(String, String)], name: &str, path: &str) -> String {
    if let Some(r) = role_lookup(roles, name) {
        return r.to_string();
    }
    if path.is_empty() {
        return "edit".into();
    }
    role_lookup(roles, &normcase(path)).unwrap_or("edit").to_string()
}

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub name: String,
    pub root: String,
    pub branch: String,
    pub base: String,
    pub primary: bool,
    pub repo_path: String,
}

/// Entries from a primary worktree's meta.json (a single repo when it has no `repos` map).
pub fn entries_from_meta(primary_work: &str, meta: &Value) -> Vec<Entry> {
    if let Some(repos) = meta.get("repos").and_then(Value::as_object).filter(|r| !r.is_empty()) {
        let mut names: Vec<&String> = repos.keys().collect();
        names.sort_by_key(|n| !crate::notify_truthy(repos[*n].get("primary")));
        return names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let entry = &repos[*name];
                let worktree = s(entry, "worktree");
                let root = nt_abspath(if index == 0 { primary_work } else { &worktree });
                // a repo whose record lacks its branch or base is not given empty names: its worktree knows the branch, the run knows the base
                let branch = Some(s(entry, "branch")).filter(|b| !b.is_empty()).unwrap_or_else(|| crate::loop_io::git(&root, &["branch", "--show-current"]).1);
                let base = [s(entry, "base"), s(&crate::loop_io::load_json(&crate::loop_state::loop_path(&root, "delivery.json")), "base"), s(meta, "base")]
                    .into_iter()
                    .find(|b| !b.is_empty())
                    .unwrap_or_default();
                Entry { name: (*name).clone(), root, branch, base, primary: index == 0, repo_path: s(entry, "path") }
            })
            .collect();
    }
    let abs = nt_abspath(primary_work);
    vec![Entry {
        name: abs.rsplit('\\').next().unwrap_or("").to_string(),
        root: abs,
        branch: s(meta, "branch"),
        base: s(meta, "base"),
        primary: true,
        repo_path: String::new(),
    }]
}

// ---------------------------------------------------------------- migrations per repository

/// `{(folder, id)}` of the migration source files among repo-relative `paths`.
pub fn migration_ids(paths: &[String]) -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for path in paths {
        let normal = path.replace('\\', "/");
        let parts: Vec<&str> = normal.split('/').collect();
        if parts.len() >= 2 && parts[parts.len() - 2] == MIGRATIONS_FOLDER {
            if let Some(m) = MIGRATION_FILE.search(parts[parts.len() - 1]) {
                found.insert((parts[..parts.len() - 2].join("/"), m.group(1).unwrap_or("").to_string()));
            }
        }
    }
    found
}

/// The ref the loop branch started from: `origin/<base>` when it exists, else the local `<base>`; empty when neither exists.
pub fn base_reference(root: &str, base: &str) -> String {
    for candidate in [format!("origin/{base}"), base.to_string()] {
        if !base.is_empty() && git_ok(root, &["rev-parse", "--verify", "--quiet", &format!("{candidate}^{{commit}}")]) {
            return candidate;
        }
    }
    String::new()
}

/// `(project folder, last base migration or "0", last loop migration)`; `Ok(None)` = nothing added; `Err` when git cannot tell.
pub fn migrations_added(root: &str, base: &str, branch: &str) -> Result<Option<(String, String, String)>, String> {
    let reference = base_reference(root, base);
    if reference.is_empty() || branch.is_empty() {
        return Err(format!("base branch {} or loop branch {} not found", pycompat::py_repr_str(base), pycompat::py_repr_str(branch)));
    }
    let fork_point = git_lines(root, &["merge-base", &reference, branch]).and_then(|v| v.into_iter().next()).unwrap_or(reference);
    let changed = git_lines(root, &["diff", "--name-only", "--diff-filter=A", &fork_point, branch]).ok_or("git diff failed")?;
    let added = migration_ids(&changed);
    if added.is_empty() {
        return Ok(None);
    }
    let at_base = git_lines(root, &["ls-tree", "-r", "--name-only", &fork_point]).unwrap_or_default();
    let known = migration_ids(&at_base);
    let (folder, last) = added.iter().max_by(|a, b| a.1.cmp(&b.1)).cloned().expect("not empty");
    let previous = known.iter().filter(|(place, _)| *place == folder).map(|(_, id)| id.clone()).max();
    Ok(Some((folder, previous.unwrap_or_else(|| "0".into()), last)))
}

pub fn migration_script(root: &str, folder: &str, first: &str, last: &str, runner: Option<Runner>) -> Result<String, String> {
    let project_dir = if folder.is_empty() { root.to_string() } else { folder.split('/').fold(root.to_string(), |acc, part| nt_join(&acc, part)) };
    let args = ["dotnet", "ef", "migrations", "script", "--idempotent", first, last];
    let (code, out, err) = match runner {
        Some(r) => r(&args, &project_dir),
        None => default_runner(&args, &project_dir),
    };
    if code != 0 || out.trim().is_empty() {
        let text = if err.is_empty() { out.clone() } else { err };
        let described = describe_failure(&text);
        let by_hand = if text.contains("design time") || text.contains("Unable to create") {
            format!(
                " The migrations tool could not build the database context at design time (a setting such as a connection string or an environment name is missing for it). Generate the script by hand: run `dotnet ef migrations script --idempotent {first} {last}` in `{project_dir}` with that setting present."
            )
        } else {
            String::new()
        };
        return Err(format!("dotnet ef migrations script failed: {}{by_hand}", if described.is_empty() { "no output".to_string() } else { described }));
    }
    Ok(strip_build_progress(&out))
}

/// `dotnet ef` prints its build progress on stdout before the SQL: those lines are dropped so the file stays runnable SQL.
pub fn strip_build_progress(out: &str) -> String {
    let mut lines = pycompat::splitlines(out);
    while let Some(first) = lines.first() {
        if first.trim().is_empty() || BUILD_PROGRESS_LINES.contains(&first.trim()) {
            lines.remove(0);
        } else {
            break;
        }
    }
    lines.join("\n").trim_matches('\n').to_string()
}

/// The useful part of a failed `dotnet ef` output: the exception line when there is one, else the last 300 characters.
pub fn describe_failure(text: &str) -> String {
    let lines: Vec<&str> = pycompat::splitlines(text).into_iter().map(str::trim).filter(|l| !l.is_empty()).collect();
    if let Some(line) = lines.iter().find(|l| l.contains("Exception:")) {
        return line.chars().take(300).collect();
    }
    let joined = lines.join(" ");
    let chars: Vec<char> = joined.chars().collect();
    chars[chars.len().saturating_sub(300)..].iter().collect()
}

// ---------------------------------------------------------------- manual scripts

fn feature_order(roadmap_root: &str) -> Vec<String> {
    parse_roadmap(&read_text(&Path::new(roadmap_root).join(".specs").join("project").join("ROADMAP.md"))).0.slugs()
}

/// `[(feature, file name, absolute path)]` of `.specs/features/*/sql/*.sql` found in the first root that has any, in ROADMAP feature
/// order (features not in the ROADMAP follow alphabetically), files sorted by name.
pub fn manual_scripts(spec_roots: &[&str], roadmap_root: &str) -> Vec<(String, String, String)> {
    let order = feature_order(if roadmap_root.is_empty() { spec_roots.first().copied().unwrap_or("") } else { roadmap_root });
    for root in spec_roots.iter().filter(|r| !r.is_empty()) {
        let mut found: Vec<(String, Vec<String>)> = Vec::new();
        let features = Path::new(root).join(".specs").join("features");
        let mut folders: Vec<_> = std::fs::read_dir(&features).map(|d| d.flatten().collect()).unwrap_or_default();
        folders.sort_by_key(|e| e.file_name());
        for folder in folders {
            let name = folder.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !folder.path().join("sql").is_dir() {
                continue;
            }
            let mut files: Vec<String> = std::fs::read_dir(folder.path().join("sql"))
                .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".sql") && !n.starts_with('.')).collect())
                .unwrap_or_default();
            if !files.is_empty() {
                files.sort();
                found.push((name, files));
            }
        }
        if !found.is_empty() {
            let folder_order: Vec<String> = order.iter().map(|slug| feature_folder_name(slug)).collect();
            found.sort_by(|a, b| {
                let rank = |slug: &str| folder_order.iter().position(|f| f == slug).unwrap_or(folder_order.len());
                (rank(&a.0), &a.0).cmp(&(rank(&b.0), &b.0))
            });
            return found
                .into_iter()
                .flat_map(|(slug, files)| {
                    let base = Path::new(root).join(".specs").join("features").join(&slug).join("sql");
                    files.into_iter().map(move |f| (slug.clone(), f.clone(), base.join(&f).display().to_string()))
                })
                .collect();
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------- the file

pub fn safe_name(text: &str) -> String {
    let cleaned = SAFE_NAME.replace_lit(text.trim(), "-");
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "project".into()
    } else {
        trimmed.to_string()
    }
}

pub fn today_iso() -> String {
    time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc()).date().to_string()
}

pub fn output_path(env: &Env, project_name: &str, output_dir: &str, today: &str) -> String {
    let dir = if output_dir.is_empty() { crate::loop_summary::default_output_dir(env) } else { output_dir.to_string() };
    nt_join(&dir, &format!("{}-prod-database-{}.sql", safe_name(project_name), today))
}

/// Builds the production script. Returns `{"path", "repos", "sql_files", "warnings"}`; never fails.
#[allow(clippy::too_many_arguments)]
pub fn generate(
    env: &Env,
    project_name: &str,
    entries: &[Entry],
    roadmap_root: &str,
    output_dir: &str,
    runner: Option<Runner>,
    today: Option<&str>,
    roles: &[(String, String)],
    extra_spec_roots: &[&str],
) -> Value {
    let (mut repos, mut sql_files, mut warnings): (Vec<Value>, Vec<Value>, Vec<Value>) = Default::default();
    let mut path = String::new();
    let mut sections: Vec<String> = Vec::new();
    for entry in entries {
        if role_of(roles, &entry.name, &entry.repo_path) != "edit" {
            continue;
        }
        let outcome = migrations_added(&entry.root, &entry.base, &entry.branch).and_then(|found| match found {
            None => Ok(None),
            Some((folder, first, last)) => migration_script(&entry.root, &folder, &first, &last, runner).map(|body| Some((folder, first, last, body))),
        });
        match outcome {
            Ok(None) => {}
            Ok(Some((folder, first, last, body))) => {
                sections.push(format!(
                    "-- ---- {}: migrations {first} -> {last} (project folder: {}) ----\n{body}\n",
                    entry.name,
                    if folder.is_empty() { "." } else { &folder }
                ));
                repos.push(json!(entry.name));
            }
            Err(error) => {
                let message = format!("{}: production script of the migrations could not be generated ({})", entry.name, error.chars().take(300).collect::<String>());
                sections.push(format!("-- WARNING: {}\n", message.replace('\n', " ")));
                warnings.push(json!(message));
            }
        }
    }
    let mut roots: Vec<&str> = vec![roadmap_root];
    roots.extend_from_slice(extra_spec_roots);
    for (slug, filename, file) in manual_scripts(&roots, roadmap_root) {
        let text = read_text(Path::new(&file)).trim_matches('\n').to_string();
        if !text.is_empty() {
            sections.push(format!("-- ---- manual script: {slug}/{filename} ----\n{text}\n"));
            sql_files.push(json!(format!("{slug}/{filename}")));
        }
    }
    if !repos.is_empty() || !sql_files.is_empty() {
        let stamp = today.map(String::from).unwrap_or_else(today_iso);
        let target = output_path(env, project_name, output_dir, &stamp);
        let content = format!("{}\n{}", header(project_name, &stamp), sections.join("\n"));
        let written = Path::new(&target).parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::write(&target, content));
        match written {
            Ok(()) => path = target,
            Err(error) => warnings.push(json!(format!("production database script failed: {}", error.to_string().chars().take(300).collect::<String>()))),
        }
    }
    json!({"path": path, "repos": repos, "sql_files": sql_files, "warnings": warnings})
}

/// Delivery entry point: entries and roles from the primary worktree's meta.json.
pub fn generate_for_meta(env: &Env, primary_work: &str, meta: &Value, output_dir: &str, runner: Option<Runner>, today: Option<&str>, project_name: &str) -> Value {
    let entries = entries_from_meta(primary_work, meta);
    let primary = entries.iter().find(|e| e.primary).unwrap_or(&entries[0]);
    let roles = read_roles(&[&primary.repo_path, primary_work]);
    let name = if project_name.is_empty() {
        let source = if primary.repo_path.is_empty() { primary_work } else { &primary.repo_path };
        WORKTREE_SUFFIX.replace_lit(nt_abspath(source).rsplit('\\').next().unwrap_or(""), "")
    } else {
        project_name.to_string()
    };
    let extra: Vec<&str> = if primary.repo_path.is_empty() { vec![] } else { vec![primary.repo_path.as_str()] };
    generate(env, &name, &entries, primary_work, output_dir, runner, today, &roles, &extra)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_names() {
        let ids = migration_ids(&[
            "Api/Migrations/20260101120000_AddOrders.cs".into(),
            "Api/Migrations/20260101120000_AddOrders.Designer.cs".into(),
            "Api/Migrations/ModelSnapshot.cs".into(),
            "x/Other/20260101120000_A.cs".into(),
        ]);
        assert_eq!(ids.into_iter().collect::<Vec<_>>(), vec![("Api".to_string(), "20260101120000_AddOrders".to_string())]);
        assert_eq!(safe_name("  my proj/x "), "my-proj-x");
        assert_eq!(safe_name("///"), "project");
    }

    #[test]
    fn a_secondary_repo_without_recorded_names_gets_its_real_branch_and_the_run_base() {
        let dir = tempfile::tempdir().unwrap();
        let web = dir.path().join("web");
        std::fs::create_dir_all(&web).unwrap();
        for args in [
            &["init", "-q", "-b", "loop/z"][..],
            &["config", "user.name", "t"],
            &["config", "user.email", "t@example.com"],
            &["commit", "-q", "--allow-empty", "-m", "one"],
        ] {
            assert!(std::process::Command::new("git").arg("-C").arg(&web).args(args).output().unwrap().status.success());
        }
        let primary = dir.path().join("app");
        std::fs::create_dir_all(&primary).unwrap();
        let meta = json!({"base": "developer", "repos": {
            "app": {"primary": true, "branch": "loop/a", "base": "developer", "worktree": primary.display().to_string()},
            "web": {"worktree": web.display().to_string()}}});
        let entries = entries_from_meta(&primary.display().to_string(), &meta);
        let second = entries.iter().find(|e| e.name == "web").unwrap();
        assert_eq!((second.branch.as_str(), second.base.as_str()), ("loop/z", "developer"), "no empty names reach the warning");
        let first = entries.iter().find(|e| e.name == "app").unwrap();
        assert_eq!((first.branch.as_str(), first.base.as_str()), ("loop/a", "developer"));
    }

    #[test]
    fn a_design_time_failure_says_what_to_do_by_hand() {
        let runner: Runner = &|_, _| (1, String::new(), "Unable to create a 'DbContext' of type ''. design time".to_string());
        let error = migration_script("C:\\work", "Api", "0", "20260101_A", Some(runner)).unwrap_err();
        assert!(error.contains("could not build the database context at design time"), "{error}");
        assert!(error.contains("dotnet ef migrations script --idempotent 0 20260101_A"), "{error}");
        let plain: Runner = &|_, _| (1, String::new(), "boom".to_string());
        assert!(!migration_script("C:\\work", "", "0", "1", Some(plain)).unwrap_err().contains("by hand"));
    }

    #[test]
    fn dotnet_output_is_cleaned() {
        assert_eq!(strip_build_progress("Build started...\nBuild succeeded.\n\nCREATE TABLE a;\nGO\n"), "CREATE TABLE a;\nGO");
        assert_eq!(describe_failure("warn NU1\nSystem.Exception: boom here\nlater"), "System.Exception: boom here");
        assert_eq!(describe_failure("a\nb"), "a b");
    }

    #[test]
    fn manual_scripts_follow_the_roadmap_and_make_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        let write = |rel: &str, text: &str| {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        write(".specs/project/ROADMAP.md", "## Stage 1\n- [b-two] status=verified\n- [a-one] status=verified\n");
        write(".specs/features/a-one/sql/2.sql", "SELECT 2;\r\n");
        write(".specs/features/a-one/sql/1.sql", "SELECT 1;\n");
        write(".specs/features/b-two/sql/x.sql", "SELECT 9;\n");
        let found: Vec<String> = manual_scripts(&[&root], &root).into_iter().map(|(s, f, _)| format!("{s}/{f}")).collect();
        assert_eq!(found, vec!["b-two/x.sql", "a-one/1.sql", "a-one/2.sql"]);
        let env = Env::from_pairs([("LOOP_DATA_DIR", "C:\\no-such-dir")]);
        let out = dir.path().join("out").display().to_string();
        let entries = vec![Entry { name: "proj".into(), root: root.clone(), ..Default::default() }];
        let result = generate(&env, "proj", &entries, &root, &out, None, Some("2026-01-02"), &[("proj".to_string(), "reference".to_string())], &[]);
        assert_eq!(result["sql_files"], json!(["b-two/x.sql", "a-one/1.sql", "a-one/2.sql"]));
        let text = std::fs::read_to_string(result["path"].as_str().unwrap()).unwrap();
        assert!(
            text.starts_with("-- ====")
                && text.contains("-- Project: proj    Generated: 2026-01-02")
                && text.contains("-- ---- manual script: a-one/2.sql ----\nSELECT 2;\n")
        );
        assert_eq!(result["warnings"], json!([]));
    }

    #[test]
    fn nothing_to_script_writes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        let env = Env::from_pairs([("LOOP_DATA_DIR", "C:\\no-such-dir")]);
        let result = generate(&env, "p", &[], &root, &root, None, Some("2026-01-02"), &[], &[]);
        assert_eq!(result, json!({"path": "", "repos": [], "sql_files": [], "warnings": []}));
    }
}
