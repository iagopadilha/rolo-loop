// SPDX-License-Identifier: MIT
//! The default policy `ask` through the whole hook: a migration or the standard dev command waits for the user's approval (one request, no stop),
//! a project list releases it, an answer settles it, and production is never released.

use cfg::Env;
use guard_core::paths::G;
use guard_core::policy;

struct Repo {
    _dir: tempfile::TempDir,
    root: String,
    g: G,
}

fn repo(permissions: Option<&str>) -> Repo {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join(".specs/project")).unwrap();
    if let Some(text) = permissions {
        std::fs::write(root.join(".specs/project/PERMISSIONS.json"), text).unwrap();
    }
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

fn run(r: &Repo, profile: &str, command: &str) -> guard_core::run::Outcome {
    let payload = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}, "cwd": r.root});
    guard_core::run::run(&r.g, profile, &payload.to_string())
}

fn specs(r: &Repo) -> String {
    format!("{}/.specs", r.root)
}

#[test]
fn a_migration_waits_for_approval_once_and_does_not_stop_the_agent() {
    let r = repo(None);
    for command in ["dotnet ef database update", "dotnet ef migrations add Init", "npx prisma migrate deploy", "python manage.py migrate"] {
        let out = run(&r, "executor", command);
        assert_eq!(out.code, 2, "{command}: {}", out.stderr);
        assert!(out.stderr.starts_with("WAITING FOR THE USER'S APPROVAL [ask-command]:"), "{}", out.stderr);
        assert!(out.stderr.contains("carry on with the features and tasks that do not depend on it"), "{}", out.stderr);
        assert!(!out.stderr.contains("STATUS: NEEDS_CONFIRM"), "{}", out.stderr);
    }
    let again = run(&r, "executor", "dotnet ef database update");
    assert_eq!(again.code, 2);
    let requests = policy::read_requests(&specs(&r));
    assert_eq!(requests.len(), 4, "the same command is requested once: {requests:?}");
    assert!(requests.iter().all(|q| q["env"] == "DEV" && q["id"].as_str().unwrap().starts_with("ap-")));
    assert!(requests.iter().any(|q| q["class"] == "migrate_create" && q["what"] == "create a database migration"));
}

#[test]
fn a_request_never_carries_a_secret() {
    let r = repo(None);
    let out = run(&r, "executor", "DB_PASSWORD=hunter2 dotnet ef database update --connection 'Server=db;Password=hunter3'");
    assert_eq!(out.code, 2);
    let text = std::fs::read_to_string(format!("{}/project/loop/internal/approvals-requests.jsonl", specs(&r))).unwrap();
    assert!(!text.contains("hunter2") && !text.contains("hunter3"), "{text}");
    assert!(!out.stderr.contains("hunter2") && !out.stderr.contains("hunter3"), "{}", out.stderr);
}

#[test]
fn the_project_list_releases_standard_commands_without_asking() {
    let r = repo(Some(r#"{"allow": [], "commands": ["migrations", "npm run dev", "dev_run_x"]}"#));
    for command in ["dotnet ef database update", "alembic revision -m x", "npm run dev", "cd web && npm run dev -- --port 3000"] {
        let out = run(&r, "executor", command);
        assert_eq!(out.code, 0, "{command}: {}", out.stderr);
    }
    // a class that is not in the list still asks
    assert_eq!(run(&r, "verifier", "dotnet run --project Api").code, 2);
    assert!(policy::read_requests(&specs(&r)).len() == 1);
}

#[test]
fn the_dev_baseline_opt_in_lets_the_app_start_but_not_migrate() {
    let r = repo(Some(r#"{"e2e_dev_baseline": {"enabled": true}}"#));
    assert_eq!(run(&r, "verifier", "dotnet run --project Api").code, 0);
    assert_eq!(run(&r, "verifier", "npm run dev").code, 0);
    assert_eq!(run(&r, "verifier", "dotnet ef database update").code, 2);
}

#[test]
fn the_users_answer_settles_the_request() {
    let r = repo(None);
    let command = "dotnet ef database update";
    assert_eq!(run(&r, "executor", command).code, 2);
    let id = policy::read_requests(&specs(&r))[0]["id"].as_str().unwrap().to_string();
    let file = format!("{}/project/loop/json/approvals.json", specs(&r));
    std::fs::create_dir_all(std::path::Path::new(&file).parent().unwrap()).unwrap();
    std::fs::write(&file, format!(r#"{{"items": {{"{id}": {{"decision": "approved"}}}}}}"#)).unwrap();
    assert_eq!(run(&r, "executor", command).code, 0);
    std::fs::write(&file, format!(r#"{{"items": {{"{id}": {{"decision": "denied"}}}}}}"#)).unwrap();
    let denied = run(&r, "executor", command);
    assert_eq!(denied.code, 2);
    assert!(denied.stderr.contains("you denied this request"), "{}", denied.stderr);
    assert_eq!(policy::read_requests(&specs(&r)).len(), 1);
}

#[test]
fn production_is_never_released_and_never_asked() {
    let r = repo(Some(r#"{"allow": ["migration-target"], "commands": ["migrations", "dev_run"], "e2e_dev_baseline": true}"#));
    for command in [
        "ASPNETCORE_ENVIRONMENT=Production dotnet ef database update",
        "dotnet ef database update --environment Production",
        "NODE_ENV=production npm run dev",
        "RAILS_ENV=production rails db:migrate",
        "dotnet run --project Api --environment prod",
    ] {
        let out = run(&r, "executor", command);
        assert_eq!(out.code, 2, "{command}: {}", out.stderr);
        assert!(out.stderr.starts_with("BLOCKED by the orchestrator guard [migration-target]:"), "{command}: {}", out.stderr);
        assert!(out.stderr.contains("run on DEV only"), "{}", out.stderr);
    }
    assert!(policy::read_requests(&specs(&r)).is_empty(), "no approval is asked for production");
}

#[test]
fn ordinary_commands_are_untouched() {
    let r = repo(None);
    for command in ["dotnet build", "dotnet test", "npm run build", "npm test", "git status", "ls"] {
        assert_eq!(run(&r, "executor", command).code, 0, "{command}");
    }
}
