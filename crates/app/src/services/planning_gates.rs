// SPDX-License-Identifier: MIT
//! Planning gates: what must be true before "Approve roadmap" is allowed (checked by the back end on approve and shown by the planning screen).
//! Pure functions over the facts `ProjectProgressService::read_project_facts` already reads.
//!
//! Order and ids are a contract with the front end: git, structure, subprojects (only for a detected monorepo or an unreadable SUBPROJECTS.json),
//! conformance, map, repos (only when an extra repo is not set up), goal (the PROJECT.md context gate), features, fields, open_questions, flows,
//! test_environment, ready (not optional; only when the facts carry `readiness`, the cached result of `readiness_service`). The first four are the
//! PREREQUISITES of the planning chat (it refuses to write without them).
//!
//! `facts` is a JSON object (the same dict the Python service passes around); `facts["features"]` is `{slug: {field: text}}` in ROADMAP order.

use super::App;
use crate::domain::app_settings;
use crate::domain::import_coverage::{check_coverage, parse_coverage};
use engine_state::delivery;
use engine_state::planning_env_validation;
use engine_state::roadmap_settings::{parse_loop_settings, ParsedSettings, DEFAULT_BASE_BRANCH, LOOP_SETTING_HEADER_NAMES};
use serde_json::{json, Map, Value};
use std::path::Path;

pub const GATE_GIT: &str = "git";
pub const GATE_STRUCTURE: &str = "structure";
/// Monorepo only: the list of subprojects must be confirmed before Conformance and the map.
pub const GATE_SUBPROJECTS: &str = "subprojects";
pub const GATE_CONFORMANCE: &str = "conformance";
pub const GATE_MAP: &str = "map";
/// Extra repos of a multi-repo project: each needs a Conformance line and (with code) a codebase map; not a prerequisite.
pub const GATE_REPOS: &str = "repos";
/// Id kept; it now checks the short PROJECT.md (the subagents' context: only the Vision is filled in).
pub const GATE_GOAL: &str = "goal";
pub const GATE_FEATURES: &str = "features";
pub const GATE_FIELDS: &str = "fields";
pub const GATE_OPEN_QUESTIONS: &str = "open_questions";
pub const GATE_FLOWS: &str = "flows";
pub const GATE_TEST_ENV: &str = "test_environment";
/// Only after a spec import was sent: every source item is in IMPORT-COVERAGE.md; not optional.
pub const GATE_IMPORT_COVERAGE: &str = "import_coverage";
pub const IMPORT_COVERAGE_MISSING: &str = "IMPORT-COVERAGE.md missing";
/// Last: the cached readiness check (`readiness_service`); only present when the facts carry `readiness`.
pub const GATE_READY: &str = "ready";
pub const PREREQUISITE_GATES: [&str; 5] = [GATE_GIT, GATE_STRUCTURE, GATE_SUBPROJECTS, GATE_CONFORMANCE, GATE_MAP];
const FILL_IN_MARK: &str = "FILL_IN";
const READINESS_NOT_RUN: &str = "readiness check did not run";
const REPOS_REJECTED_PARTS: [&str; 6] = [".specs", "project", "loop", "internal", "planning", "repos_rejected.json"];
pub const ITEMS_SHOWN: usize = 12;
const ITEM_TEXT_LIMIT: usize = 1200; // long enough for the uncommitted-files warning (12 names + "and N more")
const SUBPROJECTS_UNREADABLE: &str = "SUBPROJECTS.json unreadable";
const SUBPROJECTS_BLOCKED: &str = "blocked: confirm the list of subprojects first";
const MAP_JOB_TYPE: &str = "map";
/// What each document of `docs/codebase` (the brownfield map) is for, shown next to its name.
const MAP_DOCUMENT_PURPOSE: [(&str, &str); 7] = [
    ("STACK.md", "languages, frameworks, versions and main dependencies"),
    ("ARCHITECTURE.md", "layers, patterns and how data flows"),
    ("CONVENTIONS.md", "naming, code style and error handling in use"),
    ("STRUCTURE.md", "folder layout and where each kind of code lives"),
    ("TESTING.md", "how tests are written and run"),
    ("INTEGRATIONS.md", "external APIs, databases, queues and authentication"),
    ("CONCERNS.md", "tech debt, risks and fragile areas"),
];

pub fn t(value: Option<&Value>) -> bool {
    engine_state::notify_truthy(value)
}

fn fact<'a>(facts: &'a Value, key: &str) -> &'a Value {
    facts.get(key).unwrap_or(&Value::Null)
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => pycompat::json::py_json_dumps(other, false),
    }
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value.and_then(Value::as_array).map(|a| a.iter().map(|i| text_of(Some(i))).collect()).unwrap_or_default()
}

fn len_of(value: Option<&Value>) -> usize {
    match value {
        Some(Value::Array(a)) => a.len(),
        Some(Value::Object(o)) => o.len(),
        Some(Value::String(s)) => s.chars().count(),
        _ => 0,
    }
}

/// `N document(s) in docs/codebase, mapped at <sha>` (the folder is `.specs/codebase` for a project mapped before the move).
pub fn describe_map_documents(facts: &Value) -> String {
    let folder = Some(text_of(facts.get("map_folder"))).filter(|f| !f.is_empty()).unwrap_or_else(|| "docs/codebase".into());
    let detail = format!("{} document(s) in {folder}", len_of(facts.get("map_files")));
    if t(facts.get("map_commit")) {
        format!("{detail}, mapped at {}", text_of(facts.get("map_commit")))
    } else {
        detail
    }
}

/// One line per written map document: `NAME.md: what it is for`.
pub fn describe_map_files(map_files: &[String]) -> Vec<String> {
    map_files.iter().map(|name| MAP_DOCUMENT_PURPOSE.iter().find(|(n, _)| n == name).map(|(n, p)| format!("{n}: {p}")).unwrap_or_else(|| name.clone())).collect()
}

pub fn create_gate(gate_id: &str, label: &str, ok: bool, detail: Option<String>, items: &[String], optional: bool) -> Value {
    let shown: Vec<String> = items.iter().take(ITEMS_SHOWN).map(|i| i.chars().take(ITEM_TEXT_LIMIT).collect()).collect();
    json!({"id": gate_id, "label": label, "ok": ok, "optional": optional, "detail": detail, "items": shown})
}

