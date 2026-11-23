# How the skill finds the app

The orchestrator skill runs shell commands that start with `harness`. This is how the pieces find each other.

1. **The executable.** `harness` must be on PATH, or the skill uses the full path you give it. `harness --version` is the first check the skill makes.
2. **`harness install`** writes `<data dir>/config.json` with `app_root_dir` (the app folder) and `orchestrator_skill_dir` (the skill folder, found in the plugin cache). The data dir is `LOOP_DATA_DIR` or `~/.loop-harness`.
3. **The live app.** `harness serve` writes `app.json` (pid, port, url, token) in the data dir. A second `harness serve --project <repo>` finds it, registers the project in the running app and prints `MONITOR_URL=` and `APP_URL=`.
4. **The loop's relaunch command.** The loop writes `LOOP-CMD.json` in the project's loop state; the panel uses it to answer, resume or restart without you typing the command.
5. **The kit.** `harness paths --json` returns `maestro_plugin`, `orchestrator_skill` and `toolkit`. The skill puts `TOOLKIT: <path>` in the prompt of every agent so they can call `<toolkit>/bin/craft.exe`.
6. **The guard.** `plugin/hooks/hooks.json` registers `bin/guard.exe --profile auto` for every agent; the skill's own frontmatter registers `--profile orchestrator` for the main chat. `harness doctor` proves the guard answers.

Order of lookup for the skill folder, first folder with a `SKILL.md` wins: `LOOP_SKILL_DIR`, `orchestrator_skill_dir` in `config.json`, `$CLAUDE_PLUGIN_ROOT/skills/orchestrator`, `plugin/skills/orchestrator` of the repository the executable was built from, the installed plugin cache of Claude Code, `~/.claude/skills/orchestrator`. The kit is the highest installed version that satisfies the minimum, `>=0.4.0 <0.5.0`.
