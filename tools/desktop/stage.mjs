// SPDX-License-Identifier: MIT
// Stages what the desktop installers carry besides the app itself (crates/desktop/tauri.conf.json `bundle.resources`):
//   target/desktop-stage/plugin/            the Claude Code plugin (skills, agents, hooks, manifest) + bin/harness.exe, bin/guard.exe
//   target/desktop-stage/.claude-plugin/    marketplace.json, so the install folder is a local marketplace
// Builds harness and guard in release first. Run from anywhere: node tools/desktop/stage.mjs  (tauri build runs it as beforeBuildCommand)
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const stage = join(root, "target", "desktop-stage");
const exe = process.platform === "win32" ? ".exe" : "";

execFileSync("cargo", ["build", "--release", "-p", "cli", "-p", "guard"], { cwd: root, stdio: "inherit" });

rmSync(stage, { recursive: true, force: true });
mkdirSync(join(stage, "plugin", "bin"), { recursive: true });
mkdirSync(join(stage, ".claude-plugin"), { recursive: true });
// the plugin without its bin/ (local builds land there; the installer gets fresh release binaries)
cpSync(join(root, "plugin"), join(stage, "plugin"), { recursive: true, filter: (src) => !src.startsWith(join(root, "plugin", "bin")) });
for (const name of ["harness", "guard"]) {
  const built = join(root, "target", "release", name + exe);
  if (!existsSync(built)) throw new Error(`missing ${built}`);
  cpSync(built, join(stage, "plugin", "bin", name + exe));
}
cpSync(join(root, ".claude-plugin", "marketplace.json"), join(stage, ".claude-plugin", "marketplace.json"));
console.log(`staged ${stage}`);
