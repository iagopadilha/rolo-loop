# Changelog

All notable changes are listed here, newest first. The format follows Keep a Changelog and the versions follow Semantic Versioning.

## [Unreleased]

### Added

- Panel: STANDARD and LEGACY buttons for the Conformance of each repository in the Workspace dialog (S5), same code as `PUT repos/{name}/conformance` (the primary uses `POST conformance`); the server's refusal is shown as is.
- Panel: importing a spec feature whose folder already exists asks Merge or Replace and sends `on_existing` (P3).
- Panel: a "Permissions" button and dialog list the permissions journal (`state.permissions_log`) and release a host or a cost limit with a reason, like `harness grant`. New route `POST /p/{project_id}/api/access/grant` (token); the journal redacts values and reasons.
- Panel: the cost limit form is in every stopped state (P5), says "limit saved" when only the relaunch failed (R6), and the bottom bar shows "E2E n of m done, k left" (R3) and a warning when the estimate passes the limit (R4).
- Panel: the navbar status of a finished loop opens the Finish dialog instead of a second summary (F3); a question opens by itself only when no other dialog is open (F4).

### Changed

- Panel: the Finish dialog shows the steps the engine sends (`GET wrap-up` `steps`) and each step's own action runs the engine call and re-reads the check.
- Engine: the engine owns every process it starts for a loop. They run in a Windows job object (crate `winproc`, the only unsafe code), are recorded with pid, start time and folder, programs a turn leaves running (a dev server, an app started for a check) are recorded after each turn, and all of them are stopped, by pid only, when the loop ends, is stopped, errors or Finish runs (FD12, FD8). A reused pid or the engine itself is never stopped.
- Engine: a worktree that cannot be removed names the cause (`files_in_use` with the programs by name and pid, or `folder_left`) and says nothing is unsafe; ignored `node_modules`, `bin` and `obj` go to a hidden background deletion with the long-path form so Finish does not wait (FD7, FD11). New `harness close-loop --stop-processes` and `--delete-leftover --repos <name>`.
- Finish tells the truth: the push is what the remote holds and the pull request is what GitHub lists; a `delivery.json` of another branch is ignored, a dry run is never kept as a state, and every red line carries the real reason in plain words and the action that fixes it (FD1, FD2, FD3, FD5, U2). The check no longer calls a pushed, delivered run "not safe".
- API: `GET wrap-up` returns the Finish as ordered `steps` and `finish_options`; new `POST wrap-up/deliver` (push and PR for every repo in one call), `POST wrap-up/stop-processes` and `POST close-loop/delete-leftover/{name}`; `commit-push` commits without the loop's `.specs/` state, pushes only the loop branch by refspec and answers with the fresh check (FD4, FD6, FD9, F1-F4 data side). See `docs/API.md`.
- The pull request body now has only what this PR delivers (verified features by title), the quarantined and unverified features, what to validate, the related PRs and one closing line; no commit list, no progress map (FD13). The production database script warning names the real branches and, for a design-time failure, how to generate the script by hand (FD5).
- The project is now called Agent Maestro (panel, notices subject `[Agent-Maestro]`, docs, repository links). The plugin id `claude-maestro` stays until the plugin is reinstalled under a new id.
- Panel: the loop diagram (waves, stages, features) is fixed: always fitted and centered, no pan, zoom or dragging; clicking a node still selects it.
- Panel: a "Validations" button in the bottom bar opens what the loop checks per feature (with proof paths) and what only the user can check, from `state.validations` and `state.manual_checks`.
- Panel: AI decision cards use plain words (what was unclear, what the AI chose, how to undo it) and each button says what it does ("I agree", "Ask to change it").
- Panel: the cost cap dialog shows the engine's estimate for the whole run and quick limit buttons from `meta.cost_cap_ladder`; the navbar cost meter tooltip carries the estimate.
- Panel: a "Processes" card in the inspector lists what the engine runs right now (pid, turn, account, since when), built from `state.processes`.
- Panel: a generic step-by-step list (`components/stepper.js`) for guided flows; the Finish dialog now shows its steps in order (decisions, push, pull request, deliver) with one action at a time.
- Panel: the turns sit in a slim bar at the bottom of the loop view, with an always-visible "AI decisions" button that opens the dialog; the mouse wheel zooms every diagram (Shift + wheel pans).
- Engine: every child process (git, setup, `claude`, notices) starts without a console window, so nothing flashes on screen when an iteration starts.
- The plugin id is now `agent-maestro` (was `claude-maestro`): plugin manifest, marketplace and agent references. The locator accepts both ids, so an installed `claude-maestro` keeps working. See "Migrating the installed plugin" in `docs/HOW_TO_USE.md`.

