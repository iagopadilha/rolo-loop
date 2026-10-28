// SPDX-License-Identifier: MIT
//! Rule catalogue: profiles, the HARD set, agent-to-profile map and the fixed regexes of `guard.py`.

use crate::paths::G;
use crate::pyre::{escape, Re};
use crate::state::integration_branch;
use std::sync::LazyLock;

pub const READ_ONLY_GIT: [&str; 20] = [
    "status",
    "diff",
    "log",
    "show",
    "rev-parse",
    "ls-files",
    "blame",
    "describe",
    "grep",
    "shortlog",
    "check-ignore",
    "check-attr",
    "ls-tree",
    "cat-file",
    "rev-list",
    "merge-base",
    "name-rev",
    "show-ref",
    "for-each-ref",
    "diff-tree",
];

/// Rules that not even `PERMISSIONS.json` releases.
pub const HARD: [&str; 15] = [
    "push-protected",
    "push-force",
    "approval-write",
    "panel-trigger",
    "grafana-write",
    "headless-push",
    "headless-gh",
    "permissions-write",
    "prod-sql-write",
    "agent-allowlist",
    "model-ceiling",
    "mcp-allowlist",
    "skill-allowlist",
    "engine-file-write",
    "migration-target",
];
pub const HEADLESS_RULES: [&str; 3] = ["headless-push", "headless-gh", "permissions-write"];
pub const GUARDED_EXTRA_RULES: [&str; 3] = ["prod-sql-write", "egress-upload", "engine-file-write"];

pub const HEADLESS_MSG: &str = "git push / gh: delivery belongs to the engine (harness.py deliver / Finish); in a headless loop you only commit";
pub const PERMISSIONS_MSG: &str = "PERMISSIONS.json holds the user's releases: only the user changes it, never the loop";
pub const ENGINE_FILE_MSG: &str = "LOOP-CMD.json (the command the panel relaunches), REPOS.json (the repo list), NETWORK.json (the network list: the user edits it in the panel) and loop/internal/network-* (the run-time egress layer) belong to the engine/app: nobody under the guard writes them";
pub const NETWORK_SUGGESTED_MSG: &str = "NETWORK.suggested.json is written by the planning chat only: the user reviews it in the panel's Network section";
pub const SHORT_NAME_MSG: &str =
    "an 8.3 short name (like CLAUDE~1.JSO) that cannot be expanded, under a protected folder (.specs, the home folder, ~/.claude): it may be a protected file";
pub const SQL_WRITE_REASON: &str = "read only through MCP: write the change as a .sql script (or a migration) and ask the user (write the script in the feature folder sql/ and return STATUS: NEEDS_CONFIRM: apply <script> on DEV, then answer)";

/// `(agent_type, profile)` of `--profile auto`.
pub const AGENT_PROFILES: [(&str, &str); 8] = [
    ("spec-writer", "spec"),
    ("spec-reviewer", "spec"),
    ("brownfield-mapper", "spec"),
    ("executor-back", "executor"),
    ("executor-front", "executor"),
    ("executor-generic", "executor"),
    ("verifier", "verifier"),
    ("verifier-flow", "verifier"),
];

pub fn agent_profile(agent: &str) -> Option<&'static str> {
    AGENT_PROFILES.iter().find(|(a, _)| *a == agent).map(|(_, p)| *p)
}

