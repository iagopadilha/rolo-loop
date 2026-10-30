// SPDX-License-Identifier: MIT
// UI v2 keyboard pass: Tab through a page (a hub URL or a panel route like .../p/<id>/#/errors or .../#/settings/claude), report order,
// missing focus ring, off-screen focus and traps. With [opener] (a CSS selector, e.g. #navbar-status or #hub-details) that element is
// clicked first, so the pass runs inside the dialog it opens; Esc must then close it and give the focus back to the opener.
//   node ui_v2_keyboard.js <url> [theme] [width] [tabs] [opener]
const { chromium } = require("C:/npm-global/node_modules/@playwright/cli/node_modules/playwright-core");
(async () => {
  const [url, theme = "light", w = "1440", n = "45", opener] = process.argv.slice(2);
  const b = await chromium.launch(); const ctx = await b.newContext({ viewport: { width: +w, height: 900 }, colorScheme: theme });
  await ctx.addInitScript((t) => { localStorage.setItem("maestro.theme", t); localStorage.setItem("harness.why.seen.v1", "true"); }, theme);
  const p = await ctx.newPage(); await p.goto(url, { waitUntil: "networkidle" }); await p.waitForTimeout(1500);
  if (opener) { await p.click(opener); await p.waitForTimeout(500); }
  const seen = new Map(); const rows = [];
  for (let i = 0; i < +n; i++) {
    await p.keyboard.press("Tab"); await p.waitForTimeout(60);
    const r = await p.evaluate(() => { const e = document.activeElement; if (!e || e === document.body) return { name: "BODY" }; const cs = getComputedStyle(e), r = e.getBoundingClientRect();
      const ring = (cs.outlineStyle !== "none" && parseFloat(cs.outlineWidth) > 0) || (cs.boxShadow && cs.boxShadow !== "none");
      return { name: e.tagName.toLowerCase() + (e.id ? "#" + e.id : "") + " " + (e.getAttribute("aria-label") || e.textContent || "").trim().slice(0, 28), ring, vis: r.width > 0 && r.right > 0 && r.left < innerWidth && r.bottom > 0 && r.top < innerHeight, dlg: !!e.closest("dialog") }; });
    rows.push(r); seen.set(r.name, (seen.get(r.name) || 0) + 1);
    if (seen.get(r.name) > 2) { rows.push({ name: "-- cycle back at " + r.name }); break; }
  }
  for (const r of rows) console.log((r.ring === false ? "NORING " : r.vis === false ? "OFFSCR " : "       ") + r.name);
  if (opener) {
    const escaped = rows.some((r) => r.dlg === false && r.name !== "BODY" && !r.name.startsWith("--"));
    await p.keyboard.press("Escape"); await p.waitForTimeout(400);
    const closed = !(await p.$("dialog[open]")), back = await p.evaluate((sel) => document.activeElement === document.querySelector(sel), opener);
    console.log(escaped ? "FAIL focus left the dialog" : "OK   focus stayed in the dialog");
    console.log((closed ? "OK   " : "FAIL ") + "Esc closes the dialog");
    console.log((back ? "OK   " : "FAIL ") + "focus back on " + opener);
  }
  await b.close();
})();
