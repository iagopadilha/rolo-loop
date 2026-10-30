// SPDX-License-Identifier: MIT
//! The per-project command policy: which standard commands need the user's approval, how a pending approval is written down and how the
//! user's answer is read back. Stack-agnostic: a command CLASS names what the command does (apply a migration, create a migration, start the
//! app in development), whatever the language or tool; the detectors below only map known tools to a class.
//!
//! * Default policy: `ask`. A command of a class that the project did not release is refused with a "waiting for approval" message and ONE
//!   request is written to `loop/internal/approvals-requests.jsonl` (no secret: the command text is masked). The agent carries on with other work.
//! * The project releases a class with a simple list in `PERMISSIONS.json`: `"commands": ["migrations", "dev_run", "npm run dev"]` (a class id,
//!   the alias `migrations`, or a literal command prefix). `dev_run` (the standard dev command) is released by default.
//! * The user answers a single request with `loop/json/approvals.json` (written by the app): `approved` or `denied` for that request id.
//! * PRODUCTION is never released here: the check that calls this module refuses a production target first, with a rule `PERMISSIONS.json`
//!   cannot release, and no request is raised for it.

use crate::pyre::Re;
use crate::state::{loop_file, now_local_stamp, split_commands};
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

pub const REQUESTS_FILE: &str = "approvals-requests.jsonl";
pub const DECISIONS_FILE: &str = "approvals.json";
pub const APPROVED: &str = "approved";
pub const DENIED: &str = "denied";
pub const CLASS_MIGRATE_APPLY: &str = "migrate_apply";
pub const CLASS_MIGRATE_CREATE: &str = "migrate_create";
pub const CLASS_DEV_RUN: &str = "dev_run";
/// The names a project may put in `commands` besides a literal command prefix.
pub const CLASS_IDS: [&str; 3] = [CLASS_MIGRATE_APPLY, CLASS_MIGRATE_CREATE, CLASS_DEV_RUN];
pub const MIGRATIONS_ALIAS: &str = "migrations";
pub const COMMAND_LIMIT: usize = 140;

/// What a class does, in plain words (the text of the approval card and of the refusal).
pub fn plain(class: &str) -> &'static str {
    match class {
        CLASS_MIGRATE_APPLY => "apply a database migration",
        CLASS_MIGRATE_CREATE => "create a database migration",
        CLASS_DEV_RUN => "start the app with its standard development command",
        _ => "run a command that needs your approval",
    }
}

const PRE: &str = r#"^(?:[A-Za-z_]\w*=(?:"[^"]*"|'[^']*'|\S*)\s+)*(?:(?:npx|bunx|pnpm\s+(?:dlx|exec)|yarn\s+dlx|python\d*(?:\.\d+)?(?:\s+-m)?|py(?:\s+-3)?|uv\s+run|poetry\s+run|bundle\s+exec)\s+)?(?:\S*[\\/])?"#;

fn detector(body: &str) -> Re {
    Re::ci(&format!("{PRE}(?:{body})"))
}

static APPLY: LazyLock<Re> = LazyLock::new(|| {
    detector(
        r"dotnet(?:\.exe)?\s+ef\s+database\s+update\b|dotnet(?:\.exe)?\s+run\b[^\n]*?\s--\s+migrate\w*|prisma\s+(?:migrate\s+(?:deploy|dev|reset)|db\s+push)\b|knex\s+migrate:(?:latest|up|rollback)\b|sequelize(?:-cli)?\s+db:migrate\b|typeorm\s+migration:(?:run|revert)\b|(?:npm|pnpm|yarn|bun)\s+(?:run\s+)?(?:db:)?migrate(?::\w+)?\b|alembic\s+(?:upgrade|downgrade)\b|(?:\S+[\\/])?manage\.py\s+migrate\b|flask\s+db\s+upgrade\b|(?:rails|rake)\s+db:migrate\b|(?:php\s+)?artisan\s+migrate\b|flyway\s+migrate\b|liquibase\s+update\b|goose\b[^\n]*\s(?:up|down)\b|diesel\s+migration\s+run\b|sqlx\s+migrate\s+run\b",
    )
});
static CREATE: LazyLock<Re> = LazyLock::new(|| {
    detector(
        r"dotnet(?:\.exe)?\s+ef\s+migrations\s+(?:add|remove|script|bundle)\b|alembic\s+revision\b|(?:\S+[\\/])?manage\.py\s+makemigrations\b|flask\s+db\s+migrate\b|rails\s+g(?:enerate)?\s+migration\b|(?:php\s+)?artisan\s+make:migration\b|typeorm\s+migration:(?:generate|create)\b|knex\s+migrate:make\b|sequelize(?:-cli)?\s+migration:generate\b|diesel\s+migration\s+generate\b|sqlx\s+migrate\s+add\b",
    )
});
static DEV_RUN: LazyLock<Re> = LazyLock::new(|| {
    detector(
        r"dotnet(?:\.exe)?\s+(?:run|watch)\b|(?:npm|pnpm|bun)\s+(?:run\s+)?(?:dev|start|serve)\b|yarn\s+(?:run\s+)?(?:dev|start|serve)\b|(?:next|nuxt)\s+dev\b|ng\s+serve\b|flask\s+run\b|uvicorn\b|(?:\S+[\\/])?manage\.py\s+runserver\b|rails\s+s(?:erver)?\b|(?:php\s+)?artisan\s+serve\b",
    )
});

