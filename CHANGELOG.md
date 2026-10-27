# Changelog

All notable changes are listed here, newest first. The format follows Keep a Changelog and the versions follow Semantic Versioning.

## [Unreleased]

### Changed

- Panel: a generic step-by-step list (`components/stepper.js`) for guided flows; the Finish dialog now shows its steps in order (decisions, push, pull request, deliver) with one action at a time.
- Panel: the turns sit in a slim bar at the bottom of the loop view, with an always-visible "AI decisions" button that opens the dialog; the mouse wheel zooms every diagram (Shift + wheel pans).
- Engine: every child process (git, setup, `claude`, notices) starts without a console window, so nothing flashes on screen when an iteration starts.

### Added

- First version of the Rust rebuild.

### Fixed

- Guard false positives: `mkdir -p docs/codebase` (the folder itself, no trailing slash) passes for map agents; git-bash drive paths (`cd /c/...`) resolve to the real drive; read-only git subcommands (`check-ignore`, `check-attr`, `ls-tree`, `cat-file`, `rev-list`, `merge-base`, `name-rev`, `show-ref`, `for-each-ref`, `diff-tree`) are no longer treated as writes. Tests keep `python -I`, production hosts and credential paths blocked.
- One vocabulary for E2E results (`result_vocabulary`): the verifier's `PASS`/`FAIL`/`UNVERIFIED` and the ROADMAP `e2e=` words are mapped once; `e2e=pass|fail|unverified` are accepted and rewritten to `ok|failed|pending` before the state check instead of stopping the loop (R1, R5, R2). `pending` counts as work left in the final-phase gate.
- Cost forecast and cap ladder stored with the run (`cost_forecast`, `cost_cap_ladder`, `e2e_progress` in `meta.json`, documented in `docs/API.md`), one notice per cap when the forecast passes it, before the money is spent (R4, R6, P5 data side).
- The final E2E turn gets a budget sized by the number of features to check, so a serial run is not cut mid-way (R3).
- Preflight counts verified features without a final E2E result as work left, so "Try again" after a stop in the final phase is not refused (P4).
- A state-check failure now says what the turn already cost and that `--rewind` repeats it (R2).
- `--answer` is accepted when the loop is `paused`, `error` or `done`, not only `awaiting_user` (G8).
- The Errors tab strips terminal colour codes from turn stderr (A6).
- Live status in `GET state`: `processes` and `now` (what the engine is running: process kind, feature, step, start time, pid) and `meta.claude_events` (every switch of the Claude account, with its reason).
- Milestone notices when a turn verifies a feature or closes a stage (`LOOP_NOTIFY_MILESTONES=off` to disable).
- The loop re-reads the fallback Claude commands from the settings before every turn.
- Planning gates: warnings come apart from problems in the `ready` gate; `repos` and `import_coverage` show as not applicable instead of disappearing; a running loop with an approved plan no longer shows a stale red `ready` gate.

### Changed

- Notices name the features (with the running one marked) and the stage, no longer repeat the title, explain every known stop reason in plain language without command-line flags, cut text at a sentence or word boundary, and use one vocabulary: "verificadas" until the branch is pushed, "entregues" after. A paused notice no longer says "limite de custo" in its title.
- A roadmap with `Etapa N` headings is read as stages; a roadmap without readable stages shows "Features" instead of "Stage ?".
- The dispatch of a loop follows the ROADMAP recommendation (`--wave N`, "single chain") when no `Dispatch` line is set, and `--wave` is always passed to the engine.
