// SPDX-License-Identifier: MIT
//! What the loop reads from its worktrees between turns: roadmap score, fingerprint, new commits, leftovers (dirty files) and how they
//! are settled; plus `--rewind` and the loop lock.

use crate::loop_io::{
    atomic_write_text, git, lock_holder_alive, load_json, process_start_time, read, read_lock_holder, save_meta, status_rows, worktree_of, StatusRow,
    LOCK_START_SUFFIX,
};
use crate::loop_state::{self, feature_repo_paths, loop_path, parse_roadmap, Features};
use crate::system_map::SYSTEM_DOCUMENT;
use pycompat::json::py_json_dumps;
use pycompat::{nt_normpath, Re};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// blocked is NOT progress: it scores 0, otherwise a loop that only blocks features would look like it advances.
pub fn rank(status: &str) -> i64 {
    match status {
        "speccing" => 1,
        "specced" => 2,
        "approved" => 3,
        "building" => 4,
        "verified" => 6,
        _ => 0,
    }
}

static SCORE_LINE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^- \[[^\]]+\]\s+.*?status=(\w+)"));
static PORCELAIN_ROW: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*[A-Z?!]{1,2}\s+(.*)\z"));

pub const IN_FLIGHT_STATUSES: [&str; 3] = ["speccing", "approved", "building"];
const LITERAL_PATHSPECS: &str = "--literal-pathspecs";
const PATHS_PER_GIT_CALL: usize = 100;

fn roadmap_path(work: &str) -> PathBuf {
    Path::new(work).join(".specs").join("project").join("ROADMAP.md")
}

fn string_set(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        Some(Value::Object(o)) => o.keys().cloned().collect(),
        _ => vec![],
    }
}

/// `[(repo name, worktree path)]` the loop works in, primary first. A single-repo project = `[("", work)]`.
pub fn loop_worktrees(work: &str, meta: &Value) -> Vec<(String, String)> {
    let Some(entries) = meta.get("repos").and_then(Value::as_object).filter(|e| !e.is_empty()) else {
        return vec![(String::new(), work.to_string())];
    };
    let mut pairs: Vec<(&String, &Value)> = entries.iter().collect();
    pairs.sort_by_key(|(_, v)| !crate::notify_truthy(v.get("primary")));
    pairs
        .into_iter()
        .map(|(name, entry)| {
            let wt = entry.get("worktree").and_then(Value::as_str).filter(|w| !w.is_empty()).unwrap_or(work);
            (name.clone(), wt.to_string())
        })
        .collect()
}

// ---------------------------------------------------------------- state

/// `(score, total, verified-or-blocked)`.
pub fn roadmap_score(work: &str) -> (i64, i64, i64) {
    let (mut score, mut total, mut closed) = (0, 0, 0);
    for line in pycompat::splitlines(&read(&roadmap_path(work))) {
        if let Some(m) = SCORE_LINE.match_start(line.trim()) {
            total += 1;
            let status = m.group(1).unwrap_or("");
            score += rank(status);
            closed += i64::from(status == "verified" || status == "blocked");
        }
    }
    (score, total, closed)
}

/// `{repo name: HEAD of its worktree}` for a multi-repo project; `None` for a single-repo one.
pub fn heads_of(work: &str, meta: &Value) -> Option<Vec<(String, String)>> {
    if !crate::notify_truthy(meta.get("repos")) {
        return None;
    }
    Some(loop_worktrees(work, meta).into_iter().map(|(name, path)| (name, git(&path, &["rev-parse", "HEAD"]).1)).collect())
}

