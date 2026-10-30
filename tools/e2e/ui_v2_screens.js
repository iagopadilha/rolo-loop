// SPDX-License-Identifier: MIT
// UI v2 per-screen QA: opens each dialog/screen of a running app in both themes x 4 viewports and reports findings.
//   node ui_v2_screens.js <app url> <project id> <outDir> [filter] [themes csv] [viewports csv e.g. 1440,390]
const { chromium } = require("C:/npm-global/node_modules/@playwright/cli/node_modules/playwright-core");
const { domChecks, VIEWPORTS } = require("./ui_v2_check.js");
const fs = require("fs"), path = require("path");

function extraChecks() {
  const out = [], vh = innerHeight;
  const de = document.scrollingElement;
  if (de.scrollHeight > de.clientHeight + 1) out.push({ kind: "page-vscroll", detail: `${de.scrollHeight} > ${de.clientHeight}` });
  const re = /[A-Za-z]:[\\/]+Users[\\/]+[^\\/\s]+|\/(?:home|Users)\/[^\/\s]+\//i;
  for (const el of document.querySelectorAll("[title],[aria-label],[placeholder],[alt]")) for (const a of ["title", "aria-label", "placeholder", "alt"]) { const v = el.getAttribute(a); if (v && re.test(v)) out.push({ kind: "home-path-attr", detail: `${el.tagName}[${a}]=${v.slice(0, 80)}` }); }
  for (const el of document.querySelectorAll("input,textarea")) if (re.test(el.value || "")) out.push({ kind: "home-path-input", detail: (el.id || el.name || el.tagName) + "=" + el.value.slice(0, 60) });
  // buttons/inputs smaller than usable or cut off vertically by the viewport inside open dialogs
  for (const d of document.querySelectorAll("dialog[open]")) { const r = d.getBoundingClientRect(); if (r.height > vh + 1) out.push({ kind: "dialog-taller", detail: Math.round(r.height) + ">" + vh }); }
  return out;
}

const SCREENS = require("./ui_v2_screen_defs.js");

const report = (r) => { if (!r.findings.length) console.log(`OK   ${r.screen} ${r.theme} ${r.vp}`); else { console.log(`FAIL ${r.screen} ${r.theme} ${r.vp}`); for (const f of r.findings.slice(0, 8)) console.log(`     ${f.kind}: ${f.detail}`); } };
(async () => {
  const [appUrl, pid, outDir, filter, themesArg, vpsArg] = process.argv.slice(2);
  fs.mkdirSync(outDir, { recursive: true });
  const origin = new URL(appUrl).origin;
  const themes = themesArg ? themesArg.split(",") : ["light", "dark"];
  const vps = vpsArg ? VIEWPORTS.filter(([w]) => vpsArg.split(",").includes(String(w))) : VIEWPORTS;
  const browser = await chromium.launch();
  const res = [];
  for (const sc of SCREENS) {
    if (filter && filter !== "all" && !sc.name.includes(filter)) continue;
    for (const theme of themes) for (const [w, h] of vps) {
      const ctx = await browser.newContext({ viewport: { width: w, height: h }, colorScheme: theme });
      await ctx.addInitScript((t) => { try { localStorage.setItem("maestro.theme", t); localStorage.setItem("harness.why.seen.v1", "true"); } catch (e) {} }, theme);
      const page = await ctx.newPage();
      const findings = [];
      page.on("pageerror", (e) => findings.push({ kind: "pageerror", detail: e.message }));
      page.on("console", (m) => { if (m.type() === "error" && !(sc.ignoreAbort && /ERR_FAILED/.test(m.text()))) findings.push({ kind: "console-error", detail: m.text().slice(0, 160) }); });
      page.on("response", (r) => { if (r.status() >= 400) findings.push({ kind: "http-" + r.status(), detail: new URL(r.url()).pathname }); });
      page.on("request", (r) => { if (!r.url().startsWith(origin) && !/^(data|blob):/.test(r.url())) findings.push({ kind: "external-request", detail: r.url().slice(0, 100) }); });
      try {
        await page.goto(sc.url ? sc.url(appUrl, pid) : appUrl, { waitUntil: "networkidle" });
        await page.waitForTimeout(1200);
        const ok = await sc.run(page, { w, h, theme, appUrl, pid });
        if (ok === false) findings.push({ kind: "step-failed", detail: "could not reach screen" });
        await page.waitForTimeout(400);
        findings.push(...(await page.evaluate(domChecks)), ...(await page.evaluate(extraChecks)));
        if (sc.after) findings.push(...((await sc.after(page, { w, h, theme })) || []));
      } catch (e) { findings.push({ kind: "script-error", detail: e.message.split("\n")[0].slice(0, 160) }); }
      const shot = path.join(outDir, `${sc.name}-${theme}-${w}.png`);
      try { await page.screenshot({ path: shot }); } catch (e) {}
      const seen = new Set(); const uniq = findings.filter((f) => { if ((sc.ignore || []).includes(f.kind)) return false; const k = f.kind + f.detail; if (seen.has(k)) return false; seen.add(k); return true; });
      const r = { screen: sc.name, theme, vp: w + "x" + h, findings: uniq }; res.push(r); report(r);
      await ctx.close();
    }
  }
  await browser.close();
})().catch((e) => { console.error("FAIL " + e.message); process.exit(1); });