pub fn list_fill_in_lines(text: &str) -> Vec<String> {
    pycompat::splitlines(text).iter().filter(|l| l.contains(FILL_IN_MARK)).map(|l| l.trim().to_string()).collect()
}

/// The planning gates in the contract order. `facts` = `read_project_facts(context)`.
pub fn build_planning_gates(app: &App, facts: &Value) -> Vec<Value> {
    let mut gates = build_code_prerequisite_gates(facts);
    gates.extend(build_repos_gates(facts)); // right after the map
    gates.extend(build_plan_gates(app, facts));
    gates.extend(build_import_coverage_gates(facts)); // only after a spec import was sent
    gates.extend(build_readiness_gates(app, facts));
    gates
}

/// `(detected, confirmed, active, error)` from `facts["subprojects"]` (`SubprojectsService::describe`): a detected monorepo, its confirmation, the
/// non-skipped subprojects and the read error of SUBPROJECTS.json. A file that cannot be read counts as a single project (not detected) and only the
/// `subprojects` gate reports it.
pub fn read_subproject_state(facts: &Value) -> (bool, bool, Vec<Value>, String) {
    let described = facts.get("subprojects").filter(|d| t(Some(d))).cloned().unwrap_or(json!({}));
    let detected = described.get("monorepo") == Some(&Value::Bool(true));
    let confirmed = detected && described.get("confirmed") == Some(&Value::Bool(true));
    let active: Vec<Value> = if detected {
        described.get("projects").and_then(Value::as_array).map(|p| p.iter().filter(|project| !t(project.get("skip"))).cloned().collect()).unwrap_or_default()
    } else {
        vec![]
    };
    (detected, confirmed, active, text_of(described.get("error")))
}

pub fn is_confirmed_monorepo(facts: &Value) -> bool {
    read_subproject_state(facts).1
}

pub fn describe_subproject_line(project: &Value) -> String {
    let stack = Some(text_of(project.get("stack"))).filter(|s| !s.is_empty()).unwrap_or_else(|| "unknown".into());
    format!("{} ({stack}){}", text_of(project.get("path")), if t(project.get("has_code")) { "" } else { ": no code yet" })
}

/// `[subprojects]` for a detected monorepo (fails until the list is confirmed) or an unreadable SUBPROJECTS.json, else `[]`.
pub fn build_subprojects_gates(facts: &Value) -> Vec<Value> {
    let (detected, confirmed, active, error) = read_subproject_state(facts);
    if !error.is_empty() {
        return vec![create_gate(GATE_SUBPROJECTS, "Subprojects", false, Some(SUBPROJECTS_UNREADABLE.into()), &[error], false)];
    }
    if !detected {
        return vec![];
    }
    let skipped: Vec<Value> =
        facts["subprojects"].get("projects").and_then(Value::as_array).map(|p| p.iter().filter(|project| t(project.get("skip"))).cloned().collect()).unwrap_or_default();
    let mut items: Vec<String> = active.iter().map(describe_subproject_line).collect();
    for project in &skipped {
        let reason = if t(project.get("reason")) { format!(" ({})", text_of(project.get("reason"))) } else { String::new() };
        items.push(format!("{}: skipped{reason}", text_of(project.get("path"))));
    }
    let detail = if confirmed {
        format!("{} subproject(s) confirmed", active.len())
    } else {
        format!("confirm the list of {} subproject(s) before Conformance and the map", active.len())
    };
    vec![create_gate(GATE_SUBPROJECTS, "Subprojects", confirmed, Some(detail), &items, false)]
}

/// Aggregate: one line per non-skipped subproject; ok when every one has its `Conformance:` line.
pub fn build_subproject_conformance_gate(facts: &Value) -> Value {
    let (_, confirmed, active, _) = read_subproject_state(facts);
    if !confirmed {
        return create_gate(GATE_CONFORMANCE, "Conformance per subproject", false, Some(SUBPROJECTS_BLOCKED.into()), &[], false);
    }
    let items: Vec<String> = active
        .iter()
        .map(|p| format!("{}: {}", text_of(p.get("path")), if t(p.get("conformance")) { text_of(p.get("conformance")) } else { "missing".into() }))
        .collect();
    let missing: Vec<&Value> = active.iter().filter(|p| !t(p.get("conformance"))).collect();
    let detail = if missing.is_empty() {
        "every subproject has a Conformance line".to_string()
    } else {
        format!("{} of {} subproject(s) with Conformance", active.len() - missing.len(), active.len())
    };
    create_gate(GATE_CONFORMANCE, "Conformance per subproject", missing.is_empty(), Some(detail), &items, false)
}

fn map_documents(project: &Value) -> Vec<Value> {
    project.get("map").and_then(|m| m.get("files")).and_then(Value::as_array).cloned().unwrap_or_default()
}

fn map_running(facts: &Value) -> bool {
    facts.get("running_job").is_some_and(|job| t(Some(job)) && job.get("type").and_then(Value::as_str) == Some(MAP_JOB_TYPE))
}

/// Aggregate: ok when every non-skipped subproject with code has a codebase map; optional when none has code.
pub fn build_subproject_map_gate(facts: &Value) -> Value {
    let (_, confirmed, active, _) = read_subproject_state(facts);
    if !confirmed {
        return create_gate(GATE_MAP, "Codebase map per subproject", false, Some(SUBPROJECTS_BLOCKED.into()), &[], false);
    }
    let running = map_running(facts);
    let with_code: Vec<&Value> = active.iter().filter(|p| t(p.get("has_code"))).collect();
    let items: Vec<String> = active
        .iter()
        .map(|p| {
            let documents = map_documents(p);
            let state = if !documents.is_empty() {
                format!("{} document(s)", documents.len())
            } else if t(p.get("has_code")) {
                "no map".into()
            } else {
                "no code".into()
            };
            format!("{}: {state}", text_of(p.get("path")))
        })
        .collect();
    let missing: Vec<&&Value> = with_code.iter().filter(|p| map_documents(p).is_empty()).collect();
    let mapped = active.iter().filter(|p| !map_documents(p).is_empty()).count();
    let system = facts["subprojects"].get("system");
    let system_pending = system.is_some_and(Value::is_object)
        && missing.is_empty()
        && mapped >= 2
        && !t(system.and_then(|s| s.get("exists")))
        && facts["subprojects"].get("map_queue").and_then(|q| q.get("system")).and_then(|s| s.get("state")).and_then(Value::as_str) != Some("done");
    let detail = if running {
        "mapping now".to_string()
    } else if system_pending {
        "the system map is not made yet".to_string()
    } else if !missing.is_empty() {
        format!("{} of {} subproject(s) with code are mapped", with_code.len() - missing.len(), with_code.len())
    } else if !with_code.is_empty() {
        "every subproject with code has a map".to_string()
    } else {
        "no subproject has code: nothing to map".to_string()
    };
    create_gate(GATE_MAP, "Codebase map per subproject", missing.is_empty() && !running && !system_pending, Some(detail), &items, with_code.is_empty() && !system_pending)
}