/// The class of one command statement (`None`: a command that never needs approval).
pub fn classify_statement(statement: &str) -> Option<&'static str> {
    let text = statement.trim();
    if APPLY.is_match(text) {
        Some(CLASS_MIGRATE_APPLY)
    } else if CREATE.is_match(text) {
        Some(CLASS_MIGRATE_CREATE)
    } else if DEV_RUN.is_match(text) {
        Some(CLASS_DEV_RUN)
    } else {
        None
    }
}

/// Every classified statement of a shell command, in order: `(class, statement)`.
pub fn classify_command(command: &str) -> Vec<(&'static str, String)> {
    split_commands(command).into_iter().filter_map(|s| classify_statement(&s).map(|c| (c, s))).collect()
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The project's releases, as read from `PERMISSIONS.json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectCommands {
    pub commands: Vec<String>,
    /// `"e2e_dev_baseline": {"enabled": true}` (or plain `true`): the project opted in to the DEV baseline, which also lets the app start in DEV.
    pub e2e_dev_baseline: bool,
}

impl ProjectCommands {
    pub fn from_json(data: &Map<String, Value>) -> ProjectCommands {
        let commands = data
            .get("commands")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_str).map(squash).filter(|c| !c.is_empty()).collect())
            .unwrap_or_default();
        let e2e_dev_baseline = match data.get("e2e_dev_baseline") {
            Some(Value::Bool(b)) => *b,
            Some(Value::Object(o)) => o.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            _ => false,
        };
        ProjectCommands { commands, e2e_dev_baseline }
    }

    /// True when the project released this class or this exact statement.
    pub fn releases(&self, class: &str, statement: &str) -> bool {
        // Starting the app with its standard dev command (`dotnet run`, `npm run dev`) is released by default: it asks nothing. Only a production
        // target is refused, and that is the hard `migration-target` rule, which this list cannot release.
        if class == CLASS_DEV_RUN {
            return true;
        }
        let statement = squash(statement).to_lowercase();
        self.commands.iter().any(|entry| {
            let entry = entry.to_lowercase();
            if entry == class || (entry == MIGRATIONS_ALIAS && (class == CLASS_MIGRATE_APPLY || class == CLASS_MIGRATE_CREATE)) {
                return true;
            }
            !CLASS_IDS.contains(&entry.as_str()) && entry != MIGRATIONS_ALIAS && (statement == entry || statement.starts_with(&format!("{entry} ")))
        })
    }
}

