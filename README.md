# claude-maestro

Unattended build loops for Claude Code, run on your own subscription.

> Status: under construction. This repository is a ground-up Rust rebuild; nothing is released yet.

claude-maestro lets you describe a goal, approve a plan, and leave a loop to work through it: a planner splits the work into features, sub-agents write specs, implement and verify each one inside an isolated git worktree, and a guard hook keeps every tool call inside the lines you drew. You watch it all from a local web panel.

## What is in the box

- A Claude Code plugin: the `orchestrator` skill, eight sub-agents and the guard hook.
- `harness`, a single Windows binary: engine, local panel server, CLI.
- The panel: a small Preact app served by the binary on `localhost`.

## Requirements

- Windows 10/11, `git`, and the `claude` CLI signed in to your account.
- The companion skills package `claude-craft-kit` (plugin) for the verify/test tooling.

## Limits, honestly

- Loops spend your Claude subscription usage; long loops can hit usage windows and will wait for the reset.
- Only the feature branch is ever pushed. Protected branches are never touched by the loop.
- Windows only for now.

## License

MIT. See `LICENSE`. Third-party notices will live in `THIRD_PARTY.md`.