/// `[git, structure, (subprojects), conformance, map]`: the PREREQUISITES of the planning chat (see [`PREREQUISITE_GATES`]). For a detected
/// monorepo Conformance and the map are per subproject (the root Conformance and the root map are not asked for).
pub fn build_code_prerequisite_gates(facts: &Value) -> Vec<Value> {
    let structure = fact(facts, "structure");
    let has_code = t(facts.get("has_code"));
    let map_files = strings(facts.get("map_files"));
    let running = map_running(facts);
    let is_git = t(facts.get("is_git"));
    let mut gates = vec![
        create_gate(GATE_GIT, "Git repository", is_git, if is_git { None } else { Some("the folder is not a git repository (the loop needs git)".into()) }, &[], false),
        create_gate(
            GATE_STRUCTURE,
            ".specs/ structure",
            t(structure.get("ok")),
            if t(structure.get("ok")) { None } else { Some("initialize the .specs/ structure".into()) },
            &strings(structure.get("missing")),
            false,
        ),
    ];
    gates.extend(build_subprojects_gates(facts));
    if read_subproject_state(facts).0 {
        gates.extend([build_subproject_conformance_gate(facts), build_subproject_map_gate(facts)]);
        return gates;
    }
    let conformance = text_of(facts.get("conformance"));
    gates.push(create_gate(
        GATE_CONFORMANCE,
        "Conformance in CLAUDE.md",
        !conformance.is_empty(),
        Some(if conformance.is_empty() { "choose STANDARD or LEGACY".into() } else { format!("Conformance: {conformance}") }),
        &[],
        false,
    ));
    let detail = if running {
        format!("mapping now: {} document(s) written so far", map_files.len())
    } else if !map_files.is_empty() {
        describe_map_documents(facts)
    } else if has_code {
        "repo with code and no map: map it before planning".into()
    } else {
        "repo without code: nothing to map".into()
    };
    gates.push(create_gate(GATE_MAP, "Codebase map", !map_files.is_empty() && !running, Some(detail), &describe_map_files(&map_files), !has_code));
    gates
}

/// `[repos]` for every extra EDIT repository that is not set up yet (blocks), or, when only reference repositories lack a map, an optional passing
/// gate whose items are WARNINGs (a reference repository never blocks). `[]` when there is nothing to say.
pub fn build_repos_gates(facts: &Value) -> Vec<Value> {
    let unready: Vec<Value> = facts.get("repos_unready").and_then(Value::as_array).cloned().unwrap_or_default();
    let warnings: Vec<String> = strings(facts.get("repos_reference_warnings")).into_iter().map(|w| format!("warning: {w}")).collect();
    if unready.is_empty() {
        if warnings.is_empty() {
            return vec![];
        }
        return vec![create_gate(
            GATE_REPOS,
            "Other repositories set up",
            true,
            Some(format!("{} warning(s) about reference repositories", warnings.len())),
            &warnings,
            true,
        )];
    }
    let mut items: Vec<String> = unready.iter().map(|r| text_of(r.get("detail"))).collect();
    items.extend(warnings);
    vec![create_gate(
        GATE_REPOS,
        "Other repositories set up",
        false,
        Some(format!("{} repository(ies) without the minimum setup (Conformance + codebase map)", unready.len())),
        &items,
        false,
    )]
}

/// `[goal, features, fields, open_questions, flows, test_environment]`: the state of the planning documents.
pub fn build_plan_gates(app: &App, facts: &Value) -> Vec<Value> {
    let project_text = facts.get("project_text").and_then(Value::as_str);
    let fill_in_lines = list_fill_in_lines(project_text.unwrap_or(""));
    let features = fact(facts, "features");
    let parse_errors = strings(facts.get("roadmap_parse_errors"));
    let field_errors = strings(facts.get("roadmap_field_errors"));
    let questions = strings(facts.get("open_questions"));
    let coverage = fact(facts, "flow_coverage");
    let env_problems: Vec<String> = strings(facts.get("environment_all_problems")).iter().map(|p| friendly_environment_problem(p)).collect();
    let env_check = read_environment_validation(app, &text_of(facts.get("work_dir")));
    let check_problems: Vec<String> = env_check["problems"].as_array().map(|a| a.iter().map(|i| text_of(i.get("message"))).collect()).unwrap_or_default();
    let check_warnings: Vec<String> =
        env_check["warnings"].as_array().map(|a| a.iter().map(|i| format!("warning: {}", text_of(i.get("message")))).collect()).unwrap_or_default();
    let env_blocking = t(env_check.get("exists")) && !check_problems.is_empty() && !t(env_check.get("unfinished")); // ENV-01: only a written plan blocks
    let mut env_items = env_problems.clone();
    env_items.extend(check_problems.clone());
    env_items.extend(check_warnings);
    let mut env_all = env_problems.clone();
    env_all.extend(check_problems.clone());
    let roadmap_exists = t(facts.get("roadmap_exists"));
    let coverage_missing = strings(coverage.get("missing"));
    let flows_exists = t(facts.get("flows_exists"));
    vec![
        create_gate(
            GATE_GOAL,
            "Project context (PROJECT.md)",
            project_text.is_some_and(|p| !p.trim().is_empty()) && fill_in_lines.is_empty(),
            Some(if project_text.is_none() {
                "PROJECT.md missing".into()
            } else if !fill_in_lines.is_empty() {
                format!("{} line(s) still with FILL_IN", fill_in_lines.len())
            } else {
                "filled in".into()
            }),
            &fill_in_lines,
            false,
        ),
        create_gate(
            GATE_FEATURES,
            "Roadmap features",
            roadmap_exists && t(Some(features)) && parse_errors.is_empty(),
            Some(if !roadmap_exists {
                "ROADMAP.md missing".into()
            } else if !parse_errors.is_empty() {
                "outside the grammar".into()
            } else if t(Some(features)) {
                format!("{} feature(s)", len_of(Some(features)))
            } else {
                "ROADMAP.md has no feature line".into()
            }),
            &parse_errors,
            false,
        ),
        create_gate(
            GATE_FIELDS,
            "Feature fields",
            field_errors.is_empty(),
            if field_errors.is_empty() { None } else { Some(format!("{} error(s)", field_errors.len())) },
            &field_errors,
            false,
        ),
        create_gate(
            GATE_OPEN_QUESTIONS,
            "Open questions answered",
            questions.is_empty(),
            if questions.is_empty() { None } else { Some(format!("{} open question(s) in the ROADMAP", questions.len())) },
            &questions,
            false,
        ),
        create_gate(
            GATE_FLOWS,
            "A flow for every feature (FLOWS.md)",
            flows_exists && t(Some(features)) && coverage_missing.is_empty(),
            describe_flows(facts, coverage),
            &coverage_missing,
            false,
        ),
        create_gate(
            GATE_TEST_ENV,
            "Test environment (credentials and limits)",
            env_problems.is_empty() && check_problems.is_empty(),
            Some(describe_test_environment(&env_all, t(facts.get("regression_required")), env_blocking)),
            &env_items,
            !env_blocking, // blocks only a written TEST-ENVIRONMENT.md that ENV-01 refuses; else a regression feature makes the loop preflight (Check) require it
        ),
    ]
}

