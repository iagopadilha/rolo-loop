# Architecture

One Rust workspace builds two executables: `harness` (engine, panel server, CLI) and `guard` (the hook). The plugin folder holds the texts Claude Code loads: the orchestrator skill, the satellite skills and the eight agents.

```
plugin/                 skill, agents, hooks.json (calls bin/guard.exe)
app/frontend/           the panel: Preact + htm + CSS, served as static files
app/templates/          .specs templates the panel and the skill copy into projects
crates/
  cfg                   configuration sources and precedence, locating plugin, skill, guard and kit
  pycompat              Python behaviors the contracts depend on (regex, json, paths, urlsplit)
  mcp-policy            MCP allow/deny policy shared by guard and app
  egress-policy         network list grammar and judgement (also used by claude-craft-kit)
  guard-core            the rules of the hook: shell lexer, paths, sql, network, scope, profiles
  guard                 the hook executable
  engine-state          the loop: state machine, ROADMAP grammar, turns, gates, delivery, notify
  winproc               the only crate with unsafe code: console control, process start time, random bytes
  app                   domain, repositories, services, HTTP API, server, demo
  cli                   the `harness` executable
  desktop               the Rolo Loop desktop app (Tauri): hosts the app server in-process, frameless window, tray, installers
```

## Layers of the `app` crate

`domain` (pure types) <- `repositories` (files and git) <- `services` (use cases; `App` owns them) <- `api` (router, controllers, HTTP server). `composition` builds the `App`, the router and the server and starts them.

## Desktop app

`crates/desktop` starts the same server `harness serve` runs (composition `open_and_register` / `serve` / `finish`) on a thread, or attaches to a live one (`app.json`), and opens a frameless WebView2 window on `http://127.0.0.1:<port>/`. The page is the same front end as in a browser; inside the app it detects `window.__TAURI__` and draws macOS-style traffic lights, a drag band and a collapsible sidebar. The only IPC the page gets is its own window's minimize, zoom, close and drag (`crates/desktop/capabilities/panel.json`). Engine children run `plugin/bin/harness.exe` (`LOOP_APP_EXE` or the bundled one), never the GUI binary. Every child process starts with a hidden console (`winproc::hide_console`, `winproc::spawn_hidden_flags`): no terminal window ever shows. Installers: [INSTALLER.md](INSTALLER.md).

## State

State lives in files, never in a database: `.specs/project/ROADMAP.md` (the typed graph), `STATE.md`, `RULES.md`, and `.specs/project/loop/{json,internal}/` for the loop (`LOOP.json`, `meta.json`, turn logs, snapshots, `guard.log`, `LOOP.lock`). The app's own data is `app.json` (pid, port, token), `projects.json` and `config.json` in the data folder.

## Safety boundaries

- The server binds `127.0.0.1` only, checks the `Host` header, requires a token for every change and refuses cross-origin requests.
- `claude` runs with only the project's settings and an assembled set of agents and skills; user skills, other plugins' hooks and MCP servers do not load.
- Run-time egress: a local proxy and preloads limit what a loop session can reach to the network list; the guard checks the same list in the command text.
- Only `delivery` pushes, and only the feature branch.

## How the Rust version is checked

The behavior is pinned to a reference implementation by differential tools in `tools/`: the guard on generated tool-call payloads, the CLI on its argument grammar, the loop on simulated `claude` runs, the API on every route, and a headless browser on the panel. The front end is the v2 design (sidebar shell, light/dark/system theme, loop timeline, feature page; no external requests, no webfonts), pinned by `app/frontend.sha256`; `tools/e2e/ui_v2_check.js` checks both themes and four viewports for overflow, overlap, contrast and leaks.
