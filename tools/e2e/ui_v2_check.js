// SPDX-License-Identifier: MIT
// UI v2 layout/quality checks on a running app (spec .specs/features/ui-v2/07-validation.md).
//   node ui_v2_check.js <app url> <project id> [outDir] [routes]   ->  routes = comma list of panel hash routes (features,decisions,files,errors,feature/<slug>):
//   when given, only those panel pages are checked (the default matrix stays hub + panel loop).
//   node ui_v2_check.js <app url> <project id> [outDir]   ->   JSON {cells: [{page, theme, viewport, findings: [...]}], summary}
// Matrix: pages {hub, panel} x themes {light, dark} x viewports. Findings per cell:
//   console/pageerror, http>=400, external request, page h-scroll, element overflow/clip, element past the viewport edge,
//   sidebar/main overlap, low contrast text, home path or token in visible text, missing data-theme at first paint.
const { chromium } = require("C:/npm-global/node_modules/@playwright/cli/node_modules/playwright-core");
const fs = require("fs");
const path = require("path");
const os = require("os");

// The machine's own identity: neither the home folder nor the user name may show up in a request or in the visible text.
const HOME = os.homedir(), USER = os.userInfo().username;
const homeForms = [...new Set([HOME, HOME.split("\\").join("/"), HOME.split("\\").join("\\\\")].map((x) => x.toLowerCase()))];
const leakIn = (text) => {
  const low = String(text || "").toLowerCase();
  let dec = low; try { dec = decodeURIComponent(low); } catch (e) {}
  for (const hay of [low, dec]) for (const h of homeForms) if (h.length > 3 && hay.includes(h)) return "home path";
  if (USER.length >= 3 && new RegExp("(^|[^a-z0-9])" + USER.toLowerCase().replace(/[^a-z0-9]/g, ".?") + "([^a-z0-9]|$)").test(dec)) return "user name";
  return null;
};

const VIEWPORTS = [[1440, 900], [1180, 800], [900, 700], [390, 844]];
const THEMES = ["light", "dark"];

