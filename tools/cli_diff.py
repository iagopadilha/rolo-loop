#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Differential test of the CLI: the same `harness` command line goes to the reference (`python app/harness.py`, ORACLE_MAESTRO)
and to the Rust binary, on identical copies of a throw-away project; exit code, stdout, stderr and the resulting file tree
(JSON compared after dropping volatile keys) must be identical.

    python tools/cli_diff.py [--harness target/debug/harness.exe] [--module state] [--verbose]
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

ORACLE = os.environ.get("ORACLE_MAESTRO", r"C:\Users\someone\work\reference\claude-maestro-old")
PY_HARNESS = os.path.join(ORACLE, "app", "harness.py")
HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_RUST = os.path.join(os.path.dirname(HERE), "target", "debug", "harness.exe")
VOLATILE_KEYS = {"at", "t", "started", "finished", "updated", "updated_at"}

SCENARIOS = []  # (module, name, files {relative: text}, [argv...], mutates)


def scenario(module, name, files, argv, mutates=False):
    SCENARIOS.append((module, name, files, argv, mutates))


ROADMAPS = {
    "basic": "# Roadmap\n## Stage 1\n- [a-api] status=verified title=\"Add API\" paths=src/api kind=back scope=Medium\n- [b-api] status=approved paths=src/b deps=a-api\n- [c-ui] status=pending paths=web/c\n## Stage 2\n- [d-ui] status=pending paths=web/d deps=b-api\n",
    "empty": "",
    "no-status": "- [x] paths=a\n- [y] status=pending title=oops\n",
    "dup": "- [a] status=pending paths=x\n- [a] status=verified paths=y\n",
    "bad-fields": "- [a] status=nope scope=Huge kind=weird e2e=maybe isolation=cloud\n- [b] status=pending tries=2 deps=b\n- [c] status=building\n- [r] status=pending regression=yes\n",
    "cycle": "- [a] status=pending deps=b paths=a\n- [b] status=pending deps=c paths=b\n- [c] status=pending deps=a paths=c\n",
    "building-deps": "- [a] status=pending paths=a\n- [b] status=building paths=b deps=a\n",
    "overlap": "- [a] status=building paths=src/a\n- [b] status=building paths=src/a/x\n- [c] status=approved paths=src/c\n",
    "dirs": "- [a] status=pending paths=src/lib deps=dir:shared\n- [b] status=pending paths=shared/x\n- [c] status=pending paths=other deps=dir:shared\n",
    "blocked": "- [a] status=blocked tries=2 paths=a\n- [b] status=approved paths=b deps=a\n- [c] status=specced paths=c\n- [d] status=pending paths=d\n",
    "multi-repo": "- [a] status=approved paths=api:src/a,web:src/w\n- [b] status=approved paths=web:src/w/x\n- [c] status=pending paths=ghost:src repo-order=api,nope\n",
    "regression": "- [refactor-x] status=approved paths=src/x regression=baseline\n- [y] status=approved paths=src/y\n",
    "worktree": "- [a] status=approved paths=src/a isolation=worktree\n- [b] status=approved paths=src/b\n",
    "recover": "- [a] status=building paths=a\n- [b] status=speccing paths=b\n- [c] status=approved paths=c\n- [d] status=specced paths=d\n- [e] status=pending paths=e\n",
    "all-done": "- [a] status=verified paths=a\n- [b] status=blocked paths=b\n",
    "crlf": "## Stage A\r\n- [a] status=pending paths=a title=\"two  words\"\r\n",
    "unicode": "## Stage 1\n- [funcao-\u00e1] status=pending title=\"a\u00e7\u00e3o\" paths=src/\u00e7\n",
    "many": "\n".join("- [f%d] status=pending paths=src/f%d" % (i, i) for i in range(12)) + "\n",
}

for key, text in ROADMAPS.items():
    files = {".specs/project/ROADMAP.md": text}
    scenario("state", "validate/" + key, files, ["state", "validate", "{work}"])
    scenario("state", "graph/" + key, files, ["state", "graph", "{work}"])
    for wave in ("1", "3", "8"):
        scenario("state", "next/%s/wave%s" % (key, wave), files, ["state", "next", "{work}", "--wave", wave])
        scenario("state", "next-headless/%s/wave%s" % (key, wave), files, ["state", "next", "{work}", "--wave", wave, "--headless"])
    scenario("state", "disjoint/" + key, files, ["state", "disjoint", "{work}", "a", "b", "c"])
    scenario("state", "disjoint-bad/" + key, files, ["state", "disjoint", "{work}", "a", "ghost"])

