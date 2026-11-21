<p align="center">
  <img src="docs/media/rolo-running.svg" alt="Rolo, the mouse in the wheel" width="120">
</p>

<h1 align="center">Rolo Loop</h1>

<p align="center"><strong>Spec-driven build loops for AI coding agents.</strong><br>
You approve the plan. The agents build it, feature by feature, while you do something else.</p>

<p align="center">
  <img src="docs/media/rolo-loop-overview.gif" alt="Projects with a running loop, the loop by turn and a feature's history" width="100%">
</p>

## How it works

1. **Plan.** Describe the goal. The AI writes a roadmap: features in stages, with their dependencies.
2. **Approve.** You read it and click approve. No agent can approve for you.
3. **Loop.** Every feature goes through the same pipeline, in its own lane, on a separate branch:

   **Spec → Review → Build → Verify → Quality → Commit**

4. **Answer** when the loop needs a decision (in the app, or from a Windows, e-mail or webhook notice).
5. **Finish.** Push the branch and open the pull request.

Everything the loop decides is written to plain files in `.specs/`, so you can read, review and version every step, and a stopped loop resumes where it was.

## Get started

1. Download `Rolo Loop_<version>_x64-setup.exe` from the [releases](https://github.com/iagopadilha/rolo-loop/releases) and run it (per user, no admin).
2. Open **Rolo Loop**. A short tour walks you through the settings.
3. In **Settings > AI providers**, add the AI to use, in order: your subscription, a pay-per-use API key, a gateway or a compatible CLI. When the first one hits its limit, the next one takes over.
4. **Open folder** (an existing repository) or **New project**, plan with the AI and start the loop.

Turn on **Keep the loop going** (Settings > General) to run unattended: the loop stops only for a decision you must make, a cost limit or a real error.

<p align="center">
  <img src="docs/media/rolo-loop-settings.gif" alt="Settings: AI providers in order, agents, quality gate and Keep the loop going" width="100%">
</p>

Want to look first? `harness demo` opens the app on a fictional project. It never calls an AI.

## Good to know

- **Requirements:** Windows 10 or 11, `git`, and an agent CLI: the `claude` CLI or a Claude Code compatible command. The dev skills come from the companion plugin [claude-craft-kit](https://github.com/iagopadilha/claude-craft-kit).
- **Safe by default:** a guard checks every tool call of every agent, the agents work in a separate git worktree, only the loop branch is ever pushed, and API keys are encrypted for your Windows user and never reach the agents.
- **Costs:** loops use your subscription or API credit. Set a cost cap per loop; when every provider is at its limit, the loop waits for the reset.
- **Limits:** Windows only for now. The guard reads commands, it is not a sandbox (see [the guard notes](plugin/skills/orchestrator/hooks/README.md)).

## Docs

[How to use](docs/HOW_TO_USE.md) · [Settings](docs/CONFIG.md) · [Installer and updates](docs/INSTALLER.md) · [Architecture](docs/ARCHITECTURE.md) · [Panel API](docs/API.md) · [Changelog](CHANGELOG.md)

## Build from source

```
cargo test --workspace
cd crates/desktop && cargo tauri build     # the app and its installers (tauri-cli 2.12)
```

## Licence

MIT, see [LICENSE](LICENSE) and [THIRD_PARTY.md](THIRD_PARTY.md). Not affiliated with Anthropic; "Claude" and "Claude Code" name products it works with.
