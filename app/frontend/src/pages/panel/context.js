/* Shared state of the project panel (port of the monitor app.js). All data comes from the project API
   (/p/<id>/api/...); only visual constants, screen state and derived values live here. */
import { isNum, list } from '../../util/format.js';

// ---------- visual constants (not data) ----------
const PAL = {
  pending: ['var(--fg2)', 'var(--fill)'], speccing: ['var(--info)', 'var(--blue-bg)'], specced: ['var(--accent)', 'var(--violet-bg)'],
  approved: ['var(--cyan)', 'var(--blue-bg)'], building: ['var(--warn)', 'var(--amber-bg)'], verified: ['var(--ok)', 'var(--green-bg)'],
  blocked: ['var(--danger)', 'var(--red-bg)'], running: ['var(--warn)', 'var(--amber-bg)'], done: ['var(--ok)', 'var(--green-bg)'],
  idle: ['var(--fg2)', 'var(--fill)'], failed: ['var(--danger)', 'var(--red-bg)'], queued: ['var(--info)', 'var(--blue-bg)'],
};
export const pal = (s) => PAL[s] || PAL.idle;
export const ROLE_DESC = {
  orchestrator: 'roadmap · dispatch · commit · checkpoint',
  'spec-writer': 'writes the spec of one feature', 'spec-reviewer': 'spec gate · read-only',
  'executor-back': 'implements · back end', 'executor-front': 'implements · front end',
  'executor-generic': 'implements · infra, script, config', 'brownfield-mapper': 'maps the existing code',
  verifier: 'build · typecheck · lint', 'verifier-flow': 'validates the flow by running the app',
};
export const RESUME_WAIT_MS = 25000;

// ---------- screen state ----------
export const ui = {
  sel: null, pastN: null, past: null,
  hist: { data: null, key: undefined, loading: false, err: null, ver: 0 }, logLoading: null,
  errors: { data: null, key: undefined, loading: false, err: null, },
  pausePending: false, confirmShutdown: false, freeText: '', resuming: null, busy: {},
  agentLog: null, showNew: false,
  closed: null, // full-screen notice over the panel: { kind: 'notfound', message } | { kind: 'shutdown' }
};

/** ctx.state = last /state; ctx.api = project client; ctx.progress = last GET progress; offline/offlineMsg = polling lost. */
export const ctx = { state: null, api: null, projectId: null, progress: null, offline: false, offlineMsg: null };

// ---------- derived ----------
export const loop = () => (ctx.state && ctx.state.loop) || {};
export const status = () => loop().status || 'no_loop';
export const live = () => !ui.past;
export const features = () => (ui.past ? list(ui.past.features) : list(ctx.state && ctx.state.features));
export const agents = () => (ui.pastN != null ? list((list(ui.hist.data).find((x) => x.n === ui.pastN) || {}).agents) : list(ctx.state && ctx.state.agents));
export const agentById = (id) => agents().find((a) => a.id === id) || list(ui.hist.data).flatMap((v) => list(v.agents)).find((a) => a.id === id);
export function pauseRequested() { const v = loop().pause_requested; return typeof v === 'boolean' ? v : ui.pausePending; }
export const askKey = () => `${status()}|${loop().turn}|${loop().at}|${loop().reason}`;
export function resumingNow() {
  const r = ui.resuming;
  return !!(r && r.key === askKey() && Date.now() - r.since < RESUME_WAIT_MS);
}
export function isPauseAsk() { return /paus/i.test(String(loop().reason || '')); }
export function isRecommended(o) { return /recommended/i.test(String(o)); }
export function lastN() {
  const vs = list(ctx.state && ctx.state.turns).map((v) => v.n).filter(isNum);
  if (vs.length) return Math.max(...vs);
  return ctx.state && ctx.state.meta && isNum(ctx.state.meta.turns) ? ctx.state.meta.turns : null;
}
/** Roadmap approval as it comes in /state. */
export const approval = () => (ctx.state && ctx.state.roadmap_approval) || { approved: false, reason: null, features: 0 };
/** Project name: state.project_ref.name with a fallback to state.project (folder name). */
export const projectName = () => (ctx.state && ((ctx.state.project_ref && ctx.state.project_ref.name) || ctx.state.project)) || null;