/// A short copy of the command without anything that looks like a secret: `NAME=value` where the name looks secret, a connection argument, a
/// long token. Safe to write to a file the panel shows.
pub fn mask_command(command: &str) -> String {
    static SECRET_ASSIGN: LazyLock<Re> =
        LazyLock::new(|| Re::ci(r#"((?:[A-Za-z0-9_]*(?:PASSWORD|PASSWD|PWD|SECRET|TOKEN|KEY|CREDENTIAL|CONNECTION|DSN|URL)[A-Za-z0-9_:]*)=)("[^"]*"|'[^']*'|\S+)"#));
    static CONNECTION_ARG: LazyLock<Re> =
        LazyLock::new(|| Re::ci(r#"(--(?:connection|connection-string|connectionstring|password|token|secret)(?:\s+|=))("[^"]*"|'[^']*'|\S+)"#));
    static LONG_TOKEN: LazyLock<Re> = LazyLock::new(|| Re::new(r"[A-Za-z0-9+/=_-]{24,}"));
    let mut out = squash(command);
    for re in [&*SECRET_ASSIGN, &*CONNECTION_ARG] {
        let mut result = String::new();
        let mut last = 0;
        for m in re.find_all(&out) {
            result.push_str(&out[last..m.start]);
            result.push_str(m.group(1).unwrap_or(""));
            result.push_str("***");
            last = m.end;
        }
        result.push_str(&out[last..]);
        out = result;
    }
    let out = LONG_TOKEN.replace_lit(&out, "***");
    out.chars().take(COMMAND_LIMIT).collect()
}

/// Stable id of a request: the class plus the masked, whitespace-squashed statement (FNV-1a, 64 bits).
pub fn request_id(class: &str, statement: &str) -> String {
    let text = format!("{class}\u{1}{}", mask_command(statement));
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("ap-{hash:016x}")
}

/// One thing the loop wants to do and may not yet.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub id: String,
    pub class: &'static str,
    pub command: String,
}

impl Request {
    pub fn new(class: &'static str, statement: &str) -> Request {
        Request { id: request_id(class, statement), class, command: mask_command(statement) }
    }
}

/// The user's answers: `{id: {"decision": "approved"|"denied", ...}}` from `approvals.json` (`{"items": {...}}`).
pub fn read_decisions(specs_dir: &str) -> Map<String, Value> {
    let path = loop_file(specs_dir, DECISIONS_FILE);
    let data = std::fs::read(path).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok());
    match data.as_ref().and_then(|d| d.get("items")).and_then(Value::as_object) {
        Some(items) => items.clone(),
        None => Map::new(),
    }
}

/// `Some("approved"|"denied")` when the user answered this request.
pub fn decision_of(specs_dir: &str, id: &str) -> Option<String> {
    read_decisions(specs_dir).get(id).and_then(|v| v.get("decision")).and_then(Value::as_str).map(String::from)
}

/// Every line of the requests file (oldest first); a broken line is skipped.
pub fn read_requests(specs_dir: &str) -> Vec<Value> {
    let path = loop_file(specs_dir, REQUESTS_FILE);
    let text = std::fs::read(path).ok().map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(Value::is_object).collect()
}

/// Writes the request once (a request already in the file is left alone). Never raises.
pub fn register_request(specs_dir: &str, request: &Request, turn: Option<u64>, slug: Option<&str>, profile: &str) {
    if read_requests(specs_dir).iter().any(|r| r["id"] == request.id) {
        return;
    }
    let line = json!({
        "id": request.id, "class": request.class, "what": plain(request.class), "env": "DEV", "command": request.command,
        "turn": turn, "slug": slug.unwrap_or(""), "profile": profile, "at": now_local_stamp(),
    });
    let path = loop_file(specs_dir, REQUESTS_FILE);
    if let Some(parent) = Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_are_detected_for_every_stack_and_only_at_the_start_of_a_statement() {
        let cases = [
            ("dotnet ef database update --project Api", CLASS_MIGRATE_APPLY),
            ("ASPNETCORE_ENVIRONMENT=Development dotnet ef database update", CLASS_MIGRATE_APPLY),
            ("npx prisma migrate deploy", CLASS_MIGRATE_APPLY),
            ("npm run db:migrate", CLASS_MIGRATE_APPLY),
            ("python manage.py migrate", CLASS_MIGRATE_APPLY),
            ("alembic upgrade head", CLASS_MIGRATE_APPLY),
            ("bundle exec rails db:migrate", CLASS_MIGRATE_APPLY),
            ("php artisan migrate", CLASS_MIGRATE_APPLY),
            ("dotnet ef migrations add Init", CLASS_MIGRATE_CREATE),
            ("alembic revision --autogenerate -m x", CLASS_MIGRATE_CREATE),
            ("python manage.py makemigrations", CLASS_MIGRATE_CREATE),
            ("dotnet run --project Api", CLASS_DEV_RUN),
            ("dotnet watch run", CLASS_DEV_RUN),
            ("npm run dev", CLASS_DEV_RUN),
            ("pnpm dev", CLASS_DEV_RUN),
            ("yarn start", CLASS_DEV_RUN),
            ("flask run", CLASS_DEV_RUN),
        ];
        for (command, class) in cases {
            assert_eq!(classify_statement(command), Some(class), "{command}");
        }
        for command in ["dotnet build", "dotnet test", "npm run build", "npm test", "git commit -m 'npm run dev'", "echo dotnet run", "ls", "dotnet ef migrations list"] {
            assert_eq!(classify_statement(command), None, "{command}");
        }
        assert_eq!(classify_command("cd api && dotnet run && echo ok"), vec![(CLASS_DEV_RUN, "dotnet run".to_string())]);
        assert_eq!(classify_command("dotnet run --project A -- migrate")[0].0, CLASS_MIGRATE_APPLY);
    }

    #[test]
    fn the_project_list_releases_a_class_an_alias_or_a_literal_prefix() {
        let parse = |text: &str| ProjectCommands::from_json(serde_json::from_str::<Value>(text).unwrap().as_object().unwrap());
        let none = parse("{}");
        assert!(none.releases(CLASS_DEV_RUN, "dotnet run"), "the standard dev command is released by default");
        assert!(!none.releases(CLASS_MIGRATE_APPLY, "dotnet ef database update"));
        let list = parse(r#"{"commands": ["Migrations", "npm  run dev", "dev_run_other"]}"#);
        assert!(list.releases(CLASS_MIGRATE_APPLY, "dotnet ef database update") && list.releases(CLASS_MIGRATE_CREATE, "alembic revision"));
        assert!(list.releases(CLASS_DEV_RUN, "npm run dev -- --port 3000"));
        let by_id = parse(r#"{"commands": ["dev_run"]}"#);
        assert!(by_id.releases(CLASS_DEV_RUN, "anything") && !by_id.releases(CLASS_MIGRATE_APPLY, "dotnet ef database update"));
        assert!(parse(r#"{"e2e_dev_baseline": {"enabled": true}}"#).releases(CLASS_DEV_RUN, "dotnet run"));
        assert!(!parse(r#"{"e2e_dev_baseline": {"enabled": true}}"#).releases(CLASS_MIGRATE_APPLY, "dotnet ef database update"));
        assert!(!parse(r#"{"e2e_dev_baseline": false}"#).e2e_dev_baseline);
    }

    #[test]
    fn a_request_keeps_no_secret_and_is_written_once() {
        let masked = mask_command(
            "DB_PASSWORD=hunter2 DATABASE_URL=postgres://u:p@h/db dotnet ef database update --connection 'Server=x;Password=y' --token abcdefghijklmnopqrstuvwxyz0123",
        );
        assert!(!masked.contains("hunter2") && !masked.contains("postgres://") && !masked.contains("Password=y") && !masked.contains("abcdefghijkl"), "{masked}");
        assert!(masked.contains("dotnet ef database update"), "{masked}");
        let dir = tempfile::tempdir().unwrap();
        let specs = dir.path().join(".specs");
        std::fs::create_dir_all(&specs).unwrap();
        let specs = specs.display().to_string();
        let request = Request::new(CLASS_MIGRATE_APPLY, "dotnet  ef database update");
        assert_eq!(request.id, Request::new(CLASS_MIGRATE_APPLY, "dotnet ef database update").id);
        assert_ne!(request.id, Request::new(CLASS_MIGRATE_CREATE, "dotnet ef database update").id);
        register_request(&specs, &request, Some(3), Some("feat-a"), "executor");
        register_request(&specs, &request, Some(4), Some("feat-a"), "executor");
        let lines = read_requests(&specs);
        assert_eq!(lines.len(), 1);
        assert_eq!((lines[0]["env"].as_str(), lines[0]["turn"].as_u64(), lines[0]["slug"].as_str()), (Some("DEV"), Some(3), Some("feat-a")));
        assert_eq!(decision_of(&specs, &request.id), None);
        let file = loop_file(&specs, DECISIONS_FILE);
        std::fs::create_dir_all(Path::new(&file).parent().unwrap()).unwrap();
        std::fs::write(&file, format!(r#"{{"items": {{"{}": {{"decision": "approved"}}}}}}"#, request.id)).unwrap();
        assert_eq!(decision_of(&specs, &request.id).as_deref(), Some(APPROVED));
    }
}
