// SPDX-License-Identifier: MIT
//! Routes of the projects hub (`/api/app/projects*`, `/api/app/system/folders`), of the project checklist (`progress`, `structure/initialize`,
//! `conformance`), the roadmap and the claude jobs, the `.specs/` view, planning, the test environment, the network list and the subprojects. Only
//! translates HTTP <-> service.

use super::{route, route_with_limit};
use crate::api::responses::{context_of, project_to_dict, respond_converting_errors, respond_json};
use crate::api::router::{Request, Response, Router};
use crate::services::App;
use serde_json::{json, Value};
use std::sync::Arc;

const JOB_ACCEPTED_STATUS: u16 = 202;
const WRITE_FILE_BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024; // a large spec fits; the service still limits the text to 1 M characters
const JOB_TYPE_SUBPROJECTS: &str = "subprojects";

pub fn register(router: &mut Router, app: &Arc<App>) {
    // ProjectController
    route(router, app, "GET", "/api/app/projects", list_projects);
    route(router, app, "POST", "/api/app/projects/open", open_project);
    route(router, app, "POST", "/api/app/projects/create", create_project);
    route(router, app, "POST", "/api/app/projects/pick-folder", pick_folder);
    route(router, app, "POST", "/api/app/projects/{project_id}/forget", forget_project);
    route(router, app, "POST", "/api/app/projects/{project_id}/reset-specs", reset_specs);
    route(router, app, "GET", "/api/app/projects/{project_id}/reset-specs/preview", preview_reset_specs);
    route(router, app, "GET", "/api/app/system/folders", list_folders);
    route(router, app, "GET", "/p/{project_id}/api/progress", get_progress);
    route(router, app, "POST", "/p/{project_id}/api/structure/initialize", initialize_structure);
    route(router, app, "POST", "/p/{project_id}/api/conformance", write_conformance);
    // RoadmapController
    route(router, app, "GET", "/p/{project_id}/api/roadmap", get_roadmap);
    route(router, app, "POST", "/p/{project_id}/api/roadmap/approve", approve_roadmap);
    route(router, app, "POST", "/p/{project_id}/api/roadmap/generate", generate_roadmap);
    route(router, app, "POST", "/p/{project_id}/api/roadmap/refine", refine_roadmap);
    route(router, app, "POST", "/p/{project_id}/api/codebase/map", map_codebase);
    route(router, app, "GET", "/p/{project_id}/api/jobs", list_jobs);
    route(router, app, "GET", "/p/{project_id}/api/jobs/{job_id}", get_job);
    // SpecsController
    route(router, app, "GET", "/p/{project_id}/api/specs/tree", get_specs_tree);
    route(router, app, "GET", "/p/{project_id}/api/specs/file", read_spec_file);
    route_with_limit(router, app, "PUT", "/p/{project_id}/api/specs/file", write_spec_file, WRITE_FILE_BODY_LIMIT_BYTES);
    // PlanningController
    route(router, app, "GET", "/p/{project_id}/api/planning", get_planning);
    route(router, app, "POST", "/p/{project_id}/api/planning/send", send_planning_message);
    route(router, app, "GET", "/p/{project_id}/api/planning/job/{job_id}", get_planning_job);
    route(router, app, "POST", "/p/{project_id}/api/planning/reset", reset_planning);
    route(router, app, "POST", "/p/{project_id}/api/planning/import/preview", preview_import);
    route(router, app, "POST", "/p/{project_id}/api/planning/import", import_spec);
    route(router, app, "GET", "/p/{project_id}/api/planning/imports", list_imports);
    route(router, app, "POST", "/p/{project_id}/api/planning/import/{import_id}/send", send_import);
    route(router, app, "GET", "/p/{project_id}/api/planning/readiness", get_readiness);
    route(router, app, "POST", "/p/{project_id}/api/planning/readiness/run", run_readiness);
    // TestEnvironmentController
    route(router, app, "GET", "/p/{project_id}/api/test-environment", get_test_environment);
    route(router, app, "PUT", "/p/{project_id}/api/test-environment/values", save_test_values);
    route(router, app, "GET", "/p/{project_id}/api/test-environment/sources", list_test_sources);
    route(router, app, "POST", "/p/{project_id}/api/test-environment/import", import_test_names);
    // NetworkController
    route(router, app, "GET", "/p/{project_id}/api/network", get_network);
    route(router, app, "PUT", "/p/{project_id}/api/network", save_network);
    route(router, app, "POST", "/p/{project_id}/api/network/suggestion", answer_network_suggestion);
    // SubprojectsController
    let base = "/p/{project_id}/api/subprojects";
    route(router, app, "GET", base, describe_subprojects);
    route(router, app, "PUT", base, update_subprojects);
    route(router, app, "POST", &format!("{base}/detect"), detect_subprojects);
    route(router, app, "POST", &format!("{base}/validate"), validate_subprojects);
    route(router, app, "POST", &format!("{base}/conformance"), set_subproject_conformance);
    route(router, app, "GET", &format!("{base}/standards"), get_subproject_standards);
    route(router, app, "PUT", &format!("{base}/standards"), set_subproject_standards);
    route(router, app, "POST", &format!("{base}/map"), map_subprojects);
    route(router, app, "POST", &format!("{base}/system-map"), map_system);
}

