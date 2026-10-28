# Panel API

The panel is served by `harness serve` on `127.0.0.1` (first free port from 47800 to 47899, or `--port`). The server never listens on another interface. A project's panel is at `/p/<id>/` and its routes are at `/p/<id>/api/...`; the routes below that start with `/api/app/` belong to the app itself and take no project prefix.

## Rules every route follows

- Requests that change anything (`POST`, `PUT`, `DELETE`) must carry the app token in the `X-Monitor-Token` header. The token is created when the app starts, is injected into the pages the server serves (`<meta name="monitor-token">`) and is written to `app.json` in the data folder.
- A request whose `Host` header is not a loopback name is refused, and so is a cross-origin request.
- Bodies are limited in size (the limit is per route); a larger body gets `413`.
- Errors are JSON: `{"ok": false, "error": "<message>", "code": "<code>"}`, with the status that fits the kind of error. A route that does not exist answers `{"ok": false, "error": "route not found"}` with `404`.
- The "activity" column says whether the route counts as use of the app for the idle shutdown (`--idle-hours`).

## Routes (137)

| Method | Path | Needs token | Counts as activity |
|---|---|---|---|
| `GET` | `/api/app/projects` | no | no |
| `POST` | `/api/app/projects/open` | yes | yes |
| `POST` | `/api/app/projects/create` | yes | yes |
| `POST` | `/api/app/projects/pick-folder` | yes | yes |
| `POST` | `/api/app/projects/{project_id}/forget` | yes | yes |
| `POST` | `/api/app/projects/{project_id}/reset-specs` | yes | yes |
| `GET` | `/api/app/projects/{project_id}/reset-specs/preview` | no | no |
| `GET` | `/api/app/system/folders` | no | no |
| `GET` | `/p/{project_id}/api/progress` | no | no |
| `POST` | `/p/{project_id}/api/structure/initialize` | yes | yes |
| `POST` | `/p/{project_id}/api/conformance` | yes | yes |
| `GET` | `/p/{project_id}/api/roadmap` | no | no |
| `POST` | `/p/{project_id}/api/roadmap/approve` | yes | yes |
| `POST` | `/p/{project_id}/api/roadmap/generate` | yes | yes |
| `POST` | `/p/{project_id}/api/roadmap/refine` | yes | yes |
| `POST` | `/p/{project_id}/api/codebase/map` | yes | yes |
| `GET` | `/p/{project_id}/api/jobs` | no | no |
| `GET` | `/p/{project_id}/api/jobs/{job_id}` | no | no |
| `GET` | `/p/{project_id}/api/specs/tree` | no | no |
| `GET` | `/p/{project_id}/api/specs/file` | no | no |
| `PUT` | `/p/{project_id}/api/specs/file` | yes | yes |
| `GET` | `/p/{project_id}/api/planning` | no | no |
| `POST` | `/p/{project_id}/api/planning/send` | yes | yes |
| `GET` | `/p/{project_id}/api/planning/job/{job_id}` | no | no |
| `POST` | `/p/{project_id}/api/planning/reset` | yes | yes |
| `POST` | `/p/{project_id}/api/planning/import/preview` | yes | yes |
| `POST` | `/p/{project_id}/api/planning/import` | yes | yes |
| `GET` | `/p/{project_id}/api/planning/imports` | no | no |
| `POST` | `/p/{project_id}/api/planning/import/{import_id}/send` | yes | yes |
| `GET` | `/p/{project_id}/api/planning/readiness` | no | no |
| `POST` | `/p/{project_id}/api/planning/readiness/run` | yes | yes |
| `GET` | `/p/{project_id}/api/test-environment` | no | no |
| `PUT` | `/p/{project_id}/api/test-environment/values` | yes | yes |
| `GET` | `/p/{project_id}/api/test-environment/sources` | no | no |
| `POST` | `/p/{project_id}/api/test-environment/import` | yes | yes |
| `GET` | `/p/{project_id}/api/network` | no | no |
| `PUT` | `/p/{project_id}/api/network` | yes | yes |
| `POST` | `/p/{project_id}/api/network/suggestion` | yes | yes |
| `GET` | `/p/{project_id}/api/subprojects` | no | no |
| `PUT` | `/p/{project_id}/api/subprojects` | yes | yes |
| `POST` | `/p/{project_id}/api/subprojects/detect` | yes | yes |
| `POST` | `/p/{project_id}/api/subprojects/validate` | yes | yes |
| `POST` | `/p/{project_id}/api/subprojects/conformance` | yes | yes |
| `GET` | `/p/{project_id}/api/subprojects/standards` | no | no |
| `PUT` | `/p/{project_id}/api/subprojects/standards` | yes | yes |
| `POST` | `/p/{project_id}/api/subprojects/map` | yes | yes |
| `POST` | `/p/{project_id}/api/subprojects/system-map` | yes | yes |
| `GET` | `/p/{project_id}/api/health` | no | no |
| `GET` | `/p/{project_id}/api/state` | no | no |
| `GET` | `/p/{project_id}/api/history` | no | no |
| `GET` | `/p/{project_id}/api/agent/{agent_id*}/log` | no | no |
| `GET` | `/p/{project_id}/api/turn/{number}` | no | no |
| `GET` | `/p/{project_id}/api/graph` | no | no |
| `GET` | `/p/{project_id}/api/errors` | no | no |
| `GET` | `/p/{project_id}/api/pipeline` | no | no |
| `GET` | `/p/{project_id}/api/feature/{slug}` | no | no |
| `GET` | `/p/{project_id}/api/codebase/tree` | no | no |
| `GET` | `/p/{project_id}/api/codebase/file` | no | no |
| `GET` | `/p/{project_id}/api/decisions` | no | no |
| `POST` | `/p/{project_id}/api/decisions/{decision_id}/review` | yes | yes |
| `POST` | `/p/{project_id}/api/decisions/{decision_id}/change` | yes | yes |
| `GET` | `/p/{project_id}/api/approvals` | no | no |
| `GET` | `/p/{project_id}/api/policy` | no | no |
| `POST` | `/p/{project_id}/api/answer` | yes | yes |
| `POST` | `/p/{project_id}/api/resume` | yes | yes |
| `POST` | `/p/{project_id}/api/note` | yes | yes |
| `POST` | `/p/{project_id}/api/pause` | yes | yes |
| `POST` | `/p/{project_id}/api/loop/check` | yes | yes |
| `POST` | `/p/{project_id}/api/loop/start` | yes | yes |
| `POST` | `/p/{project_id}/api/instruction` | yes | yes |
| `POST` | `/p/{project_id}/api/loop/raise-limit` | yes | yes |
| `POST` | `/p/{project_id}/api/approvals/{approval_id}/approve` | yes | yes |
| `POST` | `/p/{project_id}/api/approvals/{approval_id}/deny` | yes | yes |
| `POST` | `/p/{project_id}/api/policy` | yes | yes |
| `POST` | `/p/{project_id}/api/shutdown` | yes | yes |
| `GET` | `/p/{project_id}/api/loop/settings` | no | no |
| `GET` | `/p/{project_id}/api/whiteboard` | no | no |
| `POST` | `/p/{project_id}/api/whiteboard` | yes | yes |
| `POST` | `/p/{project_id}/api/chat/send` | yes | yes |
| `GET` | `/p/{project_id}/api/chat/job/{job_id}` | no | no |
| `GET` | `/p/{project_id}/api/chat/threads` | no | no |
| `GET` | `/p/{project_id}/api/chat/thread/{thread_id}` | no | no |
| `GET` | `/p/{project_id}/api/actions` | no | no |
| `POST` | `/p/{project_id}/api/action/propose` | yes | yes |
| `GET` | `/p/{project_id}/api/action/job/{job_id}` | no | no |
| `POST` | `/p/{project_id}/api/action/run` | yes | yes |
| `GET` | `/api/app/projects/{project_id}/repos` | no | no |
| `POST` | `/api/app/projects/{project_id}/repos` | yes | yes |
| `POST` | `/api/app/projects/{project_id}/repos/scope/confirm` | yes | yes |
| `PATCH` | `/api/app/projects/{project_id}/repos/{name}` | yes | yes |
| `PUT` | `/api/app/projects/{project_id}/repos/{name}/role` | yes | yes |
| `DELETE` | `/api/app/projects/{project_id}/repos/{name}` | yes | yes |
| `PUT` | `/api/app/projects/{project_id}/repos/{name}/copy` | yes | yes |
| `PUT` | `/api/app/projects/{project_id}/repos/{name}/setup` | yes | yes |
| `GET` | `/api/app/projects/{project_id}/repos/{name}/copy-suggestions` | no | no |
| `GET` | `/api/app/catalog` | no | no |
| `GET` | `/api/app/catalog/sheet` | no | no |
| `POST` | `/api/app/catalog/scan` | yes | yes |
| `GET` | `/api/app/catalog/job/{job_id}` | no | no |
| `GET` | `/p/{project_id}/api/workspace` | no | no |
| `POST` | `/p/{project_id}/api/workspace/create` | yes | yes |
| `GET` | `/p/{project_id}/api/workspace/job/{job_id}` | no | no |
| `POST` | `/p/{project_id}/api/workspace/repos/{name}/commit-push` | yes | yes |
| `POST` | `/p/{project_id}/api/workspace/repos` | yes | yes |
| `PATCH` | `/p/{project_id}/api/workspace/repos/{name}` | yes | yes |
| `DELETE` | `/p/{project_id}/api/workspace/repos/{name}` | yes | yes |
| `GET` | `/p/{project_id}/api/close-loop/check` | no | no |
| `POST` | `/p/{project_id}/api/close-loop` | yes | yes |
| `GET` | `/p/{project_id}/api/wrap-up` | no | no |
| `POST` | `/p/{project_id}/api/wrap-up/finish` | yes | yes |
| `POST` | `/p/{project_id}/api/wrap-up/deliver` | yes | yes |
| `POST` | `/p/{project_id}/api/wrap-up/stop-processes` | yes | yes |
| `POST` | `/p/{project_id}/api/access/grant` | yes | yes |
| `POST` | `/p/{project_id}/api/close-loop/delete-leftover/{name}` | yes | yes |
| `GET` | `/api/app/overview` | no | no |
| `POST` | `/p/{project_id}/api/workspace/repos/{name}/commit-setup` | yes | yes |
| `GET` | `/p/{project_id}/api/follow-up` | no | no |
| `POST` | `/p/{project_id}/api/follow-up/send` | yes | yes |
| `POST` | `/p/{project_id}/api/follow-up/run` | yes | yes |
| `GET` | `/p/{project_id}/api/planning/reopen-check` | no | no |
| `POST` | `/p/{project_id}/api/planning/reopen` | yes | yes |
| `GET` | `/api/app/projects/{project_id}/repos-readiness` | no | no |
| `PUT` | `/api/app/projects/{project_id}/repos/{name}/conformance` | yes | yes |
| `POST` | `/api/app/projects/{project_id}/repos/{name}/map` | yes | yes |
| `GET` | `/api/app/projects/{project_id}/repos/{name}/standards` | no | no |
| `PUT` | `/api/app/projects/{project_id}/repos/{name}/standards` | yes | yes |
| `GET` | `/p/{project_id}/api/standards` | no | no |
| `PUT` | `/p/{project_id}/api/standards` | yes | yes |
| `GET` | `/p/{project_id}/api/quarantine/{slug}` | no | no |
| `POST` | `/p/{project_id}/api/quarantine/{slug}/summary` | yes | yes |
| `POST` | `/p/{project_id}/api/quarantine/{slug}/retry` | yes | yes |
| `POST` | `/p/{project_id}/api/quarantine/{slug}/skip` | yes | yes |
| `POST` | `/p/{project_id}/api/quarantine/{slug}/revert` | yes | yes |
| `GET` | `/api/app/health` | no | no |
| `GET` | `/api/app/info` | no | no |
| `POST` | `/api/app/notify/test` | yes | yes |
| `POST` | `/api/app/shutdown` | yes | yes |
| `GET` | `/api/app/settings` | no | no |
| `POST` | `/api/app/settings` | yes | yes |
| `GET` | `/api/app/settings/mcp-servers` | no | no |
| `POST` | `/api/app/settings/mcp-servers` | yes | yes |
| `GET` | `/api/app/settings/skills` | no | no |

