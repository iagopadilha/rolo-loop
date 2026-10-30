/* Feature page (UI v2, spec 05): one feature as a page inside the shell, deep-linkable at `#/feature/<slug>`.
   It fills the centre of the shell column (a page of the router, app.js); the sidebar and the header stay. Data: `GET feature/<slug>` (steps,
   tasks, agents; refreshed while the loop runs) and the feature in /state. The history is built only from what that data holds.
   The "Ask the agent" panel uses the same read-only chat API as the dialogs (chat/send + chat/job). */
import { html, useState, useEffect, useLayoutEffect, useRef } from '../../util/preact.js';
import { list, txt, maskHome } from '../../util/format.js';
import { Markdown } from '../../components/markdown.js';
import { State, Chip } from '../../components/page.js';
import { ctx, loop } from './context.js';
import { featureName } from './pipeline_model.js';
import { featureCalls } from './feature_history.js';
import { back, navigate } from './router.js';
import { useFeatureDetail, stateFeature } from './dialogs/feature_dialog.js';
import { conversationFor, sendQuestion } from './dialogs/chat_column.js';
import { taskGraphOf, waveModeLabel, shortDuration, agentRowsOf, fallbackSteps } from './feature_dialog_model.js';

const PATHS_COLLAPSED = 3;
const COLLAPSE_WAVES_AFTER = 4;
const ROLE_RANK = { 'spec-writer': 0, 'spec-reviewer': 1, executor: 2, verifier: 9 };
const FAIL_WORDS = ['BLOCKED', 'FAIL', 'FAILED', 'REJECTED', 'ERROR'];

// ---------- routing (router.js) ----------
/** Leaves the page: back to the previous page of the app (the Features list, the loop...). */
function closeFeature() { back(); }

// ---------- model ----------
const roleRank = (role) => { const r = String(role || ''); return r in ROLE_RANK ? ROLE_RANK[r] : r.startsWith('executor') ? ROLE_RANK.executor : 5; };
const failed = (agent) => FAIL_WORDS.includes(String(agent.return_status || '').toUpperCase()) || agent.state === 'failed';
const running = (agent) => agent.state === 'running';

/** 'verified' | 'running' | 'blocked' | 'queued' of a roadmap status. */
export function chipKind(slug, status) {
  const l = loop();
  if (status === 'verified') return 'verified';
  if (status === 'blocked') return 'blocked';
  const live = !!l.running && list(l.feature).includes(slug);
  return live || status === 'speccing' || status === 'building' ? 'running' : 'queued';
}
const CHIP_LABEL = { verified: 'Verified', running: 'Running', blocked: 'Blocked', queued: 'Queued' };

const sumSeconds = (agents) => agents.reduce((n, a) => n + (Number.isFinite(a.duration_s) ? a.duration_s : 0), 0);
const callsLabel = (n) => `${n} call${n === 1 ? '' : 's'}`;
const rolesOf = (agents) => [...new Set(agents.map((a) => a.role).filter(Boolean))].join(' · ');

function titleOf(role, agents) {
  const last = agents[agents.length - 1], ret = String(last.return_status || '').toUpperCase();
  if (agents.some(running)) return role === 'spec-writer' ? 'Writing the spec' : role === 'spec-reviewer' ? 'Reviewing the spec' : role === 'verifier' ? 'Verifying' : `${role} is working`;
  if (role === 'spec-writer') return failed(last) ? 'Spec writing failed' : 'Spec written';
  if (role === 'spec-reviewer') return ret === 'APPROVED' ? 'Spec approved' : `Spec reviewed${ret ? ' · ' + ret.toLowerCase() : ''}`;
  if (role === 'verifier') return ret === 'PASS' || ret === 'PASSED' ? 'Verification passed' : `Verification${ret ? ' · ' + ret.toLowerCase() : ''}`;
  return `Build · ${role || 'agent'}${ret && ret !== 'DONE' ? ' · ' + ret.toLowerCase() : ''}`;
}
const stateOf = (agents) => (agents.some(running) ? 'running' : agents.some(failed) ? 'failed' : 'done');