/// `loop_max_hours` of config.json (Settings), default 24: a test token that expires sooner than this is a problem (ENV-01).
pub fn read_loop_max_hours(app: &App) -> f64 {
    let definition = app_settings::find_definition(&json!("loop_max_hours")).ok();
    let stored = app.settings.read_all_or_empty();
    definition.map(|d| app_settings::read_stored_or_default(&d, &stored)).and_then(|v| v.as_f64()).unwrap_or(planning_env_validation::DEFAULT_LOOP_MAX_HOURS)
}

/// ENV-01 over the project: `{exists, unfinished, problems: [{code, name, message}], warnings: [...]}` (names only, never a value). `unfinished` =
/// the document still has FILL_IN lines (the untouched template): the plan is not written yet, so it does not block.
pub fn read_environment_validation(app: &App, work_dir: &str) -> Value {
    if work_dir.is_empty() {
        return json!({"exists": false, "unfinished": false, "problems": [], "warnings": []});
    }
    let mut result = planning_env_validation::validate_project(work_dir, &app.config.user_data_dir, None, read_loop_max_hours(app));
    let mut document = String::new();
    if t(result.get("exists")) {
        let path = planning_env_validation::DOC_RELATIVE.iter().fold(Path::new(work_dir).to_path_buf(), |a, p| a.join(p));
        document = cfg::env::read_text_sig(&path).unwrap_or_default();
    }
    result["unfinished"] = json!(!list_fill_in_lines(&document).is_empty());
    result
}

/// `[{covered, dropped, missing}]` of every SENT import (newest first) against `facts["import_coverage_text"]`; `[]` without one.
pub fn check_sent_imports_coverage(facts: &Value) -> Vec<Value> {
    let coverage = parse_coverage(&text_of(facts.get("import_coverage_text")));
    facts
        .get("imports")
        .and_then(Value::as_array)
        .map(|imports| {
            imports.iter().filter(|m| t(m.get("sent_at"))).map(|meta| check_coverage(meta.get("items").filter(|i| t(Some(i))).unwrap_or(&json!({})), &coverage)).collect()
        })
        .unwrap_or_default()
}

/// `[import_coverage]` when a spec import was sent (nothing otherwise); not optional: ok only when no source item is missing from
/// `.specs/project/IMPORT-COVERAGE.md`. Items: `missing: <id>` first, then `dropped: <item> - <reason>` (shown, never blocking).
pub fn build_import_coverage_gates(facts: &Value) -> Vec<Value> {
    let results = check_sent_imports_coverage(facts);
    if results.is_empty() {
        return vec![];
    }
    if text_of(facts.get("import_coverage_text")).trim().is_empty() {
        return vec![create_gate(
            GATE_IMPORT_COVERAGE,
            "Import coverage (IMPORT-COVERAGE.md)",
            false,
            Some(IMPORT_COVERAGE_MISSING.into()),
            &[IMPORT_COVERAGE_MISSING.into()],
            false,
        )];
    }
    let covered: usize = results.iter().map(|r| len_of(r.get("covered"))).sum();
    let dropped: Vec<&Value> = results.iter().flat_map(|r| r.get("dropped").and_then(Value::as_array).into_iter().flatten()).collect();
    let missing: Vec<String> = results.iter().flat_map(|r| strings(r.get("missing"))).collect();
    let mut items: Vec<String> = missing.iter().map(|m| format!("missing: {m}")).collect();
    for entry in &dropped {
        let reason = if t(entry.get("reason")) { text_of(entry.get("reason")) } else { "no reason given".into() };
        items.push(format!("dropped: {} \u{2014} {reason}", text_of(entry.get("item"))));
    }
    let detail = format!("{covered} covered, {} dropped, {} missing", dropped.len(), missing.len());
    vec![create_gate(GATE_IMPORT_COVERAGE, "Import coverage (IMPORT-COVERAGE.md)", missing.is_empty(), Some(detail), &items, false)]
}

/// `[ready]` only when the facts carry the cached readiness result, else `[]`. Invalid loop settings of the ROADMAP header are added as `warning:`
/// items (never a problem: the loop starts with the default of each one).
pub fn build_readiness_gates(_app: &App, facts: &Value) -> Vec<Value> {
    let Some(readiness) = facts.get("readiness").filter(|r| !r.is_null()) else { return vec![] };
    let mut gate = create_readiness_gate(readiness);
    let mut items = strings(gate.get("items"));
    items.extend(describe_loop_settings_problems(&read_roadmap_loop_settings(&text_of(facts.get("work_dir")))).into_iter().map(|i| format!("warning: {i}")));
    gate["items"] = json!(items);
    vec![gate]
}

/// `parse_loop_settings` of `<work_dir>/.specs/project/ROADMAP.md` (a missing file = every default, no problem).
pub fn read_roadmap_loop_settings(work_dir: &str) -> ParsedSettings {
    match std::fs::read(Path::new(work_dir).join(".specs").join("project").join("ROADMAP.md")) {
        Ok(bytes) => parse_loop_settings(&String::from_utf8_lossy(&bytes)),
        Err(_) => parse_loop_settings(""),
    }
}

