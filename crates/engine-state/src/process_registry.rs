// SPDX-License-Identifier: MIT
//! "What is running now": the processes the engine itself starts (the Claude turn), kept in `project/loop/json/processes.json` so the panel
//! (`GET state` -> `processes`) can list kind, feature, step, start time and pid. It holds labels and numbers only, never a command line or an
//! environment value.

use crate::loop_io::{load_json, now_iso, save_json};
use crate::loop_state::loop_path;
use serde_json::{json, Value};

const FILE: &str = "processes.json";

/// One lane of a turn: a feature at its own step.
pub fn lanes_of_prompt(prompt: &str) -> Vec<(String, String)> {
    let mut words = prompt.split_whitespace();
    while let Some(word) = words.next() {
        if word == "--lanes" {
            return words.next().unwrap_or("").split(',').filter_map(|lane| lane.split_once(':')).map(|(slug, step)| (slug.to_string(), step.to_string())).collect();
        }
    }
    vec![]
}

/// One-line title for a process (also usable as a window title): `Loop turn 7 · feature a, b · step execute`.
pub fn label_of(kind: &str, turn: i64, lanes: &[(String, String)]) -> String {
    let mut parts = vec![if kind == "claude_turn" { format!("Loop turn {turn}") } else { format!("{kind} {turn}") }];
    if !lanes.is_empty() {
        let slugs: Vec<&str> = lanes.iter().map(|(s, _)| s.as_str()).collect();
        parts.push(format!("feature {}", slugs.join(", ")));
        let mut steps: Vec<&str> = Vec::new();
        for (_, step) in lanes {
            if !steps.contains(&step.as_str()) {
                steps.push(step);
            }
        }
        parts.push(format!("step {}", steps.join(", ")));
    }
    parts.join(" \u{b7} ")
}

