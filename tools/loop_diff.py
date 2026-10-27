#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Differential test of the unsupervised loop: the same throw-away project, the same simulated `claude` (the reference's fake_claude.py) and
the same flags go to the reference (`python run_loop.py`) and to the Rust binary (`harness loop`). Exit code, normalised stdout/stderr and
the files the loop leaves in the worktree must be identical.

    python tools/loop_diff.py [--harness target/debug/harness.exe] [--guard target/debug/guard.exe] [--filter name] [--verbose]
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

ORACLE = os.environ.get("ORACLE_MAESTRO", os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))), "claude-maestro-legacy"))
BACKEND = os.path.join(ORACLE, "app", "backend")
ENGINE = os.path.join(BACKEND, "harness_backend", "engine")
FAKE = os.path.join(BACKEND, "tests", "engine", "fake_claude.py")
RUN_LOOP = os.path.join(ENGINE, "run_loop.py")
ORACLE_GUARD_DIR = os.path.join(ORACLE, "plugin", "skills", "orchestrator", "hooks")
HERE = os.path.dirname(os.path.abspath(__file__))
TARGET = os.environ.get("CARGO_TARGET_DIR") or os.path.join(os.path.dirname(HERE), "target")
VOLATILE = {"at", "t", "started", "finished", "updated", "updated_at", "created_at", "finished_at", "cost_warned_at", "read_at", "closed_at", "started_at", "base_sha", "head", "reset_at", "resetsAt"}
sys.path.insert(0, ENGINE)

SCENARIOS = []  # (name, roadmap, scenario, flags)


def scenario(name, roadmap, plan, flags=(), **settings):
    SCENARIOS.append((name, roadmap, dict(plan, **settings) if settings else plan, list(flags)))


TWO = "## Stage 1\n- [alpha] status=pending\n- [beta] status=pending\n"
THREE = "## Stage 1\n- [alpha] status=pending\n- [beta] status=pending deps=alpha\n## Stage 2\n- [gamma] status=pending\n"

scenario("two-features-done", TWO, {"features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b"}}})
scenario("dependencies", THREE, {"features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b", "deps": "alpha"}, "gamma": {"paths": "apps/g"}}}, ["--wave", "2"])
scenario("question-auto-answered", TWO, {"features": {"alpha": {"paths": "apps/a", "ask": True}, "beta": {"paths": "apps/b"}}})
scenario("question-stops", TWO, {"features": {"alpha": {"paths": "apps/a", "ask": True}, "beta": {"paths": "apps/b"}}}, ["--no-auto-answer"])
scenario("gate-blocks-quarantine", TWO, {"features": {"alpha": {"paths": "apps/a", "gate": "block"}, "beta": {"paths": "apps/b"}}})
scenario("execution-fails", TWO, {"features": {"alpha": {"paths": "apps/a", "exec": "fail"}, "beta": {"paths": "apps/b"}}}, ["--continue-on-blocked"])
scenario("cost-cap-pauses", TWO, {"features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b"}}, "cost": 0.4}, ["--max-cost", "0.7"])
scenario("idle-is-detected", TWO, {"idle": True}, ["--max-stall", "9"])
scenario("bad-loop-json-status", TWO, {"bad_status": True, "features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b"}}})
scenario("illegal-transition", TWO, {"cheat": {"turn": 2, "type": "illegal"}, "features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b"}}})
scenario("verified-without-commit", TWO, {"cheat": {"turn": 2, "type": "no_commit"}, "features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b"}}})
scenario("stream-output-with-limits", TWO, {"output": "stream", "features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b"}}})
scenario("noisy-output", TWO, {"output": "noisy", "features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b"}}})
scenario("garbage-output", TWO, {"output": "garbage"})
scenario("critical-decision-pauses", TWO, {"decision_lines": {"1": ["- [alpha] turn=1 level=critical which database? | chose: postgres | revert: migrate"]}, "features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b"}}})
scenario("forgotten-loop-json-corrected", TWO, {"forget_loop_json": True, "features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b"}}})
scenario("check-ready", TWO, {}, ["--check"])
scenario("check-json", TWO, {}, ["--check", "--json"])
scenario("dry-run", TWO, {}, ["--dry-run"])
scenario("e2e-marks", TWO, {"features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b"}}}, ["--e2e"])
scenario("e2e-missing-marks", TWO, {"skip_e2e_mark": True, "features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b"}}}, ["--e2e"])
scenario("no-roadmap-grammar", "## Stage 1\n- [alpha] status=weird\n", {}, ["--check"])
scenario("model-flag", TWO, {"argv_log": "{tmp}/argv.log", "features": {"alpha": {"paths": "apps/a"}, "beta": {"paths": "apps/b"}}}, ["--orchestrator-model", "sonnet", "--wave", "1"])