pub fn describe_loop_settings_problems(loop_settings: &ParsedSettings) -> Vec<String> {
    loop_settings
        .problems
        .iter()
        .map(|item| format!("ROADMAP setting \"{}: {}\" is invalid ({})", text_of(item.get("name")), text_of(item.get("value")), text_of(item.get("message"))))
        .collect()
}

pub fn format_loop_setting_value(key: &str, value: &Value) -> String {
    match value {
        Value::Bool(b) => (if *b { "yes" } else { "no" }).into(),
        _ if key == "branch" => {
            if t(Some(value)) {
                text_of(Some(value))
            } else {
                "automatic".into()
            }
        }
        _ if key == "setup" => {
            if t(Some(value)) {
                text_of(Some(value))
            } else {
                "(none)".into()
            }
        }
        Value::Number(n) if key == "budget_turn" || key == "budget_loop" => {
            let f = n.as_f64().unwrap_or(0.0);
            format!("US$ {}", if f.fract() == 0.0 { format!("{}", f as i64) } else { py_float_text(f) })
        }
        other => text_of(Some(other)),
    }
}

/// Python `str(float)` for the amounts shown (shortest round-trip text, `1e-05` style not needed here).
fn py_float_text(value: f64) -> String {
    let text = format!("{value}");
    if text.contains('.') {
        text
    } else {
        format!("{text}.0")
    }
}

/// `{"name", "source": "roadmap|env|default"}`: the ROADMAP `Branch base` line (valid), then LOOP_PR_BASE / LOOP_INTEGRATION_BRANCH, then
/// `developer`. Same order as the engine; the panel payload adds it as `base_branch_resolved`.
pub fn resolve_base_branch(app: &App, loop_settings: &ParsedSettings) -> Value {
    let invalid: Vec<String> = loop_settings.problems.iter().map(|i| text_of(i.get("name"))).collect();
    let header = LOOP_SETTING_HEADER_NAMES.iter().find(|(k, _)| *k == "base_branch").map(|(_, n)| *n).unwrap_or("");
    if !loop_settings.missing.iter().any(|m| m == "base_branch") && !invalid.iter().any(|n| n == header) {
        return json!({"name": loop_settings.settings.get("base_branch").cloned().unwrap_or(Value::Null), "source": "roadmap"});
    }
    let name = get_environment_integration_branch(app);
    let source = if name == DEFAULT_BASE_BRANCH { "default" } else { "env" };
    json!({"name": name, "source": source})
}

/// Base/integration branch (where new worktrees start and the PR target): the ROADMAP `**Branch base:**` first, then LOOP_PR_BASE /
/// LOOP_INTEGRATION_BRANCH, then `developer`. The one helper every payload uses (same order as the engine's delivery).
pub fn get_integration_branch(app: &App, roadmap_text: &str) -> String {
    let from_roadmap = engine_state::roadmap_settings::read_roadmap_base_branch(roadmap_text);
    if from_roadmap.is_empty() {
        get_environment_integration_branch(app)
    } else {
        from_roadmap
    }
}

/// LOOP_PR_BASE / LOOP_INTEGRATION_BRANCH (environment, then the app .env), else the default.
pub fn get_environment_integration_branch(app: &App) -> String {
    let mut env = app.env.clone();
    delivery::integration_branch(&mut env, "")
}

/// The loop settings as the loop will read them (the ROADMAP header; a missing line shows the default), plus one WARNING per invalid value. The AI
/// sets them in the ROADMAP header with the exact names; the Start dialog only shows them.
pub fn describe_loop_settings_for_ai(app: &App, loop_settings: &ParsedSettings) -> Vec<String> {
    let invalid: Vec<String> = loop_settings.problems.iter().map(|i| text_of(i.get("name"))).collect();
    let resolved = resolve_base_branch(app, loop_settings);
    let mut settings: Map<String, Value> = loop_settings.settings.clone();
    let base_header = LOOP_SETTING_HEADER_NAMES.iter().find(|(k, _)| *k == "base_branch").map(|(_, n)| *n).unwrap_or("");
    if loop_settings.missing.iter().any(|m| m == "base_branch") || invalid.iter().any(|n| n == base_header) {
        settings.insert("base_branch".into(), json!(get_environment_integration_branch(app)));
        // no usable line: the environment, then developer (as the engine resolves it)
    }
    let mut lines =
        vec!["- [OK] loop_settings: decided here in planning (ROADMAP header lines `**Name:** value`), read by the loop at Start; a missing line = the default"
            .to_string()];
    for (key, header_name) in LOOP_SETTING_HEADER_NAMES {
        let mark = if invalid.iter().any(|n| n == header_name) { " (invalid in the ROADMAP: the default is used)" } else { "" };
        lines.push(format!("    * {header_name}: {}{mark}", format_loop_setting_value(key, settings.get(key).unwrap_or(&Value::Null))));
    }
    lines.push(format!("    * base_branch_resolved: {} (source: {})", text_of(resolved.get("name")), text_of(resolved.get("source"))));
    lines.push("    * Branch base: by default the loop branches from `developer`. To change it, write `Branch base: main` (or `release/2.0`) in the ROADMAP header. Order: ROADMAP, then LOOP_INTEGRATION_BRANCH, then developer. The PR goes to the same branch.".to_string());
    lines.push("    * Max parallel tasks: executors per wave inside one feature (1..5, default 3)".to_string());
    lines.extend(describe_loop_settings_problems(loop_settings).into_iter().map(|text| format!("- [WARNING] loop_settings: {text}")));
    lines
}

/// The `ready` gate from a readiness result (`{state, at, problems, warnings, error?}`); its reason is the first problem, "readiness check did not
/// run" or "stale".
pub fn create_readiness_gate(readiness: &Value) -> Value {
    let state = text_of(readiness.get("state"));
    let problems = strings(readiness.get("problems"));
    let warnings = strings(readiness.get("warnings"));
    let detail = if state == "ready" {
        format!("ready at {}{}", text_of(readiness.get("at")), if warnings.is_empty() { String::new() } else { format!("; {} warning(s)", warnings.len()) })
    } else if state == "problems" && !problems.is_empty() {
        format!("{}{}", problems[0], if problems.len() > 1 { format!(" (+{} more)", problems.len() - 1) } else { String::new() })
    } else if state == "running" {
        "the readiness check is running".to_string()
    } else if state == "error" {
        format!("{READINESS_NOT_RUN}{}", if t(readiness.get("error")) { format!(": {}", text_of(readiness.get("error"))) } else { String::new() })
    } else if t(readiness.get("at")) {
        "stale: the plan changed since the last readiness check (run it again)".to_string()
    } else {
        "stale: the readiness check has not run yet".to_string()
    };
    let mut items = problems;
    items.extend(warnings.into_iter().map(|w| format!("warning: {w}")));
    create_gate(GATE_READY, "Ready to run the loop", state == "ready", Some(detail), &items, false)
}

