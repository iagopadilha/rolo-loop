# How to use agent-maestro

agent-maestro turns a goal into a roadmap, and an approved roadmap into an unattended loop that specs, implements and verifies each feature in an isolated git worktree. You steer from a local web panel. Everything runs on your machine, on your own Claude subscription through the `claude` CLI; no API key is used.

## Install

1. Windows 10 or 11, `git`, and the `claude` CLI signed in.
2. In Claude Code: `/plugin marketplace add iagopadilha/claude-craft-kit`, `/plugin install claude-craft-kit@claude-craft-kit`, then the same for `agent-maestro`.
3. Put `harness.exe` on your PATH (the release archive has it in the plugin's `bin/` folder next to `guard.exe`).
4. Run `harness install` once. It writes `~/.loop-harness/config.json` so the orchestrator skill finds the app and the skill folder.
5. Run `harness doctor`. It lists what is missing: the skill, the guard, `claude`, the kit, the notification channels.

## Migrating the installed plugin

The plugin id changed from `claude-maestro` to `agent-maestro` (the repository and its URL keep the old name). An install made under the old id keeps working: `harness` still finds it. To move to the new id, run in Claude Code:

```
/plugin uninstall claude-maestro@claude-maestro
/plugin marketplace remove claude-maestro
/plugin marketplace add iagopadilha/agent-maestro
/plugin install agent-maestro@agent-maestro
```

Then, in a terminal, run `harness install` and `harness doctor`. Agents are now addressed as `agent-maestro:executor-back` and so on.

## First project

1. Open your repository in Claude Code and ask the orchestrator to plan: "orchestrate this: <goal>". It maps the code (Phase 1), writes `.specs/project/ROADMAP.md` and stops for your approval.
2. Open the panel: `harness serve --project <repo>`. It prints `MONITOR_URL=` and opens the browser.
3. In the panel, read the roadmap graph, run the preflight (Check) and approve. Approval is a click; nothing else can approve.
4. Start the loop from the panel, or paste the command the orchestrator gives you into a terminal: `harness loop <repo> --wave 2 ...`. The loop runs turns of `claude -p`; each turn picks the next features, runs the six steps (spec, review, gate, execute, verify, commit) and writes its state under `.specs/project/loop/`.
5. When the loop asks something, answer in the panel. When it is done, press Deliver: the feature branch is pushed and the pull request is opened. Nothing else ever pushes.

## Useful commands

| Command | What it does |
|---|---|
| `harness serve --project <repo>` | Starts the panel, or registers the project in the running one |
| `harness check <repo>` | Preflight: prints READY and the command to run, creates nothing |
| `harness loop <repo> [flags]` | The unattended loop (see `harness loop --help`) |
| `harness doctor [--access]` | Diagnostics; `--access` shows what a loop session can reach |
| `harness demo` | The panel on a fake project with moving data (never calls `claude`) |
| `harness state / graph / summary / audit / rules / notify / deliver` | The engine commands the skill uses |

## Safety

- The guard hook blocks, in every agent the loop starts, `git push`, commits outside the orchestrator's checkpoint, writes outside the feature's paths, production writes, secrets in commands and requests to hosts that are not on the project's network list. A block is final: the agent reports it and the orchestrator asks you.
- You can allow one thing explicitly with a recorded phrase in `.specs/project/loop/json/PERMISSIONS.json`; hard rules (protected branches, force push, approval files) cannot be allowed.
- Loops spend your subscription usage; long loops wait for the usage window to reset.

## Where things live

| What | Where |
|---|---|
| Roadmap, state, rules | `.specs/project/` in your repository (git-ignored by default) |
| Loop state, turn logs, guard log | `.specs/project/loop/` |
| App data (registry, config, logs) | `~/.loop-harness/` (`LOOP_DATA_DIR` changes it) |
