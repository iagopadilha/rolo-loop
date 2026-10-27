# Changelog

All notable changes are listed here, newest first. The format follows Keep a Changelog and the versions follow Semantic Versioning.

## [Unreleased]

### Added

- First version of the Rust rebuild.
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
