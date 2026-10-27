#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Differential test of the HTTP API: the reference app (`python app/harness.py serve`, ORACLE_MAESTRO) and the Rust app (`harness serve`) start on
identical copies of the same throw-away projects; every request below goes to both and the status and the JSON body must be identical (volatile
values and the folder names normalised). POST cases run in the same order on both, so the follow-up reads also compare the files they changed.

    python tools/api_diff.py [--harness target/debug/harness.exe] [--filter text] [--verbose]
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ORACLE = os.environ.get("ORACLE_MAESTRO", r"C:\Users\someone\work\reference\claude-maestro-old")
PY_HARNESS = os.path.join(ORACLE, "app", "harness.py")
HERE = os.path.dirname(os.path.abspath(__file__))
TARGET = os.environ.get("CARGO_TARGET_DIR") or os.path.join(os.path.dirname(HERE), "target")
DEFAULT_RUST = os.path.join(TARGET, "debug", "harness.exe")
VOLATILE_KEYS = {"now", "at", "t", "opened_at", "last_opened_at", "started_at", "finished_at", "updated_at", "created_at", "pid", "version", "modified_at",
                 "validated_at", "seconds_until_shutdown", "shutdown_in_s", "idle_in_s", "age_seconds", "read_at", "closed_at", "checked_at", "stamp", "order"}
SKIP_PREFIXES = ()

CASES = []  # (name, method, path, body, token)


def case(name, method, path, body=None, token=True):
    CASES.append((name, method, path, body, token))


# ------------------------------------------------ fixtures
ROADMAP = """# Roadmap

**Branch base:** developer

## Stage 1
- [alpha] status=verified title="Alpha thing" paths=src/alpha kind=back scope=Small commit=abc1234
- [beta] status=building deps=alpha paths=src/beta kind=front scope=Medium note=working
- [gamma] status=blocked tries=2 paths=src/gamma note=flaky
## Stage 2
- [delta] status=pending deps=beta paths=src/delta
"""


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(text)


def build_project(root):
    """Same files in every copy. Returns the project folder."""
    project = os.path.join(root, "proj")
    specs = os.path.join(project, ".specs", "project")
    loop = os.path.join(specs, "loop")
    write(os.path.join(specs, "ROADMAP.md"), ROADMAP)
    write(os.path.join(specs, "PROJECT.md"), "# Project\n\nA test project.\n")
    write(os.path.join(specs, "RULES.md"), "# Rules\n")
    write(os.path.join(specs, "FLOWS.md"), "# Flows\n")
    write(os.path.join(specs, "STATE.md"), "# State\n")
    write(os.path.join(project, ".specs", "features", "alpha", "spec.md"), "# Spec: alpha - Alpha thing\nSCOPE: Small\n\n## Goal\nDo alpha well.\n\n| REQ-1 | x |\n")
    write(os.path.join(loop, "VALIDATE.md"), "- [alpha] turn=1 level=critical which db? | chose: pg | revert: drop\n- [beta] turn=2 level=important which lib? | chose: x | revert: y\n")
    write(os.path.join(loop, "ANSWERS.md"), "## Answer 2026-01-01 [applied]\nQuestion: Which db?\nUser answer: pg\n")
    write(os.path.join(loop, "json", "LOOP.json"), json.dumps({"status": "awaiting_user", "turn": 2, "reason": "feature gamma failed twice", "feature": ["beta"], "options": ["try again", "skip"]}, indent=2))
    write(os.path.join(loop, "json", "meta.json"), json.dumps({"turns": 2, "cost_usd": 1.25, "branch": "loop/test", "claude": "claude"}, indent=2))
    for number, cost in ((1, 0.5), (2, 0.75)):
        write(os.path.join(loop, "json", "turns", "%03d.json" % number), json.dumps({"total_cost_usd": cost, "duration_ms": 61000, "result": "Chose pg\nmore", "exit_code": 0, "session_id": "s-%d" % number}))
        snap = os.path.join(loop, "json", "snapshots", "%03d" % number)
        write(os.path.join(snap, "snapshot.json"), json.dumps({"head": ""}))
        write(os.path.join(snap, "ROADMAP.md"), ROADMAP)
        write(os.path.join(snap, "LOOP.json"), json.dumps({"status": "continue", "feature": "alpha" if number == 1 else "beta"}))
    write(os.path.join(project, "docs", "codebase", "STACK.md"), "# Stack\n")
    return project