The response shapes are the ones the panel front end in `app/frontend/` reads; they are pinned by `tools/api_diff.py`, which compares every route with the reference implementation.

## Additive run fields (cost forecast and E2E progress)

The engine writes these keys into the run's `meta.json` after every turn. They are additive: nothing existing changes, a client that ignores them keeps working. Where a route returns the run meta, they come with it; the panel shows them (turn bar chips and the cost limit form).

| Key | Shape | Meaning |
|---|---|---|
| `cost_forecast` | `{spent, per_turn, remaining_turns, estimated_remaining, estimated_total, cap, exceeds_cap}` (US$, rounded to cents) | Estimate of what the run still costs, from the average turn and the open features plus the verified features still to check in the final E2E phase. Absent before the first turn. |
| `cost_cap_ladder` | `{saved, rungs: [number], suggested: number or null}` | `saved` is the cap in force (the value the app stored in `max_cost_usd`, or the `--max-cost` default). `rungs` are the limits above it (x1.25, x1.5, x2, x3, rounded up to 5). `suggested` is the first rung that covers `cost_forecast.estimated_total` with a 20 % margin. Raising the limit in one step = write `suggested` to `max_cost_usd`. |
| `e2e_progress` | `{checked, total, pending}` | Only with `--e2e`: verified features with a final E2E result, verified features, and how many still lack one. "E2E 3 of 8 done, 5 left". |
| `cost_forecast_warned_cap` | number | The cap for which the "forecast passes the cap" notice was already sent (one per cap). |

