# Template of `.specs/project/TEST-ENVIRONMENT.md`

Direct on purpose: a 2 to 3 line summary and small tables of variable NAMES. No prose sections (what it is for, what may be tested, what never to do: obvious). The app copies the block below to `.specs/project/TEST-ENVIRONMENT.md`; the preflight (`harness.py check`) refuses a file that still has `FILL_IN`, holds a plaintext secret or mentions a `$NAME` no `.env` defines (so keep `$` only on names that exist). In `Where to get it` write only the source, never a value: `appsettings: <file>:<Key:Path>` (key found in appsettings*.json, .env.example or config of the repo), `Infisical: <key> (<path/environment>)` (the code reads it from the secret manager; the AI never opens Infisical or a real `.env`), or `FILL_IN` when the source cannot be determined. Values live in ONE place: `.specs/project/.env.test` (the project's only credential source; nothing else is read at run time). The Test environment tab lets the user fill the missing ones or import names from machine files (`env_sources` in Settings). `python <toolkit>/shared/scripts/testenv.py list --project <root>` shows which names are defined, values masked. Hosts the agents may reach are in the same tab (Network section, `.specs/project/NETWORK.json`): `## Allowed` and `## Forbidden` below say what they may DO there. When `$ACCESS_TOKEN` is a JWT, add an `Expected token claims:` line (below): the check compares the token with it.

```markdown
# Test environment (DEV)

FILL_IN: 2 to 3 lines: what is tested and against which app or API (DEV and localhost only).

## Where it runs
| Item | Variable | Note |
|------|----------|------|
| Local app | `$LOCAL_URL` | FILL_IN: port and the command that starts it |

## Login (to get the access token)
| User | Password | Token comes from |
|------|----------|------------------|
| `$TEST_USER_1_EMAIL` | `$TEST_USER_1_PASSWORD` | FILL_IN: the login route or page |

## API headers and tokens
| Header or call | Variable | Where to get it |
|----------------|----------|-----------------|
| FILL_IN: header name, for example Authorization | `$ACCESS_TOKEN` | FILL_IN: `appsettings: <file>:<Key:Path>` or `Infisical: <key>` |

## Test data
| What | Variable |
|------|----------|
| FILL_IN: one row per sample the features need (CNPJ, id, address) | `$SAMPLE_ID` |

## Allowed
- Read routes (GET) and screens of the app under test.
- Create test data whose name starts with `E2E_`.

## Forbidden
- Delete anything, or change user permissions.
- Send e-mail or WhatsApp messages.
- Switch company or tenant.
- Reach a host that is not in the project's network list.

## Token claims (only when the token is a JWT)
Example line to write when it applies: Expected token claims: aud=<app>; domain=<company domain>
```
