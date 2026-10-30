// SPDX-License-Identifier: MIT
//! The Finish as ordered steps (`GET wrap-up` -> `steps`), the shape the panel's Stepper renders:
//! `{id, title, what, status: done|active|todo|error|skipped, result, action}`. A step is `error` only when something real blocks it, and then
//! `result` is the real reason in plain words and `action` is the one thing that fixes it (`{id, label, method, route}`, route relative to the
//! project API). The first step that is not done or skipped and not an error is `active`; the rest wait as `todo`. Nothing here decides
//! anything: it reads the close check the engine already made (the remote and GitHub decided it).

use serde_json::{json, Value};

const STEP_ORDER: [&str; 7] = ["stop_processes", "commit", "push", "pr", "database", "backup", "remove"];

fn reasons_with<'a>(repos: &'a [Value], code: &'a str) -> impl Iterator<Item = (&'a Value, &'a Value)> + 'a {
    repos.iter().flat_map(move |repo| repo["reasons"].as_array().into_iter().flatten().filter(move |r| r["code"] == code).map(move |r| (repo, r)))
}

fn text_of(repo: &Value, reason: &Value) -> String {
    format!("{}: {}", repo["name"].as_str().unwrap_or(""), reason["text"].as_str().unwrap_or(""))
}

fn step(id: &str, title: &str, what: &str, status: &str, result: String, action: Value) -> Value {
    json!({"id": id, "title": title, "what": what, "status": status, "result": result, "action": action})
}

/// The repos that still have a worktree to finish (a closed or never-created one has nothing to commit, push or remove).
fn live(repos: &[Value]) -> Vec<Value> {
    repos.iter().filter(|r| !r["reasons"].as_array().into_iter().flatten().any(|x| x["code"] == "already_closed" || x["code"] == "worktree_missing")).cloned().collect()
}

fn blocked_step(id: &str, title: &str, what: &str, done_text: &str, code: &str, repos: &[Value]) -> Value {
    let hits: Vec<(&Value, &Value)> = reasons_with(repos, code).collect();
    if hits.is_empty() {
        return step(id, title, what, "done", done_text.to_string(), Value::Null);
    }
    let result = hits.iter().map(|(repo, reason)| text_of(repo, reason)).collect::<Vec<_>>().join(" ");
    let action = hits.iter().map(|(_, reason)| reason["action"].clone()).find(|a| a.is_object()).unwrap_or(Value::Null);
    let mut item = step(id, title, what, "error", result, action);
    item["actions"] = json!(hits.iter().map(|(_, reason)| reason["action"].clone()).filter(|a| a.is_object()).collect::<Vec<_>>());
    item
}

