# Changelog

All notable changes are listed here, newest first. The format follows Keep a Changelog and the versions follow Semantic Versioning.

## [Unreleased]

### Added

- First version of the Rust rebuild.

### Fixed

- Guard false positives: `mkdir -p docs/codebase` (the folder itself, no trailing slash) passes for map agents; git-bash drive paths (`cd /c/...`) resolve to the real drive; read-only git subcommands (`check-ignore`, `check-attr`, `ls-tree`, `cat-file`, `rev-list`, `merge-base`, `name-rev`, `show-ref`, `for-each-ref`, `diff-tree`) are no longer treated as writes. Tests keep `python -I`, production hosts and credential paths blocked.