def sh(args, cwd, check=True):
    return subprocess.run(args, cwd=cwd, capture_output=True, text=True, encoding="utf-8", check=check)


def make_fake_plugin(root, kind, guard_exe):
    skill = os.path.join(root, "fake-plugin", "skills", "orchestrator")
    os.makedirs(os.path.join(skill, "hooks"), exist_ok=True)
    with open(os.path.join(skill, "SKILL.md"), "w", encoding="utf-8") as handle:
        handle.write("---\nname: orchestrator\n---\nfake skill of the tests\n")
    plugin_hooks = os.path.join(root, "fake-plugin", "hooks")
    os.makedirs(plugin_hooks, exist_ok=True)
    if kind == "py":
        for name in os.listdir(ORACLE_GUARD_DIR):
            if (name.startswith("guard") or name == "mcp_policy.py") and name.endswith(".py"):
                shutil.copyfile(os.path.join(ORACLE_GUARD_DIR, name), os.path.join(skill, "hooks", name))
        command = 'python "${CLAUDE_PLUGIN_ROOT}/skills/orchestrator/hooks/guard.py" --profile auto'
    else:
        os.makedirs(os.path.join(root, "fake-plugin", "bin"), exist_ok=True)
        shutil.copyfile(guard_exe, os.path.join(root, "fake-plugin", "bin", "guard.exe"))
        command = '"${CLAUDE_PLUGIN_ROOT}/bin/guard.exe" --profile auto'
    with open(os.path.join(plugin_hooks, "hooks.json"), "w", encoding="utf-8") as handle:
        json.dump({"hooks": {"PreToolUse": [{"matcher": "*", "hooks": [{"type": "command", "command": command}]}]}}, handle)
    return skill


