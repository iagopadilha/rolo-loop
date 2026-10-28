// SPDX-License-Identifier: MIT
//! Creates the demo project (a real git repo with a full `.specs/`) and plays the turns that already happened, instantly.
//! Also resets the story (for `--loop`): back to the scaffold commit and the same seeded turns.

use super::catalog::{
    build_turn_plans, permissions, FEATURES, GOAL, LOOP_BRANCH, MAX_COST_USD, PROJECT_NAME, PROJECT_TEXT, RULES_TEXT, SCAFFOLD_BRANCH, SCAFFOLD_FOLDERS, SEEDED_ANSWER,
    SEEDED_QUESTION, SEEDED_TURNS, STATE_HEADER, TURN_TIMEOUT_MINUTES, VALIDATE_LINES, WAVE,
};
use super::player::{build_initial_meta, pending_features, render_roadmap, to_local_iso, AnswerProvider, DemoStoryPlayer};
use super::workspace::{remove_file, remove_tree, write_json_atomically, write_text_atomically, DemoGitRepository, DemoWorkspace};
use engine_state::loop_state;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Virtual time of the whole story ends around the real "now".
const STORY_LENGTH_BEFORE_NOW_MILLISECONDS: i64 = 60 * 60 * 1000;
/// The hidden subcommand that stands in for the engine scripts in the demo.
pub const ENGINE_STUB_SUBCOMMAND: &str = "demo-engine";
const CLAUDE_TEXT: &str = "# acme-checkout

Card checkout for the Acme store (demo project of agent-maestro).

Conformance: STANDARD

@.specs/project/RULES.md
";
const ARCHITECTURE_TEXT: &str = "# Architecture

- `services/*`: one Python service per bounded context, each with its own tests.
- `web/*`: React apps; shared components live in `web/shared/`.
- `infra/rate-limiter`: edge limits; `infra/prod/`: production values, applied by the platform team only.
- `migrations/`: reviewed SQL migrations.
";
const LOOP_STATE_ENTRIES: [&str; 10] = [
    "json/LOOP.json",
    "json/meta.json",
    "internal/turns",
    "internal/snapshots",
    "internal/guard.log",
    "internal/LOOP.lock",
    "internal/demo-answer.txt",
    "ANSWERS.md",
    "VALIDATE.md",
    "GRAPH.md",
];

pub fn now_milliseconds() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// LOOP-CMD.json of the demo: the panel's answer/resume relaunch the engine STUB (it only records the answer), never the real loop.
pub fn build_loop_command(repo_dir: &str, exe: &str) -> Value {
    json!({
        "argv": [exe, ENGINE_STUB_SUBCOMMAND, repo_dir, "--wave", WAVE.to_string(), "--max-cost", MAX_COST_USD.to_string(), "--timeout", TURN_TIMEOUT_MINUTES.to_string()],
        "cwd": repo_dir,
    })
}

pub struct DemoProjectSeeder {
    pub workspace: DemoWorkspace,
    pub repo_dir: String,
    pub git: DemoGitRepository,
    pub exe: String,
    scaffold_head: String,
    pub generation: u32,
}

impl DemoProjectSeeder {
    pub fn new(workspace: &DemoWorkspace, exe: &str) -> Self {
        DemoProjectSeeder {
            repo_dir: workspace.repo_dir.display().to_string(),
            git: DemoGitRepository::new(&workspace.repo_dir),
            workspace: workspace.clone(),
            exe: exe.to_string(),
            scaffold_head: String::new(),
            generation: 0,
        }
    }

    fn project_file(&self, name: &str) -> PathBuf {
        Path::new(&self.repo_dir).join(".specs").join("project").join(name)
    }

    fn repo_file(&self, relative: &str) -> PathBuf {
        relative.split('/').fold(PathBuf::from(&self.repo_dir), |path, part| path.join(part))
    }

    /// Creates the repo and plays the seeded turns. Returns the player positioned on the first animated step.
    pub fn seed(&mut self) -> Result<DemoStoryPlayer, String> {
        self.create_repository()?;
        self.start_story(false, None)
    }

    /// Back to the scaffold commit and a fresh `.specs/` loop state, then the seeded turns again (new session ids).
    pub fn reset_story(&mut self, keeps_lock: bool, answer_provider: Option<AnswerProvider>) -> Result<DemoStoryPlayer, String> {
        self.generation += 1;
        remove_file(&loop_state::loop_path(&self.repo_dir, "LOOP.lock"));
        self.git.reset_to(&self.scaffold_head)?;
        for entry in LOOP_STATE_ENTRIES {
            let path = entry.split('/').fold(loop_state::loop_dir(&self.repo_dir), |path, part| path.join(part));
            if path.is_dir() {
                remove_tree(&path);
            } else {
                remove_file(&path);
            }
        }
        remove_tree(&Path::new(&self.repo_dir).join(".specs").join("features"));
        remove_tree(&self.project_file("phases"));
        remove_tree(&self.workspace.transcripts_dir);
        self.start_story(keeps_lock, answer_provider)
    }

