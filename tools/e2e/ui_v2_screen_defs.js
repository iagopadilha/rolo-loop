// SPDX-License-Identifier: MIT
// Screen definitions for ui_v2_screens.js: each {name, url?, run(page, ctx), after?}.
const click = async (page, sel) => { const l = page.locator(sel).first(); if (!(await l.count())) return false; await l.click({ timeout: 3000 }).catch(() => {}); await page.waitForTimeout(500); return true; };
const menu = async (page, ctx) => { if (ctx.w < 900) await click(page, "#btn-menu"); };
const open = (sel) => async (page) => { const ok = await click(page, sel); return ok && !!(await page.$("dialog[open]")); };

// Settings sections (ids of src/pages/settings_page.js) and routes of the panel (src/pages/panel/router.js).
const SECTIONS = ["appearance", "claude", "network", "mcp", "notifications", "projects", "system"];
const ROUTES = ["loop", "features", "decisions", "files", "errors", "feature/payments-api"];
const hasDlg = async (p) => !!(await p.$("dialog[open]"));
const visible = async (p, sel) => p.evaluate((q) => { const e = document.querySelector(q); if (!e) return false; const r = e.getBoundingClientRect(), cs = getComputedStyle(e); return r.width > 0 && r.height > 0 && cs.visibility !== "hidden" && r.right > 0 && r.left < innerWidth; }, sel);
// the sidebar is a drawer under 900px: open it before touching an item of it
const drawer = async (page, ctx) => { if (ctx.w < 900) await click(page, "#btn-menu"); };
// the header folds its secondary actions into "..." under 1180px
const more = async (page, ctx) => { if (ctx.w < 1180) await click(page, "#btn-more"); };
// the panel header always keeps Finish, Back to planning, Workspace, Copy path, Shut down and "Why Agent Maestro?" in "..."
const panelMore = (page) => click(page, "#btn-more");