/** [{ key, turn, title, meta, state, tasks?, merged? }] oldest first; built only from the agents and tasks the back end sent. */
function buildEvents(slug, data, base) {
  const agents = data && Array.isArray(data.agents) ? data.agents.map((a) => ({ ...a, id: a.agent_id }))
    : agentRowsOf(featureCalls(slug), null).map((a) => ({ ...a, agent_id: a.id }));
  const payload = (data && data.tasks && !Array.isArray(data.tasks) ? data.tasks : null) || (base && base.tasks) || null;
  const graph = taskGraphOf(payload);
  const events = [];
  const groups = new Map();
  agents.forEach((agent) => {
    const role = String(agent.role || 'agent'), isExec = role.startsWith('executor');
    if (isExec && graph.tasks.length) return; // shown as waves below
    const key = `${agent.turn}|${role}`;
    if (!groups.has(key)) groups.set(key, { role, turn: agent.turn, rank: roleRank(role), agents: [] });
    groups.get(key).agents.push(agent);
  });
  groups.forEach((g, key) => {
    const secs = sumSeconds(g.agents);
    events.push({ key, turn: g.turn, rank: roleRank(g.role), title: titleOf(g.role, g.agents), state: stateOf(g.agents),
      meta: [g.role, callsLabel(g.agents.length), secs ? shortDuration(secs) : ''].filter(Boolean).join(' · '),
      said: g.agents.length === 1 && g.rank > 1 && g.agents[0].summary && g.agents[0].summary !== 'STATUS: DONE' && g.role !== 'verifier' ? String(g.agents[0].summary) : '' });
  });
  const specTurns = events.filter((e) => e.rank <= 1 && Number.isFinite(e.turn)).map((e) => e.turn);
  const fallbackTurn = specTurns.length ? Math.max(...specTurns) : 0;
  graph.waves.forEach((wave, index) => {
    const execs = wave.tasks.flatMap((t) => t.executors.map((e) => ({ ...e })));
    const turns = execs.map((e) => e.turn).filter(Number.isFinite);
    const rows = wave.tasks.map((t) => {
      const latest = t.executors[t.executors.length - 1];
      return { id: t.id, title: t.title || '—', status: t.status, time: latest && Number.isFinite(latest.duration_s) ? shortDuration(latest.duration_s) : '' };
    });
    const allDone = rows.every((r) => r.status === 'done');
    const anyFailed = rows.some((r) => r.status === 'failed');
    events.push({ key: 'wave' + wave.n, wave: wave.n, turn: turns.length ? Math.min(...turns) : null, sortTurn: turns.length ? Math.min(...turns) : fallbackTurn,
      rank: ROLE_RANK.executor + (index + 1) / 1000, title: `Wave ${wave.n} · ${waveModeLabel(wave)}`,
      meta: rolesOf(execs) || (allDone ? '' : 'not started'), state: allDone ? 'done' : anyFailed ? 'failed' : rows.some((r) => r.status === 'running') ? 'running' : 'pending', tasks: rows });
  });
  events.forEach((e) => { if (e.sortTurn == null) e.sortTurn = Number.isFinite(e.turn) ? e.turn : fallbackTurn; });
  events.sort((a, b) => a.sortTurn - b.sortTurn || a.rank - b.rank);
  // collapse long runs of finished waves after the first few
  const out = [];
  let waveSeen = 0, run = [];
  const flush = () => {
    if (run.length > 1) {
      const tasks = run.flatMap((w) => w.tasks);
      out.push({ key: 'merged' + run[0].wave, merged: true, turn: run[0].turn, state: 'done', title: `Waves ${run[0].wave} – ${run[run.length - 1].wave} · ${tasks.length} task${tasks.length === 1 ? '' : 's'}`, meta: 'all done', tasks });
    } else run.forEach((w) => out.push(w));
    run = [];
  };
  events.forEach((e) => {
    if (e.wave == null) { flush(); out.push(e); return; }
    waveSeen += 1;
    if (waveSeen > COLLAPSE_WAVES_AFTER && e.state === 'done') run.push(e); else { flush(); out.push(e); }
  });
  flush();
  const state = stateFeature(slug) || {};
  if (base && base.status === 'verified') out.push({ key: 'end', turn: null, state: 'done', title: 'Verified and committed', meta: ['verifier', state.commit ? 'commit ' + state.commit : 'checkpoint'].join(' · ') });
  else if (base && base.status === 'blocked') out.push({ key: 'end', turn: null, state: 'failed', title: 'Blocked', meta: '', said: state.note ? String(state.note) : '' });
  return out;
}