# ------------------------------------------------ cases
def cases():
    hub = [("health", "/api/app/health"), ("projects", "/api/app/projects"), ("overview", "/api/app/overview"), ("settings", "/api/app/settings"),
           ("mcp", "/api/app/settings/mcp-servers"), ("catalog", "/api/app/catalog"), ("folders-roots-missing", "/api/app/system/folders?path=C:/definitely/missing"),
           ("repos", "/api/app/projects/{pid}/repos"), ("repos-readiness", "/api/app/projects/{pid}/repos-readiness"),
           ("reset-preview", "/api/app/projects/{pid}/reset-specs/preview"), ("unknown-project-repos", "/api/app/projects/nope/repos"), ("catalog-job-missing", "/api/app/catalog/job/x"),
           ("catalog-sheet-blank", "/api/app/catalog/sheet"), ("catalog-sheet-unknown", "/api/app/catalog/sheet?path=C:/nope")]
    for name, path in hub:
        case("hub:" + name, "GET", path)
    prefix = "/p/{pid}/api/"
    reads = ["health", "state", "history", "errors", "pipeline", "graph", "roadmap", "jobs", "specs/tree", "specs/file?path=project/ROADMAP.md",
             "specs/file?path=project/loop/json/LOOP.json", "specs/file?path=nope.txt", "planning", "planning/imports", "planning/readiness", "planning/reopen-check",
             "test-environment", "test-environment/sources", "network", "subprojects", "standards", "workspace", "close-loop/check", "wrap-up", "follow-up", "decisions",
             "decisions?turn=2", "decisions?turn=x", "actions", "whiteboard", "chat/threads", "chat/thread/t-000000000000", "chat/job/j-x", "loop/settings", "turn/1", "turn/9",
             "turn/x", "feature/alpha", "feature/beta", "feature/none", "codebase/tree", "codebase/file?path=docs/codebase/STACK.md", "codebase/file?path=../x",
             "codebase/file?path=docs/codebase/NOPE.md", "quarantine/gamma", "quarantine/alpha", "agent/orch/log", "agent/v001:orch/log", "agent/v009:abc/log",
             "planning/job/j-x", "workspace/job/j-x", "action/job/j-x", "jobs/j-x", "subprojects/standards?path=x"]
    for path in reads:
        case("get:" + path, "GET", prefix + path)
    case("get:unknown-project", "GET", "/p/nope/api/state")
    case("get:unknown-route", "GET", "/p/{pid}/api/nothing-here")
    case("method-not-allowed", "DELETE", prefix + "state")
    for name, path in (("hub-page", "/"), ("project-page", "/p/{pid}/"), ("project-page-no-slash", "/p/{pid}"), ("static-main", "/static/src/main.js"),
                       ("static-css", "/static/styles/base.css"), ("static-vendor", "/static/vendor/preact.module.js"), ("static-missing", "/static/nope.js"),
                       ("static-escape", "/static/..%5C..%5Charness.py"), ("static-folder", "/static/src")):
        case("page:" + name, "GET", path)
    case("no-token", "POST", prefix + "note", {"text": "x"}, token=False)
    case("bad-token-hub", "POST", "/api/app/settings", {"key": "loop_isolation", "value": False, "confirm": True}, token=False)
    # ---- writes (same order on both)
    case("post:settings-no-confirm", "POST", "/api/app/settings", {"key": "loop_isolation", "value": False})
    case("post:settings-unknown", "POST", "/api/app/settings", {"key": "nope", "value": 1, "confirm": True})
    case("post:settings-bad-value", "POST", "/api/app/settings", {"key": "max_parallel_maps", "value": 0, "confirm": True})
    case("post:settings-set", "POST", "/api/app/settings", {"key": "max_parallel_maps", "value": 3, "confirm": True})
    case("post:settings-reset", "POST", "/api/app/settings", {"key": "max_parallel_maps", "reset": True, "confirm": True})
    case("post:settings-no-value", "POST", "/api/app/settings", {"key": "idle_hours", "confirm": True})
    case("post:mcp-save", "POST", "/api/app/settings/mcp-servers", {"servers": {"grafana": {"allow": True, "kind": "other", "read_only": True}}, "extra": ["mcp__x__*"], "confirm": True})
    case("post:mcp-unknown-field", "POST", "/api/app/settings/mcp-servers", {"bogus": 1, "confirm": True})
    case("get:mcp-after", "GET", "/api/app/settings/mcp-servers")
    case("post:mcp-clear", "POST", "/api/app/settings/mcp-servers", {"servers": {}, "extra": [], "confirm": True})
    case("post:decision-review", "POST", prefix + "decisions/{decision0}/review", {})
    case("post:decision-unknown", "POST", prefix + "decisions/nope/review", {})
    case("post:decision-change", "POST", prefix + "decisions/{decision1}/change", {})
    case("get:decisions-after", "GET", prefix + "decisions")
    case("get:follow-up-after", "GET", prefix + "follow-up")
    case("post:note", "POST", prefix + "note", {"text": "hello"})
    case("post:note-empty", "POST", prefix + "note", {"text": " "})
    case("post:whiteboard", "POST", prefix + "whiteboard", {"data": {"a": 1}, "base_version": 0})
    case("post:whiteboard-conflict", "POST", prefix + "whiteboard", {"data": {"a": 2}, "base_version": 0})
    case("get:whiteboard-after", "GET", prefix + "whiteboard")
    case("put:network-no-confirm", "PUT", prefix + "network", {"allow": ["api.example.com"]})
    case("put:network-bad", "PUT", prefix + "network", {"allow": ["bad host!"], "confirm": True})
    case("put:network", "PUT", prefix + "network", {"allow": ["api.example.com"], "confirm": True})
    case("get:network-after", "GET", prefix + "network")
    case("put:spec-write-conflict", "PUT", prefix + "specs/file", {"path": "project/PROJECT.md", "content": "new", "version": "x"})
    case("put:spec-create", "PUT", prefix + "specs/file", {"path": "features/new/spec.md", "content": "# New\n", "version": ""})
    case("put:spec-forbidden", "PUT", prefix + "specs/file", {"path": "project/loop/json/LOOP.json", "content": "{}", "version": ""})
    case("put:spec-readonly", "PUT", prefix + "specs/file", {"path": "project/loop/VALIDATE.md", "content": "x", "version": "x"})
    case("get:spec-tree-after", "GET", prefix + "specs/tree")
    case("post:roadmap-approve-no-confirm", "POST", prefix + "roadmap/approve", {})
    case("post:instruction", "POST", prefix + "instruction", {"text": "do it", "feature": "beta"})
    case("post:pause", "POST", prefix + "pause", {})
    case("post:resume-no-loop", "POST", prefix + "resume", {})
    case("post:answer-empty", "POST", prefix + "answer", {})
    case("post:loop-check", "POST", prefix + "loop/check", {})
    case("post:raise-limit", "POST", prefix + "loop/raise-limit", {"max_cost_usd": 100})
    case("post:chat-empty", "POST", prefix + "chat/send", {"text": ""})
    case("post:chat-bad-scope", "POST", prefix + "chat/send", {"text": "hi", "scope": {"type": "bad"}})
    case("post:chat-unknown-error", "POST", prefix + "chat/send", {"text": "hi", "scope": {"type": "error", "error_id": "nope"}})
    case("post:action-bad", "POST", prefix + "action/run", {"action": "rm", "confirm": True})
    case("post:action-no-confirm", "POST", prefix + "action/run", {"action": "summary"})
    case("post:action-propose-empty", "POST", prefix + "action/propose", {"text": ""})
    case("post:quarantine-retry-bad", "POST", prefix + "quarantine/gamma/retry", {"guidance": 5})
    case("post:quarantine-skip-unknown", "POST", prefix + "quarantine/nope/skip", {})
    case("post:quarantine-revert-bad", "POST", prefix + "quarantine/gamma/revert", {"paths": []})
    case("post:planning-reset-no-confirm", "POST", prefix + "planning/reset", {})
    case("post:planning-send-empty", "POST", prefix + "planning/send", {"text": ""})
    case("post:planning-import-preview-empty", "POST", prefix + "planning/import/preview", {})
    case("post:workspace-create-bad", "POST", prefix + "workspace/create", {"repos": [1]})
    case("post:commit-push-folder", "POST", prefix + "workspace/repos/proj/commit-push", {})
    case("post:wrap-up-bad-mode", "POST", prefix + "wrap-up/finish", {"mode": "nope"})
    case("post:reopen", "POST", prefix + "planning/reopen", {})
    case("post:follow-up-send-empty", "POST", prefix + "follow-up/send", {})
    case("post:follow-up-run", "POST", prefix + "follow-up/run", {})
    case("post:close-loop-bad", "POST", prefix + "close-loop", {"repos": [1]})
    case("post:subprojects-detect", "POST", prefix + "subprojects/detect", {})
    case("post:conformance-bad", "POST", prefix + "conformance", {"value": "WRONG"})
    case("put:test-values-empty", "PUT", prefix + "test-environment/values", {"values": {}})
    case("post:test-import-no-confirm", "POST", prefix + "test-environment/import", {"names": ["X"]})
    case("post:suggestion-bad", "POST", prefix + "network/suggestion", {"action": "x", "confirm": True})
    case("post:repo-add-bad", "POST", "/api/app/projects/{pid}/repos", {"path": ""})
    case("put:repo-role-bad", "PUT", "/api/app/projects/{pid}/repos/proj/role", {"role": "zzz"})
    case("delete:repo-primary", "DELETE", "/api/app/projects/{pid}/repos/proj")
    case("post:open-bad-path", "POST", "/api/app/projects/open", {"path": "C:/definitely/missing"})
    case("post:create-bad", "POST", "/api/app/projects/create", {"parent_path": "C:/definitely/missing", "name": "x"})
    case("post:catalog-scan-bad", "POST", "/api/app/catalog/scan", {"paths": []})
    case("post:notify-bad", "POST", "/api/app/notify/test", {"channels": "x"})
    case("post:notify-unknown", "POST", "/api/app/notify/test", {"channels": ["pigeon"]})
    case("post:reset-specs-wrong-name", "POST", "/api/app/projects/{pid}/reset-specs", {"confirm": "wrong"})
    case("post:legacy-shutdown", "POST", prefix + "shutdown", {})


