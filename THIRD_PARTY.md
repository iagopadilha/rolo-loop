# Third-party software

claude-maestro is MIT licensed (see `LICENSE`). The panel's front end ships two small libraries, vendored unchanged in
`app/frontend/vendor/` with their licences:

| Library | Version file | Licence | Licence text |
|---|---|---|---|
| Preact (with hooks) | `preact.module.js`, `hooks.module.js` | MIT | `app/frontend/vendor/LICENSE-preact` |
| htm (with the Preact binding) | `htm.module.js`, `htm-preact.module.js` | Apache-2.0 | `app/frontend/vendor/LICENSE-htm` |

The Rust crates the binary links are listed with their licences by `cargo deny list` (the policy is in `deny.toml`); a full notice file is
generated at release time.
