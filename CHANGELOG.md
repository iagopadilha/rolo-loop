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
