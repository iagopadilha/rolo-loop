# Changelog

All notable changes are listed here, newest first. The format follows Keep a Changelog and the versions follow Semantic Versioning.

## [Unreleased]

### Added

- First version of the Rust rebuild.
- Live status in `GET state`: `processes` and `now` (what the engine is running: process kind, feature, step, start time, pid) and `meta.claude_events` (every switch of the Claude account, with its reason).
- Milestone notices when a turn verifies a feature or closes a stage (`LOOP_NOTIFY_MILESTONES=off` to disable).
- The loop re-reads the fallback Claude commands from the settings before every turn.
- Planning gates: warnings come apart from problems in the `ready` gate; `repos` and `import_coverage` show as not applicable instead of disappearing; a running loop with an approved plan no longer shows a stale red `ready` gate.

### Changed

- Notices name the features (with the running one marked) and the stage, no longer repeat the title, explain every known stop reason in plain language without command-line flags, cut text at a sentence or word boundary, and use one vocabulary: "verificadas" until the branch is pushed, "entregues" after. A paused notice no longer says "limite de custo" in its title.
- A roadmap with `Etapa N` headings is read as stages; a roadmap without readable stages shows "Features" instead of "Stage ?".
- The dispatch of a loop follows the ROADMAP recommendation (`--wave N`, "single chain") when no `Dispatch` line is set, and `--wave` is always passed to the engine.