# ------------------------------------------------ servers
class Side:
    def __init__(self, name, root, command, env):
        self.name, self.root, self.command, self.env = name, root, command, env
        self.process = None
        self.url = ""
        self.token = ""
        self.project_id = ""
        self.project = ""

    def start(self, port):
        self.project = build_project(self.root)
        data = os.path.join(self.root, "data")
        os.makedirs(data, exist_ok=True)
        args = self.command + ["serve", "--no-browser", "--port", str(port), "--data-dir", data]
        self.process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env=self.env, cwd=self.root)
        deadline = time.time() + 30
        while time.time() < deadline:
            line = self.process.stdout.readline()
            if line.startswith("APP_URL="):
                self.url = line.strip().split("=", 1)[1]
                break
            if not line and self.process.poll() is not None:
                raise RuntimeError("%s did not start: exit %s" % (self.name, self.process.returncode))
        else:
            raise RuntimeError("%s did not announce its URL" % self.name)
        with open(os.path.join(data, "app.json"), encoding="utf-8") as handle:
            self.token = json.load(handle)["token"]
        answer = self.request("POST", "/api/app/projects/open", {"path": self.project})
        self.project_id = answer[1]["project"]["id"]

    def stop(self):
        if self.process and self.process.poll() is None:
            try:
                self.request("POST", "/api/app/shutdown", {})
                self.process.wait(10)
            except Exception:  # noqa: BLE001
                self.process.kill()

    def request(self, method, path, body=None, token=True):
        data = json.dumps(body).encode("utf-8") if body is not None else None
        request = urllib.request.Request(self.url.rstrip("/") + path, data=data, method=method)
        if body is not None:
            request.add_header("Content-Type", "application/json")
        if token:
            request.add_header("X-Monitor-Token", self.token)
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                raw = response.read().decode("utf-8")
                try:
                    return response.status, json.loads(raw or "null")
                except ValueError:
                    return response.status, raw
        except urllib.error.HTTPError as error:
            raw = error.read().decode("utf-8")
            try:
                return error.code, json.loads(raw)
            except ValueError:
                return error.code, raw
        except (ConnectionError, urllib.error.URLError, TimeoutError) as failure:
            return 0, "connection failed: %s" % type(failure).__name__


