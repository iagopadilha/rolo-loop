# Changelog

All notable changes are listed here, newest first. The format follows Keep a Changelog and the versions follow Semantic Versioning.

## [Unreleased]

### Added

- Lean agent pipeline (Anthropic, "When to fold subagents into your main agent loop", 2026-10-22): one strong loop, context and tools on demand, more agents only where planned or measured.
  - One `executor` agent replaces `executor-back`, `executor-front` and `executor-generic`; the prompt carries `KIND:` and the skills. The old names stay mapped in the guard and in the panel for old installs and run history.
  - `harness verify <root> --feature <slug>`: verification as code (the toolkit's `craft verify`), so the `verifier` agent is only a fallback when the toolkit is missing.
  - New `quality-gate` agent: reviews the feature's diff once, at the end, against the standards of the skills; `BLOCKED` goes back to the executor once, a second time to quarantine. The deterministic rules run first in code (`craft standards check`, claude-craft-kit).
  - Optional planning fields on a ROADMAP line: `split`, `gate`, `quality`, `models`, `reads`, `tools`. All default to today's behavior and a bad value is a warning, never a stop: `harness state plan <root> [slug]`.
  - `harness metrics`: size of every agent file and of the orchestrator skill, the measurement before any claim of savings. Agent files shrank from 87329 to 56392 bytes in total.
  - Guard: a bare `npm install` / `pnpm install` / `yarn install` / `bun install` (flags only, no package) is a routine install and is not blocked.
- Panel: STANDARD and LEGACY buttons for the Conformance of each repository in the Workspace dialog (S5), same code as `PUT repos/{name}/conformance` (the primary uses `POST conformance`); the server's refusal is shown as is.
- Panel: importing a spec feature whose folder already exists asks Merge or Replace and sends `on_existing` (P3).
- Panel: a "Permissions" button and dialog list the permissions journal (`state.permissions_log`) and release a host or a cost limit with a reason, like `harness grant`. New route `POST /p/{project_id}/api/access/grant` (token); the journal redacts values and reasons.
- Panel: the cost limit form is in every stopped state (P5), says "limit saved" when only the relaunch failed (R6), and the bottom bar shows "E2E n of m done, k left" (R3) and a warning when the estimate passes the limit (R4).
- Panel: the navbar status of a finished loop opens the Finish dialog instead of a second summary (F3); a question opens by itself only when no other dialog is open (F4).

### Changed
- The standard development command (`dotnet run`, `npm run dev`, ...) is released by default and asks nothing. Migrations still ask once; production is never released.

- Readiness: a well-known root file (CLAUDE.md, README, licence, CI folders, repo settings) no longer warns as "outside any subproject" (W2); the `auto` permission mode is probed from the `claude` help and nothing is said when it works (W4); the forgotten-dependencies warning points at the one-click apply (W3).
- Panel: a stopped loop always shows one fixed "Continue" button in the pause banner, and the status dialog opens once per question even across page reloads (P6).
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

- Policies for the loop (D1, D2, E5, E6, W8, H7, C3, X1, M1, R4/P5): a standard command the project did not release **asks**, and asking never stops the run. The guard detects the command by CLASS, for any stack (apply a migration, create a migration, start the app in development: `dotnet ef`, `prisma`, `alembic`, `manage.py`, `rails`, `artisan`, `npm run dev`, `dotnet run`...), refuses that one command with "WAITING FOR THE USER'S APPROVAL" and writes one request; the agents carry on with the work that does not depend on it. The panel shows one card ("Waiting for your approval") in the AI decisions dialog with Approve, Approve always for this project and Deny; the run resumes by itself once the last request is answered. Production is never released: a production target is refused by `migration-target`, now a rule `PERMISSIONS.json` cannot release, and gets no card.
- Simple per-project policy in `PERMISSIONS.json`: `commands` (classes `migrate_apply`, `migrate_create`, `dev_run`, the alias `migrations`, or literal commands such as `npm run dev`) release commands on DEV without asking; `e2e_dev_baseline` is the optional per-project DEV opt-in of the E2E baseline (off by default; the engine applies the baseline with the listed hosts before each turn, production hosts stay denied). Every answer and change is a line in the permissions journal. New routes `GET approvals`, `POST approvals/{id}/approve|deny`, `GET policy`, `POST policy` (docs/API.md); `GET state` gains `approvals`.
- Telemetry opt-out at the source (X1): every process the engine starts for a loop gets the standard opt-out variables of the common toolchains (`DOTNET_CLI_TELEMETRY_OPTOUT`, `NEXT_TELEMETRY_DISABLED`, `DO_NOT_TRACK`, update notifiers and others; `turn_environment::TELEMETRY_OPT_OUT`). A value the user already set is kept. No host is dropped from the guard log or added to the network list.
- One-click cost limit (R4, P5): `POST loop/raise-limit` also takes `{suggested: true}` (the engine's suggested rung, else the first rung above the limit in force) and the quick-limit buttons of the cost dialog now raise and continue in one click. The default limit stays US$ 50. `python -I` stays refused on purpose, with the refusal explainer.
- Map jobs of a repo that is not the primary read the confirmed subprojects from the primary's `.specs` through `LOOP_PRIMARY_SPECS` instead of a copy in the other repo (M1).
- `harness resume <repo> [--instruction TEXT]` resumes a stopped loop from outside the panel with the panel's own checks on the saved command and no token; `--instruction` is `--answer` for every stopped state (H5, G8).
- `GET state` `guard_waiting`: which install the guard refused and which feature waits for it, shown in the pause banner (I5). `GET loop/settings` gains `scripts` (the project's own scripts, safe or ask-once, with the reason a turn may wait; `POST scripts/answer` remembers an answer, I3) and `setup` (the detected Setup line, I6).
- `POST dependencies/apply` and `graph.dependency_proposals`: the forgotten dependencies shown and applied in one call (W3).
- The package registries of the stacks a project uses (node, python, dotnet with the NuGet CDN, go, rust, java, php, ruby) are readable (GET/HEAD) by default (I7).
- Settings lists the main and backup Claude commands with the account each signs in to (A3); every change of the running account reaches every notice channel (A2); the account wrapper's status table on a good turn is an info item, not a stderr complaint (A5).
- Validation report in `GET state` (`validation_report`): failed and pending checks grouped by cause (VA2), the validation plan as a versioned file per feature with change tracking (VA3), cost and attempts per feature with an estimate of what is left (VA5), a review of the checks (requirements with no case, weak cases; VA6), findings classed `fix-now`/`decide`/`manual` (Z2), `fix-now` only with the written intended behaviour and the wrong side named (Z4), and the test data the run left behind (Z3).
- A turn that changes an `e2e=` result without writing any of its listed evidence stops the run (G7). A feature `checkpoint=` is now a deliberate pause, `loop.pause_kind = validation_checkpoint`, with its own notice title (V3).
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
