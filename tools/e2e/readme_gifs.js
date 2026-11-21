// SPDX-License-Identifier: MIT
// README GIFs from `harness demo` (fictional data only): node tools/e2e/readme_gifs.js <app url> <project id> [outDir]
// Records each scene with Playwright (video), then turns it into an optimized GIF with ffmpeg (palette, 10 fps, 960 px wide). Fails when
// the visible text at any step holds the home folder or the user name (the leak rule of ui_v2_check.js); nothing is written then.
const path = require("path");
const fs = require("fs");
const os = require("os");
const { execFileSync } = require("child_process");
const { chromium } = require("C:/npm-global/node_modules/@playwright/cli/node_modules/playwright-core");
const { leakIn } = require("./ui_v2_check.js");

const VIEW = { width: 1280, height: 800 };
const pause = (page, ms) => page.waitForTimeout(ms);

/** Each scene: a name and the steps it plays (every step checks the visible text for leaks). */
const SCENES = [
  ["rolo-loop-overview", async (page, app, id, check) => {
    await page.goto(app, { waitUntil: "domcontentloaded" });
    await pause(page, 1800); // the splash: Rolo runs in the wheel
    await page.waitForSelector(".hub-row", { timeout: 10000 });
    await check(); await pause(page, 1800);
    await page.goto(app + `p/${id}/#/loop`, { waitUntil: "domcontentloaded" });
    await page.waitForSelector(".lp-title, h1", { timeout: 10000 }).catch(() => {});
    await pause(page, 1500); await check(); await pause(page, 1500);
    await page.goto(app + `p/${id}/#/features`, { waitUntil: "domcontentloaded" });
    await pause(page, 1200);
    await page.locator("main tbody tr, main [role=row], main .fl-row, main a[href*='feature']").first().click().catch(() => {});
    await pause(page, 1500); await check(); await pause(page, 1500);
  }],
  ["rolo-loop-settings", async (page, app, id, check) => {
    for (const [route, wait] of [["#/settings/providers", "#pv-order"], ["#/settings/agents", "#ag-preview"], ["#/settings/quality", "#qg-checks"], ["#/settings/general", "#st-loop-run"]]) {
      await page.goto(app + route, { waitUntil: "domcontentloaded" });
      await page.waitForSelector(wait, { timeout: 10000 }).catch(() => {});
      await pause(page, 900); await check(); await pause(page, 1600);
      if (route.endsWith("providers")) { // the order: the subscription first, a pay-per-use API key next (shown, not saved)
        await page.getByRole("button", { name: "Add", exact: true }).click().catch(() => {});
        await page.locator("#pv-order").scrollIntoViewIfNeeded().catch(() => {});
        await pause(page, 2200); await check();
      }
    }
  }],
];

function toGif(video, gif) {
  const palette = path.join(os.tmpdir(), "rolo-gif-palette.png");
  const filters = "fps=10,scale=960:-1:flags=lanczos";
  execFileSync("ffmpeg", ["-y", "-loglevel", "error", "-i", video, "-vf", `${filters},palettegen=stats_mode=diff`, palette]);
  execFileSync("ffmpeg", ["-y", "-loglevel", "error", "-i", video, "-i", palette, "-lavfi", `${filters} [x]; [x][1:v] paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle`, gif]);
  fs.rmSync(palette, { force: true });
}

(async () => {
  const [app, id, outArg] = process.argv.slice(2);
  const out = outArg || path.join(__dirname, "..", "..", "docs", "media");
  const videos = fs.mkdtempSync(path.join(os.tmpdir(), "rolo-gifs-"));
  fs.mkdirSync(out, { recursive: true });
  const browser = await chromium.launch();
  let leaks = 0;
  for (const [name, play] of SCENES) {
    const ctx = await browser.newContext({ viewport: VIEW, colorScheme: "dark", recordVideo: { dir: videos, size: VIEW } });
    await ctx.addInitScript(() => { try { localStorage.setItem("maestro.theme", "dark"); localStorage.setItem("harness.why.seen.v1", "true"); } catch (e) {} window.__MAESTRO_NO_TOUR = true; });
    const page = await ctx.newPage();
    const check = async () => { const leak = leakIn(await page.evaluate(() => document.body.innerText)); if (leak) { leaks += 1; console.log(`LEAK ${leak} in ${name}`); } };
    await play(page, app, id, check);
    const video = await page.video().path();
    await ctx.close();
    if (!leaks) {
      toGif(video, path.join(out, `${name}.gif`));
      console.log(`saved ${name}.gif (${Math.round(fs.statSync(path.join(out, `${name}.gif`)).size / 1024)} KB)`);
    }
  }
  await browser.close();
  fs.rmSync(videos, { recursive: true, force: true });
  process.exit(leaks ? 1 : 0);
})();