module.exports = [
  { name: "hub-base", run: async (p) => !!(await p.$("#hub-main")) },
  { name: "hub-settings-nav", run: async (p, c) => { await drawer(p, c); await click(p, "#btn-settings"); return /#\/settings/.test(p.url()) && !!(await p.$(".pg-settings")); } },
  ...SECTIONS.map((id) => ({ name: "hub-settings-" + id, url: (a) => a + "#/settings/" + id, run: async (p) => !!(await p.$(".pg-settings")) })),
  { name: "hub-why", run: async (p, c) => { await more(p, c); await click(p, ".btn-help, #btn-help"); return hasDlg(p); } },
  { name: "hub-compact-menu", run: async (p, c) => { if (c.w >= 1180) return true; return (await click(p, "#btn-more")) && (await visible(p, "#sh-menu")); } },
  { name: "hub-drawer", ignore: ["sidebar-main-overlap"], run: async (p, c) => { if (c.w >= 900) return true; await click(p, "#btn-menu"); return visible(p, "#shell-sidebar"); } },
  { name: "hub-open", ignoreAbort: true, run: async (p) => { await p.route("**/projects/pick-folder", (r) => r.abort()); await click(p, "#hub-open"); return !!(await p.$("dialog[open]")); } },
  { name: "hub-new", run: async (p, c) => { await more(p, c); return open("#hub-create")(p); } },
  { name: "hub-new-browse", ignoreAbort: true, run: async (p, c) => { await p.route("**/projects/pick-folder", (r) => r.abort()); await more(p, c); await click(p, "#hub-create"); await click(p, "dialog button:has-text(\"Choose\")"); return !!(await p.$("#fb-field")); } },
  { name: "hub-details", run: async (p) => { await click(p, "#hub-details"); return /#\/settings\/system/.test(p.url()) && !!(await p.$(".pg-settings")); } },
  { name: "hub-reset", run: async (p) => { await click(p, "[id^=menu-]"); await click(p, "[id^=reset-]"); return !!(await p.$("[id^=reset-confirm], input[aria-describedby^=reset-help]")); } },
  { name: "hub-forget", run: async (p) => { await click(p, "[id^=menu-]"); await click(p, "[id^=forget-]"); return !!(await p.getByText("Yes, forget").count()); } },
  { name: "hub-rowmenu", run: async (p) => click(p, "[id^=menu-]") },
  { name: "hub-switcher", run: async (p) => { await p.keyboard.press("Control+k"); await p.waitForTimeout(500); return !!(await p.$(".sh-sw")); } },
];

const P = (a, pid) => a + "p/" + pid + "/";
const R = (route) => (a, pid) => a + "p/" + pid + "/#/" + route;
const btn = (re) => async (p) => { const l = p.getByRole("button", { name: re }).first(); if (!(await l.count())) return false; await l.click({ timeout: 3000 }).catch(() => {}); await p.waitForTimeout(700); return true; };
module.exports.push(
  ...ROUTES.map((r) => ({ name: "panel-" + r.replace(/\//g, "_"), url: R(r), run: async (p) => !!(await p.$("main.pg, main.fp-main, #loop-zone")) })),
  ...SECTIONS.map((id) => ({ name: "panel-settings-" + id, url: R("settings/" + id), run: async (p) => !!(await p.$(".pg-settings")) })),
  { name: "panel-settings-nav", url: P, run: async (p, c) => { await drawer(p, c); await click(p, "#btn-settings"); return /#\/settings/.test(p.url()) && !!(await p.$(".pg-settings")); } },
  { name: "panel-status-dialog", url: P, run: async (p) => { await click(p, "#navbar-status"); return hasDlg(p); } },
  { name: "panel-account-dialog", url: P, run: async (p, c) => { await drawer(p, c); await click(p, "#sh-account"); return hasDlg(p); } },
  { name: "panel-error-dialog", url: R("errors"), run: async (p) => { await click(p, ".pg-errors .ls-row"); return hasDlg(p); } },
  { name: "panel-plan-dialog", url: R("files"), run: async (p) => { await click(p, "#btn-ask-plan"); return hasDlg(p); } },
  { name: "panel-roadmap-dialog", url: R("features"), run: async (p) => { await click(p, "#btn-open-roadmap"); return hasDlg(p); } },
  { name: "panel-features-deps", url: R("features"), run: async (p) => { await btn(/^Dependencies$/)(p); return !!(await p.$(".pg-features")); } },
  { name: "panel-agent-dialog", url: R("loop"), run: async (p) => { if (!(await btn(/Ask the agent/)(p))) { await click(p, ".lp-icon-btn"); await btn(/Ask the agent/)(p); } return hasDlg(p); } },
  { name: "panel-compact-menu", url: P, run: async (p, c) => { if (c.w >= 1180) return true; return (await click(p, "#btn-more")) && (await visible(p, "#sh-menu")); } },
  { name: "panel-drawer", url: P, ignore: ["sidebar-main-overlap"], run: async (p, c) => { if (c.w >= 900) return true; await click(p, "#btn-menu"); return visible(p, "#shell-sidebar"); } },
  { name: "panel-why", url: P, run: async (p) => { await panelMore(p); await click(p, "#btn-help"); return hasDlg(p); } },
);

// ---- mocks: patch a real JSON response (state/progress/...) to reach screens the demo data never shows ----
const patch = async (page, pattern, fn) => page.route(pattern, async (route) => { try { const r = await route.fetch(); const j = await r.json(); const out = fn(j) || j; await route.fulfill({ response: r, json: out }); } catch (e) { await route.continue().catch(() => {}); } });
const stateStatus = (status) => async (p) => {
  await patch(p, /\/api\/state(\?|$)/,(j) => { j.loop = Object.assign({}, j.loop, { status, running: false, pid: null }); j.work = j.repo + "-loop"; });
  if (status === "done" || status === "stopped") await patch(p, /wrap-up$/, (j) => { j.status = status; j.check = { repos: [{ repo: "acme-checkout", clean: false, uncommitted: ["services/coupon-rules/index.ts", "web/checkout-ui/cart.tsx"], unpushed: 2, branch: "loop/acme-checkout" }] }; j.unreviewed_critical = [{ id: "d1" }]; });
};
const clickText = async (p, text, scope = "") => { const l = (scope ? p.locator(scope).locator(`text=${text}`) : p.locator(`text=${text}`)).first(); if (!(await l.count())) return false; await l.click({ timeout: 3000 }).catch(() => {}); await p.waitForTimeout(700); return true; };
module.exports.push(
  { name: "panel-pause-dialog", url: P, run: async (p) => { await patch(p, /\/api\/state(\?|$)/, (j) => { j.quarantine = [{ id: "coupon-rules", reason: "verifier failed 3 times: tests red", since: j.now }]; }); await p.route(/quarantine.coupon-rules.summary/, (r) => r.fulfill({ json: { job: "jx" } })); await p.route(/chat.job.jx/, (r) => r.fulfill({ json: { state: "done", reply: "" } })); await p.route(/quarantine.coupon-rules(\?|$)/, (r) => r.fulfill({ json: { id: "coupon-rules", step: 3, step_agent: "verifier", turn: 7, reason: "verifier failed 3 times: tests red", attempts: 3, failed_tests: ["coupon applies twice", "expired coupon accepted"], uncommitted: ["services/coupon-rules/index.ts"], skip_impact: ["checkout-ui"], failures: [{ at: "T7", text: "expected 10 got 20" }], summary: "" } })); await p.reload({ waitUntil: "networkidle" }); await p.waitForTimeout(1200); await btn(/Understand and resolve/)(p); return hasDlg(p); } },
  { name: "panel-startloop", url: P, run: async (p) => { await stateStatus("stopped")(p); await p.reload({ waitUntil: "networkidle" }); await p.waitForTimeout(1200); await btn(/Start loop/)(p); return hasDlg(p) || !!(await p.$("#btn-start")); } },
  { name: "panel-wrapup", url: P, run: async (p) => { await stateStatus("done")(p); await p.reload({ waitUntil: "networkidle" }); await p.waitForTimeout(1500); return true; } },
  { name: "panel-finish", url: P, run: async (p) => { await stateStatus("done")(p); await p.reload({ waitUntil: "networkidle" }); await p.waitForTimeout(1500); await panelMore(p); await btn(/^Finish$/)(p); return hasDlg(p); } },
  { name: "panel-switcher", url: P, run: async (p) => { await p.keyboard.press("Control+k"); await p.waitForTimeout(500); return !!(await p.$(".sh-sw")); } },
  { name: "panel-offline", ignoreAbort: true, url: P, run: async (p) => { await p.route(/\/api\//, (r) => r.abort()); await p.waitForTimeout(6000); return true; } },
  { name: "panel-closed", ignore: ["http-404", "console-error"], url: (a) => a + "p/doesnotexist/", run: async () => true },
);

// ---- planning mode: state.planning_required + a patched GET planning ----
const planMock = (opts = {}) => async (p) => {
  if (!opts.prereq) await patch(p, /api.progress/, (j) => { j.steps = (j.steps || []).map((x) => Object.assign({}, x, { ok: !opts.subs || x.id === "folder" || x.id === "git" || x.id === "structure" })); j.next_action = "roadmap"; });
  await patch(p, /\/api\/state(\?|$)/, (j) => { j.planning_required = true; j.loop = Object.assign({}, j.loop, { status: "no_loop", running: false, pid: null }); j.roadmap_approval = { approved: false, reason: "not approved" }; });
  await patch(p, /\/api\/planning(\?|$)/, (j) => {
    j.required = true; j.approved = false; if (opts.subs) { j.gates = j.gates.filter((g) => g.id !== "subprojects").map((g) => Object.assign({}, g, { ok: g.id === "git" || g.id === "structure" })); j.gates.splice(2, 0, { id: "subprojects", label: "Subprojects", ok: false, optional: false, detail: "not confirmed", items: [] }); } else if (!opts.prereq) j.gates = j.gates.map((g) => Object.assign({}, g, { ok: g.id === "ready" ? !!opts.canApprove : true })); j.can_approve = !!opts.canApprove;
    if (opts.prereq) j.gates = j.gates.map((g) => (g.id === "structure" || g.id === "conformance" ? Object.assign({}, g, { ok: false, detail: "missing" }) : g));
    if (opts.chat) j.thread = { messages: [{ role: "user", text: "A card checkout with cart, coupons and invoices.", at: j.now }, { role: "assistant", text: "Here is the plan.\n\n- **Stage 1** payments, audit log\n- **Stage 2** cart and coupons\n\n```\nROADMAP.md written\n```\n\nShould coupons stack?", at: j.now }], busy: false, job_id: null, session: true };
  });
};
const planScreen = (name, opts, extra) => ({ name, url: P, run: async (p, c) => { await planMock(opts)(p); await p.reload({ waitUntil: "networkidle" }); await p.waitForTimeout(1800); if (extra) await extra(p, c); return !!(await p.$(".plan")) || !!(await p.$(".pg-settings")); } });
module.exports.push(
  planScreen("plan-prereq", { prereq: true }),
  planScreen("plan-chat", { chat: true }),
  planScreen("plan-approve", { chat: true, canApprove: true }),
  planScreen("plan-approve-click", { chat: true, canApprove: true }, async (p) => { await btn(/Approve roadmap/)(p); }),
  planScreen("plan-workspace", { chat: true }, async (p) => { await panelMore(p); await btn(/^Workspace/)(p); }),
  planScreen("plan-roadmap-dialog", { chat: true }, async (p) => { await click(p, "#tr-ai-roadmap, #btn-open-roadmap"); }),
  planScreen("plan-settings", { chat: true }, async (p, c) => { await drawer(p, c); await click(p, "#btn-settings"); }),
);

const subs = { confirmed: false, projects: [
  { path: "services/payments-api", name: "payments-api", exists: true, has_code: true, skip: false, stack: "node", skill: "backend", conformance: "", map: { files: 0 }, giant_files: [] },
  { path: "web/checkout-ui", name: "checkout-ui", exists: true, has_code: true, skip: false, stack: "react", skill: "frontend", conformance: "STANDARD", map: { files: 7 }, giant_files: [], warning: "2 giant files" },
  { path: "infra/legacy", name: "legacy", exists: true, has_code: true, skip: true, reason: "generated", stack: "", skill: "", conformance: "", map: { files: 0 }, giant_files: [] } ], system: { enabled: true } };
const planExtra = (kind) => async (p) => {
  await patch(p, /api.progress/, (j) => { if (kind === "subs") { j.monorepo = true; j.next_action = "subprojects"; j.subprojects = subs.projects.map((x) => ({ path: x.path, stack: x.stack, conformance: x.conformance, map_files: x.map.files })); } });
  if (kind === "subs") await p.route(/api.subprojects/, (r) => r.fulfill({ json: subs }));
  if (kind === "roadmapjob") await p.route(/api.jobs/, (r) => r.fulfill({ json: { jobs: [{ id: "j1", type: "roadmap", state: "running", started_at: new Date().toISOString() }], calls: [] } }));
};
const planScreen2 = (name, kind, opts, click1, dlg) => ({ name, url: P, run: async (p, c) => { await planMock(opts)(p); await planExtra(kind)(p); await p.reload({ waitUntil: "networkidle" }); await p.waitForTimeout(1800); if (click1) await click(p, click1); return dlg ? hasDlg(p) : !!(await p.$(".plan")); } });
module.exports.push(
  planScreen2("plan-subprojects", "subs", { subs: true }, null, false),
);
module.exports.push({ name: "hub-folder-browse", ignoreAbort: true, run: async (p, c) => { await p.route("**/projects/pick-folder", (r) => r.abort()); await more(p, c); await click(p, "#hub-create"); await click(p, "dialog button:has-text(\"Choose\")"); await click(p, ".fb-item >> text=~"); await p.waitForTimeout(800); return !!(await p.$("#fb-field")); } });
