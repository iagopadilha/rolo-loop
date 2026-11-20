# Configuration

Everything is set in the app: **Settings** (left list) and the first-run guided tour. This page is the reference of what each screen saves and
where, for review, backup and automation.

## Where things live

| File | Holds | Notes |
|---|---|---|
| `~/.loop-harness/config.json` | Every setting below, in plain JSON | Written at once on every change; survives restarts and reboots. `LOOP_DATA_DIR` moves the folder. |
| `~/.loop-harness/secrets.json` | Tokens, API keys, SMTP password, webhook URL and headers | Encrypted for your Windows user (DPAPI); only names and encrypted blobs are in the file. The API never returns a secret, only `••••abcd`. |
| `~/.loop-harness/.env` (optional) | The same variables, typed by hand | Old setups keep working; a value here wins over Settings. A real environment variable wins over both. |
| `<project>/.specs/project/ROADMAP.md` | The plan and the loop settings of one project | Written in the planning conversation; approval in the panel. |
| `<project>/.specs/project/loop/internal/policy.json` | The agents and quality checks of the current turn | Written by the engine from `config.json` before each turn; read by the orchestrator. Do not edit. |

Order of precedence for a variable: real environment > `.env` > Settings (`config.json` `environment`) > secrets store.

## AI providers (`providers`)

The loop runs on the first enabled provider; on an account usage limit or an account that cannot sign in, the next one takes the turn.

```json
"providers": [
  {"id": "main", "name": "Claude (subscription)", "kind": "claude-subscription", "enabled": true, "models": {"orchestrator": "opus", "agents": ""}},
  {"id": "api", "name": "Pay per use", "kind": "anthropic-api-key", "enabled": true},
  {"id": "gw", "name": "Local models", "kind": "anthropic-gateway", "base_url": "http://localhost:4000", "enabled": false},
  {"id": "local", "name": "Local agent", "kind": "custom-cli", "command": "agy", "args": ["--dangerously-skip-permissions"], "enabled": false}
]
```

| Kind | Signs in with | Environment the run gets |
|---|---|---|
| `claude-subscription` | `claude` logged in with your plan | `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` blanked (a key would bill instead) |
| `claude-oauth-token` | token from `claude setup-token` (secret) | `CLAUDE_CODE_OAUTH_TOKEN` |
| `anthropic-api-key` | API key (secret), billed per token | `ANTHROPIC_API_KEY` |
| `anthropic-gateway` | base URL + optional key (secret) | `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN` |
| `custom-cli` | its own | the command and `args` replace `claude`; it must accept Claude Code's headless flags and print its `stream-json` (use **Test**) |

`models.orchestrator` is the `--model` of the orchestrator turn (empty = `orchestrator_model`); `models.agents` sets `CLAUDE_CODE_SUBAGENT_MODEL`.
Secrets are stored as `PROVIDER_<ID>_SECRET`. Without `providers`, the old `claude_binary` and `claude_fallback_binaries` keys define the
order (they still work).

## Agents (`agents`)

```json
"agents": {"quality-gate": {"enabled": true, "model": "opus"}, "verifier-flow": {"enabled": false, "model": ""}}
```

Optional agents (`spec-reviewer`, `verifier-flow`, `quality-gate`) can be turned off; every agent can get a model (`opus`, `sonnet`, `haiku`,
empty = its own). Skills and MCP servers allowed in loops are in **Settings > Skills and MCP** (`loop_extra_skills`, `mcp_servers`).

## Quality gate (`quality`)

```json
"quality": {"checks": {"spec": "block", "security": "block", "standards": "block", "naming": "block", "size": "block", "patterns": "warn"}}
```

Runs once per feature, after lint, tests and the standards check. `block` sends the feature back to the executor (a second block quarantines
it), `warn` reports, `off` skips the check.

## Notifications (`environment` + secrets)

| Setting | Variable | Secret |
|---|---|---|
| Channels (`toast`, `email`, `webhook`, `file`, `console`) | `LOOP_NOTIFY_CHANNELS` | |
| Events (`awaiting_user`, `done`, `error`, `paused`, `stage_closed`, `custom`; empty = all) | `LOOP_NOTIFY_EVENTS` | |
| Recipients | `LOOP_NOTIFY_TO` | |
| E-mail provider (`smtp`, `http`) | `LOOP_EMAIL_PROVIDER` | |
| SMTP server, port, security (`starttls`, `tls`, `none`), user, from | `LOOP_SMTP_HOST`, `_PORT`, `_SECURITY`, `_USER`, `_FROM` | |
| SMTP password | `LOOP_SMTP_PASSWORD` | yes |
| HTTP e-mail API URL, from | `LOOP_EMAIL_HTTP_URL`, `LOOP_EMAIL_HTTP_FROM` | |
| HTTP e-mail API token | `LOOP_EMAIL_HTTP_TOKEN` | yes |
| Webhook URL | `LOOP_WEBHOOK_URL` | yes |
| Webhook method (`POST`, `PUT`), preset (`generic`, `slack`, `teams`, `discord`), custom body | `LOOP_WEBHOOK_METHOD`, `LOOP_WEBHOOK_PRESET`, `LOOP_WEBHOOK_BODY` | |
| Webhook headers (JSON object) | `LOOP_WEBHOOK_HEADERS` | yes |

**Amazon SES** uses its SMTP interface: host `email-smtp.<region>.amazonaws.com`, port 587, STARTTLS, SES SMTP credentials.
**Body template placeholders** (filled JSON-escaped): `{{event}} {{project}} {{subject}} {{body}} {{url}} {{turn}} {{cost}} {{at}}`.

## Guided tour (`onboarding`)

`{"done": false, "step": 0}`. The tour opens on the projects page until it is finished or skipped; **Settings > General > Start tour** opens it
again.

## Secrets never reach the agents

A loop turn starts without any value of the secrets store or a secret-looking Settings value (`turn_environment`), except the sign-in of the
provider that runs it. Notices are sent by the engine, outside the turn.