LOOP_JSON_OK = json.dumps({"status": "continue", "turn": 3, "feature": "a", "reason": "", "next": "gate"})
for name, loop in (("ok", LOOP_JSON_OK), ("bad-status", json.dumps({"status": "bogus", "turn": 1})), ("bad-turn", json.dumps({"status": "done", "turn": "x"})),
                   ("awaiting", json.dumps({"status": "awaiting_user", "turn": 2})), ("broken", "{nope"), ("lists", json.dumps({"status": "continue", "turn": 1, "options": [1]}))):
    scenario("state", "validate-loopjson/" + name, {".specs/project/ROADMAP.md": ROADMAPS["basic"], ".specs/project/loop/json/LOOP.json": loop}, ["state", "validate", "{work}"])
scenario("state", "validate/repos", {".specs/project/ROADMAP.md": ROADMAPS["multi-repo"],
                                    ".specs/project/REPOS.json": json.dumps({"repos": [{"name": "api", "path": "{work}"}, {"name": "web", "path": "{work}\\web"}]})},
         ["state", "validate", "{work}"])
for flags in (["--status", "continue", "--turn", "4"], ["--status", "done", "--turn", "9", "--feature", "a,b", "--reason", "all verified", "--next", "none"],
              ["--status", "awaiting_user", "--turn", "2", "--reason", "Which DB?", "--options", "a|b| c", "--to-check", "x|y"],
              ["--status", "awaiting_user", "--turn", "2"], ["--status", "nope", "--turn", "1"], ["--status", "continue", "--turn", "x"], ["--status", "continue"],
              ["--status", "paused", "--turn", "7", "--reason", "caf\u00e9"]):
    scenario("state", "loop/" + " ".join(flags)[:60], {".specs/project/ROADMAP.md": ROADMAPS["basic"]}, ["state", "loop", "{work}"] + flags, mutates=True)
legacy = {".specs/project/LOOP.json": "{}", ".specs/project/RESPOSTAS.md": "a", ".specs/project/loop_script/interno/guard.log": "x", ".specs/project/loop_script/voltas/001.json": "{}",
          ".specs/project/REGRAS.md": "r", ".specs/project/phases/etapa-01.md": "p", ".specs/loop-logs/002.json": "{}", ".specs/loop-logs/meta.json": "{}",
          "CLAUDE.md": "line\n@.specs/project/REGRAS.md\nend"}
scenario("state", "migrate/legacy", legacy, ["state", "migrate", "{work}"], mutates=True)
scenario("state", "migrate/none", {".specs/project/ROADMAP.md": "x"}, ["state", "migrate", "{work}"], mutates=True)
scenario("state", "migrate/partial", {".specs/project/loop/LOOP.json": "{}", ".specs/project/loop/GRAPH.md": "g", ".specs/project/loop/guard.log": "l"}, ["state", "migrate", "{work}"], mutates=True)
scenario("state", "usage/none", {}, ["state"])
scenario("state", "usage/unknown", {}, ["state", "bogus", "{work}"])
scenario("state", "usage/one-arg", {}, ["state", "validate"])


for key, text in ROADMAPS.items():
    files = {".specs/project/ROADMAP.md": text}
    for flag in ([], ["--ascii"], ["--short"], ["--mermaid"]):
        scenario("graph", "view/%s/%s" % (key, "".join(flag) or "default"), files, ["graph", "{work}"] + flag)
    scenario("graph", "write/" + key, files, ["graph", "{work}", "--write"], mutates=True)
scenario("graph", "usage", {}, ["graph"])

TASKS_MD = ("# Tasks\n### T1: Models [P]\n**Type**: impl\n**Files**: src/a.rs, src/b.rs <!-- note -->\n**Deps**: none\n**Reqs**: R1, R2\n**Done when**: it builds\n**Status**: pending\n\n"
            "### T2: Wire it\n**Type**: wiring\n**Files**: src/a.rs\n**Deps**: T1\n**Done when**: wired\n**Status**: pending\n\n### T3: Docs\n**Files**: docs\n**Done when**: documented\n")
