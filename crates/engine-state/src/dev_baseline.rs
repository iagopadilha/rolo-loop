// SPDX-License-Identifier: MIT
//! The "granted baseline" generator: on request (never silently) it writes a starting `NETWORK.json` and a `TEST-ENVIRONMENT.md` skeleton for an
//! E2E run. The localhost forms are allowed, every host the USER lists as development is allowed, and every host the user lists as production
//! is left denied. The hosts come only from the request: nothing is invented, guessed or read from the code.
//!
//! [`plan`] only looks and reports (a dry run); [`write`] changes files, keeps a backup of the network list it replaces, never overwrites an
//! existing `TEST-ENVIRONMENT.md`, and journals the grant in `PERMISSIONS-LOG.json`.

use crate::access_grant::{self, AccessContext, KIND_BASELINE, OUTCOME_GRANTED, OUTCOME_REFUSED};
use crate::network_policy;
use egress_policy::{host_matches, parse_deny, parse_entry, read_project_entries, LOCAL_ENTRIES};
use serde_json::{json, Value};
use std::path::Path;

pub const TEST_ENVIRONMENT_RELATIVE: [&str; 3] = [".specs", "project", "TEST-ENVIRONMENT.md"];

/// What the user supplies: network entries (`dev.example.com GET,POST`, the default METHODS are `GET,HEAD`) and production host patterns.
#[derive(Clone, Debug, Default)]
pub struct BaselineRequest {
    pub dev_hosts: Vec<String>,
    pub production_hosts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BaselinePlan {
    /// The whole list `NETWORK.json` will have: what it holds now, then the localhost forms, then the development hosts (no duplicates).
    pub allow: Vec<String>,
    /// The entries this baseline adds to what the file holds now.
    pub added: Vec<String>,
    /// Production hosts left denied (host patterns, as the user wrote them).
    pub production_hosts: Vec<String>,
    /// The skeleton to write, or `None` when `TEST-ENVIRONMENT.md` already exists (it is never overwritten).
    pub test_environment: Option<String>,
    /// Why nothing can be written. Empty = ready.
    pub problems: Vec<String>,
}

fn overlaps(a: &str, b: &str) -> bool {
    host_matches(a, b) || host_matches(b, a)
}

fn canonical(text: &str) -> Option<String> {
    parse_entry(&json!(text), "project").ok().map(|e| network_policy::format_entry(&e))
}

/// Looks at the project and the request and reports what a write would do. Changes nothing.
pub fn plan(repo: &str, data_dir: &Path, request: &BaselineRequest) -> BaselinePlan {
    let mut problems: Vec<String> = Vec::new();
    let mut production: Vec<String> = Vec::new();
    for text in request.production_hosts.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        match parse_deny(&json!(text)) {
            Ok(host) => {
                if !production.contains(&host) {
                    production.push(host);
                }
            }
            Err(error) => problems.push(format!("production host '{text}': {error}")),
        }
    }
    let (current, error) = read_project_entries(Path::new(repo));
    if !error.is_empty() {
        problems.push(format!("{} is unusable ({error}); fix or delete it first", network_policy::NETWORK_LABEL));
    }
    let global_deny = network_policy::load(repo, data_dir, None).deny;
    let mut allow: Vec<String> = Vec::new();
    let mut known: Vec<String> = Vec::new();
    let add = |text: String, allow: &mut Vec<String>, known: &mut Vec<String>| -> bool {
        let Some(key) = canonical(&text) else { return false };
        if known.contains(&key) {
            return false;
        }
        known.push(key);
        allow.push(text);
        true
    };
    for value in &current {
        let text = value.as_str().map(String::from).unwrap_or_else(|| value.to_string());
        if let Ok(entry) = parse_entry(value, "project") {
            if let Some(host) = production.iter().find(|p| overlaps(p, &entry.host)) {
                problems.push(format!("the list already holds '{}', which reaches the production host {host}: remove it first", entry.text));
            }
        }
        add(text, &mut allow, &mut known);
    }
    let before = allow.len();
    for text in LOCAL_ENTRIES {
        add(text.to_string(), &mut allow, &mut known);
    }
    for (index, text) in request.dev_hosts.iter().map(|t| t.trim()).enumerate().filter(|(_, t)| !t.is_empty()) {
        match parse_entry(&json!(text), "project") {
            Err(error) => problems.push(format!("development host {} ('{text}'): {error}", index + 1)),
            Ok(entry) => {
                if let Some(host) = production.iter().find(|p| overlaps(p, &entry.host)) {
                    problems.push(format!("'{text}' is listed as development and reaches the production host {host}"));
                } else if let Some(pattern) = global_deny.iter().find(|p| overlaps(p, &entry.host)) {
                    problems.push(format!("'{text}' is on the never-allow list (egress_never {pattern})"));
                } else {
                    add(entry.text, &mut allow, &mut known);
                }
            }
        }
    }
    let added: Vec<String> = allow[before.min(allow.len())..].to_vec();
    let existing_doc = TEST_ENVIRONMENT_RELATIVE.iter().fold(std::path::PathBuf::from(repo), |acc, part| acc.join(part)).is_file();
    let dev_listed: Vec<String> = allow.iter().filter(|t| !LOCAL_ENTRIES.contains(&t.as_str())).cloned().collect();
    let test_environment = (!existing_doc).then(|| skeleton(&dev_listed, &production));
    BaselinePlan { allow, added, production_hosts: production, test_environment, problems }
}