/** Suggestions from this feature's own data. */
function suggestionsOf(slug, base, events, paths) {
  const out = [];
  const tasks = events.flatMap((e) => e.tasks || []).filter((t) => t.time);
  const toSec = (t) => { const m = /(?:(\d+)h )?(?:(\d+)m )?(\d+)s?/.exec(t.time) || []; return (+m[1] || 0) * 3600 + (+m[2] || 0) * 60 + (+m[3] || 0); };
  if (tasks.length > 1) out.push(`Why was ${[...tasks].sort((a, b) => toSec(b) - toSec(a))[0].id} the slowest task?`);
  if (paths.length) out.push(`What changed in ${paths[0]}?`);
  out.push(base.status === 'verified' ? 'Summarize what this feature delivered' : base.status === 'blocked' ? 'Why is this feature blocked?' : 'What is left before this feature is verified?');
  return out.slice(0, 3);
}

// ---------- pieces ----------
const GLYPH = { back: 'M10 3 L5 8 L10 13', send: 'M8 13V3 M4 7l4-4 4 4' };
const Icon = ({ name, size = 14 }) => html`<svg width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d=${GLYPH[name]} /></svg>`;

function EventCard({ event }) {
  const [open, setOpen] = useState(!event.merged);
  return html`<div class="fp-card">
    <div class="fp-card-head"><span class="fp-card-title">${event.title}</span><span class="fp-card-meta mono">${event.meta}</span></div>
    ${event.said ? html`<div class="fp-said">${event.said}</div>` : null}
    ${event.merged ? html`<button type="button" class="fp-link" aria-expanded=${String(open)} onClick=${() => setOpen(!open)}>${open ? 'Hide tasks' : 'Show tasks'}</button>` : null}
    ${event.tasks && open ? html`<div class="fp-tasks">${event.tasks.map((t) => html`<div key=${t.id} class="fp-task">
      <span class="fp-task-id mono" title=${t.id}>${t.id}</span><span class="fp-task-title">${t.title}</span>
      <span class=${'fp-task-time mono' + (t.status === 'failed' ? ' is-bad' : '')}>${t.status === 'done' ? t.time : t.status + (t.time ? ' · ' + t.time : '')}</span></div>`)}</div>` : null}
  </div>`;
}

function Timeline({ events, loading }) {
  if (!events.length) return html`<div class="fp-empty">${loading ? 'Loading the history…' : 'No agent has worked on this feature yet.'}</div>`;
  return html`<ol class="fp-tl">${events.map((event, index) => {
    const last = index === events.length - 1;
    const glyph = event.state === 'done' ? '✓' : event.state === 'failed' ? '✕' : event.state === 'running' ? '●' : '';
    return html`<li key=${event.key} class="fp-ev">
      <span class="fp-turn mono">${Number.isFinite(event.turn) ? 'T' + event.turn : '—'}</span>
      <div class="fp-rail"><span class=${'fp-dot is-' + event.state} aria-hidden="true">${glyph}</span>${last ? null : html`<span class="fp-line"></span>`}</div>
      <div class="fp-ev-body"><${EventCard} event=${event} /></div></li>`;
  })}</ol>`;
}