/// The engine preflight words its messages for the regression/e2e case; here the file may simply not exist yet.
pub fn friendly_environment_problem(problem: &str) -> String {
    if problem.contains("is missing") {
        return "TEST-ENVIRONMENT.md does not exist yet: initialize the .specs/ structure (or send a message in the chat) and the app creates it from its template"
            .into();
    }
    problem.to_string()
}

/// Why the test environment passes or not. `blocking` (ENV-01: the document is written and the checks refuse it) stops the approval; otherwise it
/// never blocks, and with a regression feature the loop preflight (Check) refuses to start without it. Without it the loop only validates by build
/// and unit tests, never by running the real app.
pub fn describe_test_environment(problems: &[String], required: bool, blocking: bool) -> String {
    if problems.is_empty() {
        return "complete: where it runs, credential NAMES, test users and what is forbidden are written".into();
    }
    let more = if problems.len() > 1 { format!(" (+{} more)", problems.len() - 1) } else { String::new() };
    if blocking {
        return format!("fix before approving the roadmap: {}{more}", problems[0]);
    }
    let prefix = if required {
        "needed before the loop starts (a feature asks for regression or the ROADMAP says E2E: yes): "
    } else {
        "optional (no regression feature and E2E: no): "
    };
    let tail = if required { " (values: this project's .env.test; the Test environment tab can import names from your machine files)" } else { "" };
    format!("{prefix}{}{tail}", problems[0])
}

pub fn describe_flows(facts: &Value, coverage: &Value) -> Option<String> {
    if !t(facts.get("flows_exists")) {
        return Some("FLOWS.md missing".into());
    }
    let mut parts: Vec<String> = Vec::new();
    let missing = strings(coverage.get("missing"));
    if !missing.is_empty() {
        parts.push(format!("{} feature(s) without a flow of 2+ steps", missing.len()));
    } else if t(facts.get("features")) {
        parts.push("every feature has a flow".into());
    } else {
        parts.push("no feature in the ROADMAP yet".into());
    }
    let orphans = strings(coverage.get("orphans"));
    if !orphans.is_empty() {
        parts.push(format!("warning: flows of unknown features: {}", orphans.iter().take(ITEMS_SHOWN).cloned().collect::<Vec<_>>().join(", ")));
    }
    Some(parts.join("; ")).filter(|p| !p.is_empty())
}

pub fn are_prerequisites_ok(gates: &[Value]) -> bool {
    gates.iter().filter(|g| PREREQUISITE_GATES.contains(&g["id"].as_str().unwrap_or(""))).all(|g| g["ok"] == true || g["optional"] == true)
}

pub fn are_gates_ok(gates: &[Value]) -> bool {
    gates.iter().all(|g| g["ok"] == true || g["optional"] == true)
}

/// `[{path, reason}]` of the last repository suggestions the app refused (`repos_rejected.json`); tolerant: anything odd = none.
pub fn load_rejected_repos(work_dir: &str) -> Vec<Value> {
    let path = REPOS_REJECTED_PARTS.iter().fold(Path::new(work_dir).to_path_buf(), |a, p| a.join(p));
    let Some(mut data) = std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return vec![] };
    if data.is_object() {
        data = data.get("rejected").cloned().unwrap_or(Value::Null);
    }
    data.as_array()
        .map(|list| {
            list.iter().filter(|i| i.is_object() && t(i.get("path"))).map(|i| json!({"path": text_of(i.get("path")), "reason": text_of(i.get("reason"))})).collect()
        })
        .unwrap_or_default()
}

const INVENTORY_PARTS: [&str; 5] = [".specs", "project", "loop", "internal", "inventory"];
const SYSTEM_DOCUMENT_PARTS: [&str; 3] = ["docs", "codebase", "SYSTEM.md"];
const GIANT_FILES_SHOWN: usize = 5;
const GIANT_INSTRUCTION: &str = "Giant files (past the line limit) are risky to change: when a feature would touch one, add a `refactor-*` open question with a recommended answer (split the file first); when the user declines, write `note=giant-accepted` on that feature.";

/// Giant files of the last root inventory (`_root.json`): `(count, [{path, lines}] top 5)`. `(0, [])` when there is no inventory.
pub fn read_root_giants(work_dir: &str) -> (usize, Vec<Value>) {
    if work_dir.is_empty() {
        return (0, vec![]);
    }
    let path = INVENTORY_PARTS.iter().fold(Path::new(work_dir).to_path_buf(), |a, p| a.join(p)).join("_root.json");
    let Some(document) = std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return (0, vec![]) };
    let giants: Vec<&Value> =
        document.get("giant").and_then(Value::as_array).map(|g| g.iter().filter(|i| i.is_object() && t(i.get("path"))).collect()).unwrap_or_default();
    (giants.len(), giants.iter().take(GIANT_FILES_SHOWN).map(|i| json!({"path": i["path"], "lines": i.get("lines").cloned().unwrap_or(json!(0))})).collect())
}

pub fn format_giant_files(giants: &[Value]) -> String {
    giants.iter().take(GIANT_FILES_SHOWN).map(|i| format!("{} ({} lines)", text_of(i.get("path")), text_of(i.get("lines")))).collect::<Vec<_>>().join(", ")
}

/// Lines for a single project: its giant files from the last root inventory (none when there is no inventory).
pub fn describe_root_giants_for_ai(work_dir: &str) -> Vec<String> {
    let (count, top) = read_root_giants(work_dir);
    if count == 0 {
        return vec![];
    }
    vec!["[Giant files]".into(), format!("- {count} giant file(s): {}", format_giant_files(&top)), GIANT_INSTRUCTION.into()]
}

