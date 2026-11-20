# Desktop app and installers

Agents Maestro ships as a Windows desktop app (Tauri 2, `crates/desktop`) with two installers built by `cargo tauri build`:

| File | Built by | Installs to | Admin | Use it when |
|---|---|---|---|---|
| `Agents Maestro_<version>_x64-setup.exe` | NSIS | `%LOCALAPPDATA%\Agents Maestro` (per user) | no | the normal download; the in-app updater uses it |
| `Agents Maestro_<version>_x64_en-US.msi` | WiX | `%ProgramFiles%\Agents Maestro` (per machine) | yes | managed machines (Intune, GPO, `msiexec /qn`) |

Both install the same files:

```
<install folder>/
  agents-maestro.exe               the app ("Agents Maestro" in the Start menu): runs the panel server in-process and shows it in its own window
  .claude-plugin/marketplace.json  the folder is a local Claude Code marketplace
  plugin/                          skills, agents, hooks
    bin/harness.exe                the command line (on PATH after install)
    bin/guard.exe                  the PreToolUse hook
  app/frontend/  app/templates/    the panel and the .specs templates
```

What else each installer does:

- Start-menu shortcut "Agents Maestro" (Windows Search finds it) and an entry in Settings > Apps > Installed apps with the icon, publisher, version and Uninstall.
- WebView2: Windows 11 has it; on Windows 10 the installer runs the embedded bootstrapper (needs internet, +1.8 MB).
- `harness` on PATH: the NSIS installer runs `harness install --desktop-exe ... --path add` (user PATH; `--path remove` on uninstall). The MSI adds `<install>\plugin\bin` to the system PATH through a WiX fragment (`crates/desktop/windows/path.wxs`).
- `~/.loop-harness/config.json` gets `app_root_dir`, `orchestrator_skill_dir` and `desktop_exe`. The app also fills them at every start (the MSI cannot write per-user files), and remembers `desktop_port` so the panel keeps the same address (and the same theme and layout preferences).
- Uninstall removes the app, the shortcut and the PATH entry. `~/.loop-harness` (your projects list and settings) stays.
- Languages: English and Português (Brasil).

## After installing

1. Open **Agents Maestro** from the Start menu.
2. In Claude Code, add the plugin from the install folder:
   ```
   /plugin marketplace add "%LOCALAPPDATA%\Agents Maestro"
   /plugin install claude-maestro@claude-maestro
   ```
3. `harness doctor` in a new terminal.

When the orchestrator skill runs `harness serve --project <repo>`, the project opens in the desktop app (it is started if it is not running). `--no-desktop` (or `LOOP_NO_DESKTOP=1`) keeps the old headless server in the browser.

## Building the installers

```
cargo install tauri-cli --version "^2.12" --locked   # once
cd crates/desktop
cargo tauri build                                      # -> target/release/bundle/{nsis,msi}/
```

`beforeBuildCommand` runs `tools/desktop/stage.mjs`: it builds `harness` and `guard` in release and stages the plugin under `target/desktop-stage/`. The first build downloads NSIS and WiX into the Tauri cache. The MSI can only be built on Windows.

## Signing (before a public release)

Unsigned installers work, but Windows SmartScreen shows "Windows protected your PC" until the file builds reputation, and Smart App Control (Windows 11) blocks unsigned apps. Options, cheapest first:

- **Azure Artifact Signing** (formerly Trusted Signing): monthly subscription, no hardware token; set `bundle.windows.signCommand` to the Artifact Signing CLI and keep the Azure credentials as CI secrets.
- **OV code-signing certificate**: set `bundle.windows.certificateThumbprint`, `digestAlgorithm: "sha256"` and a `timestampUrl`. EV no longer skips SmartScreen.

## In-app updates

The updater (tauri-plugin-updater) needs a signing key pair, separate from Authenticode:

```
cargo tauri signer generate -w ~/.tauri/agents-maestro.key
```

Keep the private key and its password out of the repository (CI secrets `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`); put the public key in `plugins.updater.pubkey`. Losing the private key means existing installs can no longer update. The release workflow publishes `latest.json` next to the installers on GitHub Releases.