def normalise(value, side):
    """Volatile keys dropped; the side's folder, project id and port replaced by placeholders."""
    if isinstance(value, dict):
        if value.get("key") in ("guard_path", "port_range") and "value" in value:
            value = dict(value, value="<DIFFERS BY DESIGN>")
        return {key: normalise(item, side) for key, item in value.items() if key not in VOLATILE_KEYS}
    if isinstance(value, list):
        return [normalise(item, side) for item in value]
    if isinstance(value, str):
        text = value
        for folder in {side.root, os.path.realpath(side.root), side.root.replace("\\", "/"), os.path.realpath(side.root).replace("\\", "/")}:
            text = text.replace(folder, "<ROOT>")
        text = text.replace("\\", "/")
        text = text.replace(side.project_id, "<PID>")
        text = re.sub(r"127\.0\.0\.1:\d+", "127.0.0.1:<PORT>", text)
        text = re.sub(r'(name="monitor-token" content=")[0-9a-f]{64}', lambda m: m.group(1) + "<TOKEN>", text)
        return text
    return value


def run_pair(sides, name, method, path, body, token):
    results = []
    for side in sides:
        resolved = path.replace("{pid}", side.project_id)
        payload = body
        if "{decision" in resolved:
            index = int(re.search(r"\{decision(\d)\}", resolved).group(1))
            decisions = side.request("GET", "/p/%s/api/decisions" % side.project_id)[1]["items"]
            resolved = re.sub(r"\{decision\d\}", decisions[index]["id"], resolved)
        status, answer = side.request(method, resolved, payload, token)
        results.append((status, normalise(answer, side)))
    return results


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--harness", default=DEFAULT_RUST)
    parser.add_argument("--filter", default="")
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument("--show", action="store_true", help="print the (normalised) answer of the reference for every selected case")
    options = parser.parse_args()
    cases()
    scratch = tempfile.mkdtemp(prefix="api-diff-")
    clean_env = dict(os.environ, LOOP_CLAUDE_HOME=os.path.join(scratch, "claude-home"), LOOP_TOOLKIT_SIBLING="off", LOOP_MONITOR_OPEN="off", PYTHONIOENCODING="utf-8")
    for name in ("LOOP_DATA_DIR", "LOOP_MONITOR_URL", "CLAUDE_PLUGIN_ROOT"):
        clean_env.pop(name, None)
    os.makedirs(clean_env["LOOP_CLAUDE_HOME"], exist_ok=True)
    skill = os.path.join(scratch, "fake-plugin", "skills", "orchestrator")
    os.makedirs(skill, exist_ok=True)
    write(os.path.join(skill, "SKILL.md"), "---" + chr(10) + "name: orchestrator" + chr(10) + "---" + chr(10) + "fake" + chr(10))
    clean_env["LOOP_SKILL_DIR"] = skill
    reference = Side("reference", os.path.join(scratch, "ref"), [sys.executable, PY_HARNESS], clean_env)
    rust = Side("rust", os.path.join(scratch, "rust"), [options.harness], dict(clean_env, LOOP_REPO_ROOT=os.path.dirname(HERE)))
    failures, total = [], 0
    try:
        reference.start(47811)
        rust.start(47812)
        for name, method, path, body, token in CASES:
            if options.filter and options.filter not in name:
                continue
            total += 1
            (status_a, body_a), (status_b, body_b) = run_pair([reference, rust], name, method, path, body, token)
            if options.show:
                print("%s %s %s -> %s %s" % (name, method, path, status_a, json.dumps(body_a, ensure_ascii=False, sort_keys=True)[:400]))
            if (status_a, body_a) != (status_b, body_b):
                failures.append(name)
                print("DIFF %s %s %s" % (name, method, path))
                if options.verbose:
                    print("  reference %s %s" % (status_a, json.dumps(body_a, ensure_ascii=False, sort_keys=True)[:1500]))
                    print("  rust      %s %s" % (status_b, json.dumps(body_b, ensure_ascii=False, sort_keys=True)[:1500]))
    finally:
        reference.stop()
        rust.stop()
        shutil.rmtree(scratch, ignore_errors=True)
    print("%d case(s), %d difference(s)" % (total, len(failures)))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