TASKS_BAD = "### T1: a\n**Type**: weird\n### T1: b\n**Deps**: T9, T1\n### T2: c\n**Deps**: T3\n**Done when**: x\n### T3: d\n**Deps**: T2\n**Done when**: x\n"
SETTINGS = "**Max parallel tasks:** 2\n" + ROADMAPS["basic"]
for name, files in (("normal", {".specs/features/a-api/tasks.md": TASKS_MD, ".specs/project/ROADMAP.md": ROADMAPS["basic"]}),
                    ("crlf", {".specs/features/a-api/tasks.md": TASKS_MD.replace("\n", "\r\n"), ".specs/project/ROADMAP.md": SETTINGS}),
                    ("bad", {".specs/features/a-api/tasks.md": TASKS_BAD, ".specs/project/ROADMAP.md": ROADMAPS["basic"]}),
                    ("synthetic", {".specs/project/ROADMAP.md": ROADMAPS["basic"]}),
                    ("with-status", {".specs/features/a-api/tasks.md": TASKS_MD, ".specs/project/ROADMAP.md": SETTINGS,
                                     ".specs/project/loop/internal/tasks/a-api.json": json.dumps({"slug": "a-api", "updated_at": "x", "tasks": {"T1": {"status": "done", "wave": 1, "note": ""}}})})):
    scenario("tasks", "waves/" + name, files, ["tasks", "waves", "a-api", "--root", "{work}"])
    scenario("tasks", "waves-max1/" + name, files, ["tasks", "waves", "a-api", "--max", "1", "--root", "{work}"])
    scenario("tasks", "waves-max9/" + name, files, ["tasks", "waves", "a-api", "--max", "9", "--root", "{work}"])
    scenario("tasks", "set-done/" + name, files, ["tasks", "set", "a-api", "t1", "done", "--root", "{work}"], mutates=True)
    scenario("tasks", "set-failed-note/" + name, files, ["tasks", "set", "a-api", "T2", "failed", "--note", "café broke", "--root", "{work}"], mutates=True)
    scenario("tasks", "mirror/" + name, dict(files, **{".specs/project/loop/internal/tasks/a-api.json": json.dumps({"slug": "a-api", "updated_at": "x", "tasks": {"T1": {"status": "done"}, "T2": {"status": "running"}}})}),
             ["tasks", "mirror", "a-api", "--root", "{work}"], mutates=True)
scenario("tasks", "usage/none", {}, ["tasks"])
# argparse wording of a usage error depends on the Python version: only the exit code is part of the contract
scenario("tasks", "waves-colon-slug", {".specs/features/backend-version-endpoint/tasks.md": TASKS_MD, ".specs/project/ROADMAP.md": ROADMAPS["basic"]}, ["tasks", "waves", "backend:version-endpoint", "--root", "{work}"])

CLASSIFY = {
    "dotnet-standard": {"App.sln": "", "App.Api/x.cs": "", "App.Data/x.cs": "", "App.Domain/x.cs": ""},
    "dotnet-flat": {"App.csproj": "", "Controllers/a.cs": "", "Services/b.cs": "", "Models/c.cs": ""},
    "dotnet-partial": {"App.sln": "", "Api/x.cs": "", "Data/x.cs": ""},
    "nest-standard": {"package.json": "{\"dependencies\": {\"@nestjs/core\": 1}}", "src/api/a.ts": "", "src/data/a.ts": "", "src/domain/a.ts": ""},
    "nest-partial": {"package.json": "{\"dependencies\": {\"@nestjs/core\": 1}}", "src/api/a.ts": ""},
    "react-vite-standard": {"package.json": "{\"dependencies\": {\"react\": 1}}", "src/pages/a.tsx": "", "src/components/b.tsx": "", "src/hooks/h.ts": ""},
    "react-next-app": {"package.json": "{\"dependencies\": {\"react\": 1, \"next\": 1}}", "app/page.tsx": "", "src/components/b.tsx": ""},
    "node-plain": {"package.json": "{}", "src/index.js": ""},
    "empty": {"README.md": "x"},
}
for name, files in CLASSIFY.items():
    scenario("classify", "show/" + name, files, ["classify", "{work}"])
    scenario("classify", "write-standard/" + name, files, ["classify", "{work}", "--write", "standard", "--note", "checked by me"], mutates=True)
    scenario("classify", "write-legacy/" + name, dict(files, **{"CLAUDE.md": "# Mine\n\nText\n"}), ["classify", "{work}", "--write", "LEGACY"], mutates=True)