/// Lines of the planning status block for a detected monorepo (path, stack, Conformance, map) and the rule for the ROADMAP `paths=`; for a single
/// project only its giant files.
pub fn describe_subprojects_for_ai(described: Option<&Value>, work_dir: &str) -> Vec<String> {
    let Some(described) = described.filter(|d| d.get("monorepo") == Some(&Value::Bool(true))) else { return describe_root_giants_for_ai(work_dir) };
    let confirmed = described.get("confirmed") == Some(&Value::Bool(true));
    let projects: Vec<Value> = described.get("projects").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut lines = vec![format!("[Subprojects of this monorepo{}]", if confirmed { "" } else { " (list NOT confirmed yet: the user confirms it in the app)" })];
    for project in &projects {
        if t(project.get("skip")) {
            lines.push(format!(
                "- {}: skipped{}",
                text_of(project.get("path")),
                if t(project.get("reason")) { format!(" ({})", text_of(project.get("reason"))) } else { String::new() }
            ));
            continue;
        }
        let files = map_documents(project);
        let map_state = if !files.is_empty() {
            format!("map: {} document(s)", files.len())
        } else if t(project.get("has_code")) {
            "map: missing".into()
        } else {
            "map: no code".into()
        };
        let stack = Some(text_of(project.get("stack"))).filter(|s| !s.is_empty()).unwrap_or_else(|| "unknown".into());
        let conformance = Some(text_of(project.get("conformance"))).filter(|s| !s.is_empty()).unwrap_or_else(|| "missing".into());
        lines.push(format!("- {} (stack {stack}): Conformance {conformance}, {map_state}", text_of(project.get("path"))));
        let giant_count = project.get("giant_files").and_then(Value::as_i64).unwrap_or(0);
        if giant_count != 0 {
            lines.push(format!("    giant files: {giant_count} - {}", format_giant_files(&project.get("giant").and_then(Value::as_array).cloned().unwrap_or_default())));
        }
    }
    if confirmed {
        let example = projects.iter().find(|p| !t(p.get("skip"))).map(|p| text_of(p.get("path"))).unwrap_or_else(|| "backend".into());
        lines.push(format!("In the ROADMAP write every feature's `paths=` with the subproject folder as the prefix (for example `{example}/...`), never a path relative to the repository root without it."));
    }
    if !work_dir.is_empty() {
        let exists = SYSTEM_DOCUMENT_PARTS.iter().fold(Path::new(work_dir).to_path_buf(), |a, p| a.join(p)).is_file();
        lines.push(format!("docs/codebase/SYSTEM.md (system map): {}", if exists { "exists" } else { "missing" }));
    }
    if projects.iter().any(|p| p.get("giant_files").and_then(Value::as_i64).unwrap_or(0) != 0) {
        lines.push(GIANT_INSTRUCTION.into());
    }
    lines
}