### Added

- E2E gate measures evidence, not labels (G6, G9, C4, VA1, VA4, V1-V4, Z1): `e2e=ok` needs an existing result artifact, `e2e=not-covered` needs a stated reason plus proof the feature has no surface, a refused call or a missing credential stops with `awaiting_user` (credential NAME, who gives it, what if declined, option "continue without this layer"). `GET state` gains `validations`, `manual_checks` and `loop.credential_request` (docs/API.md).
- First version of the Rust rebuild.
- E2E call forms published up front (W9, E6): `guard_core::call_forms` holds the accepted forms as data (literal URL or `*_URL`/`*_HOST`/`*_BASE` variable, no wrapper functions, no `python -S/-E/-I`, write a browser script before running it in a later call, stay inside a worktree, tokens through the runner's `exec`). The guard builds its reasons from the same constants, and a generated "How the E2E must call things" block sits in `plugin/agents/verifier-flow.md` and the orchestrator `SKILL.md`; a test fails when the block and the guard drift (`UPDATE_CALL_FORMS=1 cargo test -p guard-core call_forms` rewrites it).
- Refusal explainer: when the guard blocks a call form it now adds a line after the usual message that names the rule, the exact accepted form and says "this is a call form, not a missing permission"; `python -I` and the like say "refused on purpose". Only text is added: what is blocked is unchanged, so `tools/guard_diff.py` shows a longer stderr on those refusals only.
- `harness baseline [--dev-host ...] [--production-host ...] [--write --reason ...]`: a granted E2E baseline on request (dry run by default). It allows the localhost forms and exactly the development hosts the user lists, leaves the listed production hosts denied (a development entry that reaches one is refused), keeps a backup of `NETWORK.json`, never overwrites `TEST-ENVIRONMENT.md` and writes a skeleton with `FILL_IN` lines. No host is ever invented.
- Simple access release (C1 to C3): `engine_state::access_grant::grant_access(kind, value, reason)` and `harness grant --kind host|cost_limit --value ... --reason ...` release a network entry or raise the cost limit, copy the change into the running worktree, and refuse production hosts, a missing reason and a lower limit. Every grant and every refusal is a line (what, why, who, when, backup) in `.specs/project/loop/json/PERMISSIONS-LOG.json`.
- `state.permissions_log` in the panel state payload: the permissions journal, read-only (see `docs/API.md`).

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
- Settings > "Skills and tools": every skill found on the machine with a checkbox, search, all/none (saves `loop_extra_skills`, now up to 400 items), a box per MCP server and per tool, and a warning when a root CLAUDE.md Standards section is ignored. New read route `GET /api/app/settings/skills`; names only, no secret leaves.
- History hygiene: a new run's worktree no longer inherits turns, snapshots, answers or state of another run, and the baseline snapshot is rebuilt when it does not match the approved roadmap.
- A checkpoint commit that already holds a feature's paths now counts as that feature's commit; leftovers of a feature that just became verified are committed once, and the dirty-tree stop lists the exact files.
- Preflight: stale `index.lock` warning; a confirmed-monorepo extra repo is checked per subproject for the Conformance line.
- Planning: the chat may write `IMPORT-COVERAGE.md`, is told its real (fixed) permissions, and importing into an existing feature folder takes `on_existing` merge or replace.
- A map job that wrote nothing now names the guard refusals instead of blaming the skill.
- Installs and setup (feature `installs-and-setup`): the engine runs routine installs itself, per subproject (the root and each folder one level
  down), cached by lockfile hash: `npm ci` / `pnpm` / `yarn` only where a `package.json` sits beside the lockfile, `dotnet restore` at the
  solution or project found one level down, `pip install -r`, `poetry install`, `go mod download`, `cargo fetch --locked`, `composer`,
  `bundle`. The ecosystems are a data table in `detect_setup.rs`. The guard no longer treats those exact commands as "a new dependency"
  (`npm install pkg`, `pip install pkg`, `dotnet add package` stay refused). A start that fails after creating a worktree removes the
  worktree and the new branch. Line-ending-only (CRLF) changes no longer count as changed or dirty files; the preflight reports a stale
  `index.lock` with the file, its age and what to do; the app offers a `.gitattributes` LF rule as a setup file and all the `.gitignore`
  lines it writes are recognised as setup lines.

### Changed

- The "other changed files are NOT carried" notices say what to do next. The Node preflight warning is now only about a `package.json` with no
  lockfile; the .NET restore warning is gone because the engine restores.
