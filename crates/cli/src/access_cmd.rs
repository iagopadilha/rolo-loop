// SPDX-License-Identifier: MIT
//! `harness doctor --access`: what a `claude -p` of this app can actually reach (a report, never a gate; exit 1 only when the run-time layer self
//! test failed).

use cfg::Env;
use engine_state::access_report::{build_report, format_report, PROBE_TIMEOUT_SECONDS};
use engine_state::app_ctx::AppCtx;
use engine_state::runtime_isolation::project_root_of;

pub fn run(env: &Env, project: &str, probe: bool, selftest: bool, json: bool) -> i32 {
    let ctx = AppCtx::discover(env);
    let folder = if project.is_empty() { std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default() } else { project.to_string() };
    let root = project_root_of(&folder);
    let report = build_report(env, &ctx, &root, probe, selftest, PROBE_TIMEOUT_SECONDS);
    if json {
        println!("{}", pycompat::json::py_json_dumps_indent(&report, false, 2));
    } else {
        println!("{}", format_report(&report));
    }
    if report["runtime_self_test"]["ok"] == false { 1 } else { 0 }
}