fn base_profile(profile: &str) -> Option<Vec<&'static str>> {
    Some(match profile {
        "executor" => vec![
            "git-write",
            "git-push",
            "pr-release",
            "prod-write",
            "dev-write",
            "outbound-msg",
            "secrets",
            "install",
            "state-write",
            "project-deny",
            "path-scope",
            "repo-only",
            "delivery",
            "state-script",
            "loop-run",
            "approval-write",
            "panel-trigger",
            "grafana-write",
            "sql-write",
            "migration-target",
            "ask-command",
        ],
        "verifier" => vec![
            "git-write",
            "git-push",
            "pr-release",
            "prod-write",
            "dev-write",
            "outbound-msg",
            "secrets",
            "state-write",
            "project-deny",
            "repo-only",
            "delivery",
            "state-script",
            "loop-run",
            "approval-write",
            "panel-trigger",
            "grafana-write",
            "sql-write",
            "migration-target",
            "ask-command",
        ],
        "spec" => vec![
            "specs-only",
            "map-docs",
            "git-write",
            "git-push",
            "pr-release",
            "prod-write",
            "dev-write",
            "outbound-msg",
            "secrets",
            "state-write",
            "project-deny",
            "delivery",
            "state-script",
            "loop-run",
            "approval-write",
            "panel-trigger",
            "grafana-write",
            "sql-write",
        ],
        "orchestrator" => vec![
            "git-push",
            "pr-release",
            "git-add-all",
            "prod-write",
            "secrets",
            "project-deny",
            "outbound-msg",
            "approval-write",
            "panel-trigger",
            "repo-only",
            "grafana-write",
            "sql-write",
        ],
        "planning" => vec![
            "specs-only",
            "secrets",
            "approval-write",
            "panel-trigger",
            "git-write",
            "git-push",
            "pr-release",
            "prod-write",
            "outbound-msg",
            "grafana-write",
            "sql-write",
            "capabilities-block",
        ],
        "roadmapmap" => vec![
            "specs-only",
            "map-docs",
            "secrets",
            "approval-write",
            "panel-trigger",
            "git-write",
            "git-push",
            "pr-release",
            "prod-write",
            "outbound-msg",
            "grafana-write",
            "sql-write",
            "capabilities-block",
        ],
        "subprojects" => vec![
            "specs-only",
            "subprojects-only",
            "secrets",
            "approval-write",
            "panel-trigger",
            "git-write",
            "git-push",
            "pr-release",
            "prod-write",
            "outbound-msg",
            "grafana-write",
            "sql-write",
            "capabilities-block",
        ],
        "systemmap" => vec![
            "specs-only",
            "system-only",
            "secrets",
            "approval-write",
            "panel-trigger",
            "git-write",
            "git-push",
            "pr-release",
            "prod-write",
            "outbound-msg",
            "grafana-write",
            "sql-write",
            "capabilities-block",
        ],
        _ => return None,
    })
}

fn extra_rules(profile: &str) -> Vec<&'static str> {
    let mut extra: Vec<&'static str> = GUARDED_EXTRA_RULES.to_vec();
    match profile {
        "orchestrator" => extra.extend(["agent-allowlist", "model-ceiling"]),
        "executor" => extra.push("scope-unreadable"),
        _ => {}
    }
    extra
}

/// The rule list of a profile (unknown profile = executor), extras appended once.
pub fn profile_rules(profile: &str) -> Vec<String> {
    let name = if base_profile(profile).is_some() { profile } else { "executor" };
    let mut rules: Vec<&'static str> = base_profile(name).unwrap_or_default();
    if base_profile(profile).is_some() {
        for rule in extra_rules(profile) {
            if !rules.contains(&rule) {
                rules.push(rule);
            }
        }
    } else {
        // the executor fallback keeps the executor extras too
        for rule in extra_rules("executor") {
            if !rules.contains(&rule) {
                rules.push(rule);
            }
        }
    }
    rules.into_iter().map(String::from).collect()
}

// ---- fixed regexes ---------------------------------------------------------------------------------------------------------------

macro_rules! ci {
    ($name:ident, $pat:expr) => {
        pub static $name: LazyLock<Re> = LazyLock::new(|| Re::ci($pat));
    };
}
macro_rules! cs {
    ($name:ident, $pat:expr) => {
        pub static $name: LazyLock<Re> = LazyLock::new(|| Re::new($pat));
    };
}