scenario("classify", "write-twice", {"package.json": "{}", "CLAUDE.md": "<!-- CONFORMANCE:BEGIN -->\nold\n<!-- CONFORMANCE:END -->\nrest\n"}, ["classify", "{work}", "--write", "STANDARD"], mutates=True)
scenario("classify", "bad-class", {"package.json": "{}"}, ["classify", "{work}", "--write", "weird"])
scenario("classify", "usage", {}, ["classify"])

SCALE = {
    "small": {"a.py": "x", "src/b.ts": "y", "docs/readme.md": "z"},
    "ignored": {"node_modules/x/y.js": "1", "bin/z.dll": "2", "obj/a.cs": "3", "packages/p/q.cs": "4", "src/ok.cs": "5"},
    "tops": {"src/a.cs": "x", "src/b.cs": "x", "web/c.ts": "x", "web/d.ts": "x", "web/e.ts": "x", "root.py": "x"},
    "many-areas": {"d%d/f.py" % i: "x" for i in range(12)},
    "html-css": {"a.html": "x", "b.css": "x", "c.scss": "x", "d.txt": "x", "e.vue": "x"},
}
for name, files in SCALE.items():
    scenario("scale", "text/" + name, files, ["scale", "{work}"])
    scenario("scale", "json/" + name, files, ["scale", "{work}", "--json"])
scenario("scale", "usage", {}, ["scale"])


AUDIT = {
    "ts-relative": {".specs/project/ROADMAP.md": "- [users-api] status=approved paths=src/users\n- [orders-api] status=approved paths=src/orders\n",
                    "src/orders/svc.ts": "import { User } from '../users/model';\nconst x = require('./local');\nexport * from '../users/index';\n", "src/users/model.ts": "export class User {}\n"},
    "ts-alias": {".specs/project/ROADMAP.md": "- [core-lib] status=pending paths=packages/core\n- [web-app] status=approved paths=apps/web\n",
                 "tsconfig.json": "{\n  // comment\n  \"compilerOptions\": {\"baseUrl\": \".\", \"paths\": {\"@core/*\": [\"packages/core/*\"],},}\n}\n",
                 "apps/web/page.tsx": "import { thing } from '@core/thing';\n", "packages/core/thing.ts": "export const thing = 1;\n"},
    "python": {".specs/project/ROADMAP.md": "- [data-layer] status=approved paths=app/data\n- [api-layer] status=approved paths=app/api\n",
               "app/api/views.py": "from app.data import models\nimport app.data.util as u\nfrom . import helpers\n", "app/data/models.py": "x = 1\n", "app/data/util.py": "y = 2\n", "app/api/helpers.py": "z = 3\n"},
    "csharp": {".specs/project/ROADMAP.md": "- [billing] status=approved paths=src/Billing\n- [orders] status=approved paths=src/Orders\n",
               "src/Billing/Invoice.cs": "namespace Shop.Billing\n{\n}\n", "src/Orders/Order.cs": "using Shop.Billing;\nnamespace Shop.Orders {}\n"},
    "csproj": {".specs/project/ROADMAP.md": "- [lib-x] status=approved paths=src/LibX\n- [app-x] status=approved paths=src/AppX\n",
               "src/AppX/AppX.csproj": "<Project><ItemGroup><ProjectReference Include=\"..\\LibX\\LibX.csproj\" /></ItemGroup></Project>\n", "src/LibX/LibX.csproj": "<Project/>\n"},
    "spec-text": {".specs/project/ROADMAP.md": "- [auth-service] status=approved paths=src/auth\n- [profile-page] status=approved paths=src/profile\n",
                  ".specs/features/profile-page/spec.md": "# Profile\nNeeds auth-service tokens.\n## Out of scope\nauth-service admin\n", ".specs/features/profile-page/tasks.md": "uses src/auth/token.ts\n"},
    "cycle": {".specs/project/ROADMAP.md": "- [alpha-feature] status=approved paths=src/a deps=beta-feature\n- [beta-feature] status=approved paths=src/b\n",
              ".specs/features/beta-feature/spec.md": "Needs alpha-feature first.\n"},
    "reverse-review": {".specs/project/ROADMAP.md": "- [one-feature] status=approved paths=src/one deps=two-feature\n- [two-feature] status=approved paths=src/two\n"},
    "verified-skipped": {".specs/project/ROADMAP.md": "- [done-one] status=verified paths=src/d\n- [next-one] status=approved paths=src/n\n", "src/n/a.ts": "import '../d/x';\n", "src/d/x.ts": ""},
    "refused-mark": {".specs/project/ROADMAP.md": "- [users-api] status=approved paths=src/users\n- [orders-api] status=approved paths=src/orders\n",
                     "src/orders/svc.ts": "import x from '../users/model';\n", ".specs/project/loop/DEPENDENCIES.md": "- [refused] orders-api -> users-api | import | deps=users-api | x\n"},
    "empty": {},
}
for name, files in AUDIT.items():
    scenario("audit", "show/" + name, files, ["audit", "{work}"])
    scenario("audit", "json/" + name, files, ["audit", "{work}", "--json"])
    scenario("audit", "write/" + name, files, ["audit", "{work}", "--write"], mutates=True)
    scenario("audit", "apply/" + name, files, ["audit", "{work}", "--apply"], mutates=True)
