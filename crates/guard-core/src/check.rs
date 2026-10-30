// SPDX-License-Identifier: MIT
//! The rule checks (port of `check_bash`, `check_tool` and friends of `guard.py`).

use crate::egress::egress_upload_reason;
use crate::net::{self, Ctx};
use crate::paths::{inside_any, normalize_path, path_forms, unresolved_short_name, G};
use crate::pyre::{nt_dirname, Re};
use crate::rules::*;
use crate::scope::*;
use crate::shell::{gh_api_writes, gh_calls_in, git_calls_in, shell_event_variants, Event, GhCall, GitCall};
use crate::sql::{sql_statement_writes, sql_statements, SQL_UNPARSEABLE};
use crate::state::{current_branch, log, py_json_dumps, split_commands, truthy, value_text, Permissions};
use serde_json::Value;
use std::sync::LazyLock;

pub use crate::scope::Hits;

fn hit(rule: &str, why: impl Into<String>) -> (String, String) {
    (rule.to_string(), why.into())
}

pub fn is_headless(g: &G) -> bool {
    g.env.get("LOOP_HEADLESS").as_deref() == Some("1")
}

pub fn isolated_or_headless(g: &G) -> bool {
    is_headless(g) || g.env.get("LOOP_ISOLATED").as_deref() == Some("1")
}

fn prod_hints(g: &G) -> Vec<String> {
    let raw = g.env.get("GUARD_PROD_HINTS").unwrap_or_else(|| "prod,production".to_string());
    raw.split(',').map(|i| i.trim().to_lowercase()).filter(|i| !i.is_empty()).collect()
}

fn prod_hosts(g: &G) -> Vec<String> {
    let mut hosts = prod_hints(g);
    hosts.extend(g.env.text("GUARD_PROD_HOSTS").split(',').map(|i| i.trim().to_lowercase()).filter(|i| !i.is_empty()));
    hosts
}

/// One check run: the environment, the protected-branch regexes, the user's releases and the lazily built network context.
pub struct Check<'a> {
    pub g: &'a G,
    pub prot: Protected,
    pub perm: Permissions,
    pub net: Option<Ctx<'a>>,
    /// Commands refused because they wait for the user's approval (the caller writes one request for each).
    pub requests: Vec<crate::policy::Request>,
}

impl<'a> Check<'a> {
    pub fn new(g: &'a G, perm: Permissions) -> Check<'a> {
        Check { g, prot: Protected::new(g), perm, net: None, requests: Vec::new() }
    }

    fn cwd(&self) -> Option<&str> {
        self.perm.cwd.as_deref().filter(|c| !c.is_empty())
    }

    fn specs(&self) -> Option<&str> {
        self.perm.specs_dir.as_deref().filter(|c| !c.is_empty())
    }

    fn policy(&self) -> mcp_policy::Policy {
        mcp_policy::read_policy(&self.g.env.user_data_dir())
    }

    fn net_ctx(&mut self) -> &mut Ctx<'a> {
        if self.net.is_none() {
            let (specs, cwd) = (self.perm.specs_dir.clone().filter(|s| !s.is_empty()), self.perm.cwd.clone().filter(|c| !c.is_empty()));
            let cwd_or_here = || cwd.clone().unwrap_or_else(|| std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default());
            let root = match &specs {
                Some(s) => nt_dirname(s),
                None => cwd_or_here(),
            };
            let roots = match &specs {
                Some(s) => project_roots(self.g, s),
                None => cwd.as_deref().map(|c| vec![self.g.norm(c)]).unwrap_or_default(),
            };
            self.net = Some(Ctx::new(self.g, &root, roots, Some(cwd.as_deref().unwrap_or(&root))));
        }
        self.net.as_mut().expect("set above")
    }