// ------------------------------------------------ hub
fn list_projects(app: &App, _: &Request) -> Response {
    let items: Vec<Value> = app
        .project_service()
        .list_projects()
        .iter()
        .map(|project| {
            let mut item = project_to_dict(project);
            item["folder_exists"] = json!(std::path::Path::new(&project.repo_path).is_dir());
            item["progress_summary"] = match app.project_service().get_project_context(&project.id) {
                Ok(context) => app.progress().build_progress_summary(&context),
                Err(_) => Value::Null, // one unreadable project does not take the list down
            };
            item
        })
        .collect();
    Response::json(200, json!({"projects": items}))
}

fn build_project_response(project: &crate::domain::project::Project) -> Response {
    let data = project_to_dict(project);
    Response::json(200, json!({"ok": true, "project": data, "url": data["url"]}))
}

fn open_project(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| app.project_service().open_existing_project(request.text("path")).map(|p| build_project_response(&p)))
}

fn create_project(app: &App, request: &Request) -> Response {
    let conformance = request.opt("conformance").and_then(Value::as_str).filter(|c| !c.is_empty());
    let init_git = request.field("init_git") != Some(&Value::Bool(false));
    respond_converting_errors(|| app.project_service().create_project_from_scratch(request.text("parent_path"), request.text("name"), init_git, conformance).map(|p| build_project_response(&p)))
}

fn pick_folder(app: &App, request: &Request) -> Response {
    Response::json(200, app.folder_picker().pick_folder_with_dialog(request.field("initial").and_then(Value::as_str)))
}

fn forget_project(app: &App, request: &Request) -> Response {
    respond_json(|| app.project_service().forget_project(request.param("project_id")).map(|_| json!({"ok": true})))
}

fn reset_specs(app: &App, request: &Request) -> Response {
    respond_json(|| app.specs_reset().reset_specs(&context_of(app, request)?, &request.value("confirm")))
}

fn preview_reset_specs(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.specs_reset().preview_reset(&context_of(app, request)?)))
}

fn list_folders(app: &App, request: &Request) -> Response {
    respond_json(|| app.folder_picker().list_subfolders(Some(&request.get_query("path"))))
}

fn get_progress(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.progress().build_progress(&context_of(app, request)?)))
}

fn initialize_structure(app: &App, request: &Request) -> Response {
    respond_json(|| {
        let conformance = request.opt("conformance").and_then(Value::as_str).filter(|c| !c.is_empty());
        let mut result = app.specs_structure().initialize_project_structure(&context_of(app, request)?, conformance)?;
        let context = context_of(app, request)?; // registers the MONITOR.json now that .specs/ exists
        result["ok"] = json!(true);
        result["progress"] = app.progress().build_progress(&context);
        Ok(result)
    })
}

fn write_conformance(app: &App, request: &Request) -> Response {
    respond_json(|| app.specs_structure().write_project_conformance(&context_of(app, request)?, request.text("value")).map(|value| json!({"ok": true, "conformance": value})))
}

