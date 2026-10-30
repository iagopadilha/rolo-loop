// SPDX-License-Identifier: MIT
//! What the remote and GitHub say about a loop branch: the truth the Finish check uses instead of a `delivery.json` written by an earlier run.
//!
//! * the push is confirmed only when `origin` holds the branch at the worktree HEAD (asked with `git ls-remote`, never a local ref);
//! * the pull request exists only when GitHub lists one for the branch (asked with `gh`); when GitHub cannot be asked (not a GitHub remote,
//!   `gh` missing or signed out) the answer falls back to `delivery.json`, and ONLY when that file was written for this very branch: a file
//!   from another branch (or one that does not say its branch) is ignored, and a `dry-run` line is never a state;
//! * every text here is plain words, free of credentials: a remote URL never shows its user info.

use crate::loop_io::{git, load_json, run_timeout_env, which};
use crate::loop_state::loop_path;
use serde_json::Value;

const REMOTE_TIMEOUT_SECONDS: u64 = 60;
const GITHUB_TIMEOUT_SECONDS: u64 = 60;
const NO_PROMPT: [(&str, &str); 2] = [("GIT_TERMINAL_PROMPT", "0"), ("GH_PROMPT_DISABLED", "1")];

/// What the remote holds for the branch.
#[derive(Clone, Debug, PartialEq)]
pub enum Push {
    /// The worktree has no commit to publish.
    NoHead,
    NoOrigin,
    /// `origin` holds the branch at the worktree HEAD.
    Held,
    /// `origin` has no such branch.
    Missing,
    /// `origin` holds an older commit of this branch (the newer ones are only local).
    Behind(String),
    /// `origin` holds a commit this worktree does not contain (nothing is ever forced).
    Diverged(String),
    /// The remote could not be asked (network, sign-in, bad URL): the short reason, free of credentials.
    Unreachable(String),
}

/// What GitHub says about the branch.
#[derive(Clone, Debug, PartialEq)]
pub enum Pr {
    /// An open (or merged) pull request exists.
    Found { url: String, state: String, base: String },
    /// GitHub was asked and has none.
    NoneFound,
    /// GitHub could not be asked: why, in plain words.
    Unknown(String),
}

/// `text` with the user info of every URL removed (`https://user:token@host/x` becomes `https://host/x`), so a credential never reaches a
/// message, a log or a payload.
pub fn strip_userinfo(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let end = tail.find(|c: char| c.is_whitespace() || c == '/' || c == '\'' || c == '"').unwrap_or(tail.len());
        let authority = &tail[..end];
        out.push_str(authority.rsplit_once('@').map(|x| x.1).unwrap_or(authority));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn short(text: &str, limit: usize) -> String {
    let flat = strip_userinfo(&text.split_whitespace().collect::<Vec<_>>().join(" "));
    if flat.chars().count() <= limit {
        flat
    } else {
        format!("{}...", flat.chars().take(limit).collect::<String>().trim_end())
    }
}

pub fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// Asks `origin` which commit it holds for `branch` and compares it with the worktree HEAD.
pub fn push_state(worktree: &str, branch: &str) -> Push {
    let (code, head, _) = git(worktree, &["rev-parse", "HEAD"]);
    if code != 0 || head.is_empty() {
        return Push::NoHead;
    }
    let (code, origin, _) = git(worktree, &["remote", "get-url", "origin"]);
    if code != 0 || origin.is_empty() {
        return Push::NoOrigin;
    }
    let reference = format!("refs/heads/{branch}");
    let (code, out, err) = run_timeout_env("git", &["-C", worktree, "ls-remote", "--heads", "origin", &reference], None, &NO_PROMPT, REMOTE_TIMEOUT_SECONDS);
    if code != 0 {
        return Push::Unreachable(short(if err.trim().is_empty() { "the remote did not answer" } else { &err }, 160));
    }
    let Some(remote) = out.split_whitespace().next().map(String::from) else { return Push::Missing };
    if remote == head {
        return Push::Held;
    }
    match git(worktree, &["merge-base", "--is-ancestor", &remote, "HEAD"]).0 {
        0 => Push::Behind(remote),
        _ => Push::Diverged(remote),
    }
}

/// `(url, state, base)` of each pull request in `gh pr list --json url,state,baseRefName` output; nothing for text that is not that list.
pub fn parse_prs(text: &str) -> Vec<(String, String, String)> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(text.trim()) else { return vec![] };
    items
        .iter()
        .filter_map(|item| {
            let url = item.get("url")?.as_str()?.to_string();
            let state = item.get("state").and_then(Value::as_str).unwrap_or("OPEN").to_uppercase();
            Some((url, state, item.get("baseRefName").and_then(Value::as_str).unwrap_or("").to_string()))
        })
        .collect()
}

/// The pull request that counts: an open one first, then a merged one (closed without merge is not a delivery).
pub fn pick_pr(prs: &[(String, String, String)]) -> Option<Pr> {
    let chosen = prs.iter().find(|p| p.1 == "OPEN").or_else(|| prs.iter().find(|p| p.1 == "MERGED"))?;
    Some(Pr::Found { url: chosen.0.clone(), state: chosen.1.clone(), base: chosen.2.clone() })
}