fn read_all(work: &str) -> Vec<Value> {
    load_json(&loop_path(work, FILE)).get("processes").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn write_all(work: &str, rows: Vec<Value>) {
    let path = loop_path(work, FILE);
    let _ = std::fs::create_dir_all(path.parent().expect("has parent"));
    let _ = save_json(&path, &json!({"processes": rows}));
}

/// The job object of THIS engine process: every process the loop starts is put in it (and what they start), so ending the loop ends them all,
/// even the ones whose parent is already gone. `None` where the system has no job objects.
fn job_of(work: &str) -> Option<std::sync::Arc<winproc::Job>> {
    static JOBS: std::sync::Mutex<Vec<(String, Option<std::sync::Arc<winproc::Job>>)>> = std::sync::Mutex::new(Vec::new());
    let key = pycompat::nt_normcase(work);
    let mut jobs = JOBS.lock().ok()?;
    if let Some((_, job)) = jobs.iter().find(|(k, _)| *k == key) {
        return job.clone();
    }
    let job = winproc::Job::new().map(std::sync::Arc::new);
    jobs.push((key, job.clone()));
    job
}

fn start_time_of(pid: i64) -> Option<u64> {
    crate::loop_io::process_start_time(pid)
}

fn name_of(pid: i64) -> String {
    u32::try_from(pid).ok().and_then(winproc::process_image_name).unwrap_or_default()
}

/// Records a started process and returns its id. `claude` and `account` say which command and sign-in the process runs on.
#[allow(clippy::too_many_arguments)]
pub fn register(work: &str, kind: &str, turn: i64, lanes: &[(String, String)], pid: i64, claude: &str, account: &str) -> String {
    let id = format!("{kind}-{turn}-{pid}");
    let mut rows: Vec<Value> = read_all(work).into_iter().filter(|r| r["id"] != id.as_str()).collect();
    adopt_in_job(work, pid);
    rows.push(json!({
        "id": id, "kind": kind, "turn": turn, "pid": pid, "started_at": now_iso(), "label": label_of(kind, turn, lanes),
        "feature": lanes.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>().join(","),
        "step": lanes.first().map(|(_, step)| step.as_str()).unwrap_or(""),
        "lanes": lanes.iter().map(|(s, st)| json!({"feature": s, "step": st})).collect::<Vec<_>>(),
        "claude": claude, "claude_account": account,
        "name": name_of(pid), "start_time": start_time_of(pid), "cwd": work,
    }));
    write_all(work, rows);
    id
}

/// Records any other process the engine started for the loop (a setup command, a helper, an app started for a check) with the folder it runs in,
/// and puts it in the engine's job object. Returns its id.
pub fn register_process(work: &str, kind: &str, label: &str, pid: i64, cwd: &str) -> String {
    let id = format!("{kind}-{pid}");
    let mut rows: Vec<Value> = read_all(work).into_iter().filter(|r| r["id"] != id.as_str()).collect();
    adopt_in_job(work, pid);
    rows.push(json!({
        "id": id, "kind": kind, "turn": 0, "pid": pid, "started_at": now_iso(), "label": label, "feature": "", "step": "", "lanes": [],
        "claude": "", "claude_account": "", "name": name_of(pid), "start_time": start_time_of(pid), "cwd": cwd,
    }));
    write_all(work, rows);
    id
}

fn adopt_in_job(work: &str, pid: i64) {
    if let (Some(job), Ok(pid)) = (job_of(work), u32::try_from(pid)) {
        job.assign(pid);
    }
}

/// Forgets a process that ended.
pub fn unregister(work: &str, id: &str) {
    let rows: Vec<Value> = read_all(work).into_iter().filter(|r| r["id"] != id).collect();
    write_all(work, rows);
}

/// Forgets every process (the loop ended or starts over: nothing it started is running any more).
pub fn clear(work: &str) {
    if loop_path(work, FILE).is_file() {
        write_all(work, vec![]);
    }
}

/// Records the processes in this engine's job that nobody registered: the programs a turn left running (a dev server, an app started for a
/// check). Their folder is unknown. Returns how many were added.
pub fn sweep(work: &str) -> usize {
    let Some(job) = job_of(work) else { return 0 };
    let known: Vec<i64> = read_all(work).iter().filter_map(|r| r["pid"].as_i64()).collect();
    let own = i64::from(std::process::id());
    let mut rows = read_all(work);
    let mut added = 0;
    for pid in job.pids().into_iter().map(i64::from).filter(|pid| *pid != own && !known.contains(pid)) {
        let name = name_of(pid);
        if ["conhost.exe", "openconsole.exe"].contains(&name.to_lowercase().as_str()) {
            continue; // the console host of a hidden program: it ends with its program and is noise in the list
        }
        rows.push(json!({
            "id": format!("left_over-{pid}"), "kind": "left_over", "turn": 0, "pid": pid, "started_at": now_iso(),
            "label": format!("left running by a turn: {}", if name.is_empty() { "unknown program" } else { &name }), "feature": "", "step": "", "lanes": [],
            "claude": "", "claude_account": "", "name": name, "start_time": start_time_of(pid), "cwd": "",
        }));
        added += 1;
    }
    if added > 0 {
        write_all(work, rows);
    }
    added
}

/// The row's process is still the one that was registered: alive, not this process, and (when its start time was recorded) the same start time,
/// so a pid the system gave to another program later is never mistaken for it.
fn is_the_registered_process(row: &Value) -> bool {
    let Some(pid) = row["pid"].as_i64().filter(|p| *p > 0) else { return false };
    if pid == i64::from(std::process::id()) || !crate::loop_io::pid_alive(pid) {
        return false;
    }
    match (row["start_time"].as_u64(), start_time_of(pid)) {
        (Some(then), Some(now)) => then == now,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn describe(row: &Value) -> Value {
    json!({"pid": row["pid"], "name": row["name"], "kind": row["kind"], "label": row["label"]})
}

/// Ends every registered process that is still the registered one, with its children (by pid only, never by program name), then forgets all
/// the rows. When this process owns the job, the job is ended too, which reaches the orphans no registry row names. Returns what was stopped.
pub fn stop_all(work: &str) -> Vec<Value> {
    sweep(work);
    let rows = read_all(work);
    let mut stopped = Vec::new();
    for row in &rows {
        if is_the_registered_process(row) {
            if let Some(pid) = row["pid"].as_i64() {
                crate::loop_io::kill_tree(pid);
                stopped.push(describe(row));
            }
        }
    }
    if let Some(job) = job_of(work) {
        job.terminate();
    }
    clear(work);
    stopped
}

/// Every registered process that is still running (names and pids only).
pub fn running(work: &str) -> Vec<Value> {
    sweep(work);
    read_all(work).iter().filter(|row| is_the_registered_process(row)).map(describe).collect()
}

/// The registered processes that may hold files under `folder`: still the registered process, and either started in a folder under it or
/// started somewhere unknown (a program a turn left running). Names and pids only.
pub fn holders(work: &str, folder: &str) -> Vec<Value> {
    sweep(work);
    let root = pycompat::nt_normcase(&pycompat::nt_abspath(folder));
    read_all(work)
        .iter()
        .filter(|row| is_the_registered_process(row))
        .filter(|row| {
            let cwd = row["cwd"].as_str().unwrap_or("");
            cwd.is_empty() || {
                let there = pycompat::nt_normcase(&pycompat::nt_abspath(cwd));
                there == root || there.starts_with(&format!("{root}\\"))
            }
        })
        .map(describe)
        .collect()
}

/// `node.exe (pid 12), cmd.exe (pid 34)`: how the holders are named in a message.
pub fn holders_text(holders: &[Value]) -> String {
    holders
        .iter()
        .map(|h| {
            let name = h["name"].as_str().filter(|n| !n.is_empty()).unwrap_or("a program");
            format!("{name} (pid {})", h["pid"].as_i64().unwrap_or(0))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The processes to show. A pid cannot be checked here, so `running` (the loop itself is alive) gates the list: a crashed loop leaves none.
pub fn list(work: &str, running: bool) -> Vec<Value> {
    if running {
        read_all(work)
    } else {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_and_label() {
        let lanes = lanes_of_prompt("/orchestrator continue --turn 7 --phase x --lanes a-api:execute,b-ui:spec --ready a-api");
        assert_eq!(lanes, vec![("a-api".to_string(), "execute".to_string()), ("b-ui".to_string(), "spec".to_string())]);
        assert_eq!(label_of("claude_turn", 7, &lanes), "Loop turn 7 \u{b7} feature a-api, b-ui \u{b7} step execute, spec");
        assert_eq!(label_of("claude_turn", 2, &[]), "Loop turn 2");
        assert!(lanes_of_prompt("/orchestrator continue").is_empty());
    }

    fn sleeper() -> std::process::Child {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "ping -n 60 127.0.0.1 > nul"]);
        crate::loop_io::hidden(&mut command).spawn().unwrap()
    }

    fn wait_gone(pid: i64) -> bool {
        (0..40).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            !crate::loop_io::pid_alive(pid)
        })
    }

    #[cfg(windows)]
    #[test]
    fn stop_all_ends_what_was_registered_with_its_children_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let (mut mine, mut bystander) = (sleeper(), sleeper());
        let (mine_pid, bystander_pid) = (i64::from(mine.id()), i64::from(bystander.id()));
        register_process(&work, "dev_server", "dev server of the E2E", mine_pid, &work);
        std::thread::sleep(std::time::Duration::from_millis(800));
        let kids: Vec<u32> = winproc_children_of_job(&work, mine_pid);
        let stopped = stop_all(&work);
        assert!(stopped.iter().any(|s| s["pid"] == mine_pid && s["kind"] == "dev_server"), "{stopped:?}");
        assert!(wait_gone(mine_pid), "the registered process is gone");
        assert!(kids.iter().all(|k| wait_gone(i64::from(*k))), "its child is gone too");
        assert!(crate::loop_io::pid_alive(bystander_pid), "a process nobody registered is never touched");
        assert!(list(&work, true).is_empty());
        let _ = mine.wait();
        bystander.kill().unwrap();
        let _ = bystander.wait();
    }

    /// The pids in this process's job besides `root` (the shell's children), read before they are stopped.
    #[cfg(windows)]
    fn winproc_children_of_job(work: &str, root: i64) -> Vec<u32> {
        job_of(work).map(|j| j.pids().into_iter().filter(|p| i64::from(*p) != root).collect()).unwrap_or_default()
    }

    #[cfg(windows)]
    #[test]
    fn a_reused_pid_and_the_engine_itself_are_never_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let mut other = sleeper();
        let other_pid = i64::from(other.id());
        // a row whose recorded start time is not the process's: the pid was reused by a program the loop never started
        let rows = vec![
            json!({"id": "old-1", "kind": "dev_server", "pid": other_pid, "start_time": 1u64, "name": "x.exe", "label": "old", "cwd": ""}),
            json!({"id": "self", "kind": "helper", "pid": std::process::id(), "name": "engine", "label": "self", "cwd": ""}),
        ];
        write_all(&work, rows);
        assert!(holders(&work, &work).is_empty());
        assert!(stop_all(&work).is_empty());
        assert!(crate::loop_io::pid_alive(other_pid) && crate::loop_io::pid_alive(i64::from(std::process::id())));
        other.kill().unwrap();
        let _ = other.wait();
    }

    #[cfg(windows)]
    #[test]
    fn holders_are_the_live_registered_processes_under_the_folder_or_of_unknown_folder() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let tree = dir.path().join("app-worktree-loop").display().to_string();
        let other_tree = dir.path().join("web-worktree-loop").display().to_string();
        let (mut here, mut there) = (sleeper(), sleeper());
        register_process(&work, "dev_server", "dev server", i64::from(here.id()), &format!("{tree}\\front"));
        register_process(&work, "api", "api of another repo", i64::from(there.id()), &other_tree);
        let found = holders(&work, &tree);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0]["pid"], i64::from(here.id()));
        assert!(holders_text(&found).contains(&format!("(pid {})", here.id())));
        assert!(holders(&work, &format!("{tree}-other")).is_empty(), "a folder that only shares the prefix is another folder");
        stop_all(&work);
        for child in [&mut here, &mut there] {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[test]
    fn register_list_unregister() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let lanes = vec![("a".to_string(), "gate".to_string())];
        let id = register(&work, "claude_turn", 3, &lanes, 4242, "claude", "dev@example.com");
        let rows = list(&work, true);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0]["pid"].as_i64(), rows[0]["feature"].as_str(), rows[0]["step"].as_str()), (Some(4242), Some("a"), Some("gate")));
        assert!(rows[0]["started_at"].as_str().is_some_and(|s| s.len() >= 19));
        assert!(list(&work, false).is_empty());
        unregister(&work, &id);
        assert!(list(&work, true).is_empty());
        register(&work, "claude_turn", 4, &[], 1, "claude", "");
        clear(&work);
        assert!(list(&work, true).is_empty());
    }
}