// ------------------------------------------------ roadmap and jobs
fn get_roadmap(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.roadmap().get_roadmap(&context_of(app, request)?)))
}

fn approve_roadmap(app: &App, request: &Request) -> Response {
    respond_json(|| app.roadmap().approve_roadmap(&context_of(app, request)?, &request.value("confirm")).map(|approval| json!({"ok": true, "approval": approval})))
}

fn respond_job(job: Value) -> Response {
    Response::json(JOB_ACCEPTED_STATUS, json!({"ok": true, "job": job["id"], "detail": job}))
}

fn generate_roadmap(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| {
        let map_first = request.field("map_first") == Some(&Value::Bool(true));
        app.roadmap().generate_roadmap(&context_of(app, request)?, &request.value("goal"), map_first, request.field("budget_usd")).map(respond_job)
    })
}

fn refine_roadmap(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| app.roadmap().refine_roadmap(&context_of(app, request)?, &request.value("text"), request.field("budget_usd")).map(respond_job))
}

fn map_codebase(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| app.roadmap().map_codebase(&context_of(app, request)?).map(respond_job))
}

fn list_jobs(app: &App, request: &Request) -> Response {
    respond_json(|| {
        let context = context_of(app, request)?;
        let calls = app.call_log.list_calls(&[context.repo_path().to_string(), context.work_dir()], 50);
        Ok(json!({"jobs": app.jobs().list_jobs(context.repo_path()), "calls": calls}))
    })
}

fn get_job(app: &App, request: &Request) -> Response {
    respond_json(|| app.jobs().get_job(context_of(app, request)?.repo_path(), request.param("job_id")))
}

// ------------------------------------------------ specs
fn get_specs_tree(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.spec_files().build_specs_tree(&context_of(app, request)?)))
}

fn read_spec_file(app: &App, request: &Request) -> Response {
    respond_json(|| app.spec_files().read_spec_file(&context_of(app, request)?, &request.get_query("path")))
}

fn write_spec_file(app: &App, request: &Request) -> Response {
    respond_json(|| {
        let mut result = app.spec_files().write_spec_file(&context_of(app, request)?, request.text("path"), &request.value("content"), &request.value("version"))?;
        result["ok"] = json!(true);
        Ok(result)
    })
}

// ------------------------------------------------ planning
fn get_planning(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.planning().get_planning(&context_of(app, request)?)))
}

fn send_planning_message(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| app.planning().send_message(&context_of(app, request)?, &request.value("text"), None).map(|job| Response::json(JOB_ACCEPTED_STATUS, job)))
}

fn get_planning_job(app: &App, request: &Request) -> Response {
    respond_json(|| app.planning().get_planning_job(&context_of(app, request)?, request.param("job_id")))
}

fn reset_planning(app: &App, request: &Request) -> Response {
    respond_json(|| app.planning().reset_conversation(&context_of(app, request)?, &request.value("confirm")))
}

fn preview_import(app: &App, request: &Request) -> Response {
    respond_json(|| app.planning_imports().preview_import(&context_of(app, request)?, &request.body))
}

fn import_spec(app: &App, request: &Request) -> Response {
    respond_json(|| app.planning_imports().import_spec(&context_of(app, request)?, &request.body))
}

fn list_imports(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.planning_imports().list_imports(&context_of(app, request)?)))
}

fn send_import(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| app.planning_imports().send_import(&context_of(app, request)?, request.param("import_id")).map(|job| Response::json(JOB_ACCEPTED_STATUS, job)))
}

fn get_readiness(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.readiness().get(&context_of(app, request)?)))
}

/// Starts the check in the background (202) and returns the live state (`running`); poll `GET planning/readiness`.
fn run_readiness(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| Ok(Response::json(JOB_ACCEPTED_STATUS, app.readiness().run(&context_of(app, request)?, false))))
}

// ------------------------------------------------ test environment
fn get_test_environment(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.test_environment().get_test_environment(&context_of(app, request)?)))
}

fn save_test_values(app: &App, request: &Request) -> Response {
    let target = request.field("target").map_or("project", |t| t.as_str().unwrap_or(""));
    respond_json(|| app.test_environment().save_values(&context_of(app, request)?, &request.value("values"), target))
}

