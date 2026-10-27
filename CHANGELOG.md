# Changelog

All notable changes are listed here, newest first. The format follows Keep a Changelog and the versions follow Semantic Versioning.

## [Unreleased]

### Added

- First version of the Rust rebuild.
- History hygiene: a new run's worktree no longer inherits turns, snapshots, answers or state of another run, and the baseline snapshot is rebuilt when it does not match the approved roadmap.
- A checkpoint commit that already holds a feature's paths now counts as that feature's commit; leftovers of a feature that just became verified are committed once, and the dirty-tree stop lists the exact files.
- Preflight: stale `index.lock` warning; a confirmed-monorepo extra repo is checked per subproject for the Conformance line.
- Planning: the chat may write `IMPORT-COVERAGE.md`, is told its real (fixed) permissions, and importing into an existing feature folder takes `on_existing` merge or replace.
- A map job that wrote nothing now names the guard refusals instead of blaming the skill.
