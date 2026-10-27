# Changelog

All notable changes are listed here, newest first. The format follows Keep a Changelog and the versions follow Semantic Versioning.

## [Unreleased]

### Changed

- Panel: a "Processes" card in the inspector lists what the engine runs right now (pid, turn, account, since when), built from `state.processes`.
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
