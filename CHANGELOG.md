# Changelog

All notable changes are listed here, newest first. The format follows Keep a Changelog and the versions follow Semantic Versioning.

## [Unreleased]

### Added

- UI v2 of the panel: sidebar shell with Ctrl+K quick switcher, projects table with system strip, loop timeline (timeline / graph / list) with inspector, feature page (`#feature=<slug>`) with an Ask panel, v2 dialogs and settings, splash. Light, dark and system themes (`maestro.theme`), applied before first paint. Displayed paths mask the user home folder; the panel makes no external request.
- `tools/e2e/ui_v2_check.js`: layout, contrast, overflow and leak checks over themes and viewports.
- First version of the Rust rebuild.