function AskPanel({ slug, base, events, paths, agents }) {
  const newest = agents[agents.length - 1];
  const agentName = (newest && newest.role) || 'the agent';
  const scope = newest && newest.agent_id ? { type: 'agent', agent_id: newest.agent_id, turn: Number.isFinite(newest.turn) ? newest.turn : null } : { type: 'general', agent_id: null, turn: null };
  const conversation = conversationFor(JSON.stringify({ page: 'feature', slug, scope }));
  const [, refresh] = useState(0);
  const listRef = useRef(null), inputRef = useRef(null);
  useEffect(() => { const l = () => refresh((n) => n + 1); conversation.listeners.add(l); return () => conversation.listeners.delete(l); }, [conversation]);
  const count = conversation.messages.length, pending = !!conversation.pending;
  useLayoutEffect(() => { const el = listRef.current; if (el) el.scrollTop = el.scrollHeight; }, [count, pending, conversation.error]);
  const reason = ctx.offline ? 'The app is disconnected: questions cannot be sent now.' : '';
  const off = !!reason || pending;
  const ask = (text) => {
    const q = String(text || '').trim();
    if (!q || off) return false;
    sendQuestion(conversation, scope, q, false, (t) => { const i = inputRef.current; if (i && !i.value) i.value = t; });
    return true;
  };
  const send = () => { const i = inputRef.current; if (i && ask(i.value)) i.value = ''; };
  const suggestions = suggestionsOf(slug, base, events, paths);
  return html`<aside class="fp-ask" aria-label="Ask the agent">
    <div class="fp-ask-head"><span class="fp-ask-title">Ask the agent</span><span class="fp-sub">Answers come from this feature's specs, turns and files (read only).</span></div>
    <div class="fp-ask-list" role="log" aria-live="polite" ref=${listRef}>
      ${conversation.messages.map((m, i) => html`<div key=${i} class=${'fp-msg ' + (m.role === 'ai' ? 'is-ai' : 'is-user')}>${m.role === 'ai' ? html`<div class="md"><${Markdown} source=${m.text} /></div>` : m.text}</div>`)}
      ${pending ? html`<div class="fp-sub" role="status">thinking…</div>` : null}
      ${conversation.error ? html`<div class="fp-err" role="alert"><span>${conversation.error}</span>${conversation.lastText ? html` <button type="button" class="fp-link" disabled=${pending} onClick=${() => sendQuestion(conversation, scope, conversation.lastText, true)}>try again</button>` : null}</div>` : null}
      <div class="fp-try"><span class="fp-label">Try</span>
        ${suggestions.map((s) => html`<button key=${s} type="button" class="fp-sugg" disabled=${off} onClick=${() => ask(s)}>${s}</button>`)}</div>
    </div>
    <div class="fp-ask-foot">
      <label class="fp-label" for="fp-ask-input">${'Message to ' + agentName}</label>
      <div class="fp-ask-row">
        <input id="fp-ask-input" ref=${inputRef} type="text" class="fp-input" placeholder="Ask about this feature…" disabled=${!!reason}
          onKeyDown=${(e) => { if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); send(); } }} />
        <button type="button" class="fp-send" aria-label="Send" title=${reason || 'Send'} disabled=${off} onClick=${send}><${Icon} name="send" size=${14} /></button>
      </div>
      ${reason ? html`<span class="fp-sub" role="status">${reason}</span>` : null}
    </div>
  </aside>`;
}

function Toolbar({ slug, label }) {
  return html`<div class="fp-bar">
    <button type="button" class="fp-back" aria-label="Back" title="Back (Esc)" onClick=${closeFeature}><${Icon} name="back" size=${14} /></button>
    <nav class="fp-crumbs" aria-label="Breadcrumb"><a class="fp-crumb-dim" href="#/features" onClick=${(e) => { e.preventDefault(); navigate('/features'); }}>Features</a><span class="fp-crumb-dim" aria-hidden="true">/</span><span class="fp-crumb-cur" title=${slug}>${label}</span></nav>
  </div>`;
}

function NotFound({ slug }) {
  return html`<div class="fp">
    <${Toolbar} slug=${slug} label=${slug} />
    <div class="fp-state" role="alert"><h1 class="fp-h1">Feature not found</h1>
      <p class="fp-sub">${'No feature named "' + slug + '" exists in this project.'}</p>
      <button type="button" class="btn btn-sm" onClick=${closeFeature}>Go back</button></div>
  </div>`;
}

