// SPDX-License-Identifier: MIT
// README screenshots from `harness demo` (fictional data only): node tools/e2e/readme_screens.js <app url> <project id> [outDir]
// Writes docs/media/<name>-<theme>.png for the hub, the loop, a feature page, Settings (AI providers, agents) and the guided tour, and fails
// when the visible text of any shot holds the home folder or the user name (the leak rule of ui_v2_check.js).
const path = require("path");
const fs = require("fs");
const { chromium } = require("C:/npm-global/node_modules/@playwright/cli/node_modules/playwright-core");
const { leakIn } = require("./ui_v2_check.js");

const SHOTS = [
  ["projects", "", null],
  ["loop", "p/{id}/#/loop", ".lp-title, h1"],
  ["feature", "p/{id}/#/features", null],
  ["settings-providers", "#/settings/providers", "#pv-order"],
  ["settings-agents", "#/settings/agents", "#ag-preview"],
];

(async () => {
  const [app, id, outArg] = process.argv.slice(2);
  const out = outArg || path.join(__dirname, "..", "..", "docs", "media");
  fs.mkdirSync(out, { recursive: true });
  const browser = await chromium.launch();
  let leaks = 0;
  for (const theme of ["light", "dark"]) {
    const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: theme, deviceScaleFactor: 1 });
    await ctx.addInitScript((t) => { try { localStorage.setItem("maestro.theme", t); localStorage.setItem("harness.why.seen.v1", "true"); } catch (e) {} window.__MAESTRO_NO_TOUR = true; }, theme);
    const page = await ctx.newPage();
    for (const [name, route, wait] of SHOTS) {
      await page.goto(app + route.replace("{id}", id), { waitUntil: "domcontentloaded" });
      if (wait) await page.waitForSelector(wait, { timeout: 8000 }).catch(() => {});
      if (name === "feature") {
        await page.waitForTimeout(800);
        await page.locator("main tbody tr, main [role=row], main .fl-row, main a[href*='feature']").first().click().catch(() => {});
      }
      await page.waitForTimeout(1200);
      const leak = leakIn(await page.evaluate(() => document.body.innerText));
      if (leak) { leaks += 1; console.log(`LEAK ${leak} in ${name} (${theme})`); continue; }
      await page.screenshot({ path: path.join(out, `${name}-${theme}.png`) });
      console.log(`saved ${name}-${theme}.png`);
    }
    await ctx.close();
  }
  // the guided tour (its second step, over Settings > Agents), light theme
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: "light" });
  await ctx.addInitScript(() => { try { localStorage.setItem("maestro.theme", "light"); localStorage.setItem("harness.why.seen.v1", "true"); } catch (e) {} });
  const page = await ctx.newPage();
  await page.goto(app + "?tour=4", { waitUntil: "domcontentloaded" });
  await page.waitForSelector(".tour-spot", { timeout: 10000 }).catch(() => {});
  await page.waitForTimeout(1200);
  if (!leakIn(await page.evaluate(() => document.body.innerText))) { await page.screenshot({ path: path.join(out, "tour-light.png") }); console.log("saved tour-light.png"); }
  await page.locator("#tour-skip").click().catch(() => {});
  await browser.close();
  process.exit(leaks ? 1 : 0);
})();