// Runs inside the page: returns a list of {kind, detail} findings from the DOM.
function domChecks() {
  const out = [];
  const vw = window.innerWidth, vh = window.innerHeight;
  const doc = document.documentElement;
  if (doc.scrollWidth > vw + 1) out.push({ kind: "page-hscroll", detail: `scrollWidth ${doc.scrollWidth} > ${vw}` });
  if (!doc.getAttribute("data-theme")) out.push({ kind: "no-data-theme", detail: "" });

  const visible = (el) => { const r = el.getBoundingClientRect(); const cs = getComputedStyle(el); return r.width > 0 && r.height > 0 && cs.visibility !== "hidden" && cs.display !== "none" && cs.opacity !== "0"; };
  const sel = (el) => { let s = el.tagName.toLowerCase(); if (el.id) s += "#" + el.id; else if (el.classList.length) s += "." + [...el.classList].slice(0, 2).join("."); const t = (el.textContent || "").trim().replace(/\s+/g, " ").slice(0, 40); return s + (t ? ` "${t}"` : ""); };
  const clipAncestor = (el) => { for (let p = el.parentElement; p; p = p.parentElement) { const o = getComputedStyle(p).overflowX; if (o === "auto" || o === "scroll" || o === "hidden" || o === "clip") return p; } return null; };

  const all = [...document.body.querySelectorAll("*")].filter(visible);
  const seen = new Set();
  const add = (kind, el, detail) => { const k = kind + sel(el); if (seen.has(k) || out.length > 60) return; seen.add(k); out.push({ kind, detail: sel(el) + (detail ? " " + detail : "") }); };

  for (const el of all) {
    if (el.closest("[data-v2-check-ignore]") || el.classList.contains("sr-only")) continue;
    const cs = getComputedStyle(el), r = el.getBoundingClientRect();
    const hasText = [...el.childNodes].some((n) => n.nodeType === 3 && n.textContent.trim());
    // text clipped by its own box
    if (hasText && el.scrollWidth > el.clientWidth + 1 && el.clientWidth > 0) {
      const o = cs.overflowX;
      const intended = cs.textOverflow === "ellipsis" || o === "auto" || o === "scroll";
      if (!intended) add("text-overflow", el, `scrollW ${el.scrollWidth} > clientW ${el.clientWidth} overflow-x:${o}`);
    }
    // sticks out of the viewport horizontally, and no scroll container catches it
    if (r.right > vw + 1 || r.left < -1) {
      const clip = clipAncestor(el);
      if (!clip && !el.closest("dialog:not([open])")) add("past-viewport", el, `left ${Math.round(r.left)} right ${Math.round(r.right)} vw ${vw}`);
    }
  }
  // dialogs inside the viewport and not taller than it
  for (const d of document.querySelectorAll("dialog[open]")) {
    const r = d.getBoundingClientRect();
    if (r.left < -1 || r.top < -1 || r.right > vw + 1 || r.bottom > vh + 1) out.push({ kind: "dialog-outside", detail: sel(d) + ` ${Math.round(r.left)},${Math.round(r.top)},${Math.round(r.right)},${Math.round(r.bottom)} vs ${vw}x${vh}` });
  }
  // sidebar vs main overlap
  const side = document.querySelector("aside[aria-label='Sidebar'], .v2-sidebar"), main = document.querySelector("main, .v2-main");
  if (side && main && visible(side) && visible(main)) {
    const a = side.getBoundingClientRect(), b = main.getBoundingClientRect();
    const ox = Math.min(a.right, b.right) - Math.max(a.left, b.left), oy = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
    if (ox > 1 && oy > 1) out.push({ kind: "sidebar-main-overlap", detail: `overlap ${Math.round(ox)}x${Math.round(oy)}` });
  }

  // contrast
  const parse = (c) => { const m = c.match(/rgba?\(([^)]+)\)/); if (!m) return null; const p = m[1].split(/[ ,\/]+/).filter(Boolean).map(Number); return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 }; };
  const lum = ({ r, g, b }) => { const f = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); }; return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b); };
  const blend = (top, bot) => ({ r: top.r * top.a + bot.r * (1 - top.a), g: top.g * top.a + bot.g * (1 - top.a), b: top.b * top.a + bot.b * (1 - top.a), a: 1 });
  const effBg = (el) => { const stack = []; for (let p = el; p; p = p.parentElement) { const c = parse(getComputedStyle(p).backgroundColor); if (c && c.a > 0) { stack.push(c); if (c.a >= 1) break; } } let base = { r: 255, g: 255, b: 255, a: 1 }; for (let i = stack.length - 1; i >= 0; i--) base = blend(stack[i], base); return base; };
  let low = 0;
  for (const el of all) {
    if (low > 25) break;
    if (el.closest("[data-v2-check-ignore]") || el.closest("svg")) continue;
    if (![...el.childNodes].some((n) => n.nodeType === 3 && n.textContent.trim())) continue;
    const cs = getComputedStyle(el);
    const fg = parse(cs.color); if (!fg) continue;
    const bg = effBg(el);
    const f = blend(fg, bg);
    const L1 = lum(f), L2 = lum(bg), ratio = (Math.max(L1, L2) + 0.05) / (Math.min(L1, L2) + 0.05);
    const px = parseFloat(cs.fontSize), bold = parseInt(cs.fontWeight, 10) >= 700;
    const need = px >= 24 || (px >= 18.66 && bold) ? 3 : 4.5;
    if (el.disabled || el.closest("[disabled],[aria-disabled='true']")) continue;
    if (ratio < need) { low++; add("low-contrast", el, `ratio ${ratio.toFixed(2)} < ${need}`); }
  }
  // leaks in visible text
  const text = document.body.innerText || "";
  if (/[A-Za-z]:[\\\/]+Users[\\\/]+[^\\\/\s]+/i.test(text) || /\/(?:home|Users)\/[^\/\s]+\//.test(text)) out.push({ kind: "home-path-visible", detail: (text.match(/[A-Za-z]:[\\\/]+Users[\\\/]+[^\\\/\s]+|\/(?:home|Users)\/[^\/\s]+/) || [""])[0] });
  const tok = document.querySelector("meta[name='monitor-token']");
  if (tok && tok.content && tok.content.length > 12 && text.includes(tok.content)) out.push({ kind: "token-visible", detail: "" });
  return out;
}

