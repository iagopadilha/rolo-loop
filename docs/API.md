# Panel API

The panel is served by `harness serve` on `127.0.0.1` (first free port from 47800 to 47899, or `--port`). The server never listens on another interface. A project's panel is at `/p/<id>/` and its routes are at `/p/<id>/api/...`; the routes below that start with `/api/app/` belong to the app itself and take no project prefix.

## Rules every route follows

- Requests that change anything (`POST`, `PUT`, `DELETE`) must carry the app token in the `X-Monitor-Token` header. The token is created when the app starts, is injected into the pages the server serves (`<meta name="monitor-token">`) and is written to `app.json` in the data folder.
- A request whose `Host` header is not a loopback name is refused, and so is a cross-origin request.
- Bodies are limited in size (the limit is per route); a larger body gets `413`.
- Errors are JSON: `{"ok": false, "error": "<message>", "code": "<code>"}`, with the status that fits the kind of error. A route that does not exist answers `{"ok": false, "error": "route not found"}` with `404`.
- The "activity" column says whether the route counts as use of the app for the idle shutdown (`--idle-hours`).

## Routes (132)

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
| `POST` | `/p/{project_id}/api/answer` | yes | yes |
| `POST` | `/p/{project_id}/api/resume` | yes | yes |
| `POST` | `/p/{project_id}/api/note` | yes | yes |
| `POST` | `/p/{project_id}/api/pause` | yes | yes |
| `POST` | `/p/{project_id}/api/loop/check` | yes | yes |
| `POST` | `/p/{project_id}/api/loop/start` | yes | yes |
| `POST` | `/p/{project_id}/api/instruction` | yes | yes |
| `POST` | `/p/{project_id}/api/loop/raise-limit` | yes | yes |
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

The response shapes are the ones the panel front end in `app/frontend/` reads; they are pinned by `tools/api_diff.py`, which compares every route with the reference implementation.