/// `check_repos` = the repo items of the close check; `processes` = the loop's programs still running; `record` = the saved delivery record
/// (for the production database warnings); `remove_on_finish` = the run asked to remove the worktrees on Finish.
pub fn build_steps(check_repos: &[Value], processes: &[Value], record: &Value, remove_on_finish: bool) -> Vec<Value> {
    let repos = live(check_repos);
    let all_closed = !check_repos.is_empty() && repos.is_empty();
    let holders: Vec<&Value> = repos.iter().flat_map(|r| r["holders"].as_array().into_iter().flatten()).collect();
    let mut steps: Vec<Value> = Vec::new();

    let running_names = |list: &[&Value]| {
        list.iter()
            .map(|p| format!("{} (pid {})", p["name"].as_str().filter(|n| !n.is_empty()).unwrap_or("a program"), p["pid"].as_i64().unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let still: Vec<&Value> = processes.iter().chain(holders.iter().copied()).collect();
    steps.push(if still.is_empty() {
        step(
            "stop_processes",
            "Stop what the loop started",
            "Dev servers and apps the loop started for the checks must not keep files open.",
            "done",
            "Nothing the loop started is still running.".into(),
            Value::Null,
        )
    } else {
        let mut unique: Vec<&Value> = Vec::new();
        for process in still {
            if !unique.iter().any(|known| known["pid"] == process["pid"]) {
                unique.push(process);
            }
        }
        let mut item = step(
            "stop_processes",
            "Stop what the loop started",
            "Dev servers and apps the loop started for the checks must not keep files open.",
            "todo",
            format!("Still running: {}. Finish stops them first; you can stop them now.", running_names(&unique)),
            json!({"id": "stop_processes", "label": "Stop what the loop started", "method": "POST", "route": "wrap-up/stop-processes"}),
        );
        item["processes"] = json!(unique);
        item
    });

    steps.push(blocked_step(
        "commit",
        "Commit the work",
        "Every file the loop changed must be in a commit, or it is lost when the worktree goes.",
        "Everything is committed.",
        "tree_dirty",
        &repos,
    ));
    steps.push(blocked_step(
        "push",
        "Push the branch",
        "The remote must hold the loop branch at the last commit. Only the loop branch is pushed, never main or the integration branch.",
        "The remote holds every loop branch at its last commit.",
        "push_unconfirmed",
        &repos,
    ));
    steps.push(blocked_step(
        "pr",
        "Open the pull request",
        "GitHub must list a pull request from the loop branch. Nothing is merged.",
        "Every repo has its pull request.",
        "pr_missing",
        &repos,
    ));

    let warnings: Vec<String> = record["prod_database"]["warnings"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect();
    let script = record["prod_database"]["path"].as_str().unwrap_or("");
    steps.push(if !warnings.is_empty() {
        let mut item = step(
            "database",
            "Production database script",
            "The script you run before the production deploy; its warnings decide what you do by hand.",
            "todo",
            format!("{} warning(s): {}", warnings.len(), warnings.join(" ")),
            Value::Null,
        );
        item["warnings"] = json!(warnings);
        item
    } else if !script.is_empty() {
        step("database", "Production database script", "The script you run before the production deploy.", "done", format!("Script written: {script}"), Value::Null)
    } else {
        step(
            "database",
            "Production database script",
            "The script you run before the production deploy.",
            "skipped",
            "No database change to script, or it is not generated yet.".into(),
            Value::Null,
        )
    });

    steps.push(if all_closed {
        step(
            "backup",
            "Back up the run",
            "A verified copy of the run's notes and the summary is saved before anything is removed.",
            "done",
            "Done: the worktrees are already closed.".into(),
            Value::Null,
        )
    } else {
        step(
            "backup",
            "Back up the run",
            "A verified copy of the run's notes and the summary is saved before anything is removed.",
            "todo",
            "Finish does this just before removing the worktrees.".into(),
            Value::Null,
        )
    });
    steps.push(if !remove_on_finish {
        step(
            "remove",
            "Remove the worktrees",
            "Takes the loop folders away. The branches and commits stay.",
            "skipped",
            "This run was set to keep the worktrees.".into(),
            Value::Null,
        )
    } else if all_closed {
        step("remove", "Remove the worktrees", "Takes the loop folders away. The branches and commits stay.", "done", "The worktrees are removed.".into(), Value::Null)
    } else {
        let left = repos
            .iter()
            .flat_map(|r| r["reasons"].as_array().into_iter().flatten().filter(|x| x["code"] == "folder_left" || x["code"] == "files_in_use").map(move |x| text_of(r, x)))
            .collect::<Vec<_>>();
        if left.is_empty() {
            step(
                "remove",
                "Remove the worktrees",
                "Takes the loop folders away. The branches and commits stay.",
                "todo",
                "Finish removes them once the steps above are done.".into(),
                Value::Null,
            )
        } else {
            let action =
                repos.iter().flat_map(|r| r["reasons"].as_array().into_iter().flatten()).map(|x| x["action"].clone()).find(|a| a.is_object()).unwrap_or(Value::Null);
            step("remove", "Remove the worktrees", "Takes the loop folders away. The branches and commits stay.", "error", left.join(" "), action)
        }
    });

    debug_assert!(steps.iter().zip(STEP_ORDER).all(|(s, id)| s["id"] == id));
    let mut activated = false;
    for item in steps.iter_mut() {
        if !activated && item["status"] == "todo" {
            item["status"] = json!("active");
            activated = true;
        } else if item["status"] == "error" {
            activated = true; // the first real blocker is "now"; what comes after it waits
        }
    }
    steps
}

/// What each Finish button does, in plain words, so no click is a surprise. `risky` marks the one that can lose work.
pub fn finish_options(remove_on_finish: bool) -> Value {
    json!([
        {"mode": "safe", "label": "Finish", "risky": false,
         "what_happens": if remove_on_finish {
             "Pushes the loop branches and opens the pull requests if that is still pending, saves a backup, then removes the worktrees. Nothing is removed if anything would be lost: you see the reason instead."
         } else {
             "Pushes the loop branches and opens the pull requests if that is still pending. The worktrees stay, as this run was set to keep them."
         }},
        {"mode": "keep_worktree", "label": "Deliver and keep the worktrees", "risky": false,
         "what_happens": "Pushes the loop branches and opens the pull requests. The worktree folders stay on disk."},
        {"mode": "remove_anyway", "label": "Remove anyway", "risky": true,
         "what_happens": "Skips the delivery. Saves a backup, then deletes the worktree folders even if they hold work that was never pushed. The branches and their commits stay in your repo; uncommitted files are lost."}
    ])
}

/// Commit lines (`<sha> <subject>`) grouped by the feature in the subject's scope (`feat(<feature>): ...`); a subject without a scope is `other`.
pub fn group_commits_by_feature(commits: &[String]) -> Vec<Value> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for line in commits {
        let subject = line.split_once(' ').map(|x| x.1).unwrap_or(line);
        let scope = subject
            .split_once('(')
            .and_then(|(head, rest)| rest.split_once(')').filter(|(_, tail)| tail.starts_with(':') || tail.starts_with("!:")).map(|(s, _)| (head, s)));
        let feature = scope.filter(|(head, _)| !head.contains(' ')).map(|(_, s)| s.to_string()).unwrap_or_else(|| "other".to_string());
        match groups.iter_mut().find(|(f, _)| *f == feature) {
            Some((_, list)) => list.push(subject.to_string()),
            None => groups.push((feature, vec![subject.to_string()])),
        }
    }
    groups.into_iter().map(|(feature, subjects)| json!({"feature": feature, "commits": subjects})).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str, reasons: Value) -> Value {
        json!({"name": name, "reasons": reasons, "holders": []})
    }

    fn reason(code: &str, text: &str, action: &str) -> Value {
        json!({"code": code, "text": text, "action": if action.is_empty() { Value::Null } else { json!({"id": action, "label": "L", "method": "POST", "route": format!("r/{action}")}) }})
    }

    fn status_of(steps: &[Value], id: &str) -> String {
        steps.iter().find(|s| s["id"] == id).unwrap()["status"].as_str().unwrap().to_string()
    }

    #[test]
    fn every_blocker_is_a_red_step_with_its_real_reason_and_its_action() {
        let repos = vec![
            repo(
                "app",
                json!([
                    reason("tree_dirty", "2 file(s) are not committed.", "commit_push"),
                    reason("push_unconfirmed", "The remote does not have the branch `loop/x`.", "commit_push"),
                    reason("pr_missing", "No pull request yet.", "deliver")
                ]),
            ),
            repo("web", json!([])),
        ];
        let steps = build_steps(&repos, &[], &json!({}), true);
        assert_eq!(steps.iter().map(|s| s["id"].as_str().unwrap()).collect::<Vec<_>>(), STEP_ORDER);
        assert_eq!((status_of(&steps, "commit"), status_of(&steps, "push"), status_of(&steps, "pr")), ("error".into(), "error".into(), "error".into()));
        let commit = &steps[1];
        assert_eq!(commit["result"], "app: 2 file(s) are not committed.");
        assert_eq!(commit["action"]["id"], "commit_push");
        assert_eq!(steps[3]["action"]["route"], "r/deliver");
        assert_eq!(status_of(&steps, "stop_processes"), "done");
        assert_eq!(status_of(&steps, "backup"), "todo", "after a blocker everything waits");
    }

    #[test]
    fn a_clean_run_has_the_first_open_step_active_and_nothing_red() {
        let repos = vec![repo("app", json!([]))];
        let steps = build_steps(&repos, &[], &json!({}), true);
        assert!(steps.iter().all(|s| s["status"] != "error"));
        assert_eq!(status_of(&steps, "commit"), "done");
        assert_eq!(status_of(&steps, "database"), "skipped");
        assert_eq!(status_of(&steps, "backup"), "active");
        assert_eq!(status_of(&steps, "remove"), "todo");
        let kept = build_steps(&repos, &[], &json!({}), false);
        assert_eq!(status_of(&kept, "remove"), "skipped");
    }

    #[test]
    fn programs_the_loop_started_are_named_with_a_stop_action() {
        let repos = vec![json!({"name": "app", "reasons": [], "holders": [{"pid": 12, "name": "node.exe"}]})];
        let steps = build_steps(&repos, &[json!({"pid": 34, "name": "api.exe"}), json!({"pid": 12, "name": "node.exe"})], &json!({}), true);
        let first = &steps[0];
        assert_eq!(first["status"], "active");
        assert!(first["result"].as_str().unwrap().contains("api.exe (pid 34), node.exe (pid 12)"), "{first}");
        assert_eq!(first["action"]["route"], "wrap-up/stop-processes");
    }

    #[test]
    fn database_warnings_are_listed_and_a_closed_run_is_done() {
        let record = json!({"prod_database": {"warnings": ["app: base branch '' not found", "web: no migrations"]}});
        let steps = build_steps(&[repo("app", json!([]))], &[], &record, true);
        assert_eq!(steps[4]["warnings"].as_array().unwrap().len(), 2);
        assert!(steps[4]["result"].as_str().unwrap().starts_with("2 warning(s): app: base branch"));
        let closed = build_steps(&[repo("app", json!([reason("already_closed", "Already closed.", "")]))], &[], &json!({}), true);
        assert_eq!((status_of(&closed, "backup"), status_of(&closed, "remove"), status_of(&closed, "push")), ("done".into(), "done".into(), "done".into()));
    }

    #[test]
    fn a_folder_that_cannot_go_is_a_red_remove_step_with_the_retry() {
        let repos = vec![repo("app", json!([reason("files_in_use", "Files are in use. Nothing is unsafe.", "stop_processes")]))];
        let steps = build_steps(&repos, &[], &json!({}), true);
        assert_eq!(steps[6]["status"], "error");
        assert!(steps[6]["result"].as_str().unwrap().starts_with("app: Files are in use. Nothing is unsafe."));
    }

    #[test]
    fn buttons_say_what_they_do_and_commits_group_by_feature() {
        let options = finish_options(true);
        assert_eq!(options.as_array().unwrap().iter().filter(|o| o["risky"] == true).count(), 1);
        assert_eq!(options[2]["mode"], "remove_anyway");
        assert!(options[2]["what_happens"].as_str().unwrap().contains("uncommitted files are lost"));
        let commits = vec![
            "a1b2c3 feat(orders-export): add csv".to_string(),
            "d4e5f6 fix(orders-export): header".to_string(),
            "0a0b0c chore: tidy".to_string(),
            "1d1e1f feat(login): form".to_string(),
        ];
        let groups = group_commits_by_feature(&commits);
        assert_eq!(groups.iter().map(|g| g["feature"].as_str().unwrap()).collect::<Vec<_>>(), vec!["orders-export", "other", "login"]);
        assert_eq!(groups[0]["commits"], json!(["feat(orders-export): add csv", "fix(orders-export): header"]));
    }
}