`e2e=` in the ROADMAP now has four canonical words: `ok`, `failed`, `not-covered`, `pending`. The verifier's `PASS`/`FAIL`/`UNVERIFIED` (and `pass`, `fail`, `unverified`) are accepted and rewritten to `ok`/`failed`/`pending` by the engine.
## Pending approvals, project policy and the cost limit (feature `loop-policies`)

Policy: **a standard command that the project did not release asks; production is never released.** A migration (apply or create) and the standard development command (`dotnet run`, `npm run dev` and the like for other stacks) are detected by CLASS (`migrate_apply`, `migrate_create`, `dev_run`), not by one project's tool. The guard refuses the command, writes ONE request and tells the agent to carry on with the work that does not depend on it: the run does not stop. The panel shows one card per request. A command aimed at production (`--environment Production`, `NODE_ENV=production`, a production connection...) is refused for good, no request is made and there is no card or button for it.

| Route | Body | Answer |
|---|---|---|
| `GET /p/{project_id}/api/approvals` | none | `{pending: [{id, class, what, env: "DEV", command, turn, slug, at, state, scope}], counts: {pending, approved, denied}, policy}`. `command` is masked (no `NAME=secret`, connection argument or long token). |
| `POST /p/{project_id}/api/approvals/{approval_id}/approve` | `{always?: bool, reason?: text}` | `{ok, approval, resumed, pending}`. `always: true` also adds the class to the project's `commands` (the simple list below). When the last waiting request is answered and the run had stopped only because it waited (status `paused`, `stopped`, or `awaiting_user` with an approval in the reason) the run is relaunched (`resumed: true`). 404 for an id the guard never asked about. |
| `POST /p/{project_id}/api/approvals/{approval_id}/deny` | `{reason?: text}` | Same answer; the agents are told not to retry or work around it. |
| `GET /p/{project_id}/api/policy` | none | `{default: "ask", classes, alias, commands, production: "never", e2e_dev_baseline: {enabled, dev_hosts, production_hosts}}`. |
| `POST /p/{project_id}/api/policy` | `{commands?: [text], e2e_dev_baseline?: {enabled?, dev_hosts?, production_hosts?}, reason: text}` | `{ok, policy}`. 400 when the reason is missing, an entry names production, an entry is not a command the policy asks about, or a development host reaches a production host. |

