/* Guided tour (first run, and Settings > General > Start tour): one card per step, over the REAL screen it explains. Each step opens its
   screen (the projects list or a Settings section), highlights the area it talks about and says what it is, why it matters and what to do.
   The step and the done state are saved by the app (GET/POST /api/app/onboarding), so a restart resumes the tour and a finished tour never
   comes back by itself. Keyboard: Enter or → next, ← back, Esc closes. Runs only on the projects page (the hub). */
import { html, render, useState, useEffect, useRef, useLayoutEffect } from '../util/preact.js';
import { appApi } from '../api/api_client.js';
import { navigate } from '../pages/panel/router.js';

const STEPS = [
  { route: '/loop', target: null, title: 'Welcome to Rolo Loop',
    body: 'Rolo Loop runs spec-driven build loops with AI coding agents (Claude Code or a compatible CLI) on your machine. You describe a goal, approve a plan, and a team of agents writes each feature\'s spec, builds it, verifies it and reviews it, one feature per lane, while you follow here and answer when they ask.',
    why: 'A few settings decide which AI works for you, which agents and tools it may use, and how it reaches you. This tour shows each one once, in about two minutes.' },
  { route: '/settings/agents', target: '#ag-preview', title: 'The agents of a loop',
    body: 'Every feature goes through Spec, Gate, Build, Verify, Quality and Commit. Below the preview you choose the model of each agent and turn off the optional ones (spec review, flow validation, quality gate).',
    why: 'Stronger models where judgment matters, faster ones where the work is routine: this is where cost and quality are balanced.' },
  { route: '/settings/mcp', target: '#st-content', title: 'Skills and MCP tools',
    body: 'Skills found on this machine (frontend-dev, backend-dev, your company\'s standards) and MCP servers (tools such as a database or a browser) that the agents of a loop may use. Nothing else is loaded.',
    why: 'Agents follow the standards of the skills you allow here, and reach only the tools you approve.' },
  { route: '/settings/quality', target: '#qg-checks', title: 'The quality gate',
    body: 'After lint and tests, one last review checks the feature against its spec, security, your skills\' standards, naming, size and design patterns. Block sends it back to the executor; Warn only reports.',
    why: 'It keeps giant files, vague names and security holes out of what the AI delivers.' },
  { route: '/settings/providers', target: '#pv-order', title: 'Which AI runs the loop',
    body: 'Add your Claude subscription, a token from `claude setup-token`, an API key (pay per use), a gateway (LiteLLM, Bedrock, local models) or a Claude Code compatible local agent. Test each one.',
    why: 'The order is the fallback: when the first one hits a usage limit, the next one takes the turn and the loop keeps going.' },
  { route: '/settings/notifications', target: '#nt-channels', title: 'How the loop reaches you',
    body: 'Windows notifications, e-mail (SMTP or Amazon SES) and webhooks (Slack, Teams, Discord or your own JSON). Choose the events and send a test.',
    why: 'Loops run for hours: you only come back when the loop needs a decision or finished.' },
  { route: '/loop', target: '#hub-actions', title: 'Your first project',
    body: 'Open folder picks an existing repository (with or without .specs); New project creates one. The setup asks how the AI should treat the code (Conformance) and maps it once.',
    why: 'Everything the loop decides is written to files in .specs/: you can read, review and version every step.' },
  { route: '/loop', target: null, title: 'From a plan to a running loop',
    body: '1. Plan with the AI: describe the goal; it writes the roadmap (features in stages) and the flows. 2. Approve the roadmap: only your click does it. 3. Start the loop: the agents work feature by feature. 4. Answer when it asks. 5. Finish: push and pull request.',
    why: 'You stay in control of the plan and the decisions; the loop does the repetitive work in between.' },
];
const TARGET_WAIT_MS = 4000;
const CARD_WIDTH = 380;
const GAP = 14;

let root = null;
const save = (step, done) => appApi.post('onboarding', { step, done }).catch(() => { /* the tour still works; it may show again */ });

function placement(rect) {
  const vw = window.innerWidth, vh = window.innerHeight, width = Math.min(CARD_WIDTH, vw - 32);
  if (!rect) return { left: (vw - width) / 2, top: Math.max(24, vh / 2 - 160), width };
  const below = rect.bottom + GAP;
  const top = below + 240 < vh ? below : Math.max(16, rect.top - GAP - 260);
  const left = Math.min(Math.max(16, rect.left), vw - width - 16);
  return { left, top, width };
}