ci!(WRITE_SQL, r"\b(insert|update|delete|drop|create|alter|truncate|replace|grant|revoke|call|merge)\b");
ci!(DB_TOOL, r"^mcp__.*(mysql|postgres|mariadb|mssql|sqlite|sql|database)");
ci!(WRITE_VERB, r"(create|insert|update|delete|remove|drop|write|set|add|put|post|patch|execute|exec|send|grant|revoke|assign|upsert|reorder|upload|publish)");
ci!(OUTBOUND_HINT, r"(ticket|issue|jira|confluence|slack|mail|message|sms|chat|comment|notif|channel|teams|discord)");
ci!(OUTBOUND_VERB, r"(create|update|delete|add|send|post|reply|comment|transition|edit|publish)");
ci!(INSTALL, r"\b(npm|pnpm|yarn|bun)\s+(i|install|add|ci|update|upgrade)\b|\bdotnet\s+(restore|add\s+package)\b|\bpip\s+install\b");
// A routine install restores what a lockfile or manifest ALREADY declares (no package name, no custom feed): the whole command must be
// that and nothing else. `npm install left-pad`, `pip install x`, `dotnet add package x` stay refused by INSTALL.
ci!(
    ROUTINE_INSTALL,
    r"^\s*(?:npm\s+ci(?:\s+--[a-z-]+)*|(?:pnpm|yarn)\s+install\s+--frozen-lockfile|dotnet\s+restore(?:\s+[^\s\x22\x27;&|<>$-][^\s\x22\x27;&|<>$]*|\s+\x22[^\x22;&|<>$]*\x22)?|(?:python[0-9.]*\s+-m\s+)?pip[0-9.]*\s+install\s+(?:-r|--requirement)\s+[^\s:;&|<>$-][^\s:;&|<>$]*|cargo\s+fetch(?:\s+--locked)?|poetry\s+install|go\s+mod\s+download)\s*$"
);
ci!(
    SECRET_HINT,
    r"\.secrets[\\/]|[\\/]_?shared[\\/]+(?:[^\\/\s\x22\x27]+[\\/]+)*\.env(?!\.example)|\.env\.test(?!\.example)|\.loop-harness[\\/]+(?:\.env(?!\.example)|app\.json)|\bid_(rsa|ed25519)\b|[\\/]\.ssh[\\/]+(?!\S*\.example)|[\\/]\.aws[\\/]+(?!\S*\.example)|~[\\/]\.npmrc(?!\.example)|[\\/]\.netrc(?!\.example)|[\\/]\.git-credentials(?!\.example)|[\\/]gh[\\/]hosts\.yml(?!\.example)|[\\/]\.docker[\\/]config\.json(?!\.example)|[\\/]\.kube[\\/]config(?!\.example)"
);
ci!(
    STATE_FILE,
    r"\.specs[\\/]project[\\/](ROADMAP\.md|FLOWS\.md|STATE\.md|RULES\.md|phases[\\/]|archive[\\/]|loop(?:[\\/]|\n?\z)|VALIDATE\.md|DEPENDENCIES\.md|GRAPH\.md|ANSWERS\.md|NOTICE\.txt|rules\.tests\.json|SCOPE\.json|LOOP\.json|LOOP\.lock|PERMISSIONS\.json|ROADMAP-APPROVED\.json|delivery\.json|REPOS\.json)"
);
ci!(APPROVAL_FILE, r"\.specs[\\/]project[\\/](?:loop[\\/](?:json[\\/])?)?ROADMAP-APPROVED\.json");
ci!(
    ENGINE_FILE_RE,
    r"\.specs[\\/]project[\\/](?:loop[\\/](?:[^\\/]+[\\/])*LOOP-CMD\.json|LOOP-CMD\.json|REPOS\.json|NETWORK\.json|loop[\\/]internal[\\/]network-[^\\/]+)(?=\n?\z)"
);
ci!(NETWORK_SUGGESTED_RE, r"\.specs[\\/]project[\\/]NETWORK\.suggested\.json(?=\n?\z)");
ci!(
    PANEL_TRIGGER_RE,
    r"(?:https?://[^/\s]+|(?:localhost|127(?:\.\d+){3}|\[::1\]|0\.0\.0\.0)(?::\d+)?|[\w.-]+:\d+|\$\{?\w+\}?|%\w+%)/p/[^/\s]+/api/(?:loop/|roadmap/approve\b|action/|answer\b|resume\b|instruction\b)"
);
cs!(PANEL_GLUE_RE, r#"["']\s*\+\s*["']|["'`^]"#);
cs!(CONTINUATION_RE, r"\\\r?\n|`\r?\n");
const HARNESS_CLI: &str = r#"\bharness(?:\.py|\.exe)?["']?\s+"#;
pub static STATE_SCRIPT_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::ci(&format!(
        r"\brules\.py\s+write\b|\baudit_deps\.py\b[^\n]*--(?:write|apply)\b|\bgraph_view\.py\b[^\n]*--write\b|\bloop_state\.py\s+(?:loop|migrate)\b|{h}rules\s+write\b|{h}audit\b[^\n]*--(?:write|apply)\b|{h}graph\b[^\n]*--write\b|{h}state\s+(?:loop|migrate)\b",
        h = HARNESS_CLI
    ))
});
pub static DELIVERY_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(&format!(r"\bdelivery\.py\b|{HARNESS_CLI}deliver\b")));
pub static LOOP_RUN_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(&format!(r"\brun_loop\.py\b|{HARNESS_CLI}(?:loop|check|serve)\b")));
pub static PERMISSIONS_CMD_RE: LazyLock<Re> =
    LazyLock::new(|| Re::ci(&format!(r"\brules\.py\s+(?:write|allow|release)\b|{HARNESS_CLI}rules\s+(?:write|allow|release)\b")));