/// True for a remote URL on github.com (the only host `gh` is asked about).
pub fn is_github_remote(url: &str) -> bool {
    crate::delivery::github_slug(url).is_some()
}

/// Asks GitHub for the pull requests of `branch`.
pub fn pr_state(worktree: &str, branch: &str) -> Pr {
    let origin = git(worktree, &["remote", "get-url", "origin"]).1;
    if !is_github_remote(&origin) {
        return Pr::Unknown("the remote is not on GitHub, so there is no pull request to look for".into());
    }
    if which("gh").is_none() {
        return Pr::Unknown("the GitHub command line (`gh`) is not installed here, so GitHub could not be asked".into());
    }
    let (code, out, err) =
        run_timeout_env("gh", &["pr", "list", "--head", branch, "--state", "all", "--json", "url,state,baseRefName"], Some(worktree), &NO_PROMPT, GITHUB_TIMEOUT_SECONDS);
    if code != 0 {
        return Pr::Unknown(format!("GitHub did not answer ({})", short(if err.trim().is_empty() { "no detail" } else { &err }, 120)));
    }
    pick_pr(&parse_prs(&out)).unwrap_or(Pr::NoneFound)
}

/// The worktree's `delivery.json` when it was written for `branch`; otherwise an empty object and the reason it was ignored. A file that does
/// not name its branch cannot be tied to this run, so it is ignored too.
pub fn saved_delivery(worktree: &str, branch: &str) -> (Value, Option<String>) {
    let saved = load_json(&loop_path(worktree, "delivery.json"));
    let Some(map) = saved.as_object().filter(|m| !m.is_empty()) else { return (Value::Object(Default::default()), None) };
    match map.get("branch").and_then(Value::as_str).filter(|b| !b.is_empty()) {
        Some(recorded) if recorded == branch => (saved, None),
        Some(recorded) => {
            (Value::Object(Default::default()), Some(format!("the saved delivery record is from branch `{recorded}`, not from `{branch}`: it was ignored")))
        }
        None => (Value::Object(Default::default()), Some("the saved delivery record does not say which branch it is from: it was ignored".to_string())),
    }
}

/// Plain words for a push that is not confirmed.
pub fn push_text(push: &Push, branch: &str) -> String {
    match push {
        Push::Held => format!("The remote holds `{branch}` at the last commit."),
        Push::NoHead => format!("The branch `{branch}` has no commit to publish."),
        Push::NoOrigin => format!("This repo has no remote named `origin`, so `{branch}` cannot be published. Add the remote, then deliver."),
        Push::Missing => format!("The remote does not have the branch `{branch}` yet: nothing was published."),
        Push::Behind(remote) => format!("The remote holds `{branch}` only up to an older commit ({}); the newer commits exist only on this computer.", short_sha(remote)),
        Push::Diverged(remote) => {
            format!("The remote holds `{branch}` at a commit this worktree does not contain ({}). Nothing is forced: bring the two together by hand.", short_sha(remote))
        }
        Push::Unreachable(why) => format!("The remote could not be asked ({why}). Check the network or the sign-in, then try again."),
    }
}

