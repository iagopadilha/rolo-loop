# Changelog

All notable changes are listed here, newest first. The format follows Keep a Changelog and the versions follow Semantic Versioning.

## [Unreleased]

### Added

- First version of the Rust rebuild.
- Settings > "Skills and tools": every skill found on the machine with a checkbox, search, all/none (saves `loop_extra_skills`, now up to 400 items), a box per MCP server and per tool, and a warning when a root CLAUDE.md Standards section is ignored. New read route `GET /api/app/settings/skills`; names only, no secret leaves.