The per-project policy is plain data in `PERMISSIONS.json` (other keys are kept, the replaced file is backed up): `"commands": ["migrations", "dev_run", "npm run dev"]` releases the listed classes (`migrate_apply`, `migrate_create`, `dev_run`, or the alias `migrations` for both migration classes) or literal commands; `"e2e_dev_baseline": {"enabled": true, "dev_hosts": [...], "production_hosts": [...]}` is the per-project DEV opt-in of the E2E baseline (off by default; when on, the engine applies the baseline before each turn and the app can start in DEV). Every answer and every policy change is a line in the permissions journal (`kind` `command_approval` or `policy`; no secret). The state payload carries `approvals` (same shape as `GET approvals` without `policy`).

The guard summary of the state payload (`events`, `counts`) also carries `groups` (X2): `[{rule, detail, count, first_t, last_t, caller}]`, one row per repeated refusal (up to 10, most frequent first) with the command or tool call that made it (redacted). A map job that the guard would refuse fails before it starts, with every refused path in one message (M6).

`POST loop/raise-limit` takes `{max_cost_usd: number}` or `{suggested: true}`. The default cost limit stays US$ 50. `suggested` raises it to the engine's suggested rung (`meta.cost_cap_ladder.suggested`, else the first rung above the limit in force), saves it and relaunches the loop: one click, no number to type. The quick-limit buttons of the panel call it with their rung.