async function settle(page, name) {
  try { await page.waitForSelector("#root *", { timeout: 8000 }); } catch (e) { /* reported by checks */ }
  await page.waitForTimeout(1500);
  // dismiss the first-visit "Why" dialog and any open dialog so the page underneath is what is measured
  for (let i = 0; i < 3; i++) {
    const open = await page.$("dialog[open]");
    if (!open) break;
    await page.keyboard.press("Escape");
    await page.waitForTimeout(250);
  }
}

module.exports = { domChecks, VIEWPORTS, THEMES, leakIn };
if (require.main === module) (async () => {
  const [appUrl, projectId, outArg, routesArg] = process.argv.slice(2);
  const outDir = outArg || path.join(".playwright-cli", "images", "ui-v2-check");
  fs.mkdirSync(outDir, { recursive: true });
  const origin = new URL(appUrl).origin;
  const pages = routesArg
    ? routesArg.split(",").filter(Boolean).map((route) => ["panel-" + route.replace(/\//g, "_"), appUrl + "p/" + projectId + "/#/" + route])
    : [["hub", appUrl], ["panel", appUrl + "p/" + projectId + "/"]];
  const browser = await chromium.launch({ headless: true });
  const cells = [];
  for (const [pname, url] of pages) for (const theme of THEMES) for (const [w, h] of VIEWPORTS) {
    const ctx = await browser.newContext({ viewport: { width: w, height: h }, colorScheme: theme });
    await ctx.addInitScript((t) => { try { localStorage.setItem("maestro.theme", t); localStorage.setItem("harness.why.seen.v1", "true"); } catch (e) {} }, theme);
    const page = await ctx.newPage();
    const findings = [];
    page.on("pageerror", (e) => findings.push({ kind: "pageerror", detail: e.message }));
    // net::ERR_NO_BUFFER_SPACE = the local TCP stack of a Windows box ran out of buffers while many pages load at once; not the app.
    page.on("console", (m) => { if (m.type() === "error" && !/ERR_NO_BUFFER_SPACE/.test(m.text())) findings.push({ kind: "console-error", detail: m.text().slice(0, 200) }); });
    page.on("request", (r) => { const what = leakIn(r.url()) || leakIn(r.postData()); if (what) findings.push({ kind: "request-leak", detail: what + " in " + r.method() + " " + new URL(r.url()).pathname }); });
    page.on("response", (r) => { if (r.status() >= 400) findings.push({ kind: "http-" + r.status(), detail: new URL(r.url()).pathname }); });
    page.on("request", (r) => { if (!r.url().startsWith(origin) && !r.url().startsWith("data:") && !r.url().startsWith("blob:")) findings.push({ kind: "external-request", detail: r.url().slice(0, 120) }); });
    await page.goto(url, { waitUntil: "networkidle" });
    await settle(page, pname);
    const applied = await page.evaluate(() => document.documentElement.getAttribute("data-theme"));
    if (applied !== theme) findings.push({ kind: "theme-not-applied", detail: `${applied} != ${theme}` });
    findings.push(...(await page.evaluate(domChecks)));
    const visibleLeak = leakIn(await page.evaluate(() => document.body.innerText));
    if (visibleLeak) findings.push({ kind: "text-leak", detail: visibleLeak + " in the visible text" });
    const shot = path.join(outDir, `${pname}-${theme}-${w}x${h}.png`);
    await page.screenshot({ path: shot });
    cells.push({ page: pname, theme, viewport: `${w}x${h}`, shot, findings });
    await ctx.close();
  }
  await browser.close();
  const bad = cells.filter((c) => c.findings.length);
  const byKind = {};
  for (const c of cells) for (const f of c.findings) byKind[f.kind] = (byKind[f.kind] || 0) + 1;
  console.log(JSON.stringify({ summary: { cells: cells.length, cellsWithFindings: bad.length, byKind }, cells }, null, 1));
})().catch((e) => { console.error("FAIL " + e.message); process.exit(1); });
