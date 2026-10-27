// SPDX-License-Identifier: MIT
// Loads the hub and the project panel of one running app in headless Chromium and prints what a user would see:
//   node smoke.js <app url> <project id>   ->   JSON {pages: [{name, text, errors, failed}]}
const { chromium } = require("C:/npm-global/node_modules/@playwright/cli/node_modules/playwright-core");

async function visit(browser, name, url, settleSelector) {
  const page = await browser.newPage({ viewport: { width: 1500, height: 900 } });
  const errors = [], failed = [];
  page.on("pageerror", (e) => errors.push("pageerror: " + e.message));
  page.on("console", (m) => { if (m.type() === "error") errors.push("console: " + m.text()); });
  page.on("response", (r) => { if (r.status() >= 400) failed.push(r.status() + " " + new URL(r.url()).pathname); });
  await page.goto(url, { waitUntil: "networkidle" });
  if (settleSelector) { try { await page.waitForSelector(settleSelector, { timeout: 8000 }); } catch (e) { errors.push("missing: " + settleSelector); } }
  await page.waitForTimeout(1500);
  const text = await page.evaluate(() => document.body.innerText);
  const title = await page.title();
  await page.close();
  return { name, title, text, errors, failed: failed.sort() };
}

(async () => {
  const [appUrl, projectId] = process.argv.slice(2);
  const browser = await chromium.launch({ headless: true });
  const pages = [];
  pages.push(await visit(browser, "hub", appUrl, "#root *"));
  pages.push(await visit(browser, "panel", appUrl + "p/" + projectId + "/", "#root *"));
  await browser.close();
  console.log(JSON.stringify({ pages }));
})().catch((e) => { console.error("FAIL " + e.message); process.exit(1); });