export function FeaturePage({ slug }) {
  const fromState = stateFeature(slug);
  const { data, error, loading } = useFeatureDetail(fromState ? slug : null);
  const rootRef = useRef(null), headRef = useRef(null);
  const [allPaths, setAllPaths] = useState(false);
  const missing = !fromState; // /state is loaded when the page shows: a slug it does not list is unknown
  useEffect(() => { // focus management: the title takes focus when the page opens or the feature changes
    const el = headRef.current;
    if (el) el.focus({ preventScroll: true });
  }, [slug, !!(fromState || data)]);
  if (missing || (!fromState && !data && error)) return html`<${NotFound} slug=${slug} />`;
  if (!fromState && !data) return html`<div class="fp"><${Toolbar} slug=${slug} label=${slug} /><div class="fp-state" role="status">Loading the feature…</div></div>`;

  const base = { ...(fromState || {}), ...(data || {}), status: (fromState && fromState.status) || (data && data.status) };
  const kind = chipKind(slug, base.status);
  const name = featureName(slug), sameName = name === slug;
  const events = buildEvents(slug, data, fromState);
  const graph = taskGraphOf((data && data.tasks && !Array.isArray(data.tasks) ? data.tasks : null) || (fromState && fromState.tasks) || null);
  const calls = data && Array.isArray(data.agents) ? data.agents.length : featureCalls(slug).reduce((n, t) => n + t.agents.length, 0);
  const claude = data && data.claude && (data.claude.command || data.claude.account) ? data.claude : null;
  const meta = [
    base.stage != null ? 'Stage ' + base.stage : '', base.wave != null && base.wave !== '' ? 'wave ' + (typeof base.wave === 'object' ? base.wave.turn : base.wave) : '',
    graph.tasks.length ? `${graph.tasks.length} task${graph.tasks.length === 1 ? '' : 's'} in ${graph.waves.length} wave${graph.waves.length === 1 ? '' : 's'}` : '',
    calls ? callsLabel(calls).replace('call', 'agent call') : '', claude ? 'last run with ' + [claude.command, claude.account].filter(Boolean).join(' · ') : '',
  ].filter(Boolean);
  const deps = list(base.deps).map(String);
  const paths = list(base.paths).map((p) => maskHome(p));
  const shown = allPaths ? paths : paths.slice(0, PATHS_COLLAPSED);
  const steps = data && Array.isArray(data.steps) && data.steps.length ? data.steps : fallbackSteps(base.status);
  const current = data && data.current_step;

  return html`<div class="fp" ref=${rootRef} data-feature=${slug}>
    <${Toolbar} slug=${slug} label=${name} />
    ${error ? html`<div class="fp-warn" role="alert">${'Live feature data unavailable: ' + error + (fromState ? ' (showing the panel state)' : '')}</div>` : null}
    <div class="fp-body">
      <main class="fp-main" aria-label="Feature">
        <header class="fp-head">
          <h1 class="fp-h1" tabIndex="-1" ref=${headRef}>${name}</h1>
          ${sameName ? null : html`<span class="fp-slug mono">${slug}</span>`}
          <div class="fp-status"><${State} kind=${kind}>${CHIP_LABEL[kind]}<//>${meta.length ? html`<span class="fp-sub">${meta.join(' · ')}</span>` : null}</div>
          ${current && kind === 'running' ? html`<span class="fp-sub">${'Now: ' + txt(current) + ' · ' + steps.filter((s) => s.state === 'done').length + ' of ' + steps.length + ' steps done'}</span>` : null}
          ${base.summary && base.summary.goal ? html`<p class="fp-goal">${base.summary.goal}</p>` : null}
        </header>
        <div class="fp-facts">
          <div class="fp-fact"><span class="fp-label">Depends on</span>
            ${deps.length ? html`<div class="fp-chips">${deps.map((d) => d.startsWith('dir:') ? html`<${Chip} key=${d} title=${d}>${d}<//>` : html`<${Chip} key=${d} title=${d} href=${'#/feature/' + encodeURIComponent(d)} onOpen=${() => navigate('/feature/' + encodeURIComponent(d))}>${featureName(d)}<//>`)}</div>` : html`<span class="fp-sub">Nothing</span>`}</div>
          <div class="fp-fact"><span class="fp-label">Paths</span>
            ${paths.length ? html`<ul class="fp-paths">${shown.map((p) => html`<li key=${p} class="mono" title=${p}>${p}</li>`)}</ul>` : html`<span class="fp-sub">None declared</span>`}
            ${paths.length > PATHS_COLLAPSED ? html`<button type="button" class="fp-link" aria-expanded=${String(allPaths)} onClick=${() => setAllPaths(!allPaths)}>${allPaths ? 'Show fewer' : `Show all (${paths.length}) ›`}</button>` : null}</div>
        </div>
        <section class="fp-hist" aria-label="History"><span class="fp-label fp-hist-label">History</span><${Timeline} events=${events} loading=${loading} /></section>
      </main>
      <${AskPanel} slug=${slug} base=${base} events=${events} paths=${paths} agents=${list(data && data.agents)} />
    </div>
  </div>`;
}