    fn network_rules(&self, rules: &[String]) -> Vec<&'static str> {
        if isolated_or_headless(self.g) {
            net::RULES.iter().copied().filter(|r| has_rule(rules, r)).collect()
        } else {
            vec![]
        }
    }

    // ---- protected files --------------------------------------------------------------------------------------------------------

    /// The rules about files nobody (or only the orchestrator) writes, for ONE write target.
    pub fn protected_target_hits(&self, target: &str, rules: &[String], cwd: Option<&str>, state_message: &str) -> Hits {
        let mut hits = Vec::new();
        let forms = path_forms(self.g, target, cwd);
        let matches = |p: &Re| forms.iter().any(|f| p.is_match(f));
        if has_rule(rules, "approval-write") && matches(&APPROVAL_FILE) {
            hits.push(hit("approval-write", "ROADMAP-APPROVED.json is written only by the user's click in the panel"));
        }
        if has_rule(rules, "permissions-write") && matches(&PERMISSIONS_FILE) {
            hits.push(hit("permissions-write", PERMISSIONS_MSG));
        }
        if has_rule(rules, "state-write") && matches(&STATE_FILE) {
            hits.push(hit("state-write", state_message));
        }
        if has_rule(rules, "engine-file-write") {
            if matches(&ENGINE_FILE_RE) {
                hits.push(hit("engine-file-write", ENGINE_FILE_MSG));
            } else if !has_rule(rules, "network-suggest") && matches(&NETWORK_SUGGESTED_RE) {
                hits.push(hit("engine-file-write", NETWORK_SUGGESTED_MSG));
            } else if forms.len() > 1 && unresolved_short_name(self.g, forms.last().map(String::as_str).unwrap_or(""), self.specs()) {
                hits.push(hit("engine-file-write", SHORT_NAME_MSG));
            }
        }
        hits
    }

    // ---- git and gh -----------------------------------------------------------------------------------------------------------------

    fn git_call_hits(&self, call: &GitCall, rules: &[String], cwd: Option<&str>, roots: &[String]) -> Hits {
        let mut hits = Vec::new();
        let g = self.g;
        let mut effective: Option<String> = cwd.map(String::from);
        for value in &call.dirs_c {
            effective = Some(resolve_dir(g, value, effective.as_deref()));
        }
        let mut folders = vec![resolve_dir(g, ".", effective.as_deref())];
        folders.extend(call.extra_dirs.iter().map(|item| resolve_dir(g, item, cwd)));
        let sub = call.sub.as_str();
        if call.is_push() {
            if has_rule(rules, "headless-push") {
                hits.push(hit("headless-push", HEADLESS_MSG));
            }
            if has_rule(rules, "git-push") {
                hits.push(hit("git-push", "git push: only on the user's explicit request (at the end of the flow, delivery is done by `harness.py deliver`)"));
            }
            let (force, mirror_all, positional) = call.push_facts();
            if mirror_all || positional.iter().any(|arg| self.prot.name_re.is_match(arg)) {
                hits.push(hit("push-protected", "push to a protected branch (integration branch/main/master...): never, only feature branches"));
            } else if positional.len() <= 1 || positional.iter().skip(1).any(|arg| arg == "HEAD" || arg == "@") {
                let branch = current_branch(Some(&folders[0]));
                if self.prot.is_protected(&branch.to_lowercase()) {
                    hits.push(hit("push-protected", format!("git push without a refspec while on the protected branch {branch}")));
                }
            }
            if force {
                hits.push(hit("push-force", "forced push: never"));
            }
        }
        if !READ_ONLY_GIT.contains(&sub) {
            if !roots.is_empty() && has_rule(rules, "repo-only") {
                let outside: Vec<&String> = folders.iter().filter(|f| !inside_any(f, roots)).collect();
                if let Some(first) = outside.first() {
                    let shown: String = first.chars().take(80).collect();
                    hits.push(hit("repo-only", format!("git {sub} {} ({shown}): the repo of its -C/cwd must be one of them", crate::call_forms::OUTSIDE_WORKTREES)));
                }
            }
            if sub != "push" && has_rule(rules, "git-write") {
                hits.push(hit("git-write", format!("git {sub}: write git is the orchestrator's only")));
            }
        }
        static COMMIT_ALL: LazyLock<Re> = LazyLock::new(|| Re::new(r"^-[A-Za-z]*a[A-Za-z]*\z"));
        if sub == "add" && has_rule(rules, "git-add-all") && call.args.iter().any(|a| a == "-A" || a == "--all" || a == ".") {
            hits.push(hit("git-add-all", "git add . / -A: add files by name"));
        }
        if sub == "commit" && has_rule(rules, "git-add-all") && call.args.iter().any(|a| a == "--all" || COMMIT_ALL.is_match(a)) {
            hits.push(hit("git-add-all", "git commit -a: add files by name"));
        }
        hits
    }

    fn gh_call_hits(&self, call: &GhCall, rules: &[String]) -> Hits {
        const WRITE_FAMILIES: [&str; 5] = ["pr", "release", "repo", "gist", "workflow"];
        const REVIEWED: [&str; 12] = ["issue", "label", "secret", "variable", "ruleset", "run", "project", "cache", "codespace", "ssh-key", "gpg-key", "extension"];
        const READ_VERBS: [&str; 5] = ["view", "list", "diff", "status", "checks"];
        let mut hits = Vec::new();
        if has_rule(rules, "headless-gh") {
            hits.push(hit("headless-gh", HEADLESS_MSG));
        }
        if has_rule(rules, "pr-release") {
            let reads = READ_VERBS.contains(&call.sub2.as_str());
            let sub = call.sub.as_str();
            if (WRITE_FAMILIES.contains(&sub) && !((sub == "pr" || sub == "repo") && reads)) || (REVIEWED.contains(&sub) && !reads) {
                hits.push(hit("pr-release", "gh pr/release/gist: leaves the machine; the end-of-flow PR is made by `harness.py deliver`"));
            } else if sub == "api" && gh_api_writes(&call.args) {
                hits.push(hit("pr-release", "gh api that writes (method other than GET, or -f/-F/--field/--input): leaves the machine"));
            }
        }
        hits
    }

    fn check_git_and_gh(&self, command: &str, rules: &[String], start_cwd: Option<&str>, roots: &[String]) -> Hits {
        let mut hits = Vec::new();
        let mut pushed = false;
        for events in shell_event_variants(command) {
            let mut cwd: Option<String> = start_cwd.map(String::from);
            let mut stack: Vec<Option<String>> = Vec::new();
            for event in events {
                match event {
                    Event::Open => {
                        stack.push(cwd.clone());
                        continue;
                    }
                    Event::Close => {
                        if let Some(top) = stack.pop() {
                            cwd = top;
                        }
                        continue;
                    }
                    Event::Cmd(words) => {
                        for call in git_calls_in(&words) {
                            pushed = pushed || call.is_push();
                            hits.extend(self.git_call_hits(&call, rules, cwd.as_deref(), roots));
                        }
                        for call in gh_calls_in(&words) {
                            hits.extend(self.gh_call_hits(&call, rules));
                        }
                        if let Some(moved) = cd_target(&words) {
                            cwd = Some(resolve_dir(self.g, &moved, cwd.as_deref()));
                        }
                    }
                }
            }
        }
        let flat = CONTINUATION_RE.replace_lit(command, " ");
        if PUSH_RE.is_match(&flat) {
            if !pushed {
                if has_rule(rules, "headless-push") {
                    hits.push(hit("headless-push", HEADLESS_MSG));
                }
                if has_rule(rules, "git-push") {
                    hits.push(hit("git-push", "git push: only on the user's explicit request (at the end of the flow, delivery is done by `harness.py deliver`)"));
                }
            }
            if self.prot.push_protected_re.is_match(&flat) {
                hits.push(hit("push-protected", "push to a protected branch (integration branch/main/master...): never, only feature branches"));
            }
            if PUSH_FORCE_RE.is_match(&flat) {
                hits.push(hit("push-force", "forced push: never"));
            }
        }
        hits
    }

    // ---- shell ----------------------------------------------------------------------------------------------------------------------

    fn list_allows_upload(&mut self, statement: &str, rules: &[String]) -> bool {
        if !self.network_rules(rules).contains(&"egress-host") {
            return false;
        }
        let ctx = self.net_ctx();
        net::inline_upload_allowed(ctx, statement)
    }

    fn project_pattern_matches(pattern: &Value, text: &str, warnings: &mut String) -> bool {
        let Value::String(p) = pattern else {
            return false;
        };
        match Re::try_build_msg(p, true) {
            Ok(re) => re.is_match(text),
            Err(message) => {
                warnings.push_str(&format!(
                    "guard: invalid project regex in PERMISSIONS.json skipped ({}: {message})
",
                    pycompat::py_repr_str(p)
                ));
                false
            }
        }
    }

    /// Violated `(rule, reason)` of a Bash/PowerShell command.
    pub fn check_bash(&mut self, command: &str, rules: &[String], warnings: &mut String) -> Hits {
        let g = self.g;
        let mut hits = Hits::new();
        let cwd = self.cwd().map(String::from);
        let specs = self.specs().map(String::from);
        let roots = specs.as_deref().map(|s| project_roots(g, s)).unwrap_or_default();
        let mut running_cwd: Option<String> = cwd.clone().or_else(|| roots.first().cloned());
        let start_cwd = running_cwd.clone();
        hits.extend(self.check_git_and_gh(command, rules, running_cwd.as_deref(), &roots));
        for segment in split_commands(command) {
            let segment_cwd = running_cwd.clone();
            if let Some(m) = CD_RE.match_start(&segment) {
                running_cwd = Some(resolve_dir(g, m.group(1).unwrap_or(""), running_cwd.as_deref()));
            }
            if INSTALL.is_match(&segment) && !ROUTINE_INSTALL.is_match(&segment) && has_rule(rules, "install") {
                hits.push(hit("install", "installing a dependency: only if the spec asks for it"));
            }
            if has_rule(rules, "secrets")
                && (SECRET_HINT.is_match(&segment)
                    || secret_path_hit(g, &segment, segment_cwd.as_deref())
                    || mentions_copied_secret(g, &segment, segment_cwd.as_deref(), specs.as_deref()))
            {
                hits.push(hit("secrets", "credential path (.secrets/shared .env/app data .env or app.json/private key/config copied into a worktree)"));
            }
        }
        if has_rule(rules, "state-script") && STATE_SCRIPT_RE.is_match(command) {
            hits.push(hit(
                "state-script",
                "only the orchestrator writes rule, dependency, graph or LOOP.json (harness.py rules write, audit --write/--apply, graph --write, state loop|migrate)",
            ));
        }
        if has_rule(rules, "delivery") && DELIVERY_RE.is_match(command) {
            hits.push(hit("delivery", "only the orchestrator delivers (push of the feature branch + PR)"));
        }
        if has_rule(rules, "loop-run") && LOOP_RUN_RE.is_match(command) {
            hits.push(hit("loop-run", "the loop and the panel are started by the user (or by the orchestrator at the user's request), never by a subagent"));
        }
        if has_rule(rules, "panel-trigger") && (PANEL_TRIGGER_RE.is_match(command) || PANEL_TRIGGER_RE.is_match(&PANEL_GLUE_RE.replace_lit(command, ""))) {
            hits.push(hit("panel-trigger", "panel endpoint that only the user's click triggers (loop, roadmap approval, Act, answer/resume/instruction)"));
        }
        if has_rule(rules, "egress-upload") {
            for statement in split_statements(command, false) {
                let reason = egress_upload_reason(&statement);
                if !reason.is_empty() && !self.list_allows_upload(&statement, rules) {
                    hits.push(hit("egress-upload", reason));
                }
            }
        }
        if has_rule(rules, "secrets") {
            let mut statement_cwd = start_cwd.clone();
            for statement in split_statements(command, false) {
                let statement = statement.trim().to_string();
                let reason = if statement.is_empty() {
                    String::new()
                } else {
                    let r = broad_secret_read(g, &statement, statement_cwd.as_deref(), specs.as_deref());
                    if r.is_empty() {
                        credential_dir_read(g, &statement, statement_cwd.as_deref())
                    } else {
                        r
                    }
                };
                if !reason.is_empty() {
                    hits.push(hit("secrets", reason));
                }
                if let Some(m) = CD_RE.match_start(&statement) {
                    statement_cwd = Some(resolve_dir(g, m.group(1).unwrap_or(""), statement_cwd.as_deref()));
                }
            }
        }
        if specs.is_some() && ["path-scope", "repo-only", "specs-only"].iter().any(|r| has_rule(rules, r)) {
            let mut statement_cwd = start_cwd.clone();
            for statement in split_statements(command, true) {
                let statement = statement.trim().to_string();
                for target in confined_write_targets(&statement) {
                    hits.extend(check_shell_target(
                        g,
                        &target,
                        statement_cwd.as_deref(),
                        rules,
                        specs.as_deref(),
                        self.perm.slug.as_deref(),
                        self.perm.task.as_deref(),
                        false,
                    ));
                }
                for target in file_command_targets(&statement) {
                    for (rule, why) in
                        check_shell_target(g, &target, statement_cwd.as_deref(), rules, specs.as_deref(), self.perm.slug.as_deref(), self.perm.task.as_deref(), true)
                    {
                        let shown: String = target.chars().take(80).collect();
                        hits.push((rule, format!("{why} [target: {shown}]")));
                    }
                }
                if let Some(m) = CD_RE.match_start(&statement) {
                    statement_cwd = Some(resolve_dir(g, m.group(1).unwrap_or(""), statement_cwd.as_deref()));
                }
            }
        }
        if has_rule(rules, "permissions-write") && PERMISSIONS_CMD_RE.is_match(command) {
            hits.push(hit("permissions-write", PERMISSIONS_MSG));
        }
        let written = shell_write_targets(command);
        let mut file_targets = Vec::new();
        for statement in split_statements(command, true) {
            file_targets.extend(confined_write_targets(&statement));
            file_targets.extend(file_command_targets(&statement));
        }
        let mut all: Vec<String> = Vec::new();
        for t in written.iter().chain(file_targets.iter()) {
            if !all.contains(t) {
                all.push(t.clone());
            }
        }
        for target in &all {
            hits.extend(self.protected_target_hits(
                target,
                rules,
                running_cwd.as_deref(),
                "only the orchestrator writes ROADMAP/STATE/RULES/phases and the loop folder (via shell too)",
            ));
            if has_rule(rules, "secrets") && secret_target_hit(g, target, running_cwd.as_deref(), specs.as_deref()) {
                hits.push(hit("secrets", "write to a credential path"));
            }
        }
        for target in &written {
            for pattern in self.perm.deny_path_regex.clone() {
                if Self::project_pattern_matches(&pattern, target, warnings) {
                    hits.push(hit("project-deny", format!("path forbidden by the project: {}", value_text(&pattern))));
                }
            }
        }
        let classified = crate::policy::classify_command(command);
        if has_rule(rules, "migration-target") {
            let reason = self.migration_target_reason(command, &classified);
            if !reason.is_empty() {
                hits.push(hit("migration-target", reason));
            }
        }
        if has_rule(rules, "ask-command") && !hits.iter().any(|(rule, _)| rule == "migration-target") {
            self.ask_command_hits(&classified, specs.as_deref(), &mut hits);
        }
        for pattern in self.perm.deny_bash_regex.clone() {
            if Self::project_pattern_matches(&pattern, command, warnings) {
                hits.push(hit("project-deny", format!("project rule: {}", value_text(&pattern))));
            }
        }
        let net_rules = self.network_rules(rules);
        if !net_rules.is_empty() {
            let ctx = self.net_ctx();
            hits.extend(net::command_hits(ctx, command, &net_rules));
        }
        hits
    }

    /// A migration, or any command the project policy asks about (create/apply a migration, start the app), aimed at production: refused for
    /// good (`migration-target` is a HARD rule, `PERMISSIONS.json` cannot release it and no approval is ever asked for it).
    fn migration_target_reason(&self, command: &str, classified: &[(&'static str, String)]) -> String {
        if !MIGRATION_CMD_RE.is_match(command) && classified.is_empty() {
            return String::new();
        }
        let what = if MIGRATION_CMD_RE.is_match(command) || classified.iter().any(|(c, _)| *c != crate::policy::CLASS_DEV_RUN) { "migration" } else { "dev command" };
        if PROD_ENV_RE.is_match(command) {
            return format!("{what} against the Production environment: migrations and dev commands run on DEV only");
        }
        let inherited = ["ASPNETCORE_ENVIRONMENT", "DOTNET_ENVIRONMENT"].iter().map(|n| format!("{n}={}", self.g.env.text(n))).collect::<Vec<_>>().join(" ");
        if PROD_ENV_RE.is_match(&inherited) {
            return format!("{what} while ASPNETCORE_ENVIRONMENT/DOTNET_ENVIRONMENT is Production: migrations and dev commands run on DEV only");
        }
        let hosts = prod_hosts(self.g);
        for pattern in [&*CONNECTION_ARG_RE, &*CONNECTION_ENV_RE] {
            for m in pattern.find_all(command) {
                let value = m.group(1).unwrap_or("").to_lowercase();
                if hosts.iter().any(|h| value.contains(h.as_str())) {
                    return format!("{what} whose connection points to production: migrations and dev commands run on DEV only");
                }
            }
        }
        String::new()
    }

    /// The default policy `ask`: a classified command the project did not release (`commands` in PERMISSIONS.json) and the user did not
    /// answer waits for approval. The request is kept in `self.requests`; the caller writes it. An answer of `denied` keeps it refused.
    fn ask_command_hits(&mut self, classified: &[(&'static str, String)], specs: Option<&str>, hits: &mut Hits) {
        use crate::policy::{decision_of, plain, Request, APPROVED, DENIED};
        for (class, statement) in classified {
            if self.perm.project.releases(class, statement) {
                continue;
            }
            let request = Request::new(class, statement);
            match specs.and_then(|s| decision_of(s, &request.id)).as_deref() {
                Some(APPROVED) => continue,
                Some(DENIED) => {
                    hits.push(hit("ask-command", format!("you denied this request ({}): {} [{}]", request.id, plain(class), request.command)));
                    continue;
                }
                _ => {}
            }
            hits.push(hit("ask-command", format!("waiting for approval ({}): {} on DEV [{}]", request.id, plain(class), request.command)));
            self.requests.push(request);
        }
    }

    // ---- MCP, SQL, skills, agents -----------------------------------------------------------------------------------------------------

    fn is_prod_tool(&self, tool_name: &str) -> bool {
        let lowered = tool_name.to_lowercase();
        mcp_policy::is_production_tool(&self.policy(), tool_name) || prod_hints(self.g).iter().any(|h| lowered.contains(h.as_str()))
    }

    fn sql_texts(input: &Value) -> Vec<String> {
        if let Value::Object(map) = input {
            let named: Vec<String> = ["query", "sql", "statement"].iter().filter_map(|k| map.get(*k).and_then(Value::as_str).map(String::from)).collect();
            if !named.is_empty() {
                return named;
            }
            return map.values().filter_map(|v| v.as_str().map(String::from)).collect();
        }
        vec![value_text(input)]
    }

    fn first_writing_keyword(texts: &[String]) -> String {
        for text in texts {
            for statement in sql_statements(text) {
                let keyword = sql_statement_writes(&statement);
                if !keyword.is_empty() {
                    return keyword;
                }
            }
        }
        String::new()
    }

    fn sql_statements_write_reason(tool_name: &str, input: &Value) -> String {
        let keyword = Self::first_writing_keyword(&Self::sql_texts(input));
        if keyword.is_empty() {
            String::new()
        } else {
            format!("{keyword} through {tool_name}: {SQL_WRITE_REASON}")
        }
    }

    fn prod_sql_reason(&self, tool_name: &str, input: &Value) -> String {
        let is_sql = mcp_policy::is_sql_tool(&self.policy(), tool_name) || DB_TOOL.match_start(tool_name).is_some();
        if !is_sql || !self.is_prod_tool(tool_name) {
            return String::new();
        }
        let keyword = Self::first_writing_keyword(&Self::sql_texts(input));
        if keyword.is_empty() {
            String::new()
        } else {
            format!("{keyword} on a production database through {tool_name}: never, not even with PERMISSIONS.json; production is read only")
        }
    }

    fn grafana_write_reason(tool_name: &str, input: &Value) -> String {
        const PREFIX: &str = "mcp__grafana__";
        if !tool_name.to_lowercase().starts_with(PREFIX) {
            return String::new();
        }
        let verb = tool_name[PREFIX.len()..].to_lowercase();
        if verb == "grafana_api_request" {
            let method = input.get("method").filter(|v| truthy(v)).map(value_text).unwrap_or_else(|| "GET".into()).trim().to_uppercase();
            return if method == "GET" { String::new() } else { format!("grafana_api_request with method {method} (Grafana is read-only)") };
        }
        if verb == "create_snapshot" || verb == "delete_snapshot" || !GRAFANA_READ_RE.is_match(&verb) {
            return format!("{tool_name} writes to Grafana (only read tools are allowed)");
        }
        String::new()
    }

    fn agent_call_hits(tool_name: &str, input: &Value) -> Hits {
        if (tool_name != "Agent" && tool_name != "Task") || !input.is_object() {
            return vec![];
        }
        let mut hits = Vec::new();
        let raw = input.get("subagent_type").filter(|v| truthy(v)).map(value_text).unwrap_or_default();
        let agent = raw.rsplit(':').next().unwrap_or("").trim().to_string();
        if agent_profile(&agent).is_none() {
            let mut names: Vec<&str> = AGENT_PROFILES.iter().map(|(a, _)| *a).collect();
            names.sort();
            let shown = pycompat::py_repr_str(if agent.is_empty() { "(none)" } else { &agent });
            hits.push(hit("agent-allowlist", format!("agent {shown} is not one of the plugin agents ({}): dispatch only those", names.join(", "))));
        }
        let model = input.get("model").filter(|v| truthy(v)).map(value_text).unwrap_or_default().trim().to_lowercase();
        if !model.is_empty() && !model.contains("sonnet") && !model.contains("haiku") {
            let shown: String = model.chars().take(40).collect();
            hits.push(hit("model-ceiling", format!("model '{shown}' is above the ceiling: subagents run at Sonnet or Haiku (RULES.md)")));
        }
        hits
    }

    pub fn mcp_allowlist_reason(&self, tool_name: &str, input: &Value, profile: &str) -> String {
        if !tool_name.to_lowercase().starts_with("mcp__") || !isolated_or_headless(self.g) {
            return String::new();
        }
        let policy = self.policy();
        let reason = mcp_policy::deny_reason(&policy, tool_name, profile);
        if !reason.is_empty() {
            return reason;
        }
        if let Some((_, options)) = mcp_policy::find_server(&policy, tool_name) {
            if options.kind == mcp_policy::KIND_SQL && options.read_only {
                let reason = Self::sql_statements_write_reason(tool_name, input);
                if !reason.is_empty() {
                    return format!("{reason} (the server is marked read-only in Settings > MCP servers)");
                }
            }
        }
        String::new()
    }

    pub fn skill_allowlist_reason(&self, tool_name: &str, input: &Value, profile: &str) -> String {
        if tool_name != "Skill" {
            return String::new();
        }
        let path = self.g.env.text("LOOP_SKILLS_FILE");
        if path.is_empty() {
            return String::new();
        }
        let table = std::fs::read(&path)
            .map_err(|_| "OSError")
            .and_then(|b| String::from_utf8(b).map_err(|_| "UnicodeDecodeError"))
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|_| "JSONDecodeError"));
        let table = match table {
            Ok(Value::Object(m)) => m,
            Ok(_) => return "LOOP_SKILLS_FILE is set but unreadable or invalid (ValueError): skills are denied".into(),
            Err(kind) => return format!("LOOP_SKILLS_FILE is set but unreadable or invalid ({kind}): skills are denied"),
        };
        let Some(Value::Array(names)) = table.get(profile) else {
            return format!("profile '{profile}' is not in LOOP_SKILLS_FILE: skills are denied");
        };
        let skill = skill_call_name(input);
        if !skill.is_empty() && names.iter().any(|n| normalize_skill_name(&value_text(n)) == skill) {
            return String::new();
        }
        format!("skill {} is not on the {profile} profile's skill allowlist", pycompat::py_repr_str(if skill.is_empty() { "(none)" } else { &skill }))
    }

    fn capabilities_edit_reason(&self, tool_name: &str, input: &Value, target: &str, cwd: Option<&str>) -> String {
        if target.is_empty() || !CAPABILITIES_FILE_RE.is_match(target) || !input.is_object() {
            return String::new();
        }
        let reason = "the capabilities block of PROJECT.md (between maestro:capabilities markers) is written by the server, never by you".to_string();
        let mut path = target.to_string();
        if !crate::pyre::nt_isabs(&path) {
            if let Some(c) = cwd {
                path = crate::pyre::nt_join(c, &path);
            }
        }
        let current = read_text_file(&path);
        let field = |k: &str| input.get(k).filter(|v| truthy(v)).map(value_text).unwrap_or_default().replace("\r\n", "\n");
        let blocks = |text: &str| -> Vec<String> { CAPABILITIES_BLOCK_RE.find_all(text).into_iter().map(|m| m.text().to_string()).collect() };
        if tool_name == "Write" {
            let content = field("content");
            if blocks(&current) != blocks(&content) {
                return reason;
            }
            if current.contains(CAPABILITIES_OPEN) != content.contains(CAPABILITIES_OPEN) || current.contains(CAPABILITIES_CLOSE) != content.contains(CAPABILITIES_CLOSE)
            {
                return reason;
            }
            return String::new();
        }
        if tool_name == "Edit" {
            let old = field("old_string");
            let new = field("new_string");
            if old.contains("maestro:capabilities") || new.contains("maestro:capabilities") {
                return reason;
            }
            if !old.is_empty() && blocks(&current).iter().any(|b| b.contains(&old)) {
                return reason;
            }
        }
        String::new()
    }

    // ---- the tool dispatcher -------------------------------------------------------------------------------------------------------------

    pub fn check_tool(&mut self, tool_name: &str, input: &Value, rules: &[String], warnings: &mut String) -> Result<Hits, String> {
        let g = self.g;
        let mut hits = Hits::new();
        hits.extend(Self::agent_call_hits(tool_name, input).into_iter().filter(|(r, _)| has_rule(rules, r)));
        if tool_name == "Bash" || tool_name == "PowerShell" {
            let command = input
                .get("command")
                .map(|c| {
                    if c.is_string() {
                        value_text(c)
                    } else if truthy(c) {
                        value_text(c)
                    } else {
                        String::new()
                    }
                })
                .unwrap_or_default();
            return Ok(self.check_bash(&command, rules, warnings));
        }
        if has_rule(rules, "grafana-write") {
            let reason = Self::grafana_write_reason(tool_name, input);
            if !reason.is_empty() {
                hits.push(hit("grafana-write", reason));
            }
        }
        if tool_name.starts_with("mcp__") {
            let short_name = tool_name.rsplit("__").next().unwrap_or("").to_string();
            if has_rule(rules, "prod-sql-write") {
                let reason = self.prod_sql_reason(tool_name, input);
                if !reason.is_empty() {
                    hits.push(hit("prod-sql-write", reason));
                }
            }
            let policy = self.policy();
            let sql_text = |input: &Value| -> Result<String, String> {
                for key in ["query", "sql"] {
                    if let Some(v) = input.get(key).filter(|v| truthy(v)) {
                        return match v {
                            Value::String(s) => Ok(s.clone()),
                            _ => Err("TypeError: expected string or bytes-like object".into()),
                        };
                    }
                }
                Ok(py_json_dumps(input, true))
            };
            if mcp_policy::is_sql_tool(&policy, tool_name) {
                if has_rule(rules, "sql-write") {
                    let keyword = Self::first_writing_keyword(&Self::sql_texts(input));
                    if !keyword.is_empty() {
                        hits.push(hit("sql-write", format!("{keyword} through {tool_name}: {SQL_WRITE_REASON}")));
                    }
                } else {
                    let sql = sql_text(input)?;
                    if WRITE_SQL.is_match(&sql) {
                        let rule = if self.is_prod_tool(tool_name) { "prod-write" } else { "dev-write" };
                        if has_rule(rules, rule) {
                            hits.push(hit(rule, format!("write SQL in {tool_name}")));
                        }
                    }
                }
            } else if DB_TOOL.match_start(tool_name).is_some() {
                let sql = sql_text(input)?;
                if WRITE_SQL.is_match(&sql) {
                    let rule = if self.is_prod_tool(tool_name) { "prod-write" } else { "dev-write" };
                    if has_rule(rules, rule) {
                        hits.push(hit(rule, format!("write SQL in {tool_name}")));
                    }
                }
            } else if WRITE_VERB.is_match(&short_name) && self.is_prod_tool(tool_name) && has_rule(rules, "prod-write") {
                hits.push(hit("prod-write", format!("{tool_name} writes to production")));
            }
            if OUTBOUND_HINT.is_match(tool_name) && OUTBOUND_VERB.is_match(&short_name) && has_rule(rules, "outbound-msg") {
                hits.push(hit("outbound-msg", format!("{tool_name} sends/changes something outside the machine (ticket, issue, message: never in the feature's scope)")));
            }
        }
        if ["Write", "Edit", "MultiEdit", "NotebookEdit"].contains(&tool_name) {
            let target = input.get("file_path").filter(|v| truthy(v)).or_else(|| input.get("notebook_path").filter(|v| truthy(v))).map(value_text).unwrap_or_default();
            let (cwd, specs) = (self.cwd().map(String::from), self.specs().map(String::from));
            hits.extend(self.protected_target_hits(&target, rules, cwd.as_deref(), "only the orchestrator writes ROADMAP/STATE/RULES/phases and the loop folder"));
            if has_rule(rules, "secrets") && secret_target_hit(g, &target, cwd.as_deref(), specs.as_deref()) {
                hits.push(hit("secrets", "credential path"));
            }
            if has_rule(rules, "capabilities-block") && (tool_name == "Write" || tool_name == "Edit") {
                let reason = self.capabilities_edit_reason(tool_name, input, &target, cwd.as_deref());
                if !reason.is_empty() {
                    hits.push(hit("capabilities-block", reason));
                }
            }
            hits.extend(check_specs_only(g, &target, rules, specs.as_deref(), cwd.as_deref()));
            hits.extend(check_scope(g, &target, rules, specs.as_deref(), self.perm.slug.as_deref(), cwd.as_deref(), self.perm.task.as_deref()));
            for pattern in self.perm.deny_path_regex.clone() {
                if Self::project_pattern_matches(&pattern, &target, warnings) {
                    hits.push(hit("project-deny", format!("path forbidden by the project: {}", value_text(&pattern))));
                }
            }
        }
        if ["Read", "Glob", "Grep"].contains(&tool_name) {
            let (cwd, specs) = (self.cwd().map(String::from), self.specs().map(String::from));
            let target = py_json_dumps(input, true);
            let values: Vec<String> = input.as_object().map(|m| m.values().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
            if has_rule(rules, "secrets")
                && (SECRET_HINT.is_match(&target)
                    || secret_path_hit(g, &target, cwd.as_deref())
                    || mentions_copied_secret(g, &target, cwd.as_deref(), specs.as_deref())
                    || values.iter().any(|v| secret_target_hit(g, v, cwd.as_deref(), specs.as_deref())))
            {
                hits.push(hit("secrets", "reading a credential"));
            }
            if has_rule(rules, "secrets") && (tool_name == "Glob" || tool_name == "Grep") {
                let mut reason = tool_broad_secret(g, tool_name, input, cwd.as_deref(), specs.as_deref());
                if reason.is_empty() {
                    reason = search_reaches_credentials(g, tool_name, input, cwd.as_deref());
                }
                if !reason.is_empty() {
                    hits.push(hit("secrets", reason));
                }
            }
        }
        let net_rules = self.network_rules(rules);
        if !net_rules.is_empty() {
            let ctx = self.net_ctx();
            hits.extend(net::tool_hits(ctx, tool_name, input, &net_rules));
        }
        Ok(hits)
    }

    /// One `allow` line per network request the allowed call makes.
    pub fn log_egress(&self, specs_dir: Option<&str>, profile: &str) {
        if let Some(ctx) = &self.net {
            for (decision, detail) in &ctx.log {
                log(self.g, specs_dir, profile, "egress-host", decision, detail, None, None);
            }
        }
    }
}

pub fn read_text_file(path: &str) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes).to_string();
            text.strip_prefix('\u{feff}').unwrap_or(&text).replace("\r\n", "\n")
        }
        Err(_) => String::new(),
    }
}

pub fn normalize_skill_name(value: &str) -> String {
    let name = value.trim().trim_start_matches('/');
    name.rsplit(':').next().unwrap_or("").trim().trim_start_matches('/').to_lowercase()
}

pub fn skill_call_name(input: &Value) -> String {
    let Value::Object(map) = input else {
        return String::new();
    };
    for key in ["skill", "name", "command"] {
        if let Some(Value::String(s)) = map.get(key) {
            if !s.trim().is_empty() {
                return normalize_skill_name(s);
            }
        }
    }
    String::new()
}

/// `unique`: first occurrence of each `(rule, reason)`.
pub fn unique(hits: Hits) -> Hits {
    let mut out: Hits = Vec::new();
    for h in hits {
        if !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

pub fn unparseable_marker() -> &'static str {
    SQL_UNPARSEABLE
}

pub fn normalize_for_tests(g: &G, p: &str) -> String {
    normalize_path(g, p, None, None)
}
