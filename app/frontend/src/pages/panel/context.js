/* Shared state of the project panel (port of the monitor app.js). All data comes from the project API
   (/p/<id>/api/...); only visual constants, screen state and derived values live here. */
import { isNum, list } from '../../util/format.js';

// ---------- visual constants (not data) ----------
export const PAL = {
  pending: ['#6F6B86', '#1D1C27'], speccing: ['#8AA4FF', '#1A1B2E'], specced: ['#B3A8FF', '#1C1833'],
  approved: ['#5CC8D6', '#14232A'], building: ['#B3A8FF', '#1C1833'], verified: ['#4FD1A1', '#0F2A21'],
  blocked: ['#F07A6E', '#2A1715'], running: ['#B3A8FF', '#1C1833'], done: ['#4FD1A1', '#0F2A21'],
  idle: ['#6F6B86', '#1D1C27'], failed: ['#F07A6E', '#2A1715'], queued: ['#8AA4FF', '#1A1B2E'],
};
export const pal = (s) => PAL[s] || PAL.idle;
export const STATUS = {
  running: { label: 'Running', c: '#B3A8FF', bg: '#1C1833', line: '#3A3270', pulse: true },
  continue: { label: 'Continuing', c: '#8AA4FF', bg: '#1A1B2E', line: '#33406E' },
  awaiting_user: { label: 'Waiting for you', c: '#F5B44A', bg: '#2A2010', line: '#F5B44A', pulse: true },
  done: { label: 'Done', c: '#4FD1A1', bg: '#0F2A21', line: '#1F4D3D' },
  error: { label: 'Error', c: '#F07A6E', bg: '#2A1715', line: '#5A2A26' },
  stopped: { label: 'Stopped', c: '#A7A3BD', bg: '#1D1C27', line: '#3A3750' },
  no_loop: { label: 'No loop', c: '#8A86A1', bg: '#13121A', line: '#262434' },
  planning: { label: 'Planning', c: '#B3A8FF', bg: '#1C1833', line: '#3A3270' }, // planning mode (state.planning_required)
};
export const ROLE_DESC = {
  orchestrator: 'roadmap · dispatch · commit · checkpoint',
  'spec-writer': 'writes the spec of one feature', 'spec-reviewer': 'spec gate · read-only',
  executor: 'implements', 'quality-gate': 'reviews the code against the standards', 'executor-back': 'implements · back end', 'executor-front': 'implements · front end',
  'executor-generic': 'implements · infra, script, config', 'brownfield-mapper': 'maps the existing code',
  verifier: 'build · typecheck · lint', 'verifier-flow': 'validates the flow by running the app',
};
export const LOG_KIND = {
  sys: ['context', '#8A86A1', '#0F0E15', '#1D1C27', false], ai: [null, '#EDEBF7', '#13121A', '#1D1C27', false],
  tool: ['tool', '#8AA4FF', '#13121A', '#262434', true], guard: ['guard', '#F07A6E', '#2A1715', '#5A2A26', true],
  user: ['you → ANSWERS.md', '#B3A8FF', '#1C1833', '#3A3270', false],
};
export const TURN_COLOR = { error: '#F07A6E', running: '#B3A8FF', awaiting_user: '#B3A8FF', done: '#4FD1A1', continue: '#4A4664' };
export const LIFECYCLE = ['pending', 'speccing', 'specced', 'approved', 'building', 'verified'];
export const FEATURE_STATUSES = ['pending', 'speccing', 'specced', 'approved', 'building', 'verified', 'blocked'];
export const RESUME_WAIT_MS = 25000;
export const NL_STATES = ['done', 'error', 'stopped', 'awaiting_user'];

// ---------- screen state ----------
export const ui = {
  view: 'turn', sel: null, tab: 'log', pastN: null, past: null, loadingN: null, pinLast: null, det: {}, histOpen: {},
  hist: { data: null, key: undefined, loading: false, err: null, ver: 0 }, logLoading: null,
  errors: { data: null, key: undefined, loading: false, err: null, open: {}, confirmDiscard: false },
  pausePending: false, confirmShutdown: false, freeText: '', resuming: null, busy: {},
  qSent: {}, agentLog: null, showNew: false, showWorkspace: false, hov: null, attach: [], chatGeneral: false,
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
export const featById = () => Object.fromEntries(features().map((f) => [f.id, f]));
export const agentById = (id) => agents().find((a) => a.id === id) || list(ui.hist.data).flatMap((v) => list(v.agents)).find((a) => a.id === id);
export function pauseRequested() { const v = loop().pause_requested; return typeof v === 'boolean' ? v : ui.pausePending; }
export const askKey = () => `${status()}|${loop().turn}|${loop().at}|${loop().reason}`;
export function resumingNow() {
  const r = ui.resuming;
  return !!(r && r.key === askKey() && Date.now() - r.since < RESUME_WAIT_MS);
}
export function isPauseAsk() { return /paus/i.test(String(loop().reason || '')); }
export function isRecommended(o) { return /recommended/i.test(String(o)); }
export function focusFeatureId() {
  if (!ui.sel) return null;
  if (ui.sel.kind === 'feature') return ui.sel.id;
  const a = agentById(ui.sel.id);
  return a ? a.feature || null : null;
}
export function lastN() {
  const vs = list(ctx.state && ctx.state.turns).map((v) => v.n).filter(isNum);
  if (vs.length) return Math.max(...vs);
  return ctx.state && ctx.state.meta && isNum(ctx.state.meta.turns) ? ctx.state.meta.turns : null;
}
export const selAgentId = () => (ui.sel && ui.sel.kind === 'agent' ? ui.sel.id : null);
export function verInfo() {
  const id = selAgentId(), m = id ? /^v0*(\d+):/.exec(id) : null, last = lastN();
  const n = m ? Number(m[1]) : ui.pastN != null ? ui.pastN : last;
  return { old: isNum(n) && isNum(last) && n !== last, n };
}
/** Roadmap approval as it comes in /state. */
export const approval = () => (ctx.state && ctx.state.roadmap_approval) || { approved: false, reason: null, features: 0 };
/** Project name: state.project_ref.name with a fallback to state.project (folder name). */
export const projectName = () => (ctx.state && ((ctx.state.project_ref && ctx.state.project_ref.name) || ctx.state.project)) || null;
