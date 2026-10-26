// SPDX-License-Identifier: MIT
//! The services of the app. Every Python service class is a small view over the shared [`App`] (`app.xyz()` or `XyzService { app }`): the state
//! they own (job tables, caches, locks) lives in the `App`, so a service can call another one without the Python wiring dance.

use crate::config::{load_app_config, AppConfig, Overrides};
use crate::infrastructure::claude_client::ClaudeClient;
use crate::infrastructure::git_client::GitClient;
use crate::infrastructure::process_runner::{ProcessRunner, SystemProcesses};
use crate::infrastructure::structured_log::StructuredLog;
use crate::repositories::app_config::{AppConfigRepository, ProjectRepository};
use crate::repositories::claude_transcripts::ClaudeTranscriptRepository;
use crate::repositories::loop_command_file::LoopCommandFileRepository;
use crate::repositories::loop_file::LoopFileRepository;
use crate::repositories::project_files::{ChatJobRepository, ChatRepository};
use crate::repositories::small_stores::ErrorEventRepository;
use crate::repositories::specs_file::SpecsFileRepository;
use cfg::Env;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

pub mod claude_accounts;
pub mod claude_call_log;
pub mod claude_jobs;
pub mod code_inventory;
pub mod context_block;
pub mod planning_requirement;

use claude_accounts::ClaudeAccountService;
use claude_call_log::ClaudeCallLogService;
use code_inventory::CodeInventoryService;

/// Tables of the claude job service (one lock, like Python's `jobs_lock`).
#[derive(Default)]
pub struct JobTables {
    /// Job id -> repo path, in start order.
    pub running: Vec<(String, String)>,
    /// Job id -> parallel key, only for the jobs that may run next to others.
    pub parallel_keys: Vec<(String, String)>,
}

/// Everything the services share.
pub struct App {
    pub config: AppConfig,
    pub env: Env,
    pub log: StructuredLog,
    pub processes: Arc<dyn ProcessRunner>,
    pub git: GitClient,
    pub claude: ClaudeClient,
    pub specs: SpecsFileRepository,
    pub projects: ProjectRepository,
    pub settings: AppConfigRepository,
    pub accounts: Arc<ClaudeAccountService>,
    pub call_log: Arc<ClaudeCallLogService>,
    pub inventory: CodeInventoryService,
    pub chat_threads: ChatRepository,
    pub chat_jobs: ChatJobRepository,
    pub error_events: ErrorEventRepository,
    pub transcripts: ClaudeTranscriptRepository,
    pub loop_files: LoopFileRepository,
    pub loop_commands: LoopCommandFileRepository,
    pub job_tables: Mutex<JobTables>,
    /// Seconds a claude job may run (`DEFAULT_JOB_TIMEOUT_SECONDS`; tests shorten it).
    pub job_timeout_seconds: u64,
    /// URL of the running app (`http://localhost:<port>`), empty until the server is up.
    pub app_url: Mutex<String>,
    /// Project id -> (when, planning gates): the last gates computed, for the chat send's cheap refusal.
    pub gates_cache: Mutex<BTreeMap<String, (Instant, Value)>>,
    pub started_at: String,
    /// `MONITOR.json` path -> project id, and project id -> work dir where it was written.
    pub monitors: Mutex<(Vec<(String, String)>, BTreeMap<String, String>)>,
    me: Weak<App>,
}

impl App {
    pub fn new(env: &Env, overrides: &Overrides, processes: Arc<dyn ProcessRunner>) -> Arc<App> {
        let config = load_app_config(env, overrides);
        Self::from_config(config, processes)
    }

    pub fn from_config(config: AppConfig, processes: Arc<dyn ProcessRunner>) -> Arc<App> {
        let env = config.ctx.env.clone();
        let accounts = Arc::new(ClaudeAccountService::for_commands(&config.claude_binary, &config.claude_fallback_binaries));
        let call_log = Arc::new(ClaudeCallLogService::new(Some(accounts.clone())));
        let mut claude = ClaudeClient::new(
            env.clone(),
            processes.clone(),
            &config.claude_binary,
            config.ctx.guard.clone(),
            config.ctx.plugin_root.clone(),
            config.ctx.toolkit_root.clone(),
            config.claude_fallback_binaries.clone(),
        );
        let recorder = call_log.clone();
        claude.call_recorder = Some(Arc::new(move |folder: &str, kind: &str, result: &Value| recorder.record_call(folder, kind, result)));
        let git = GitClient::new(processes.clone());
        let log = StructuredLog::app(&config.logs_dir().display().to_string());
        Arc::new_cyclic(|me| App {
            inventory: CodeInventoryService::new(&config),
            projects: ProjectRepository::new(&config.projects_registry_path()),
            settings: AppConfigRepository::new(&config.user_data_dir),
            transcripts: ClaudeTranscriptRepository::new(&config.claude_transcripts_dir),
            loop_files: LoopFileRepository::new(env.clone()),
            env,
            log,
            processes,
            git,
            claude,
            specs: SpecsFileRepository::new(),
            accounts,
            call_log,
            chat_threads: ChatRepository,
            chat_jobs: ChatJobRepository::new(),
            error_events: ErrorEventRepository::new(),
            loop_commands: LoopCommandFileRepository,
            job_tables: Mutex::new(JobTables::default()),
            job_timeout_seconds: claude_jobs::DEFAULT_JOB_TIMEOUT_SECONDS,
            app_url: Mutex::new(String::new()),
            gates_cache: Mutex::new(BTreeMap::new()),
            started_at: crate::repositories::loop_command_file::now_iso(),
            monitors: Mutex::new((Vec::new(), BTreeMap::new())),
            config,
            me: me.clone(),
        })
    }

    /// The `Arc` this app lives in (jobs run in threads that need to own it).
    pub fn shared(&self) -> Arc<App> {
        self.me.upgrade().expect("the app is alive while a service runs")
    }

    pub fn app_url(&self) -> String {
        self.app_url.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_app_url(&self, url: &str) {
        *self.app_url.lock().unwrap_or_else(|e| e.into_inner()) = url.to_string();
    }

    pub fn planning_requirement(&self) -> planning_requirement::PlanningRequirementService<'_> {
        planning_requirement::PlanningRequirementService { app: self }
    }

    pub fn context_blocks(&self) -> context_block::ContextBlockService<'_> {
        context_block::ContextBlockService { code_inventory: Some(&self.inventory), log: Some(&self.log) }
    }

    /// An app on a temporary data dir with the real process runner (unit tests).
    #[cfg(test)]
    pub fn for_tests(dir: &std::path::Path) -> Arc<App> {
        Self::for_tests_with(dir, Arc::new(SystemProcesses))
    }

    #[cfg(test)]
    pub fn for_tests_with(dir: &std::path::Path, processes: Arc<dyn ProcessRunner>) -> Arc<App> {
        let data = dir.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let env = Env::from_pairs([("LOOP_DATA_DIR", data.to_str().unwrap()), ("LOOP_CLAUDE_HOME", data.to_str().unwrap()), ("LOOP_TOOLKIT_SIBLING", "off")]);
        Self::new(&env, &Overrides::default(), processes)
    }
}