## `GET /p/{project_id}/api/state`: live status fields

These fields are added to the state payload (feature `live-status-and-notices`). None of them carries a command line, an environment value or a credential path.

| Field | Shape | Meaning |
|---|---|---|
| `processes` | `[{id, kind, turn, pid, started_at, label, feature, step, lanes: [{feature, step}], claude, claude_account}]` | What the engine started and is running now (`kind` is `claude_turn`, or `left_over` for a program a turn started and left running; each row also has `name` (program file name) and `cwd`). Empty when the loop is not alive. `label` is a one-line title such as `Loop turn 7 · feature a, b · step execute`. |
| `now` | `{running, turn, label, started_at, pid, claude, claude_account, lanes: [{feature, name, step, stage, stage_title}]}` | The "running now" strip built from `processes` and the roadmap view. `{running: false, lanes: []}` when idle. |
| `meta.claude_events` | `[{kind, turn, at, from: {command, account}, to: {command, account}, to_main, reason, unusable}]` | Each switch of the Claude account, newest 30. `kind` is `usage_limit_switch`, `picker_switch`, `back_to_main` or `account_unusable` (an unusable account has no reset time). The account of each turn is in the turn rows (`claude`, `claude_account`). |

The state payload also carries the permissions journal (feature `e2e-dev-baseline`), read-only:

| Field | Shape | Meaning |
|---|---|---|
| `permissions_log` | `{path, total, granted, refused, entries: [{id, at, kind, value, reason, who, outcome, detail, backup}]}` | The newest 50 lines of `.specs/project/loop/json/PERMISSIONS-LOG.json`, newest first. `kind` is `host`, `cost_limit`, `baseline`, `command_approval` (an answer to a pending approval) or `policy` (a change of the project policy); `outcome` is `granted` or `refused` (`detail` says what was done or why it was refused); `backup` is the copy of the replaced file, relative to the work folder, or empty. `reason` and `value` are redacted before they are written, so the journal never holds a credential. Grants come from `harness grant` / `harness baseline` and the engine function `grant_access`; the panel writes it only through the approval and policy routes below. |

Planning gates (`/api/pipeline` and the readiness routes) also change:

- The `ready` gate carries `problems` and `warnings` as separate lists; `items` keeps both, as before. When a loop is running with an approved plan, a stale readiness result no longer turns the gate red (`approved_running: true`).
- The `repos` and `import_coverage` gates are always present; when they do not apply they are `ok`, `optional` and `not_applicable: true`.

Notices (message, e-mail, `NOTICE.txt`) name the features and the stage, use one vocabulary ("verificadas" until the branch is pushed, then "entregues"), and also fire when a turn verifies a feature or closes a stage (`LOOP_NOTIFY_MILESTONES=off` turns that off).

### Finish: delivery state, steps and processes

The delivery state in `GET close-loop/check` and `GET wrap-up` is decided by the remote and GitHub, never by a `delivery.json` left by an earlier run. The push is confirmed only when `origin` holds the loop branch at the worktree HEAD (`git ls-remote`). The pull request exists only when GitHub lists one for the branch (`gh`); when GitHub cannot be asked (not a GitHub remote, `gh` missing or signed out) the saved record is used, and only if it names this same branch. A record of another branch, or one that does not name its branch, is ignored (`delivery_ignored` says so), and a dry run is never written as a state. Nothing here pushes `main` or the integration branch: delivery pushes only the loop branch, by explicit refspec.

Each repo item of the check (`repos[]`) now also carries `delivery_source` (`remote`, `saved record` or `none`), `delivery_ignored` (text or `null`), `holders` (`[{pid, name, kind, label}]`: programs the loop started that may hold files in the worktree), `trash_folders_left` (folders still being deleted in the background) and, in every `reasons[]` item, `action`: `{id, label, method, route}` or `null`. Every `reasons[].text` is the real cause in plain words. New reason codes: `folder_left` (git forgot the worktree but files remain; action `delete_leftover`) and `files_in_use` (a close that could not remove the folder; the text says "Nothing is unsafe", lists the holders by name and pid and adds that the work is pushed and backed up). Credentials in a remote URL are stripped from every text.

`GET close-loop/check` and `GET wrap-up` (`check`) also carry `processes`: `[{pid, name, kind, label}]`, the programs the loop started that are still running.

`GET wrap-up` adds:

