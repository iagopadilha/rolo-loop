// SPDX-License-Identifier: MIT
//! M1: a map job of a repo that is not the project's primary reads the confirmed subprojects from the primary's `.specs`
//! (`LOOP_PRIMARY_SPECS`), so nothing has to be copied into the other repo.

use cfg::Env;
use guard_core::paths::G;

struct Fixture {
    _dir: tempfile::TempDir,
    primary: String,
    other: String,
    data: String,
    home: String,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let primary = dir.path().join("primary");
    let other = dir.path().join("other");
    for repo in [&primary, &other] {
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
    }
    std::fs::write(
        primary.join(".specs/project/SUBPROJECTS.json"),
        r#"{"repos": {"other": {"monorepo": true, "confirmed": true, "projects": [{"path": "svc"}, {"path": "skipped", "skip": true}]}}}"#,
    )
    .unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    Fixture {
        primary: primary.display().to_string(),
        other: other.display().to_string(),
        data: data.display().to_string(),
        home: dir.path().display().to_string(),
        _dir: dir,
    }
}

fn write_outcome(f: &Fixture, with_primary: bool, relative: &str) -> i32 {
    let mut pairs = vec![("LOOP_DATA_DIR", f.data.clone()), ("USERPROFILE", f.home.clone()), ("HOME", f.home.clone())];
    if with_primary {
        pairs.push(("LOOP_PRIMARY_SPECS", format!("{}/.specs", f.primary)));
    }
    let g = G::new(Env::from_pairs(pairs));
    let payload = serde_json::json!({"tool_name": "Write", "tool_input": {"file_path": format!("{}/{relative}", f.other), "content": "x"}, "cwd": f.other});
    guard_core::run::run(&g, "spec", &payload.to_string()).code
}

#[test]
fn the_primary_specs_path_lets_a_map_job_write_the_subproject_docs() {
    let f = fixture();
    assert_eq!(write_outcome(&f, false, "svc/docs/codebase/STACK.md"), 2, "without the export the folder is unknown");
    assert_eq!(write_outcome(&f, true, "svc/docs/codebase/STACK.md"), 0);
    assert_eq!(write_outcome(&f, true, "docs/codebase/STACK.md"), 0);
}

#[test]
fn the_primary_specs_path_opens_nothing_else() {
    let f = fixture();
    assert_eq!(write_outcome(&f, true, "svc/src/app.rs"), 2);
    assert_eq!(write_outcome(&f, true, "skipped/docs/codebase/STACK.md"), 2);
    assert_eq!(write_outcome(&f, true, "other/docs/codebase/STACK.md"), 2);
}