function Tour({ start }) {
  const [index, setIndex] = useState(start);
  const [rect, setRect] = useState(null);
  const next = useRef(null);
  const step = STEPS[index];
  useEffect(() => { navigate(step.route, { replace: true }); save(index, false); }, [index]);
  useLayoutEffect(() => {
    setRect(null);
    if (!step.target) return undefined;
    const began = performance.now();
    let frame = 0;
    const find = () => {
      const el = document.querySelector(step.target);
      if (el && el.getBoundingClientRect().width > 0) {
        el.scrollIntoView({ block: 'nearest' });
        const r = el.getBoundingClientRect();
        setRect({ left: r.left, top: r.top, width: r.width, height: Math.min(r.height, window.innerHeight - r.top - 16), bottom: r.top + Math.min(r.height, 320) });
      }
      if (performance.now() - began < TARGET_WAIT_MS) frame = setTimeout(find, 150); // the screen may still be loading its data
    };
    find();
    const onResize = () => { const el = document.querySelector(step.target); if (el) { const r = el.getBoundingClientRect(); setRect({ left: r.left, top: r.top, width: r.width, height: Math.min(r.height, window.innerHeight - r.top - 16), bottom: r.top + Math.min(r.height, 320) }); } };
    window.addEventListener('resize', onResize);
    return () => { clearTimeout(frame); window.removeEventListener('resize', onResize); };
  }, [index]);
  useEffect(() => { if (next.current) next.current.focus(); }, [index]);
  const close = (done) => { save(index, done); render(null, root); };
  const go = (to) => { if (to >= STEPS.length) close(true); else if (to >= 0) setIndex(to); };
  const onKey = (e) => {
    if (e.key === 'Escape') { e.preventDefault(); close(true); }
    else if (e.key === 'ArrowRight') { e.preventDefault(); go(index + 1); }
    else if (e.key === 'ArrowLeft') { e.preventDefault(); go(index - 1); }
  };
  const card = placement(rect);
  const last = index === STEPS.length - 1;
  return html`<div class="tour" onKeyDown=${onKey}>
    ${rect ? html`<div class="tour-spot" aria-hidden="true" style=${{ left: rect.left - 6 + 'px', top: rect.top - 6 + 'px', width: rect.width + 12 + 'px', height: rect.height + 12 + 'px' }}></div>`
      : html`<div class="tour-dim" aria-hidden="true"></div>`}
    <div class="tour-card" role="dialog" aria-modal="false" aria-labelledby="tour-title" aria-describedby="tour-body" style=${{ left: card.left + 'px', top: card.top + 'px', width: card.width + 'px' }}>
      <div class="tour-count">Step ${index + 1} of ${STEPS.length}</div>
      <h2 class="tour-title" id="tour-title">${step.title}</h2>
      <p class="tour-body" id="tour-body">${step.body}</p>
      <p class="tour-why"><strong>Why: </strong>${step.why}</p>
      <div class="tour-dots" aria-hidden="true">${STEPS.map((_, i) => html`<span key=${i} class=${'tour-dot' + (i === index ? ' is-on' : i < index ? ' is-done' : '')}></span>`)}</div>
      <div class="tour-actions">
        <button type="button" class="btn btn-sm btn-ghost" id="tour-skip" onClick=${() => close(true)}>Skip tour</button>
        <span class="sh-grow"></span>
        <button type="button" class="btn btn-sm" id="tour-back" disabled=${index === 0} onClick=${() => go(index - 1)}>Back</button>
        <button type="button" class="btn btn-sm btn-primary" id="tour-next" ref=${next} onClick=${() => go(index + 1)}>${last ? 'Finish' : 'Next'}</button>
      </div>
    </div></div>`;
}

/** Opens the tour at `step` (on the projects page; from a project page it goes there first). */
export function startTour(step = 0) {
  if (location.pathname !== '/') { location.assign('/?tour=' + step); return; }
  try { localStorage.setItem('harness.why.seen.v1', 'true'); } catch (_) { /* the panel may show its first-visit explainer once */ }
  const open = document.querySelector('dialog[open]');
  if (open && open.close) open.close(); // the tour is the first-run guide: no dialog over it
  if (!root) { root = document.createElement('div'); root.id = 'tour-root'; document.body.appendChild(root); }
  render(null, root);
  render(html`<${Tour} start=${Math.max(0, Math.min(STEPS.length - 1, step))} />`, root);
}

/** On the projects page: resumes the tour after `?tour=N`, or opens it the first time (until it is finished or skipped). */
export async function maybeStartTour() {
  if (location.pathname !== '/' || window.__MAESTRO_NO_TOUR) return;
  const asked = new URLSearchParams(location.search).get('tour');
  if (asked != null) { history.replaceState(history.state, '', '/' + location.hash); startTour(Number(asked) || 0); return; }
  try {
    const state = await appApi.get('onboarding');
    if (state && !state.done) {
      const began = performance.now();
      const wait = () => (document.getElementById('splash') && performance.now() - began < 8000 ? setTimeout(wait, 200) : startTour(state.step || 0));
      wait();
    }
  } catch (_) { /* no tour without the app */ }
}