    fn start_story(&mut self, keeps_lock: bool, answer_provider: Option<AnswerProvider>) -> Result<DemoStoryPlayer, String> {
        let story_start = now_milliseconds() - STORY_LENGTH_BEFORE_NOW_MILLISECONDS;
        self.write_specs_baseline(story_start)?;
        let mut player = DemoStoryPlayer::new(&self.workspace, &self.git, build_turn_plans(), story_start, self.generation);
        player.play_until_turn(SEEDED_TURNS);
        let validate = format!("# Validate later\n\n{}\n", VALIDATE_LINES.join("\n"));
        write_text_atomically(&loop_state::state_file(&self.repo_dir, "VALIDATE.md"), &validate).map_err(|e| e.to_string())?;
        player.keeps_lock = keeps_lock;
        if let Some(provider) = answer_provider {
            player.answer_provider = provider;
        }
        Ok(player)
    }

    fn create_repository(&mut self) -> Result<(), String> {
        self.git.initialize(SCAFFOLD_BRANCH)?;
        let readme = format!("# {PROJECT_NAME}\n\n{GOAL}\n\nDemo project generated by `harness demo`.\n");
        let mut scaffold: Vec<(String, String)> = vec![
            ("README.md".into(), readme),
            (".gitignore".into(), ".specs/\n".into()),
            ("CLAUDE.md".into(), CLAUDE_TEXT.into()),
            ("docs/ARCHITECTURE.md".into(), ARCHITECTURE_TEXT.into()),
            ("infra/prod/terraform.tfvars".into(), "region = \"eu-west-1\"\nedge_rate_limit = 600\n".into()),
            ("migrations/0006_add_orders.sql".into(), "CREATE TABLE orders (id BIGINT PRIMARY KEY, total_cents BIGINT NOT NULL);\n".into()),
        ];
        for folder in SCAFFOLD_FOLDERS {
            let name = folder.rsplit('/').next().unwrap_or(folder);
            scaffold.push((format!("{folder}/README.md"), format!("# {name}\n\nPart of {PROJECT_NAME}.\n")));
        }
        let first: Vec<String> = ["README.md", ".gitignore", "CLAUDE.md"]
            .iter()
            .map(|s| s.to_string())
            .chain(scaffold.iter().map(|(p, _)| p.clone()).filter(|p| p.ends_with("/README.md")))
            .collect();
        for (relative, text) in &scaffold {
            write_text_atomically(&self.repo_file(relative), text).map_err(|e| e.to_string())?;
        }
        self.git.commit_paths(&first, "chore: scaffold services, web apps and infra folders")?;
        let rest: Vec<String> = scaffold.iter().map(|(p, _)| p.clone()).filter(|p| !first.contains(p)).collect();
        self.git.commit_paths(&rest, "docs: architecture notes, prod values and the orders migration")?;
        self.git.create_branch(LOOP_BRANCH)?;
        self.scaffold_head = self.git.get_head(false)?;
        Ok(())
    }

    /// PROJECT, ROADMAP (all pending), STATE, RULES, PERMISSIONS, LOOP-CMD, meta, ANSWERS, the approval and the turn 0 snapshot.
    fn write_specs_baseline(&self, story_start: i64) -> Result<(), String> {
        let io = |e: std::io::Error| e.to_string();
        debug_assert_eq!(pending_features().len(), FEATURES.len());
        write_text_atomically(&self.project_file("PROJECT.md"), PROJECT_TEXT).map_err(io)?;
        write_text_atomically(&self.project_file("ROADMAP.md"), &render_roadmap(&pending_features())).map_err(io)?;
        write_text_atomically(&self.project_file("STATE.md"), STATE_HEADER).map_err(io)?;
        write_text_atomically(&self.project_file("RULES.md"), RULES_TEXT).map_err(io)?;
        write_json_atomically(&loop_state::loop_path(&self.repo_dir, "PERMISSIONS.json"), &permissions()).map_err(io)?;
        write_json_atomically(&loop_state::loop_path(&self.repo_dir, "LOOP-CMD.json"), &build_loop_command(&self.repo_dir, &self.exe)).map_err(io)?;
        write_json_atomically(&loop_state::loop_path(&self.repo_dir, "meta.json"), &build_initial_meta(story_start)).map_err(io)?;
        let answers = format!("# Answers\n\n## Answer {} [applied]\nQuestion: {SEEDED_QUESTION}\nUser answer: {SEEDED_ANSWER}\n", to_local_iso(story_start));
        write_text_atomically(&loop_state::state_file(&self.repo_dir, "ANSWERS.md"), &answers).map_err(io)?;
        loop_state::approve_roadmap(&self.repo_dir, &to_local_iso(story_start))?;
        loop_state::snapshot(&self.repo_dir, 0, &self.git.get_head(false)?, None);
        Ok(())
    }
}
