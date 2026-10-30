// SPDX-License-Identifier: MIT
//! The refusal explainer through the whole hook: what it adds, and that it never changes what is blocked.

use cfg::Env;
use guard_core::paths::G;

struct Repo {
    _dir: tempfile::TempDir,
    root: String,
    g: G,
}

fn repo() -> Repo {
    // under the target dir: the OS temp dir can be an 8.3 short path, which the guard expands on one side only
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = dir.path().join("proj");
    // a repository of its own: the guard finds the project by walking up to the nearest `.git`, and the target folder sits inside another one
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join(".specs/project")).unwrap();
    std::fs::write(root.join(".specs/project/.env.test"), "API_URL=https://api.example.com\n").unwrap();
    std::fs::write(root.join(".specs/project/NETWORK.json"), r#"{"allow": ["api.example.com"]}"#).unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let pairs = [
        ("LOOP_DATA_DIR", data.display().to_string()),
        ("USERPROFILE", dir.path().display().to_string()),
        ("HOME", dir.path().display().to_string()),
        ("LOOP_HEADLESS", "1".to_string()),
    ];
    Repo { root: root.display().to_string(), _dir: dir, g: G::new(Env::from_pairs(pairs)) }
}

fn run_bash(r: &Repo, command: &str) -> guard_core::run::Outcome {
    let payload = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}, "cwd": r.root});
    guard_core::run::run(&r.g, "verifier", &payload.to_string())
}

#[test]
fn a_call_form_refusal_names_the_accepted_form_and_says_it_is_not_a_permission() {
    let r = repo();
    let refused = run_bash(&r, "curl $TARGET/health");
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(refused.stderr.starts_with("BLOCKED by the orchestrator guard [egress-host]:"), "{}", refused.stderr);
    assert!(refused.stderr.contains("What to do instead:"), "{}", refused.stderr);
    assert!(refused.stderr.contains("[egress-host]"), "{}", refused.stderr);
    assert!(refused.stderr.contains("curl $API_URL/health"), "{}", refused.stderr);
    assert!(refused.stderr.contains("This is a call form, not a missing permission"), "{}", refused.stderr);
}

#[test]
fn a_refusal_on_purpose_is_not_presented_as_a_call_form() {
    let r = repo();
    let refused = run_bash(&r, "python -I script.py");
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(refused.stderr.contains("refused on purpose"), "{}", refused.stderr);
    assert!(!refused.stderr.contains("This is a call form"), "{}", refused.stderr);
}

#[test]
fn accepted_forms_still_pass_and_unlisted_hosts_are_still_refused_without_a_hint() {
    let r = repo();
    for command in ["curl $API_URL/health", "curl https://api.example.com/health"] {
        let outcome = run_bash(&r, command);
        assert_eq!(outcome.code, 0, "{command}: {}", outcome.stderr);
    }
    let unlisted = run_bash(&r, "curl https://evil.example.org/x");
    assert_eq!(unlisted.code, 2);
    assert!(!unlisted.stderr.contains("What to do instead"), "{}", unlisted.stderr);
    assert_eq!(run_bash(&r, "python -S script.py").code, 2);
    assert_eq!(run_bash(&r, "python -E script.py").code, 2);
}
