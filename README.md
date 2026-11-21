<p align="center">
  <img src="docs/media/rolo-running.svg" alt="Rolo, the mouse in the wheel" width="132">
</p>

<h1 align="center">Rolo Loop</h1>

<p align="center"><strong>Unattended, spec-driven build loops for AI coding agents, on your subscription or a pay-per-use API key.</strong></p>

> Status: early (0.4.0), Windows only. Works with Claude Code or any Claude Code compatible CLI, on a subscription or a pay-per-use API key.

**Meet Rolo.** A mouse in its wheel runs without leaving the place and starts again at every turn: that is a loop. Rolo Loop puts that wheel to work. You describe a goal and approve a plan; a planner splits the work into features; sub-agents write each feature's spec, review it, build it, verify it and pass it through a quality gate inside an isolated git worktree; a guard hook keeps every tool call inside the lines you drew. Turn after turn, until the roadmap is done. You follow it from the desktop app (or a local web panel) and answer when the loop asks.

<p align="center">
  <img src="docs/media/rolo-loop-overview.gif" alt="Projects with a running loop, the loop by turn and a feature's history" width="100%">
</p>

## Usage

1. **Install and open Rolo Loop.** The first start runs a short guided tour of the settings.
2. **Choose the AI** in Settings > AI providers: your subscription (the `claude` CLI signed in), a pay-per-use API key, a gateway or a compatible CLI. Put them in the order to use: when the first one hits its usage limit, the next one takes the turn. Keys are encrypted for your Windows user and never reach the agents.
3. **Open a folder** (an existing repository or a new project). The setup asks how the AI should treat the code (Conformance) and maps it once.
4. **Plan with the AI**: describe the goal in the planning chat; it writes the roadmap (features in stages, with dependencies) and the flows.
5. **Approve the roadmap.** Only your click does it.
6. **Start the loop.** Each feature goes through spec, review, build, verify and the quality gate in its own lane, on a loop branch in a separate worktree. Turn on **Keep the loop going** (Settings > General) to run unattended.
7. **Answer when it asks** (in the app, or from a Windows, e-mail or webhook notice), then **Finish**: push the branch and open the pull request.

<p align="center">
  <img src="docs/media/rolo-loop-settings.gif" alt="Settings: AI providers in priority order, agents, quality gate and Keep the loop going" width="100%">
</p>

<p align="center">
  <img src="docs/media/loop-dark.png" alt="The loop: features by turn, the inspector and the agents" width="49%">
  <img src="docs/media/tour-light.png" alt="The guided tour" width="49%">
</p>

## What is in the box

- A plugin in the Claude Code plugin format (the agent CLI the loop drives): the `orchestrator` skill, three satellite skills (audit dependencies, create a block rule, show the progress graph), the `repo-catalog` skill and seven sub-agents (`spec-writer`, `spec-reviewer`, `executor`, `quality-gate`, `verifier`, `verifier-flow`, `brownfield-mapper`).
- `harness`, one Windows binary: the loop engine, the local panel server and the command line.
- `guard`, the hook binary the agent CLI runs before every tool call.
- The panel: a small Preact app served on `127.0.0.1`, shown by the desktop app (or a browser with `harness serve --no-desktop`).
- The Rolo Loop desktop app (Windows installer): the panel in a native window, a tray icon that keeps a running loop alive when you close the window, and in-app updates.
- Settings: AI providers in priority order, agents and their models, the quality gate, skills and MCP tools, notifications (Windows, e-mail, webhook). Secrets are encrypted for your Windows user.

## How it works

1. In Claude Code, the orchestrator skill maps your code, writes `.specs/project/ROADMAP.md` (a typed graph of features with dependencies, stages and paths) and stops.
2. In the panel you read the graph, run the preflight and approve. Approval is a click; no agent can do it for you.
3. The loop runs turns of `claude -p`. Each turn takes the features that are ready and runs six steps for each: spec, review, gate, execute, verify, commit. State is in files, so a stopped loop resumes.
4. When something needs you (a choice, a blocked action), the loop stops and asks in the panel. Turn on **Keep the loop going** (Settings > General) and it never stops for housekeeping: leftover work becomes a checkpoint commit and a blocked feature is set aside while the others go on.
5. Deliver pushes the feature branch and opens the pull request. It is the only push there is.

More in [docs/HOW_TO_USE.md](docs/HOW_TO_USE.md); every setting in [docs/CONFIG.md](docs/CONFIG.md). The architecture is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), the panel API in [docs/API.md](docs/API.md), and how the skill finds the app in [docs/SKILL_BRIDGE.md](docs/SKILL_BRIDGE.md).

## Requirements

- Windows 10 or 11 and `git`.
- An agent CLI the loop can drive: the `claude` CLI (signed in for a subscription, or with an API key set in Settings) or a Claude Code compatible command.
- The companion plugin [claude-craft-kit](https://github.com/iagopadilha/claude-craft-kit) (version 0.3.x) for the dev skills and the `craft` tools.

## Try it without touching anything

```
harness demo
```

Opens the panel on a fictional project with moving data. It never calls `claude` and removes its files when you stop it.

## Limits, honestly

- Loops spend your Claude subscription usage; long loops can reach a usage window and then wait for the reset.
- When every AI provider in your list reaches its usage limit, the loop waits for the first reset.
- Only the feature branch is ever pushed. Protected branches are never touched by the loop.
- The guard is text analysis of commands, not a sandbox: a program that builds its target at run time is judged only by the run-time network layer, not by the guard. Read the residual-risk notes in `plugin/skills/orchestrator/hooks/README.md`.
- Windows only for now.

## Install

Download `Rolo Loop_<version>_x64-setup.exe` from the releases and run it (per user, no admin; an `.msi` exists for managed machines). It installs the app (Start menu: **Rolo Loop**), puts `harness` on your PATH and carries the plugin. To drive the loop from a Claude Code chat as well, add the plugin there:

```
/plugin marketplace add "%LOCALAPPDATA%\Rolo Loop"
/plugin install rolo-loop@rolo-loop
```

(or `/plugin marketplace add iagopadilha/rolo-loop` from GitHub) and `harness doctor` in a new terminal. The first start opens a guided tour of the settings. Details, signing and updates: [docs/INSTALLER.md](docs/INSTALLER.md).

## Build from source

```
cargo build --release
cargo test --workspace
cd crates/desktop && cargo tauri build     # the desktop app and its installers (needs tauri-cli 2.12)
```

The differential tools in `tools/` compare this build with the reference implementation (they need the reference clone, named in each tool's header).

## Not affiliated

This project is not affiliated with Anthropic. "Claude" and "Claude Code" name products it works with; any Claude Code compatible CLI can drive the loop.

## Licence

MIT, see `LICENSE`. Third-party notices are in [THIRD_PARTY.md](THIRD_PARTY.md).