pub fn fingerprint(work: &str, meta: &Value) -> String {
    let mut head = git(work, &["rev-parse", "HEAD"]).1;
    if let Some(mut heads) = heads_of(work, meta) {
        heads.sort();
        head.push_str(&heads.into_iter().map(|(_, v)| v).collect::<String>()); // a commit in any worktree is a change
    }
    let text = format!("{}{}{}", read(&roadmap_path(work)), read(&Path::new(work).join(".specs").join("project").join("STATE.md")), head);
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Commits made since the previous turn's snapshot, summed over EVERY worktree of the project.
pub fn count_new_commits(prev: Option<&serde_json::Map<String, Value>>, work: &str, meta: &Value, head_now: &str) -> i64 {
    let Some(prev) = prev else { return 0 };
    let prev_head = prev.get("head").and_then(Value::as_str).unwrap_or("");
    let count = git(work, &["rev-list", "--count", &format!("{prev_head}..{head_now}")]).1;
    let mut total: i64 = if !count.is_empty() && count.chars().all(|c| c.is_ascii_digit()) { count.parse().unwrap_or(0) } else { 0 };
    let before = prev.get("heads").and_then(Value::as_object);
    for (name, path) in loop_worktrees(work, meta).iter().skip(1) {
        if let Some(h) = before.and_then(|b| b.get(name)).and_then(Value::as_str).filter(|h| !h.is_empty()) {
            let extra = git(path, &["rev-list", "--count", &format!("{h}..HEAD")]).1;
            if !extra.is_empty() && extra.chars().all(|c| c.is_ascii_digit()) {
                total += extra.parse::<i64>().unwrap_or(0);
            }
        }
    }
    total
}

/// `[(repo name, rows)]` of every worktree with changes outside .specs/. The name is "" for a single-repo project.
pub fn dirty_worktrees(work: &str, meta: &Value) -> Vec<(String, Vec<StatusRow>)> {
    let mut dirty = Vec::new();
    for (name, path) in loop_worktrees(work, meta) {
        let repo = meta.get("repos").and_then(|r| r.get(&name));
        let mut ignore: BTreeSet<String> = string_set(meta.get("copied")).into_iter().collect();
        ignore.extend(string_set(repo.and_then(|r| r.get("copied"))));
        ignore.extend(string_set(meta.get("carried_setup_files"))); // carried setup files wait for the first checkpoint commit
        ignore.extend(string_set(repo.and_then(|r| r.get("carried_setup_files"))));
        if crate::notify_truthy(meta.get("system_md_uncommitted")) && nt_normpath(&path) == nt_normpath(work) {
            ignore.insert(SYSTEM_DOCUMENT.to_string()); // the engine's contract merge left it for the next checkpoint commit
        }
        let rows = dirty_outside_specs(&path, &ignore);
        if !rows.is_empty() {
            dirty.push((name, rows));
        }
    }
    dirty
}

/// Rows of the changes outside .specs/ and outside `ignore`.
pub fn dirty_outside_specs(work: &str, ignore: &BTreeSet<String>) -> Vec<StatusRow> {
    let (_, found, _) = status_rows(work, &[]);
    let mut rows = Vec::new();
    for row in found {
        let path = row.path.clone();
        if path.starts_with(".specs/") || ignore.contains(path.trim_end_matches('/')) {
            continue;
        }
        if path.ends_with('/') && row.status == "??" {
            // an untracked folder is one collapsed row: expand it when an ignored file may sit inside
            if ignore.iter().any(|name| name.starts_with(&path)) {
                let (_, listing, _) = status_rows(work, &["-uall", "--", &path]);
                rows.extend(listing.into_iter().filter(|item| !ignore.contains(&item.path)));
                continue;
            }
        }
        rows.push(row);
    }
    rows
}

fn git_escape(c: char) -> Option<u8> {
    Some(match c {
        'a' => 7,
        'b' => 8,
        't' => 9,
        'n' => 10,
        'v' => 11,
        'f' => 12,
        'r' => 13,
        '"' => 34,
        '\\' => 92,
        _ => return None,
    })
}

/// A path as `git status` (without -z) prints it: in double quotes, C escapes, bytes as octal.
pub fn unquote_git_path(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if !(chars.len() >= 2 && chars[0] == '"' && chars[chars.len() - 1] == '"') {
        return text.to_string();
    }
    let body = &chars[1..chars.len() - 1];
    let (mut raw, mut index) = (Vec::<u8>::new(), 0);
    while index < body.len() {
        let c = body[index];
        if c != '\\' || index + 1 >= body.len() {
            let mut buf = [0u8; 4];
            raw.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            index += 1;
            continue;
        }
        let digits: String = body[index + 1..].iter().copied().take(3).take_while(|d| ('0'..='7').contains(d)).collect();
        if !digits.is_empty() {
            raw.push((u32::from_str_radix(&digits, 8).unwrap_or(0) & 0xFF) as u8);
            index += 1 + digits.len();
            continue;
        }
        let escaped = body[index + 1];
        match git_escape(escaped) {
            Some(b) => raw.push(b),
            None => {
                let mut buf = [0u8; 4];
                raw.extend_from_slice(escaped.encode_utf8(&mut buf).as_bytes());
            }
        }
        index += 2;
    }
    String::from_utf8_lossy(&raw).into_owned()
}

/// Path of a plain `git status --porcelain` line (quotes and escapes decoded, `old -> new` reduced to the new name).
pub fn row_path_text(row: &str) -> String {
    let text = PORCELAIN_ROW.match_start(row).and_then(|m| m.group(1).map(String::from)).unwrap_or_else(|| row.to_string());
    let mut text = text.trim().to_string();
    if text.contains(" -> ") {
        text = text.rsplit(" -> ").next().unwrap_or("").trim().to_string();
    }
    unquote_git_path(&text).replace('\\', "/")
}

/// `(inside, outside)`: the dirty rows that fall under the `paths=` of an in-flight feature, and the rest.
pub fn feature_scope_rows(rows: &[StatusRow], repo_name: &str, features: &Features, primary_name: &str) -> (Vec<StatusRow>, Vec<StatusRow>) {
    let mut scopes: Vec<String> = Vec::new();
    for (_, feature) in features.iter() {
        if !IN_FLIGHT_STATUSES.contains(&feature.status()) {
            continue;
        }
        for (repo, path) in feature_repo_paths(feature, primary_name) {
            if repo_name.is_empty() || repo.is_empty() || repo == repo_name {
                scopes.push(path.trim_matches('/').replace('\\', "/"));
            }
        }
    }
    let (mut inside, mut outside) = (Vec::new(), Vec::new());
    for row in rows {
        let path = row.path.trim_end_matches('/').to_string();
        let covered = scopes.iter().any(|s| !s.is_empty() && (path == *s || path.starts_with(&format!("{s}/")) || s.starts_with(&format!("{path}/"))));
        if covered { inside.push(row.clone()) } else { outside.push(row.clone()) }
    }
    (inside, outside)
}

fn tail(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}

/// Commits exactly these rows on the loop branch (the executors never run git, so their work is still loose after the turn).
pub fn commit_leftovers(path: &str, rows: &[StatusRow], message: &str) -> (bool, String) {
    let names: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
    for chunk in names.chunks(PATHS_PER_GIT_CALL) {
        let mut args = vec![LITERAL_PATHSPECS, "add", "-A", "--"];
        args.extend_from_slice(chunk);
        let (code, _, err) = git(path, &args);
        if code != 0 {
            return (false, format!("git add failed: {}", tail(&err, 200)));
        }
    }
    let (code, _, err) = git(path, &["commit", "-m", message, "--no-verify"]);
    if code == 0 { (true, String::new()) } else { (false, format!("git commit failed: {}", tail(&err, 200))) }
}

fn delete_path(path: &str, name: &str) {
    let full = Path::new(path).join(name.trim_end_matches('/'));
    match std::fs::symlink_metadata(&full) {
        Ok(m) if m.is_file() || m.file_type().is_symlink() => {
            let _ = std::fs::remove_file(&full);
        }
        Ok(m) if m.is_dir() => {
            let _ = std::fs::remove_dir_all(&full);
        }
        _ => {}
    }
}

/// Throws away exactly these rows: tracked files go back to HEAD, untracked ones (and staged new ones) are deleted, a staged
/// rename/copy is undone.
pub fn discard_leftovers(path: &str, rows: &[StatusRow]) {
    for row in rows {
        let name = row.path.as_str();
        let status = row.status.as_str();
        if status == "??" {
            delete_path(path, name);
        } else if status.starts_with(['R', 'C']) {
            let mut reset = vec![LITERAL_PATHSPECS, "reset", "-q", "HEAD", "--", name];
            if !row.origin.is_empty() {
                reset.push(&row.origin);
            }
            git(path, &reset);
            delete_path(path, name);
            if !row.origin.is_empty() {
                git(path, &[LITERAL_PATHSPECS, "checkout", "HEAD", "--", &row.origin]);
            }
        } else if status.starts_with('A') {
            git(path, &[LITERAL_PATHSPECS, "rm", "-q", "-f", "--cached", "--", name]);
            delete_path(path, name);
        } else {
            git(path, &[LITERAL_PATHSPECS, "checkout", "HEAD", "--", name]);
        }
    }
}

/// The options of the loop that matter for leftovers.
#[derive(Clone, Debug, Default)]
pub struct LeftoverOpts {
    /// `""`, `commit` or `discard` (panel recovery).
    pub leftovers: String,
    pub primary_name: String,
}

/// Runs before every turn. Returns the `[(repo name, rows)]` that are still dirty (the loop stops on them). Leftovers of the previous
/// turn that sit under the paths of an in-flight feature are committed as a checkpoint; anything else stays and stops the loop.
pub fn settle_dirty_worktrees(work: &str, meta: &mut Value, turn: i64, opts: &LeftoverOpts) -> Vec<(String, Vec<StatusRow>)> {
    let features = parse_roadmap(&read(&roadmap_path(work))).0;
    let mut still_dirty = Vec::new();
    for (name, rows) in dirty_worktrees(work, meta) {
        let path = loop_worktrees(work, meta).into_iter().find(|(n, _)| *n == name).map(|(_, p)| p).unwrap_or_else(|| work.to_string());
        let in_repo = if name.is_empty() { String::new() } else { format!(" in repo {name}") };
        if opts.leftovers == "discard" {
            discard_leftovers(&path, &rows);
            println!("[loop] --leftovers discard: {} leftover file(s) discarded{in_repo}", rows.len());
            continue;
        }
        let (inside, mut outside) = if opts.leftovers == "commit" { (rows.clone(), vec![]) } else { feature_scope_rows(&rows, &name, &features, &opts.primary_name) };
        if !inside.is_empty() {
            let (saved, problem) = commit_leftovers(&path, &inside, &format!("chore(loop): checkpoint of work left uncommitted by turn {}", turn - 1));
            if saved {
                println!("[loop] checkpoint: {} file(s) left by turn {} committed on the loop branch{}", inside.len(), turn - 1, if name.is_empty() { String::new() } else { format!(" (repo {name})") });
                if let Some(map) = meta.as_object_mut() {
                    let list = map.entry("checkpoints").or_insert_with(|| json!([]));
                    if let Some(a) = list.as_array_mut() {
                        a.push(json!({"turn": turn - 1, "files": inside.len(), "repo": name}));
                    }
                }
            } else {
                outside = rows.clone();
                println!("[loop] checkpoint failed: {problem}");
            }
        }
        if !outside.is_empty() {
            still_dirty.push((name, outside));
        }
    }
    still_dirty
}

/// Time travel: restores the state (ROADMAP/STATE/LOOP/ANSWERS/RULES) and the loop branch HEAD to right after turn N.
/// `Err(message)` is what the script prints before exiting with 1.
pub fn rewind(repo: &str, turn: u64, yes: bool) -> Result<i32, String> {
    let work = worktree_of(repo);
    if Path::new(&work).is_dir() {
        loop_state::migrate_layout(&work);
    }
    let meta_path = loop_path(&work, "meta.json");
    let mut meta = load_json(&meta_path);
    let branch = meta.get("branch").and_then(Value::as_str).unwrap_or("").to_string();
    if !Path::new(&work).is_dir() || branch.is_empty() {
        return Err(format!("there is no loop worktree for {repo}; nothing to rewind"));
    }
    let lock = loop_path(&work, "LOOP.lock");
    if lock.exists() && lock_holder_alive(&lock) {
        return Err("a loop is running in this worktree; stop it before rewinding".into());
    }
    let Some(snap) = loop_state::load_snapshot(&work, turn) else {
        let folder = loop_path(&work, "snapshots");
        let mut have: Vec<String> = std::fs::read_dir(&folder).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        have.sort();
        return Err(format!("no snapshot for turn {turn}. Available: {}", if have.is_empty() { "none".to_string() } else { have.join(", ") }));
    };
    let snap_head = snap.get("head").and_then(Value::as_str).unwrap_or("").to_string();
    let current = git(&work, &["rev-parse", "HEAD"]).1;
    let lost = git(&work, &["log", "--oneline", "--no-decorate", &format!("{snap_head}..{current}")]).1;
    let short = |h: &str| h.chars().take(8).collect::<String>();
    println!("[rewind] go back to the state after turn {turn} (HEAD {}; today {})", short(&snap_head), short(&current));
    println!("[rewind] commits leaving branch {branch} (they stay in the git reflog):\n{}", if lost.is_empty() { "  (none)" } else { &lost });
    if !yes {
        println!("[rewind] nothing was changed. Confirm with --yes");
        return Ok(0);
    }
    let copied: BTreeSet<String> = string_set(meta.get("copied")).into_iter().collect();
    if !dirty_outside_specs(&work, &copied).is_empty() {
        return Err("dirty tree outside .specs/ in the worktree: resolve it before rewinding".into());
    }
    let (code, _, err) = git(&work, &["reset", "--hard", &snap_head]);
    if code != 0 {
        return Err(format!("git reset failed: {err}"));
    }
    loop_state::restore(&work, turn);
    if let Some(map) = meta.as_object_mut() {
        map.insert("turns".into(), json!(turn));
        map.insert("stopped".into(), json!("rewind"));
        map.insert("reason".into(), json!(format!("state restored to after turn {turn}")));
    }
    let _ = save_meta(&meta_path, &mut meta);
    println!("[rewind] done. Run the loop command again to continue from turn {}.", turn + 1);
    Ok(0)
}

// ---------------------------------------------------------------- lock

/// Creates the lock file atomically: exactly one process gets it. Plain PID inside (the panel reads it with int()); the start time of
/// this process goes in a sidecar next to it, so a later process that reuses the PID is not taken for the owner.
pub fn create_lock_file(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    write!(file, "{}", std::process::id())?;
    drop(file);
    if let Some(started) = process_start_time(i64::from(std::process::id())) {
        let sidecar = PathBuf::from(format!("{}{LOCK_START_SUFFIX}", path.display()));
        let _ = atomic_write_text(&sidecar, &py_json_dumps(&json!({"pid": std::process::id(), "started": started}), true)); // without the sidecar the lock is judged by the PID alone
    }
    Ok(())
}

/// Takes a stale lock out of the way. It is renamed first and the PID inside is compared with the one judged stale: if another
/// process replaced the file in between (a live lock), it goes back.
pub fn remove_stale_lock(path: &Path, stale_pid: i64) {
    let taken = PathBuf::from(format!("{}.stale-{}", path.display(), std::process::id()));
    if crate::loop_io::replace_file(path, &taken).is_err() {
        return; // somebody else removed it first
    }
    if read_lock_holder(&taken).0 != stale_pid {
        let _ = std::fs::rename(&taken, path);
        return;
    }
    let _ = std::fs::remove_file(&taken);
    let _ = std::fs::remove_file(format!("{}{LOCK_START_SUFFIX}", path.display()));
}

/// Takes LOOP.lock for this process or fails when a live loop holds it. A stale lock (dead PID, or a PID now owned by another process)
/// is removed and the creation is tried once more, still atomically.
pub fn acquire_lock(work: &str) -> Result<PathBuf, String> {
    let path = loop_path(work, "LOOP.lock");
    let _ = std::fs::create_dir_all(path.parent().expect("has parent"));
    for attempt in 0..2 {
        match create_lock_file(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_holder_alive(&path) {
                    break;
                }
                if attempt == 0 {
                    remove_stale_lock(&path, read_lock_holder(&path).0);
                }
            }
            Err(_) => break,
        }
    }
    let pid = read_lock_holder(&path).0;
    Err(format!("a loop is already running in this worktree (PID {}). Wait or stop it.", if pid != 0 { pid.to_string() } else { "unknown".into() }))
}

