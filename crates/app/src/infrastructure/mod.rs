// SPDX-License-Identifier: MIT
//! The outside world: processes, git, the claude CLI, the log, activity.

pub mod activity_monitor;
pub mod claude_client;
pub mod git_client;
pub mod process_runner;
pub mod structured_log;
pub mod workspace_file_guard;
