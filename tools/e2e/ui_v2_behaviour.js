// SPDX-License-Identifier: MIT
// UI v2 behaviour checks on a running app: Settings page (theme radios persist, System follows the emulated scheme, sections, Back leaves Settings),
// Esc closes dialogs / the switcher / the drawer / the "..." menu and gives the focus back, focus trap in a dialog, hash routes and Back in the panel.
//   node ui_v2_behaviour.js <app url> <project id>
const { chromium } = require("C:/npm-global/node_modules/@playwright/cli/node_modules/playwright-core");

const SECTIONS = ["appearance", "claude", "network", "mcp", "notifications", "projects", "system"];

(async () => {
  const [appUrl, pid] = process.argv.slice(2);
  const b = await chromium.launch();
  const res = [];
  const ok = (n, c, d = "") => res.push((c ? "PASS " : "FAIL ") + n + (d ? " " + d : ""));
  const ctx = await b.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: "light" });
  await ctx.addInitScript(() => { try { localStorage.setItem("harness.why.seen.v1", "true"); } catch (e) {} });
  const p = await ctx.newPage();
  const errors = [];
  p.on("pageerror", (e) => errors.push(e.message));
  p.on("console", (m) => { if (m.type() === "error") errors.push(m.text().slice(0, 120)); });
  const wait = (ms = 350) => p.waitForTimeout(ms);
  const theme = () => p.evaluate(() => [document.documentElement.getAttribute("data-theme"), localStorage.getItem("maestro.theme")]);
  const hash = () => p.evaluate(() => location.hash);
  const focusId = () => p.evaluate(() => (document.activeElement && document.activeElement.id) || "");
  const dialogOpen = async () => !!(await p.$("dialog[open]"));

  // ---- hub: Settings page ----
  await p.goto(appUrl, { waitUntil: "networkidle" }); await wait(1200);
  await p.click("#btn-settings"); await wait(500);
  ok("sidebar Settings opens the Settings page", /^#\/settings/.test(await hash()) && !!(await p.$(".pg-settings")), await hash());
  ok("Settings is a page, not a dialog", !(await dialogOpen()));
  const radios = await p.$$("[role=radio][data-theme-choice]");
  ok("appearance has 3 theme radios", radios.length === 3, String(radios.length));
  for (const value of ["dark", "light", "system"]) { await p.locator("[role=radio][data-theme-choice=" + value + "]").first().click(); await wait(250); ok("theme " + value + " -> " + JSON.stringify(await theme()), (await theme())[1] === value); }
  await p.locator("[role=radio][data-theme-choice=dark]").first().click(); await wait(250);
  await p.reload({ waitUntil: "networkidle" }); await wait(800);
  let t = await theme(); ok("dark persists after reload", t[0] === "dark" && t[1] === "dark", JSON.stringify(t));
  await p.evaluate(() => localStorage.setItem("maestro.theme", "system")); await p.emulateMedia({ colorScheme: "dark" }); await p.reload({ waitUntil: "networkidle" });
  t = await theme(); ok("system + dark scheme -> dark", t[0] === "dark", JSON.stringify(t));
  await p.emulateMedia({ colorScheme: "light" }); await wait(500); t = await theme(); ok("system follows live change -> light", t[0] === "light", JSON.stringify(t));

  // every section renders and is addressable; Back leaves Settings (sections replace the entry instead of stacking)
  await p.goto(appUrl, { waitUntil: "networkidle" }); await wait(800);
  await p.click("#btn-settings"); await wait(400);
  for (const id of SECTIONS) {
    await p.click("#st-nav-" + id); await wait(300);
    const body = await p.evaluate(() => (document.querySelector(".pg-settings .pg-body") || {}).innerText || "");
    ok("section " + id, (await hash()) === "#/settings/" + id && body.trim().length > 10, await hash());
  }
  await p.goBack(); await wait(500);
  ok("Back from a section leaves Settings", !/settings/.test(await hash()) && !(await p.$(".pg-settings")), await hash());

  // ---- dialogs: Esc closes, focus returns to the opener, focus stays inside ----
  await p.click("#hub-details"); await wait(400);
  ok("Details goes to Settings > System (no dialog)", (await hash()) === "#/settings/system" && !(await dialogOpen()), await hash());
  await p.goBack(); await wait(400);
  await p.click("#hub-create"); await wait(400);
  ok("New project opens a dialog", await dialogOpen());
  let outside = 0;
  for (let i = 0; i < 40; i++) { await p.keyboard.press("Tab"); if (!(await p.evaluate(() => document.activeElement === document.body || !!document.activeElement.closest("dialog")))) outside++; }
  ok("dialog keeps focus inside while tabbing", outside === 0, "outside=" + outside);
  await p.keyboard.press("Escape"); await wait(300);
  ok("Esc closes the dialog", !(await dialogOpen()));
  ok("focus back on the opener after Esc", (await focusId()) === "hub-create", await focusId());

  // ---- quick switcher ----
  await p.keyboard.press("Control+k"); await wait(400); ok("Ctrl+K opens the switcher", !!(await p.$(".sh-sw")));
  await p.keyboard.press("Escape"); await wait(300); ok("Esc closes the switcher", !(await p.$(".sh-sw")));
  await p.click("#btn-search"); await wait(300); ok("sidebar Search opens the switcher", !!(await p.$(".sh-sw")));
  await p.keyboard.press("Escape"); await wait(200);

  // ---- panel: routes and Back ----
  await p.goto(appUrl + "p/" + pid + "/", { waitUntil: "networkidle" }); await wait(1200);
  ok("panel opens on the loop", /^(#\/loop)?$/.test(await hash()) && !!(await p.$("#loop-zone")), await hash());
  for (const [route, sel] of [["features", ".pg-features"], ["decisions", ".pg-decisions"], ["files", ".pg-files"], ["errors", ".pg-errors"]]) {
    await p.evaluate((r) => { location.hash = "#/" + r; }, route); await wait(500);
    ok("route #/" + route, !!(await p.$(sel)), await hash());
  }
  await p.goBack(); await wait(400); ok("Back goes to the previous page", (await hash()) === "#/files", await hash());
  await p.goto(appUrl + "p/" + pid + "/#/feature/payments-api", { waitUntil: "networkidle" }); await wait(800);
  ok("deep link to a feature page", !!(await p.$("main.fp-main")), await hash());
  await p.click(".sh-crumbs a, a.sh-crumb").catch(() => {}); await wait(300);
  await p.goto(appUrl + "p/" + pid + "/#/nonsense", { waitUntil: "networkidle" }); await wait(600);
  ok("unknown route falls back to the loop", !!(await p.$("#loop-zone")), await hash());

  // status dialog: Esc closes and the focus goes back to the pill
  if (await p.$("button#navbar-status")) {
    await p.click("#navbar-status"); await wait(400);
    ok("status pill opens the status dialog", await dialogOpen());
    await p.keyboard.press("Escape"); await wait(300);
    ok("Esc closes the status dialog", !(await dialogOpen()));
    ok("focus back on the status pill", (await focusId()) === "navbar-status", await focusId());
  }

  // ---- compact: "..." menu and drawer ----
  await p.setViewportSize({ width: 900, height: 700 }); await p.reload({ waitUntil: "networkidle" }); await wait(1000);
  await p.click("#btn-more"); await wait(300);
  ok("compact menu opens", await p.evaluate(() => { const m = document.querySelector("#sh-menu"); return !!m && !m.hidden && m.getBoundingClientRect().width > 0; }));
  ok("compact menu carries the theme toggle", !!(await p.$("#sh-menu [data-theme-choice]")));
  await p.keyboard.press("Escape"); await wait(300);
  ok("Esc closes the compact menu", await p.evaluate(() => document.querySelector("#sh-menu").hidden));
  ok("focus back on the menu button", (await focusId()) === "btn-more", await focusId());
  await p.setViewportSize({ width: 390, height: 844 }); await p.goto(appUrl + "p/" + pid + "/", { waitUntil: "networkidle" }); await wait(1200);
  await p.click("#btn-menu"); await wait(400);
  ok("drawer opens", await p.evaluate(() => document.querySelector("#shell-sidebar").getBoundingClientRect().right > 100));
  await p.keyboard.press("Escape"); await wait(300);
  ok("Esc closes the drawer", await p.evaluate(() => document.querySelector("#shell-sidebar").getBoundingClientRect().right <= 0));
  await p.click("#btn-menu"); await wait(300);
  await p.click("#shell-sidebar >> text=Features"); await wait(500);
  ok("picking a page closes the drawer and goes there", (await hash()) === "#/features" && (await p.evaluate(() => document.querySelector("#shell-sidebar").getBoundingClientRect().right <= 0)), await hash());

  ok("no console errors or page errors", errors.length === 0, errors.slice(0, 2).join(" | "));
  await b.close();
  console.log(res.join("\n"));
  if (res.some((r) => r.startsWith("FAIL"))) process.exitCode = 1;
})().catch((e) => { console.error("FAIL " + e.message); process.exit(1); });