/// Plain-text status block sent to the planning AI before every user message: what passes and what is missing. With `work_dir` (or
/// `rejected_repos`) it also lists the repository suggestions the app refused, so the AI fixes them. `loop_settings` (the result of
/// `parse_loop_settings`) adds the loop settings block with a WARNING per invalid value.
pub fn describe_gates_for_ai(
    app: &App,
    gates: &[Value],
    work_dir: &str,
    rejected_repos: Option<&[Value]>,
    subprojects: Option<&Value>,
    loop_settings: Option<&ParsedSettings>,
) -> String {
    let mut lines = vec!["[Planning status from the app, read just now: the source of truth, newer than your memory. Fix what is MISSING in the planning documents; do not ask the user for what is already OK.]".to_string()];
    for gate in gates {
        let mark = if gate["ok"] == true {
            "OK"
        } else if gate["optional"] == true {
            "optional"
        } else {
            "MISSING"
        };
        let detail = if t(gate.get("detail")) { format!(" - {}", text_of(gate.get("detail"))) } else { String::new() };
        lines.push(format!("- [{mark}] {}: {}{detail}", text_of(gate.get("id")), text_of(gate.get("label"))));
        if gate["ok"] != true {
            lines.extend(strings(gate.get("items")).into_iter().map(|item| format!("    * {item}")));
        }
    }
    let rejected: Vec<Value> = match rejected_repos {
        Some(list) => list.to_vec(),
        None if !work_dir.is_empty() => load_rejected_repos(work_dir),
        None => vec![],
    };
    if !rejected.is_empty() {
        lines.push("- [MISSING] repos_rejected: the last repository suggestions were NOT applied; fix REPOS-SUGGESTED.json".into());
        lines.extend(rejected.iter().take(ITEMS_SHOWN).map(|item| format!("    * {}: {}", text_of(item.get("path")), text_of(item.get("reason")))));
    }
    if let Some(settings) = loop_settings {
        lines.extend(describe_loop_settings_for_ai(app, settings));
    }
    lines.extend(describe_subprojects_for_ai(subprojects, work_dir));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_facts() -> Value {
        json!({
            "work_dir": "", "folder_exists": true, "is_git": true, "has_specs": true, "has_code": true,
            "structure": {"ok": true, "missing": []}, "conformance": "STANDARD", "repos_unready": [], "repos_reference_warnings": [], "readiness": null,
            "map_files": ["STACK.md", "X.md"], "map_folder": "docs/codebase", "map_commit": "abc",
            "roadmap_exists": true, "features": {"a": {"status": "pending"}}, "roadmap_errors": [], "roadmap_parse_errors": [], "roadmap_field_errors": [],
            "open_questions": [], "project_text": "# P\n\nVision: x\n", "flows_exists": true, "flow_coverage": {"missing": [], "orphans": []},
            "environment_problems": [], "environment_all_problems": [], "regression_required": false, "running_job": null,
        })
    }

    #[test]
    fn a_complete_plan_passes_every_gate() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let gates = build_planning_gates(&app, &base_facts());
        let ids: Vec<&str> = gates.iter().map(|g| g["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["git", "structure", "conformance", "map", "goal", "features", "fields", "open_questions", "flows", "test_environment"]);
        assert!(are_gates_ok(&gates) && are_prerequisites_ok(&gates));
        assert_eq!(gates[3]["detail"], "2 document(s) in docs/codebase, mapped at abc");
        assert_eq!(gates[3]["items"], json!(["STACK.md: languages, frameworks, versions and main dependencies", "X.md"]));
        assert_eq!(gates[9]["optional"], true);
    }

    #[test]
    fn missing_pieces_are_reported_with_the_contract_wording() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let mut facts = base_facts();
        facts["is_git"] = json!(false);
        facts["conformance"] = Value::Null;
        facts["project_text"] = json!("Vision: FILL_IN\nrest");
        facts["roadmap_parse_errors"] = json!(["bad line"]);
        facts["open_questions"] = json!(["q1"]);
        facts["flow_coverage"] = json!({"missing": ["a"], "orphans": ["zz"]});
        facts["environment_all_problems"] = json!(["TEST-ENVIRONMENT.md is missing"]);
        facts["readiness"] = json!({"state": "problems", "problems": ["p1", "p2"], "warnings": ["w"], "at": "t"});
        facts["running_job"] = json!({"type": "map", "id": "j-1"});
        let gates = build_planning_gates(&app, &facts);
        let by_id = |id: &str| gates.iter().find(|g| g["id"] == id).unwrap().clone();
        assert_eq!(by_id("git")["detail"], "the folder is not a git repository (the loop needs git)");
        assert_eq!(by_id("conformance")["detail"], "choose STANDARD or LEGACY");
        assert_eq!((by_id("map")["ok"].clone(), by_id("map")["detail"].clone()), (json!(false), json!("mapping now: 2 document(s) written so far")));
        assert_eq!(by_id("goal")["detail"], "1 line(s) still with FILL_IN");
        assert_eq!(
            (by_id("features")["detail"].clone(), by_id("open_questions")["detail"].clone()),
            (json!("outside the grammar"), json!("1 open question(s) in the ROADMAP"))
        );
        assert_eq!(by_id("flows")["detail"], "1 feature(s) without a flow of 2+ steps; warning: flows of unknown features: zz");
        assert!(by_id("test_environment")["detail"]
            .as_str()
            .unwrap()
            .starts_with("optional (no regression feature and E2E: no): TEST-ENVIRONMENT.md does not exist yet"));
        assert_eq!(by_id("ready")["detail"], "p1 (+1 more)");
        assert_eq!(by_id("ready")["items"], json!(["p1", "p2", "warning: w"]));
        assert!(!are_gates_ok(&gates) && !are_prerequisites_ok(&gates));
        let text = describe_gates_for_ai(&app, &gates, "", Some(&[json!({"path": "C:\\x", "reason": "no"})]), None, None);
        assert!(text.starts_with("[Planning status from the app, read just now") && text.contains("- [MISSING] git: Git repository - the folder"));
        assert!(
            text.contains("    * bad line")
                && text.contains("- [MISSING] repos_rejected:")
                && text.contains("    * C:\\x: no")
                && text.contains("- [optional] test_environment:")
        );
    }

    #[test]
    fn monorepo_gates_are_per_subproject() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        let mut facts = base_facts();
        facts["subprojects"] = json!({"monorepo": true, "confirmed": false, "projects": [{"path": "api", "stack": "dotnet", "has_code": true}, {"path": "old", "skip": true, "reason": "dead"}]});
        let gates = build_code_prerequisite_gates(&facts);
        let ids: Vec<&str> = gates.iter().map(|g| g["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["git", "structure", "subprojects", "conformance", "map"]);
        assert_eq!(gates[2]["items"], json!(["api (dotnet)", "old: skipped (dead)"]));
        assert_eq!(gates[3]["detail"], SUBPROJECTS_BLOCKED);
        facts["subprojects"]["confirmed"] = json!(true);
        facts["subprojects"]["projects"][0]["conformance"] = json!("STANDARD");
        facts["subprojects"]["projects"][0]["map"] = json!({"files": ["STACK.md"]});
        let gates = build_planning_gates(&app, &facts);
        let find = |id: &str| gates.iter().find(|g| g["id"] == id).unwrap().clone();
        assert_eq!(
            (find("conformance")["ok"].clone(), find("map")["ok"].clone(), find("map")["detail"].clone()),
            (json!(true), json!(true), json!("every subproject with code has a map"))
        );
        let described = describe_subprojects_for_ai(Some(&facts["subprojects"]), "");
        assert_eq!(described[1], "- api (stack dotnet): Conformance STANDARD, map: 1 document(s)");
        assert!(described.last().unwrap().starts_with("In the ROADMAP write every feature's `paths=`") || described.iter().any(|l| l.contains("`api/...`")));
        facts["subprojects"] = json!({"monorepo": false, "error": "broken"});
        assert!(build_subprojects_gates(&facts)[0]["items"][0] == "broken");
    }

    #[test]
    fn readiness_import_coverage_and_settings_text() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_tests(dir.path());
        assert_eq!(create_readiness_gate(&json!({"state": "ready", "at": "t", "warnings": ["w"]}))["detail"], "ready at t; 1 warning(s)");
        assert_eq!(create_readiness_gate(&json!({"state": "stale", "at": "t"}))["detail"], "stale: the plan changed since the last readiness check (run it again)");
        assert_eq!(create_readiness_gate(&json!({"state": "error", "error": "boom"}))["detail"], "readiness check did not run: boom");
        let mut facts = base_facts();
        facts["imports"] = json!([{"sent_at": "t", "items": {"files": []}}]);
        assert_eq!(build_import_coverage_gates(&facts)[0]["detail"], IMPORT_COVERAGE_MISSING);
        let settings = parse_loop_settings("**Dispatch:** 4\n**Branch base:** release/2.0\n**Max spend per turn:** 7.5\n");
        let lines = describe_loop_settings_for_ai(&app, &settings);
        assert!(lines.contains(&"    * Dispatch: 4".to_string()) && lines.contains(&"    * Branch base: release/2.0".to_string()));
        assert!(
            lines.contains(&"    * Max spend per turn: US$ 7.5".to_string()) && lines.contains(&"    * base_branch_resolved: release/2.0 (source: roadmap)".to_string())
        );
        assert_eq!(format_loop_setting_value("budget_loop", &json!(50.0)), "US$ 50");
        assert_eq!((format_loop_setting_value("e2e", &json!(true)), format_loop_setting_value("branch", &json!(""))), ("yes".to_string(), "automatic".to_string()));
        assert_eq!(resolve_base_branch(&app, &parse_loop_settings(""))["source"], "default");
        std::fs::create_dir_all(dir.path().join(".specs/project/loop/internal/planning")).unwrap();
        std::fs::write(dir.path().join(".specs/project/loop/internal/planning/repos_rejected.json"), r#"{"rejected": [{"path": "p", "reason": "r"}, {"nope": 1}]}"#)
            .unwrap();
        assert_eq!(load_rejected_repos(&dir.path().display().to_string()), vec![json!({"path": "p", "reason": "r"})]);
    }
}