/// The `TEST-ENVIRONMENT.md` skeleton: the template's sections, the environment rules, the hosts the user listed and the call forms the guard accepts.
/// The `FILL_IN` lines stay on purpose: the preflight refuses the file until the user has filled them, so nothing is assumed.
pub fn skeleton(dev_hosts: &[String], production_hosts: &[String]) -> String {
    let bullets = |items: &[String], none: &str| -> String {
        if items.is_empty() {
            format!("- {none}\n")
        } else {
            items.iter().map(|i| format!("- `{i}`\n")).collect::<String>()
        }
    };
    let mut out = String::new();
    out.push_str("# Test environment (DEV)\n\n");
    out.push_str(
        "FILL_IN: 2 to 3 lines: what is tested and against which app or API (DEV and localhost only). Written by `harness baseline`; fill the FILL_IN lines.\n\n",
    );
    out.push_str("## Environment (fixed)\n");
    out.push_str("- Every app starts in the DEV environment, set explicitly in the command or the launch profile; a default is never trusted. An app whose log says production is stopped and reported.\n");
    out.push_str("- `localhost` in every form is allowed. Only the hosts below are reachable besides it.\n\n");
    out.push_str("### Development hosts (allowed, project network list)\n");
    out.push_str(&bullets(dev_hosts, "none listed yet: ask the user, do not guess one"));
    out.push_str("\n### Production hosts (always denied)\n");
    out.push_str(&bullets(production_hosts, "none listed: any host that is not above stays denied"));
    out.push_str(
        "\n## Where it runs\n| Item | Variable | Note |\n|------|----------|------|\n| Local app | `$LOCAL_URL` | FILL_IN: port and the command that starts it |\n\n",
    );
    out.push_str("## Login (to get the access token)\n| User | Password | Token comes from |\n|------|----------|------------------|\n| `$TEST_USER_1_EMAIL` | `$TEST_USER_1_PASSWORD` | FILL_IN: the login route or page |\n\n");
    out.push_str("## Allowed\n- Read routes (GET) and screens of the app under test.\n- Create test data whose name starts with `E2E_`.\n\n");
    out.push_str("## Forbidden\n- Delete anything, or change user permissions.\n- Send e-mail or WhatsApp messages.\n- Switch company or tenant.\n- Reach a host that is not in the project's network list.\n\n");
    out.push_str(&guard_core::call_forms::render_block());
    out
}

/// What [`write`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    pub network_written: bool,
    pub network_backup: String,
    pub test_environment_written: bool,
    pub message: String,
}