cs!(REDIRECT_RE, r#"(?<![<>])(?:[&*]|\d)?>>?\|?\s*(?!&)("[^"]+"|'[^']+'|[^\s;|&<>]+)"#);
ci!(CMD_TARGET_RE, r#"\b(?:tee(?:\s+-a)?|Set-Content|Add-Content|Out-File)\s+(?:-\w+\s+)*("[^"]+"|'[^']+'|[^\s;|&<>]+)"#);
cs!(SED_INPLACE_RE, r#"\bsed\s+-i\S*\s+(?:-e\s+)?(?:'[^']*'|"[^"]*"|\S+)\s+(\S+)"#);
pub const NULL_TARGETS: [&str; 4] = ["/dev/null", "$null", "nul", "null"];
ci!(PERMISSIONS_FILE, r"(?:^|[\\/])PERMISSIONS\.json(?=\n?\z)");
ci!(PS_PATH_RE, r#"-(?:Path|FilePath|LiteralPath)\s+("[^"]+"|'[^']+'|[^\s;|&<>]+)"#);
ci!(SET_CONTENT_WORD, r"\b(?:Set-Content|Add-Content|Out-File)\b");
ci!(CD_RE, r#"^(?:cd|pushd|Set-Location|sl)\s+(?:-(?:Path|LiteralPath)\s+)?("[^"]+"|'[^']+'|\S+)"#);
ci!(
    MIGRATION_CMD_RE,
    r"(?:^|[\s(;&|])(?:\S*[\\/])?dotnet(?:\.exe)?\s+(?:ef\s+(?:migrations\s+(?:add|remove|list|script|bundle)|database\s+(?:update|drop))\b|run\b[^\n;&|]*?\s--\s+migrate\w*)"
);
const PROD_ENV_VALUE: &str = r#"["']?(?:production|prod)["']?"#;
pub static PROD_ENV_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::ci(&format!(
        r#"(?:ASPNETCORE_ENVIRONMENT|DOTNET_ENVIRONMENT|NODE_ENV|APP_ENV|RAILS_ENV|FLASK_ENV|DJANGO_ENV)["']?\s*=\s*{PROD_ENV_VALUE}(?![\w-])|--(?:environment|env|stage)(?:\s+|=){PROD_ENV_VALUE}(?![\w-])"#
    ))
});
ci!(CONNECTION_ARG_RE, r#"--(?:connection|connection-string|connectionstring)(?:\s+|=)("[^"]*"|'[^']*'|\S+)"#);
ci!(CONNECTION_ENV_RE, r#"\b(?:ConnectionStrings?[:_]{1,2}\w*|DB_(?:HOST|CONNECTION)\w*|DATABASE_URL)\s*=\s*("[^"]*"|'[^']*'|\S+)"#);
ci!(GRAFANA_READ_RE, r"^(?:get|list|query|search|find|check|analyze|suggest|validate)_|^generate_deeplink\n?\z");
ci!(CAPABILITIES_FILE_RE, r"\.specs[\\/]project[\\/]PROJECT\.md(?=\n?\z)");
pub static CAPABILITIES_BLOCK_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!(r"(?s){}.*?{}", escape(CAPABILITIES_OPEN), escape(CAPABILITIES_CLOSE))));
pub const CAPABILITIES_OPEN: &str = "<!-- maestro:capabilities -->";
pub const CAPABILITIES_CLOSE: &str = "<!-- /maestro:capabilities -->";

// ---- regexes that depend on the integration branch ---------------------------------------------------------------------------

const GIT_OPTIONS: &str = r#"(?:\s+(?:-C|-c)\s+(?:\"[^\"]*\"|'[^']*'|\S+)|\s+-(?![Cc]\s)[-\w.]*(?:=(?:\"[^\"]*\"|'[^']*'|\S+))?)*"#;

pub fn push_cmd() -> String {
    format!(r#"(?:\bgit(?:\.exe)?['\"]?{GIT_OPTIONS}\s+push\b|['\"]git(?:\.exe)?['\"]\s*,\s*['\"]push['\"])"#)
}

pub static GIT_GREP_BYPASS_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(&format!(r"\bgit(?:\.exe)?{GIT_OPTIONS}\s+grep\b[^\n]*?\s--(?:no-index|no-exclude-standard)\b")));
/// Compiles the large git patterns side by side (each takes several milliseconds, and a push command needs all of them). Called only for a command
/// that mentions git.
pub fn warm_git(g: &G) {
    let text = "git push --force --mirror --all main production; git grep --no-index";
    let protected = Protected::new(g);
    std::thread::scope(|scope| {
        scope.spawn(|| PUSH_RE.is_match(text));
        scope.spawn(|| PUSH_FORCE_RE.is_match(text));
        scope.spawn(|| GIT_GREP_BYPASS_RE.is_match(text));
        let (name_re, push_re) = (protected.name_re.clone(), protected.push_protected_re.clone());
        scope.spawn(move || name_re.is_match(text));
        scope.spawn(move || push_re.is_match(text));
    });
}

pub static PUSH_RE: LazyLock<Re> = LazyLock::new(|| Re::ci(&push_cmd()));
pub static PUSH_FORCE_RE: LazyLock<Re> =
    LazyLock::new(|| Re::ci(&format!(r"{}[^\n]*?(?:\s--force\b|\s--force-with-lease\b|\s-[A-Za-z]*f[A-Za-z]*(?=\s|\z)|\s\+\S)", push_cmd())));

/// The regexes and names that depend on the integration branch (read from the environment per call).
pub struct Protected {
    pub names: Vec<String>,
    pub name_re: std::sync::Arc<Re>,
    pub push_protected_re: std::sync::Arc<Re>,
}

impl Protected {
    pub fn new(g: &G) -> Protected {
        let mut names: Vec<String> = ["developer", "develop", "main", "master", "release", "production", "dev"].iter().map(|s| s.to_string()).collect();
        let integration = integration_branch(g);
        if !names.contains(&integration) {
            names.push(integration);
        }
        let mut sorted = names.clone();
        sorted.sort_by_key(|n| std::cmp::Reverse(n.len()));
        let alt = sorted.iter().map(|n| escape(n)).collect::<Vec<_>>().join("|");
        let name_re = Re::cached(&format!(r"(?<![\w.-])(?:{alt})(?![\w.-])"), true);
        let push_protected_re = Re::cached(&format!(r"{}[^\n]*?(?:(?<![\w.-])(?:{alt})(?![\w.-])|\s--mirror\b|\s--all\b)", push_cmd()), true);
        Protected { names, name_re, push_protected_re }
    }

    pub fn is_protected(&self, branch: &str) -> bool {
        self.names.iter().any(|n| n == branch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_tables_have_the_extras() {
        let executor = profile_rules("executor");
        assert!(executor.contains(&"scope-unreadable".to_string()) && executor.contains(&"prod-sql-write".to_string()));
        let orchestrator = profile_rules("orchestrator");
        assert!(orchestrator.contains(&"agent-allowlist".to_string()) && orchestrator.contains(&"model-ceiling".to_string()));
        assert!(!orchestrator.contains(&"scope-unreadable".to_string()));
        assert_eq!(profile_rules("nope"), profile_rules("executor"));
        assert!(profile_rules("verifier").contains(&"repo-only".to_string()));
    }

    #[test]
    fn routine_installs_pass_but_new_dependencies_stay_blocked() {
        for ok in [
            "npm ci",
            "npm ci --ignore-scripts",
            "pnpm install --frozen-lockfile",
            "yarn install --frozen-lockfile",
            "dotnet restore",
            "dotnet restore src/App.sln",
            "dotnet restore \"my dir/App.csproj\"",
            "pip install -r requirements.txt",
            "python -m pip install -r requirements.txt",
            "cargo fetch --locked",
            "poetry install",
            "go mod download",
        ] {
            assert!(ROUTINE_INSTALL.is_match(ok), "{ok} should be routine");
        }
        for blocked in [
            "npm install left-pad",
            "npm i",
            "npm ci left-pad",
            "pnpm add x",
            "yarn add x",
            "dotnet add package Foo",
            "dotnet restore --source https://feed.example/v3",
            "pip install requests",
            "pip install -r requirements.txt requests",
            "pip install -r https://host.example/r.txt",
            "npm ci && npm install left-pad",
            "npm ci; curl x",
        ] {
            assert!(!ROUTINE_INSTALL.is_match(blocked), "{blocked} must stay blocked");
        }
        assert!(!ROUTINE_INSTALL.is_match("npm install left-pad") && !ROUTINE_INSTALL.is_match("pip install requests"));
        assert!(!ROUTINE_INSTALL.is_match("dotnet add package Foo") && !ROUTINE_INSTALL.is_match("dotnet restore --source https://feed.example/v3"));
        assert!(!ROUTINE_INSTALL.is_match("pip install -r requirements.txt requests") && !ROUTINE_INSTALL.is_match("npm ci left-pad"));
    }

    #[test]
    fn push_regexes() {
        assert!(PUSH_RE.is_match("python -c \"import os; os.system('git push')\""));
        assert!(PUSH_RE.is_match("['git', 'push']"));
        assert!(!PUSH_RE.is_match("git pull"));
        assert!(PUSH_FORCE_RE.is_match("git push --force origin x"));
        assert!(PUSH_FORCE_RE.is_match("git push origin +x"));
        assert!(!PUSH_FORCE_RE.is_match("git push origin x"));
    }

    #[test]
    fn protected_names_follow_the_environment() {
        let g = G::new(cfg::Env::from_pairs([("LOOP_PR_BASE", "staging"), ("LOOP_DATA_DIR", "C:\\no-such-dir")]));
        let p = Protected::new(&g);
        assert!(p.is_protected("staging") && p.is_protected("main"));
        assert!(p.name_re.is_match("push origin staging"));
        assert!(!p.name_re.is_match("push origin mainframe"));
        assert!(p.push_protected_re.is_match("git push origin main"));
        assert!(p.push_protected_re.is_match("git push --mirror"));
        assert!(!p.push_protected_re.is_match("git push origin feature/x"));
    }

    #[test]
    fn selected_patterns_match_like_the_reference() {
        assert!(SECRET_HINT.is_match("C:\\Users\\x\\.ssh\\id_rsa"));
        assert!(!SECRET_HINT.is_match("~/.ssh/config.example"));
        assert!(STATE_FILE.is_match(".specs/project/loop/json/LOOP.json"));
        assert!(PANEL_TRIGGER_RE.is_match("curl http://localhost:47801/p/abc/api/loop/start"));
        assert!(STATE_SCRIPT_RE.is_match("harness rules write x"));
        assert!(STATE_SCRIPT_RE.is_match("python harness.py audit --apply"));
        assert!(REDIRECT_RE.is_match("echo a > out.txt"));
        assert!(!REDIRECT_RE.is_match("echo a 2>&1"));
    }
}