scenario("audit", "usage", {}, ["audit"])


def make_project(root, name, files, work):
    base = os.path.join(root, name)
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(base)
    for relative, text in files.items():
        path = os.path.join(base, *relative.split("/"))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8", newline="") as handle:
            handle.write(text.replace("{work}", base.replace("\\", "\\\\") if relative.endswith("REPOS.json") else base))
    return base


def tree(base):
    found = {}
    for folder, _, names in os.walk(base):
        for name in names:
            path = os.path.join(folder, name)
            rel = os.path.relpath(path, base).replace("\\", "/")
            try:
                with open(path, encoding="utf-8", newline="") as handle:
                    text = handle.read()
            except UnicodeDecodeError:
                text = "<binary>"
            if rel.endswith(".json"):
                try:
                    data = json.loads(text)
                    if isinstance(data, dict):
                        data = {k: v for k, v in data.items() if k not in VOLATILE_KEYS}
                    text = json.dumps(data, sort_keys=True)
                except ValueError:
                    pass
            found[rel] = text
    return found


def run(command, argv, work, data):
    env = {k: v for k, v in os.environ.items() if not k.startswith(("LOOP_", "GUARD_"))}
    env.update({"LOOP_DATA_DIR": data, "PYTHONIOENCODING": "utf-8", "USERPROFILE": data, "HOME": data})
    args = [a.replace("{work}", work) for a in argv]
    done = subprocess.run(command + args, capture_output=True, text=True, encoding="utf-8", errors="replace", env=env, timeout=120)
    stamp = re.compile(r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d(?::\d\d)?Z?")
    return done.returncode, stamp.sub("<ts>", done.stdout.replace(work, "<work>")), stamp.sub("<ts>", done.stderr.replace(work, "<work>"))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--harness", default=DEFAULT_RUST)
    parser.add_argument("--module", default="")
    parser.add_argument("--filter", default="")
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()
    if not os.path.isfile(PY_HARNESS) or not os.path.isfile(args.harness):
        print("need ORACLE_MAESTRO and a built harness: %s | %s" % (PY_HARNESS, args.harness))
        return 2
    root = os.path.realpath(tempfile.mkdtemp(prefix="cli-diff-"))
    data = os.path.join(root, "data")
    os.makedirs(data)
    bad = total = 0
    try:
        for module, name, files, argv, mutates in SCENARIOS:
            if (args.module and module != args.module) or args.filter not in name:
                continue
            total += 1
            safe = re.sub(r"[^A-Za-z0-9]+", "-", name)[:50]
            work_py = make_project(root, "py-" + safe, files, "")
            work_rs = os.path.join(root, "py-" + safe)  # same path for both: messages carry absolute paths
            py = run([sys.executable, PY_HARNESS], argv, work_py, data)
            py_tree = tree(work_py) if mutates else None
            make_project(root, "py-" + safe, files, "")
            rs = run([args.harness], argv, work_rs, data)
            rs_tree = tree(work_rs) if mutates else None
            if py != rs or py_tree != rs_tree:
                bad += 1
                print("MISMATCH [%s/%s]" % (module, name))
                for label, a, b in (("exit", py[0], rs[0]), ("stdout", py[1], rs[1]), ("stderr", py[2], rs[2])):
                    if a != b:
                        print("   %s py: %r\n   %s rs: %r" % (label, a if label == "exit" else a[:500], label, b if label == "exit" else b[:500]))
                if py_tree != rs_tree:
                    print("   tree py: %s\n   tree rs: %s" % (json.dumps(py_tree)[:400], json.dumps(rs_tree)[:400]))
            elif args.verbose:
                print("ok   [%s/%s]" % (module, name))
        print("%d scenarios, %d mismatches" % (total, bad))
        return 1 if bad else 0
    finally:
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
