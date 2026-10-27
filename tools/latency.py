#!/usr/bin/env python3
"""Latency report: how long the guard and a few CLI commands take, per call, as a process start (the cost Claude Code pays on every tool call).

Usage: python tools/latency.py [--runs 200] [--guard PATH] [--harness PATH]
Prints median, p95 and max in milliseconds. The target for the guard is p95 under 20 ms.
"""
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time

TARGET = os.environ.get("CARGO_TARGET_DIR") or os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "target")


def option(name, default):
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else default


def measure(command, runs, stdin_text=None, cwd=None, env=None):
    times = []
    for _ in range(runs):
        start = time.perf_counter()
        subprocess.run(command, input=stdin_text, capture_output=True, text=True, cwd=cwd, env=env)
        times.append((time.perf_counter() - start) * 1000)
    times.sort()
    return statistics.median(times), times[int(len(times) * 0.95) - 1], times[-1]


def main():
    runs = int(option("--runs", "200"))
    guard = option("--guard", os.path.join(TARGET, "release", "guard.exe"))
    harness = option("--harness", os.path.join(TARGET, "release", "harness.exe"))
    project = tempfile.mkdtemp(prefix="latency-")
    env = dict(os.environ, LOOP_DATA_DIR=os.path.join(project, "_data"))
    payloads = {
        "allowed Bash (ls)": {"tool_name": "Bash", "tool_input": {"command": "ls -la src"}, "agent_type": "executor-back"},
        "blocked Bash (git push)": {"tool_name": "Bash", "tool_input": {"command": "git push origin main"}, "agent_type": "executor-back"},
        "allowed Write in scope": {"tool_name": "Write", "tool_input": {"file_path": os.path.join(project, "src", "a.ts"), "content": "x"}, "agent_type": "executor-front"},
    }
    print("%-34s %8s %8s %8s" % ("case", "median", "p95", "max"))
    worst = 0
    for name, payload in payloads.items():
        median, p95, high = measure([guard, "--profile", "auto"], runs, json.dumps(payload), project, env)
        worst = max(worst, p95)
        print("%-34s %8.1f %8.1f %8.1f" % ("guard: " + name, median, p95, high))
    for name, command in {"harness --version": [harness, "--version"], "harness paths --json": [harness, "paths", "--json"]}.items():
        median, p95, high = measure(command, max(runs // 4, 20), None, project, env)
        print("%-34s %8.1f %8.1f %8.1f" % (name, median, p95, high))
    print("\nguard worst p95: %.1f ms (target < 20 ms): %s" % (worst, "OK" if worst < 20 else "ABOVE TARGET"))
    return 0 if worst < 20 else 1


if __name__ == "__main__":
    sys.exit(main())