| Key | Shape | Meaning |
|---|---|---|
| `steps` | `[{id, title, what, status, result, action}]` | The Finish as ordered steps, the shape `components/stepper.js` renders. `id` is one of `stop_processes`, `commit`, `push`, `pr`, `database`, `backup`, `remove`. `status` is `done`, `active` (the first open step), `todo`, `error` (something real blocks it; `result` is the real reason, `action` the one thing that fixes it) or `skipped`. `action` is `{id, label, method, route}` (route relative to the project API) or `null`. A step may also carry `actions` (one per repo), `warnings` (step `database`: the production database script warnings) and `processes` (step `stop_processes`). |
| `finish_options` | `[{mode, label, risky, what_happens}]` | What each Finish button does in plain words; `risky` marks `remove_anyway`. |
| `check.repos[].unpushed_by_feature` | `[{feature, commits}]` | The unpushed commit subjects grouped by the feature in the subject's scope (`feat(<feature>): ...`), `other` when there is none. |

`POST wrap-up/deliver` (no body) is ONE call that pushes the loop branch and opens the PR of every repo of the run (the primary first, the PRs linked to each other). It answers `{ok, delivered: {<repo>: {branch, push, pr_status, pr}}, warnings, check}` (`check` is the same object as `GET wrap-up` `check`). `POST wrap-up/stop-processes` ends the programs the loop started (by pid, with their children; never by program name) and answers `{ok, stopped: [{pid, name, kind, label}], check}`. `POST close-loop/delete-leftover/{name}` deletes, in the background, a worktree folder that git already forgot (refused for anything that is not clearly a loop worktree folder). All three answer 409 `loop_running` while the loop runs.

`POST workspace/repos/{name}/commit-push` now commits the work without the loop's own `.specs/` state, pushes only the loop branch by explicit refspec, and answers with `check` (that repo's item from the Finish check, asked to the remote and GitHub) and `next` (what is still missing, in plain words), so the click never ends in a dead end.

`POST wrap-up/finish` stops what the loop started first (`receipt.stopped_processes`), delivers only what the check found pending, and when it removes worktrees it moves the ignored `node_modules`, `bin` and `obj` to a background deletion (`receipt.background_delete`); a folder that cannot go is reported as `files_in_use` or `folder_left`, not as "not safe to remove". The PR body has only: what this PR delivers (the verified features by title), the quarantined and the unverified features, what to validate, the related PRs and one closing line.

## Payload notes

- `POST /p/{project_id}/api/planning/import`: the body may carry `on_existing` (`"merge"` or `"replace"`). When the feature folder `.specs/features/<slug>` already exists without an import record and `on_existing` is missing, the answer is `409` with `code: "feature_folder_exists"` and `choices: ["merge", "replace"]`. `merge` keeps the files already in the folder and adds the missing ones; `replace` swaps the folder for the imported copy. Any other value is a `400`.

## Validation evidence (`GET state`)

Additive fields from feature `validation-evidence-and-incentives`. Nothing carries a credential value; credentials appear by name only.

| Field | Shape | Meaning |
|---|---|---|
| `validations` | `[{feature, layer, what, evidence, status}]` | What each validation checks, per verified feature and layer (`api`, `ui`, or `run`). `evidence` is the list of artifact paths. `status` is `ok`, `failed`, `pending`, `unproven` (ok without evidence), `not-covered` (accepted), `rejected` (not-covered without proof of no surface), `blocked` (a call was refused) or `skipped-by-user`. |
| `manual_checks` | `[{feature, what, reason, source}]` | What stays with the user and why. `source` is `by_design`, `not_covered`, `skipped` or `checkpoint`; `feature` is `null` for by-design items. |
| `loop.credential_request` | `{name, names, feature, who, if_declined, options}` or `null` | Present while the loop waits on a credential decision. `options` always includes `continue without this layer`. |

ROADMAP feature fields read by the gate: `evidence=a,b`, `no-surface=why_words`, `blocked-call=call`, `needs-credential=NAME`, `credential-owner=who`. The same facts may be written as `key: value` lines in `.specs/features/<slug>/E2E.md` (`evidence:`, `no-surface:`, `blocked-call:`, `credential:`, `credential-owner:`, `if-declined:`). With `--e2e`, `e2e=ok` without existing evidence and `e2e=not-covered` without a reason and proof of no surface end the run with `error` and a message naming what is missing; a blocked call or a missing credential ends it with `awaiting_user`. `.specs/project/loop/json/EVIDENCE.json` may override `surface_markers` and `no_surface_kinds`.
