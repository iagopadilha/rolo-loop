#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""End-to-end smoke test of the panel: the reference app and the Rust app serve the same throw-away project; headless Chromium loads the hub and the
project panel of each and what the user sees (page text, console errors, failed requests) must be identical.

    python tools/e2e/run.py [--harness target/debug/harness.exe]
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import api_diff  # noqa: E402


def normalise(text, side):
    for folder in {side.root, os.path.realpath(side.root), side.root.replace("\\", "/"), os.path.realpath(side.root).replace("\\", "/")}:
        text = text.replace(folder, "<ROOT>")
    text = text.replace(side.project_id, "<PID>")
    text = re.sub(r"127\.0\.0\.1:\d+", "127.0.0.1:<PORT>", text)
    text = re.sub(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}", "<TIME>", text)
    # the diagnostics block describes the machine (where the plugin, the guard and the toolkit are): it differs by design between the two apps
    return text.split("DIAGNOSTICS")[0] + ("DIAGNOSTICS" if "DIAGNOSTICS" in text else "")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--harness", default=api_diff.DEFAULT_RUST)
    options = parser.parse_args()
    scratch = tempfile.mkdtemp(prefix="e2e-")
    env = dict(os.environ, LOOP_CLAUDE_HOME=os.path.join(scratch, "claude-home"), LOOP_TOOLKIT_SIBLING="off", LOOP_MONITOR_OPEN="off", PYTHONIOENCODING="utf-8")
    for name in ("LOOP_DATA_DIR", "LOOP_MONITOR_URL", "CLAUDE_PLUGIN_ROOT", "LOOP_SKILL_DIR"):
        env.pop(name, None)
    os.makedirs(env["LOOP_CLAUDE_HOME"], exist_ok=True)
    reference = api_diff.Side("reference", os.path.join(scratch, "ref"), [sys.executable, api_diff.PY_HARNESS], env)
    rust = api_diff.Side("rust", os.path.join(scratch, "rust"), [options.harness], dict(env, LOOP_REPO_ROOT=os.path.dirname(os.path.dirname(HERE))))
    results = {}
    try:
        reference.start(47821)
        rust.start(47822)
        for side in (reference, rust):
            done = subprocess.run(["node", os.path.join(HERE, "smoke.js"), side.url, side.project_id], capture_output=True, text=True, encoding="utf-8", timeout=180)
            if done.returncode != 0:
                print("%s: browser run failed: %s" % (side.name, done.stderr.strip()[:500]))
                return 1
            pages = json.loads(done.stdout)["pages"]
            for page in pages:
                page["text"] = normalise(page["text"], side)
                page["errors"] = [normalise(e, side) for e in page["errors"]]
            results[side.name] = pages
    finally:
        reference.stop()
        rust.stop()
        shutil.rmtree(scratch, ignore_errors=True)
    bad = 0
    for ref_page, rust_page in zip(results["reference"], results["rust"]):
        for key in ("title", "text", "errors", "failed"):
            if ref_page[key] != rust_page[key]:
                bad += 1
                print("DIFF %s.%s\n  reference: %s\n  rust:      %s" % (ref_page["name"], key, json.dumps(ref_page[key], ensure_ascii=False)[:600], json.dumps(rust_page[key], ensure_ascii=False)[:600]))
        print("%s: %d chars of text, %d console error(s), %d failed request(s)" % (ref_page["name"], len(rust_page["text"]), len(rust_page["errors"]), len(rust_page["failed"])))
    print("%d difference(s)" % bad)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