fn list_test_sources(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.test_environment().list_sources(&context_of(app, request)?)))
}

fn import_test_names(app: &App, request: &Request) -> Response {
    let replace = request.field("replace").cloned().unwrap_or(json!(false));
    respond_json(|| app.test_environment().import_names(&context_of(app, request)?, &request.value("names"), &replace, &request.value("confirm")))
}

// ------------------------------------------------ network
fn get_network(app: &App, request: &Request) -> Response {
    respond_json(|| Ok(app.network().get_network(&context_of(app, request)?)))
}

fn save_network(app: &App, request: &Request) -> Response {
    respond_json(|| app.network().save_network(&context_of(app, request)?, &request.value("allow"), &request.value("confirm")))
}

fn answer_network_suggestion(app: &App, request: &Request) -> Response {
    respond_json(|| app.network().answer_suggestion(&context_of(app, request)?, &request.value("action"), &request.value("confirm")))
}

// ------------------------------------------------ subprojects
fn repo_name(request: &Request) -> Option<String> {
    Some(request.get_query("repo")).filter(|r| !r.is_empty())
}

/// The running validation job, else the last one of type `subprojects` (`null` when there is none).
fn find_validation_job(app: &App, repo_path: &str) -> Value {
    let jobs: Vec<Value> = app.jobs().list_jobs(repo_path).into_iter().filter(|j| j["type"] == JOB_TYPE_SUBPROJECTS).collect();
    jobs.iter().find(|j| j["state"] == "running").or_else(|| jobs.first()).cloned().unwrap_or(Value::Null)
}

fn describe_subprojects(app: &App, request: &Request) -> Response {
    respond_json(|| {
        let context = context_of(app, request)?;
        let repo = repo_name(request);
        let mut description = app.subprojects().describe(&context, repo.as_deref())?;
        description["system"] = app.system_maps().status(&context, repo.as_deref())?;
        description["validation_job"] = find_validation_job(app, context.repo_path());
        Ok(description)
    })
}

fn detect_subprojects(app: &App, request: &Request) -> Response {
    respond_json(|| app.subprojects().detect_again(&context_of(app, request)?, repo_name(request).as_deref(), request.field("force") == Some(&Value::Bool(true))))
}

fn update_subprojects(app: &App, request: &Request) -> Response {
    respond_json(|| app.subprojects().update_list(&context_of(app, request)?, &request.body, repo_name(request).as_deref()))
}

fn validate_subprojects(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| {
        let job = app.subproject_validation().start(&context_of(app, request)?, repo_name(request).as_deref())?;
        Ok(Response::json(JOB_ACCEPTED_STATUS, json!({"ok": true, "job": job})))
    })
}

fn set_subproject_conformance(app: &App, request: &Request) -> Response {
    respond_json(|| {
        let (context, repo) = (context_of(app, request)?, repo_name(request));
        if request.field("all") == Some(&Value::Bool(true)) {
            return app.subprojects().set_conformance_all(&context, repo.as_deref());
        }
        app.subprojects().set_conformance(&context, request.text("path"), request.text("value"), repo.as_deref())
    })
}

fn get_subproject_standards(app: &App, request: &Request) -> Response {
    respond_json(|| app.standards().get_subproject(&context_of(app, request)?, &request.get_query("path"), repo_name(request).as_deref()))
}

fn set_subproject_standards(app: &App, request: &Request) -> Response {
    respond_json(|| app.standards().put_subproject(&context_of(app, request)?, request.text("path"), &request.value("bindings"), repo_name(request).as_deref()))
}

fn map_subprojects(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| {
        let started = app.subproject_maps().start(&context_of(app, request)?, request.field("paths"), repo_name(request).as_deref())?;
        Ok(Response::json(JOB_ACCEPTED_STATUS, json!({"ok": true, "job": started["job"], "queue": started["queue"]})))
    })
}

fn map_system(app: &App, request: &Request) -> Response {
    respond_converting_errors(|| {
        let started = app.system_maps().start(&context_of(app, request)?, repo_name(request).as_deref())?;
        Ok(Response::json(JOB_ACCEPTED_STATUS, json!({"ok": true, "job": started["job"]})))
    })
}