/// Removes the lock this process holds (never another loop's).
pub fn release_lock(path: &Path) {
    if read_lock_holder(path).0 != i64::from(std::process::id()) {
        return;
    }
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}{LOCK_START_SUFFIX}", path.display()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_and_paths() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let road = dir.path().join(".specs").join("project");
        std::fs::create_dir_all(&road).unwrap();
        std::fs::write(road.join("ROADMAP.md"), "## Stage 1\n- [a] status=verified\n- [b] status=blocked\n- [c] scope=x status=building\n- [d] status=pending\n").unwrap();
        assert_eq!(roadmap_score(&work), (10, 4, 2));
        assert_eq!(unquote_git_path("\"configura\\303\\247\\303\\243o.txt\""), "configuração.txt");
        assert_eq!(unquote_git_path("\"a\\tb\\\"c\""), "a\tb\"c");
        assert_eq!(unquote_git_path("plain"), "plain");
        assert_eq!(row_path_text("R  old.txt -> \"new name.txt\""), "new name.txt");
        assert_eq!(row_path_text(" M src/a.rs"), "src/a.rs");
        let meta = json!({"repos": {"b": {"worktree": "WB"}, "a": {"primary": true, "worktree": "WA"}}});
        assert_eq!(loop_worktrees("W", &meta), vec![("a".to_string(), "WA".to_string()), ("b".to_string(), "WB".to_string())]);
        assert_eq!(loop_worktrees("W", &json!({})), vec![(String::new(), "W".to_string())]);
    }

    #[test]
    fn scope_rows_split_by_in_flight_paths() {
        let features = parse_roadmap("## Stage 1\n- [a] status=building paths=src/api\n- [b] status=pending paths=web\n").0;
        let row = |p: &str| StatusRow { status: "??".into(), path: p.into(), origin: String::new() };
        let (inside, outside) = feature_scope_rows(&[row("src/api/x.rs"), row("web/y.js"), row("src/")], "", &features, "");
        assert_eq!((inside.len(), outside.len()), (2, 1));
        assert_eq!(outside[0].path, "web/y.js");
    }

    #[test]
    fn lock_is_exclusive_and_released() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let path = acquire_lock(&work).unwrap();
        assert_eq!(read_lock_holder(&path).0, i64::from(std::process::id()));
        assert!(acquire_lock(&work).unwrap_err().starts_with("a loop is already running in this worktree (PID "));
        release_lock(&path);
        assert!(!path.exists());
        std::fs::write(&path, "999999999").unwrap(); // a dead pid: stale
        assert!(acquire_lock(&work).is_ok());
    }
}