/// Writes the baseline of `plan` (made with [`plan`] just before) and journals it. `Err` lists the problems; nothing is written then, and the
/// refusal is journaled.
pub fn write(ctx: &AccessContext, plan: &BaselinePlan, reason: &str) -> Result<Written, String> {
    let summary = format!("{} development entries added, {} production hosts left denied", plan.added.len(), plan.production_hosts.len());
    let refuse = |why: String| -> Result<Written, String> {
        let _ = access_grant::record(ctx.work, KIND_BASELINE, &summary, reason, ctx.who, OUTCOME_REFUSED, &why, "");
        Err(why)
    };
    if reason.trim().is_empty() {
        return refuse("a reason is required: say why this baseline is needed".into());
    }
    if !plan.problems.is_empty() {
        return refuse(plan.problems.join("; "));
    }
    let entries: Vec<Value> = plan.allow.iter().map(|t| json!(t)).collect();
    let current: Vec<String> = read_project_entries(Path::new(ctx.repo)).0.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let network_changed = !plan.added.is_empty() || current.len() != plan.allow.len();
    let mut backup = String::new();
    if network_changed {
        let network = Path::new(ctx.repo).join(".specs").join("project").join("NETWORK.json");
        backup = match access_grant::backup_file(ctx.work, &network) {
            Ok(path) => path,
            Err(error) => return refuse(error),
        };
        if let Err(error) = network_policy::write_project(ctx.repo, &entries) {
            return refuse(error.to_string());
        }
    }
    let mut doc_written = false;
    if let Some(text) = &plan.test_environment {
        let target = TEST_ENVIRONMENT_RELATIVE.iter().fold(std::path::PathBuf::from(ctx.repo), |acc, part| acc.join(part));
        if target.is_file() {
            // appeared since the plan: never overwrite
        } else if let Err(error) = crate::loop_io::atomic_write_text(&target, text) {
            return refuse(format!("could not write {} ({:?})", TEST_ENVIRONMENT_RELATIVE.join("/"), error.kind()));
        } else {
            doc_written = true;
        }
    }
    if network_changed && !crate::setup_files::same_folder(ctx.repo, ctx.work) && Path::new(ctx.work).is_dir() {
        crate::setup_files::refresh_loop_inputs(ctx.repo, ctx.work, &ctx.data_dir.display().to_string(), ctx.toolkit_root);
    }
    let message = format!(
        "{summary}; NETWORK.json {}; TEST-ENVIRONMENT.md {}",
        if network_changed { "updated" } else { "already had them" },
        if doc_written { "written" } else { "kept as it was" }
    );
    let _ = access_grant::record(ctx.work, KIND_BASELINE, &summary, reason, ctx.who, OUTCOME_GRANTED, &message, &backup);
    Ok(Written { network_written: network_changed, network_backup: backup, test_environment_written: doc_written, message })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        repo: String,
        data: std::path::PathBuf,
    }

    fn fixture(network: Option<&str>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".specs/project")).unwrap();
        if let Some(n) = network {
            std::fs::write(repo.join(".specs/project/NETWORK.json"), n).unwrap();
        }
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        Fixture { repo: repo.display().to_string(), data, _dir: dir }
    }

    fn request(dev: &[&str], production: &[&str]) -> BaselineRequest {
        BaselineRequest { dev_hosts: dev.iter().map(|s| s.to_string()).collect(), production_hosts: production.iter().map(|s| s.to_string()).collect() }
    }

    fn context(f: &Fixture) -> AccessContext<'_> {
        AccessContext { repo: &f.repo, work: &f.repo, data_dir: &f.data, toolkit_root: "", who: "tester" }
    }

    #[test]
    fn a_plan_is_a_dry_run_and_lists_only_what_the_user_gave() {
        let f = fixture(None);
        let p = plan(&f.repo, &f.data, &request(&["dev.example.com GET,POST"], &["prod.example.com", "*.live.example.com"]));
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        assert!(p.allow.contains(&"localhost * cred".to_string()) && p.allow.contains(&"dev.example.com GET,POST".to_string()));
        assert!(p.allow.iter().all(|e| !e.contains("prod.example.com") && !e.contains("live.example.com")));
        assert_eq!(p.production_hosts, vec!["prod.example.com", "*.live.example.com"]);
        assert!(!Path::new(&f.repo).join(".specs/project/NETWORK.json").exists(), "a plan must not write");
        let doc = p.test_environment.unwrap();
        assert!(doc.contains("`dev.example.com GET,POST`") && doc.contains("`prod.example.com`") && doc.contains("FILL_IN"));
        assert!(doc.contains("How the E2E must call things"));
    }

    #[test]
    fn with_no_hosts_only_localhost_is_allowed_and_nothing_is_invented() {
        let f = fixture(None);
        let p = plan(&f.repo, &f.data, &BaselineRequest::default());
        assert!(p.problems.is_empty());
        assert_eq!(p.allow, LOCAL_ENTRIES.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(p.test_environment.unwrap().contains("none listed yet: ask the user, do not guess one"));
    }

    #[test]
    fn writing_updates_the_list_keeps_a_backup_and_journals() {
        let f = fixture(Some(r#"{"version": 1, "allow": ["tools.example.com"]}"#));
        let p = plan(&f.repo, &f.data, &request(&["dev.example.com POST"], &["prod.example.com"]));
        let written = write(&context(&f), &p, "E2E needs the development settings host").unwrap();
        assert!(written.network_written && written.test_environment_written, "{}", written.message);
        let network: Value = serde_json::from_str(&std::fs::read_to_string(Path::new(&f.repo).join(".specs/project/NETWORK.json")).unwrap()).unwrap();
        let allow: Vec<&str> = network["allow"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert_eq!(allow[0], "tools.example.com");
        assert!(allow.contains(&"dev.example.com POST") && !allow.iter().any(|e| e.contains("prod")), "{allow:?}");
        let saved = std::fs::read_to_string(Path::new(&f.repo).join(&written.network_backup)).unwrap();
        assert!(saved.contains("tools.example.com") && !saved.contains("dev.example.com"));
        let journal = access_grant::read_journal(&f.repo);
        assert_eq!((journal[0]["kind"].as_str(), journal[0]["outcome"].as_str()), (Some("baseline"), Some("granted")));
        // a second run changes nothing and never overwrites the document
        std::fs::write(Path::new(&f.repo).join(".specs/project/TEST-ENVIRONMENT.md"), "mine").unwrap();
        let again = write(&context(&f), &plan(&f.repo, &f.data, &request(&["dev.example.com POST"], &["prod.example.com"])), "again").unwrap();
        assert!(!again.network_written && !again.test_environment_written);
        assert_eq!(std::fs::read_to_string(Path::new(&f.repo).join(".specs/project/TEST-ENVIRONMENT.md")).unwrap(), "mine");
    }

    #[test]
    fn production_must_stay_denied_in_every_way() {
        let f = fixture(Some(r#"{"allow": ["api.live.example.com"]}"#));
        // a development entry that reaches a production host, directly or by wildcard
        for dev in ["prod.example.com", "*.example.com", "svc.live.example.com"] {
            let p = plan(&f.repo, &f.data, &request(&[dev], &["prod.example.com", "*.live.example.com"]));
            assert!(p.problems.iter().any(|m| m.contains("production host")), "{dev}: {:?}", p.problems);
        }
        // an entry the list already holds that reaches production
        let p = plan(&f.repo, &f.data, &request(&[], &["*.live.example.com"]));
        assert!(p.problems.iter().any(|m| m.contains("api.live.example.com") && m.contains("remove it first")), "{:?}", p.problems);
        let before = std::fs::read_to_string(Path::new(&f.repo).join(".specs/project/NETWORK.json")).unwrap();
        let err = write(&context(&f), &p, "needed").unwrap_err();
        assert!(err.contains("production host"));
        assert_eq!(std::fs::read_to_string(Path::new(&f.repo).join(".specs/project/NETWORK.json")).unwrap(), before);
        assert_eq!(access_grant::read_journal(&f.repo)[0]["outcome"], "refused");
    }

    #[test]
    fn bad_input_and_a_missing_reason_are_refused() {
        let f = fixture(None);
        let p = plan(&f.repo, &f.data, &request(&["https://dev.example.com", "dev two"], &["prod.example.com:443", ""]));
        assert!(p.problems.len() >= 3, "{:?}", p.problems);
        assert!(write(&context(&f), &p, "x").is_err());
        let ok = plan(&f.repo, &f.data, &request(&["dev.example.com"], &[]));
        assert!(write(&context(&f), &ok, "  ").unwrap_err().contains("reason is required"));
        assert!(!Path::new(&f.repo).join(".specs/project/NETWORK.json").exists());
        let broken = fixture(Some("{ nope"));
        assert!(plan(&broken.repo, &broken.data, &BaselineRequest::default()).problems[0].contains("unusable"));
    }

    #[test]
    fn the_never_allow_list_of_the_config_is_respected() {
        let f = fixture(None);
        std::fs::write(f.data.join("config.json"), r#"{"egress_never": ["blocked.example.com"]}"#).unwrap();
        let p = plan(&f.repo, &f.data, &request(&["blocked.example.com"], &[]));
        assert!(p.problems.iter().any(|m| m.contains("never-allow")), "{:?}", p.problems);
    }
}