def build_project(tmp, roadmap, plan):
    import loop_state  # the reference's own approval, so both runs start from the same files
    repo = os.path.join(tmp, "proj")
    os.makedirs(repo)
    sh(["git", "init", "-q", "-b", "main"], repo)
    for key, value in (("user.name", "tester"), ("user.email", "t@t.t"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")):
        sh(["git", "config", key, value], repo)
    with open(os.path.join(repo, "README.md"), "w") as handle:
        handle.write("base\n")
    sh(["git", "add", "README.md"], repo)
    sh(["git", "commit", "-q", "-m", "base"], repo)
    project = os.path.join(repo, ".specs", "project")
    os.makedirs(project)
    with open(os.path.join(project, "ROADMAP.md"), "w", encoding="utf-8", newline="\n") as handle:
        handle.write(roadmap)
    try:
        loop_state.approve_roadmap(repo, "2026-01-01T00:00:00")
    except ValueError:
        pass
    with open(os.path.join(repo, "CLAUDE.md"), "w") as handle:
        handle.write("Conformance: STANDARD\n")
    scenario_path = os.path.join(tmp, "scenario.json")
    with open(scenario_path, "w") as handle:
        json.dump(json.loads(json.dumps(plan).replace("{tmp}", tmp.replace("\\", "/"))), handle)
    open(os.path.join(tmp, "empty.env"), "w").close()
    return repo, scenario_path


def environment(tmp, scenario_path, skill):
    env = dict(os.environ)
    env.update(LOOP_ENV_FILE=os.path.join(tmp, "empty.env"), LOOP_NOTIFY_CHANNELS="file", FAKE_SCENARIO=scenario_path, PYTHONIOENCODING="utf-8", LOOP_PR_MODE="link",
               LOOP_MONITOR="off", LOOP_SKILL_DIR=skill, LOOP_DATA_DIR=os.path.join(tmp, "app-data"), LOOP_USER_SHARED_DIR=os.path.join(tmp, "user-shared"),
               LOOP_CLAUDE_CONFIG_FILE=os.path.join(tmp, "claude.json"), LOOP_CLAUDE_HOME=os.path.join(tmp, "claude-home"), USERPROFILE=tmp, HOME=tmp)
    for key in ("LOOP_NOTIFY_TO", "LOOP_NOTIFY_REQUIRED", "LOOP_SMTP_HOST", "LOOP_SMTP_FROM", "LOOP_WEBHOOK_URL", "LOOP_PR_BASE", "LOOP_INTEGRATION_BRANCH",
                "LOOP_GIT_USER", "GUARD_PROD_HINTS", "LOOP_MONITOR_URL", "LOOP_HEADLESS"):
        env.pop(key, None)
    return env


STAMPS = [(re.compile(r"\d\d:\d\d:\d\d"), "<time>"), (re.compile(r"in \d+s"), "in <n>s"), (re.compile(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d"), "<ts>"),
          (re.compile(r"\d\d/\d\d \d\d:\d\d"), "<dm>"), (re.compile(r"\d{8}-\d{4}"), "<stamp>"), (re.compile(r"looptest-[a-z0-9_]+"), "<tmp>"),
          (re.compile(r"127\.0\.0\.1:\d+"), "127.0.0.1:<port>"), (re.compile(r"\b[0-9a-f]{40}\b"), "<sha>"), (re.compile(r"\b[0-9a-f]{7,10}\b(?=[ ;)])"), "<sha>")]


# Intentional differences of the product (the program is the `harness` binary, not python scripts): same meaning, different words.
PRODUCT = [(re.compile(r"settings-[0-9a-f]{10}\.json"), "settings-<hash>.json"), (re.compile(r'python "[^"]*harness\.py"'), "<harness>"), (re.compile(r'"[^"]*harness\.exe"'), "<harness>"), (re.compile(r"python harness\.py"), "harness"),
           (re.compile(r"python loop_state\.py"), "harness state"), (re.compile(r"\[dry-run\] turn 1 command: (?:C:/Python\d+/python\.exe|python) "), "[dry-run] turn 1 command: <python> "),
           (re.compile(r"\d{4}-\d\d-\d\d \d\d:\d\d"), "<ts>"), (re.compile(r"\d\d/\d\d/\d{4} \d\d:\d\d"), "<ts>")]


def normalise_text(text, tmp):
    text = text.replace("\\", "/").replace(tmp.replace("\\", "/"), "<tmp>")
    for pattern, replacement in PRODUCT:
        text = pattern.sub(replacement, text)
    text = re.sub(r"[A-Za-z]:/[^\s'\"]*?/diff-[a-z0-9_]+", "<tmp>", text)
    for pattern, replacement in STAMPS:
        text = pattern.sub(replacement, text)
    return text.replace("\r\n", "\n")


def scrub(value, tmp):
    if isinstance(value, dict):
        return {key: scrub(item, tmp) for key, item in value.items() if key not in VOLATILE}
    if isinstance(value, list):
        return [scrub(item, tmp) for item in value]
    if isinstance(value, str):
        return normalise_text(value, tmp)
    if isinstance(value, float):
        return round(value, 4)
    return value


KEEP_SUFFIXES = (".md", ".json", ".txt")
SKIP_PARTS = ("internal/inventory", "/guard.log", "snapshots/")


def tree(work, tmp):
    found = {}
    base = os.path.join(work, ".specs", "project")
    for folder, _, files in os.walk(base):
        for name in files:
            full = os.path.join(folder, name)
            relative = os.path.relpath(full, work).replace("\\", "/")
            if not name.endswith(KEEP_SUFFIXES) or any(part in "/" + relative for part in SKIP_PARTS) or name in ("LOOP.lock", "LOOP.lock.start"):
                continue
            with open(full, encoding="utf-8", errors="replace") as handle:
                text = handle.read()
            if name.endswith(".json"):
                try:
                    data = json.loads(text)
                    if name == "LOOP-CMD.json" and isinstance(data.get("argv"), list):
                        data["argv"] = ["<program>", "<subcommand>"] + data["argv"][2:]  # the program and the entry differ by design
                    found[relative] = json.dumps(scrub(data, tmp), sort_keys=True, indent=1)
                    continue
                except ValueError:
                    pass
            found[relative] = normalise_text(text, tmp)
    log = sh(["git", "log", "--format=%s", "--no-decorate"], work, check=False).stdout if os.path.isdir(work) else ""
    found["<git log>"] = log
    return found


def run_one(kind, args, roadmap, plan, flags, harness, guard):
    tmp = tempfile.mkdtemp(prefix="looptest-")
    try:
        repo, scenario_path = build_project(tmp, roadmap, plan)
        skill = make_fake_plugin(tmp, kind, guard)
        env = environment(tmp, scenario_path, skill)
        summary = os.path.join(tmp, "summary")
        common = [repo, "--summary-dir", summary, "--max-stall", "5", "--allow-silent", "--claude", FAKE] + flags
        command = [sys.executable, RUN_LOOP] + common if kind == "py" else [harness, "loop"] + common
        done = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", errors="replace", env=env, timeout=300)
        work = repo + "-worktree-loop"
        return {"code": done.returncode, "stdout": normalise_text(done.stdout, tmp), "stderr": normalise_text(done.stderr, tmp), "tree": tree(work, tmp)}
    finally:
        sh(["git", "worktree", "prune"], os.path.join(tmp, "proj"), check=False)
        shutil.rmtree(tmp, ignore_errors=True)


def window(left, right):
    """The two lines around their first differing character."""
    at = next((i for i, (x, y) in enumerate(zip(left, right)) if x != y), min(len(left), len(right)))
    start = max(0, at - 60)
    return left[start:at + 140], right[start:at + 140]


def compare(py, rs):
    problems = []
    if py["code"] != rs["code"]:
        problems.append("exit code: py %s rs %s" % (py["code"], rs["code"]))
    for key in ("stdout", "stderr"):
        if py[key] != rs[key]:
            a, b = py[key].splitlines(), rs[key].splitlines()
            for number in range(max(len(a), len(b))):
                left, right = a[number] if number < len(a) else "<missing>", b[number] if number < len(b) else "<missing>"
                if left != right:
                    problems.append("%s line %d:\n    py: %r\n    rs: %r" % ((key, number + 1) + window(left, right)))
                    break
    for name in sorted(set(py["tree"]) | set(rs["tree"])):
        if py["tree"].get(name) != rs["tree"].get(name):
            left, right = py["tree"].get(name), rs["tree"].get(name)
            if left is None or right is None:
                problems.append("file %s only in %s" % (name, "py" if right is None else "rs"))
                continue
            a, b = left.splitlines(), right.splitlines()
            for number in range(max(len(a), len(b))):
                x, y = a[number] if number < len(a) else "<missing>", b[number] if number < len(b) else "<missing>"
                if x != y:
                    problems.append("file %s line %d:\n    py: %r\n    rs: %r" % ((name, number + 1) + window(x, y)))
                    break
    return problems


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--harness", default=os.path.join(TARGET, "debug", "harness.exe"))
    parser.add_argument("--guard", default=os.path.join(TARGET, "debug", "guard.exe"))
    parser.add_argument("--filter", default="")
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()
    for path in (RUN_LOOP, FAKE, args.harness, args.guard):
        if not os.path.isfile(path):
            print("missing: %s" % path)
            return 2
    bad = total = 0
    for name, roadmap, plan, flags in SCENARIOS:
        if args.filter not in name:
            continue
        total += 1
        py = run_one("py", None, roadmap, plan, flags, args.harness, args.guard)
        rs = run_one("rs", None, roadmap, plan, flags, args.harness, args.guard)
        problems = compare(py, rs)
        if problems:
            bad += 1
            print("MISMATCH [%s]" % name)
            for item in problems[:6]:
                print("   " + item)
        elif args.verbose:
            print("ok   [%s] exit %s" % (name, py["code"]))
    print("%d scenarios, %d mismatches" % (total, bad))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
