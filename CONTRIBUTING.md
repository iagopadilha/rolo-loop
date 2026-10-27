# Contributing to claude-maestro

Thanks for helping. This repository holds the claude-maestro plugin (orchestrator skill, agents, guard hook) and the `harness` binary (engine, local panel, CLI).

## Ground rules

- Windows x64 is the supported platform for now.
- Conventional Commits (`feat(scope): ...`, `fix: ...`, `docs: ...`). One logical change per commit.
- Sign every commit off (`git commit -s`): the sign-off states the Developer Certificate of Origin (https://developercertificate.org/) for your change. Pull requests without it are not merged.
- No names, data or hosts of any real company, customer or person in code, examples, fixtures, issues or commit messages. Use `example.com` and synthetic data.
- Never commit a secret. Never print a secret value in a log or a test.
- Every Rust file starts with `// SPDX-License-Identifier: MIT`.
- Behavior is pinned to a reference implementation by differential tools in `tools/` (`guard_diff.py`, `cli_diff.py`, `loop_diff.py`, `api_diff.py`). A change that alters visible behavior needs its own entry in `CHANGELOG.md` and an explanation in the PR.
- The panel front end in `app/frontend/` is pinned by `app/frontend.sha256`; change it only on purpose and refresh the manifest in the same commit.

## Before you open a pull request

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Fill in the pull request template. Small pull requests are reviewed faster.

## Reporting problems

Use the issue templates. For a security problem do not open an issue: follow `SECURITY.md`.
