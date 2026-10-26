// SPDX-License-Identifier: MIT
//! Domain errors of the app. None of them knows HTTP: they say what happened, not how to answer (the controllers map each kind to a status).

use serde_json::{Map, Value};

pub const PLANNING_REQUIRED_CODE: &str = "planning_required";
pub const PLANNING_BUSY_CODE: &str = "planning_busy";
pub const PLANNING_PREREQUISITES_CODE: &str = "planning_prerequisites";
pub const PLANNING_GATES_CODE: &str = "planning_gates";
pub const LOOP_RUNNING_CODE: &str = "loop_running";

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Project id the registry does not know.
    ProjectNotFound,
    /// The file changed since it was read (different sha1 version).
    FileVersionConflict,
    /// A folder, name or path that does not work.
    InvalidPath,
    /// A subproject operation before the list was confirmed.
    SubprojectsNotConfirmed,
    /// Incomplete or malformed request body.
    InvalidRequest,
    /// File path outside `.specs/` or in a forbidden folder.
    PathOutsideSpecs,
    /// The file exists but the app cannot edit it.
    FileEditRefused,
    /// The requested file inside `.specs/` does not exist.
    SpecFileNotFound,
    /// Unknown (or invalid) job id in the project.
    JobNotFound,
    /// The operation cannot happen while the loop is running.
    LoopRunning,
    /// A claude job is already running in this project.
    JobInProgress,
    /// Refining without a previous roadmap job with a claude session.
    RoadmapSessionMissing,
    /// Generating a roadmap in the repo while the loop worktree already exists.
    LoopWorktreeExists,
    /// ROADMAP outside the grammar or without features.
    InvalidRoadmap,
    /// Creating from scratch in a folder that already exists with content.
    ProjectAlreadyExists,
    /// The project is in planning mode.
    PlanningRequired,
    /// Planning gates that are not ok (`gates` carries the checklist).
    PlanningGates,
    /// A repo that cannot be listed or a bad repo/copy value.
    InvalidRepo,
    /// Repo short name the project does not list.
    RepoNotFound,
    /// Refused by state (a loop runs, the primary repo cannot be removed...).
    RepoConflict,
    /// The repo is used by the running loop of ANOTHER project.
    RepoInUse,
    /// A test-environment value for a name TEST-ENVIRONMENT.md does not mention.
    UnknownCredentialName,
    /// The settings key is unknown or the value does not fit it.
    InvalidSetting,
    /// The setting cannot be turned on in this installation.
    SettingUnavailable,
    /// config.json exists but is not a JSON object.
    ConfigFileUnreadable,
}

/// An error with a machine code and extra response fields.
#[derive(Clone, Debug)]
pub struct AppError {
    pub kind: Kind,
    pub message: String,
    pub code: Option<String>,
    pub extras: Map<String, Value>,
    pub current_version: Option<String>,
    pub gates: Option<Value>,
}

pub type Result<T> = std::result::Result<T, AppError>;

impl AppError {
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        AppError { kind, message: message.into(), code: None, extras: Map::new(), current_version: None, gates: None }
    }

    pub fn with_code(mut self, code: &str) -> Self {
        self.code = Some(code.to_string());
        self
    }

    pub fn with_extra(mut self, key: &str, value: Value) -> Self {
        self.extras.insert(key.to_string(), value);
        self
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(Kind::InvalidRequest, message)
    }

    pub fn invalid_path(message: impl Into<String>) -> Self {
        Self::new(Kind::InvalidPath, message)
    }

    pub fn project_not_found() -> Self {
        Self::new(Kind::ProjectNotFound, "project not found")
    }

    pub fn loop_running(message: impl Into<String>) -> Self {
        Self::new(Kind::LoopRunning, message)
    }

    pub fn version_conflict(message: impl Into<String>, current_version: &str) -> Self {
        let mut e = Self::new(Kind::FileVersionConflict, message);
        e.current_version = Some(current_version.to_string());
        e
    }

    pub fn planning_gates(message: impl Into<String>, gates: Value, code: &str) -> Self {
        let mut e = Self::new(Kind::PlanningGates, message).with_code(code);
        e.gates = Some(gates);
        e
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}
