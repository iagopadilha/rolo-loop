// SPDX-License-Identifier: MIT
//! Typed state of the orchestrator graph and the pure parts of the engine around it.

pub mod audit_deps;
pub mod classify_structure;
pub mod code_inventory;
pub mod auto_answers;
pub mod context_files;
pub mod decisions;
pub mod delivery;
pub mod detect_setup;
pub mod email;
pub mod graph_view;
pub mod loop_io;
pub mod loop_state;
pub mod loop_summary;
pub mod network_policy;
pub mod notify;
pub mod prod_database_script;
pub mod project_repos;
pub mod repos;
pub mod roadmap_settings;
pub mod rules;
pub mod scale;
pub mod standards;
pub mod subprojects;
pub mod setup_approval;
pub mod setup_files;
pub mod system_map;
pub mod task_waves;
pub mod turn_gates;
pub mod worktree_state;

pub(crate) use notify::truthy as notify_truthy;