/// Plain words for a missing pull request.
pub fn pr_text(pr: &Pr, branch: &str, base: &str) -> String {
    let target = if base.is_empty() { "the integration branch".to_string() } else { format!("`{base}`") };
    match pr {
        Pr::Found { url, .. } => format!("A pull request exists: {url}"),
        Pr::NoneFound => format!("GitHub has no pull request from `{branch}` to {target}. Deliver opens it."),
        Pr::Unknown(why) => {
            format!("No pull request is confirmed from `{branch}` to {target}: {why}. Open it from the compare link, or deliver again once GitHub can be reached.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn run_git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A repo on `loop/x` with a local bare repo as `origin`.
    fn repo_with_origin(dir: &Path) -> (String, String) {
        let bare = dir.join("origin.git");
        std::fs::create_dir_all(&bare).unwrap();
        run_git(&bare, &["init", "-q", "--bare"]);
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-q", "-b", "loop/x"]);
        run_git(&repo, &["config", "user.name", "t"]);
        run_git(&repo, &["config", "user.email", "t@example.com"]);
        std::fs::write(repo.join("a.txt"), "1").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "one"]);
        run_git(&repo, &["remote", "add", "origin", &bare.display().to_string()]);
        (repo.display().to_string(), bare.display().to_string())
    }

    #[test]
    fn credentials_never_survive_in_a_shown_url() {
        assert_eq!(
            strip_userinfo("fatal: unable to access 'https://user:secret@example.com/o/r.git/': nope"),
            "fatal: unable to access 'https://example.com/o/r.git/': nope"
        );
        assert_eq!(strip_userinfo("https://tok@example.com and ssh://git@host/x"), "https://example.com and ssh://host/x");
        assert_eq!(strip_userinfo("no url here"), "no url here");
        assert!(!short("failed https://a:b@example.com/x", 200).contains("a:b"));
    }

    #[test]
    fn the_push_is_what_the_remote_holds() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, _) = repo_with_origin(dir.path());
        assert_eq!(push_state(&repo, "loop/x"), Push::Missing);
        run_git(Path::new(&repo), &["push", "-q", "origin", "loop/x"]);
        assert_eq!(push_state(&repo, "loop/x"), Push::Held);
        std::fs::write(Path::new(&repo).join("b.txt"), "2").unwrap();
        run_git(Path::new(&repo), &["add", "."]);
        run_git(Path::new(&repo), &["commit", "-q", "-m", "two"]);
        assert!(matches!(push_state(&repo, "loop/x"), Push::Behind(_)));
        // the remote gets a commit this worktree does not have
        let other = dir.path().join("other");
        run_git(dir.path(), &["clone", "-q", &dir.path().join("origin.git").display().to_string(), &other.display().to_string()]);
        run_git(&other, &["config", "user.name", "t"]);
        run_git(&other, &["config", "user.email", "t@example.com"]);
        run_git(&other, &["checkout", "-q", "loop/x"]);
        std::fs::write(other.join("c.txt"), "3").unwrap();
        run_git(&other, &["add", "."]);
        run_git(&other, &["commit", "-q", "-m", "elsewhere"]);
        run_git(&other, &["push", "-q", "origin", "loop/x"]);
        assert!(matches!(push_state(&repo, "loop/x"), Push::Diverged(_)));
    }

    #[test]
    fn no_origin_and_an_unreachable_origin_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, _) = repo_with_origin(dir.path());
        run_git(Path::new(&repo), &["remote", "set-url", "origin", &dir.path().join("nowhere").display().to_string()]);
        let Push::Unreachable(why) = push_state(&repo, "loop/x") else { panic!("expected unreachable") };
        assert!(!why.is_empty());
        run_git(Path::new(&repo), &["remote", "remove", "origin"]);
        assert_eq!(push_state(&repo, "loop/x"), Push::NoOrigin);
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        run_git(&empty, &["init", "-q"]);
        assert_eq!(push_state(&empty.display().to_string(), "loop/x"), Push::NoHead);
    }

    #[test]
    fn github_answers_are_read_and_a_closed_pr_is_not_a_delivery() {
        let text = r#"[{"url":"https://github.com/o/r/pull/3","state":"CLOSED","baseRefName":"developer"},{"url":"https://github.com/o/r/pull/4","state":"OPEN","baseRefName":"developer"}]"#;
        let prs = parse_prs(text);
        assert_eq!(prs.len(), 2);
        assert_eq!(pick_pr(&prs), Some(Pr::Found { url: "https://github.com/o/r/pull/4".into(), state: "OPEN".into(), base: "developer".into() }));
        assert_eq!(pick_pr(&prs[..1]), None);
        let merged = parse_prs(r#"[{"url":"https://github.com/o/r/pull/9","state":"MERGED","baseRefName":"main"}]"#);
        assert!(matches!(pick_pr(&merged), Some(Pr::Found { state, .. }) if state == "MERGED"));
        assert!(parse_prs("not json").is_empty() && parse_prs("{}").is_empty());
        let dir = tempfile::tempdir().unwrap();
        let (repo, _) = repo_with_origin(dir.path());
        assert!(matches!(pr_state(&repo, "loop/x"), Pr::Unknown(_)), "a local remote is never asked about pull requests");
    }

    #[test]
    fn a_delivery_record_counts_only_for_its_own_branch() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let write = |text: &str| {
            let path = loop_path(&work, "delivery.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(r#"{"push": "done: origin/loop/x", "branch": "loop/x", "pr_status": "PR created"}"#);
        let (kept, why) = saved_delivery(&work, "loop/x");
        assert_eq!((kept["pr_status"].as_str(), why), (Some("PR created"), None));
        let (ignored, why) = saved_delivery(&work, "loop/y");
        assert!(ignored.as_object().unwrap().is_empty());
        assert!(why.unwrap().contains("`loop/x`, not from `loop/y`"));
        write(r#"{"push": "done: origin/loop/x", "pr_status": "PR created"}"#);
        assert!(saved_delivery(&work, "loop/x").1.unwrap().contains("does not say which branch"));
        write("{}");
        assert_eq!(saved_delivery(&work, "loop/x").1, None);
    }

    #[test]
    fn texts_name_the_cause_in_plain_words() {
        assert!(push_text(&Push::Missing, "loop/x").contains("does not have the branch `loop/x`"));
        assert!(push_text(&Push::Behind("abcdef123456".into()), "loop/x").contains("older commit (abcdef1)"));
        assert!(push_text(&Push::Diverged("abcdef123456".into()), "loop/x").contains("Nothing is forced"));
        assert!(push_text(&Push::NoOrigin, "loop/x").contains("no remote named `origin`"));
        assert!(pr_text(&Pr::NoneFound, "loop/x", "developer").contains("no pull request from `loop/x` to `developer`"));
        assert!(pr_text(&Pr::Unknown("x".into()), "loop/x", "").contains("integration branch"));
    }
}
