/* "Why Agent Maestro?" dialog: opens from the "?" button and by itself on the first visit (remembered in localStorage). */
import { html } from '../util/preact.js';
import { openDialog } from './dialog.js';
import { readStored, writeStored } from '../util/storage.js';

const SEEN_KEY = 'harness.why.seen.v1';

const GLOSSARY = [
  ['turn', 'one run of claude -p with the /orchestrator skill. The loop is a sequence of turns; each one moves the roadmap forward and writes the state.'],
  ['wave', 'features that run together in the same turn because they touch different files and do not depend on each other.'],
  ['gate', 'quality gate: a spec only becomes code after the spec-reviewer approves it; code only becomes verified after the verifier passes it.'],
  ['quarantine', 'where a feature that got stuck (blocked) waits. The loop carries on with what does not depend on it; you decide to try again or skip it.'],
  ['VALIDATE', 'VALIDATE.md: decisions the loop took on its own that you should check (with what it chose and how to revert it).'],
  ['ANSWERS', 'ANSWERS.md: your answers and instructions to the loop; the next turn reads and applies them.'],
  ['worktree', 'a working copy of the repository in a sibling folder (…-worktree-loop) on the loop branch. The loop works there, not in your folder.'],
];

const PLEX_SANS = 'var(--sans)', PLEX_MONO = 'var(--mono)';
const Box = ({ x, y, w, title, sub, color }) => html`<g>
  <rect x=${x} y=${y} width=${w} height="58" rx="10" fill="var(--fill)" stroke=${color} stroke-width="1.5" />
  <text x=${x + w / 2} y=${y + 24} text-anchor="middle" fill="var(--fg)" font-size="13" font-weight="600" font-family=${PLEX_SANS}>${title}</text>
  <text x=${x + w / 2} y=${y + 42} text-anchor="middle" fill="var(--fg2)" font-size="10.5" font-family=${PLEX_MONO}>${sub}</text>
</g>`;
const Arrow = ({ x1, x2, y, label }) => html`<g>
  <path d=${`M${x1} ${y} L${x2 - 4} ${y}`} stroke="var(--line4)" stroke-width="1.5" marker-end="url(#why-arrow)" />
  <text x=${(x1 + x2) / 2} y=${y - 7} text-anchor="middle" fill="var(--fg2)" font-size="8" font-family=${PLEX_MONO}>${label}</text>
</g>`;

function FlowDiagram() {
  return html`<svg class="why-diagram" viewBox="0 0 660 92" role="img" aria-label="Flow: the /orchestrator skill plans and dispatches; the harness engine runs the turns behind the guard; everything is written to .specs files; this app reads them and sends your clicks back to the engine.">
    <defs><marker id="why-arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto"><path d="M0 0 L8 4 L0 8 z" fill="var(--line4)" /></marker></defs>
    <${Box} x=${4} y=${18} w=${140} title="/orchestrator" sub="plans and dispatches" color="var(--accent)" />
    <${Arrow} x1=${144} x2=${186} y=${47} label="each turn" />
    <${Box} x=${186} y=${18} w=${140} title="harness engine" sub="turns, state, guard" color="var(--warn)" />
    <${Arrow} x1=${326} x2=${368} y=${47} label="writes" />
    <${Box} x=${368} y=${18} w=${120} title=".specs/" sub="roadmap, specs" color="var(--cyan)" />
    <${Arrow} x1=${488} x2=${530} y=${47} label="reads" />
    <${Box} x=${530} y=${18} w=${126} title="this app" sub="timeline, answers" color="var(--ok)" />
    <path d="M593 76 C593 90, 256 90, 256 78" fill="none" stroke="var(--warn)" stroke-width="1.2" stroke-dasharray="4 4" marker-end="url(#why-arrow)" />
    <text x="424" y="88" text-anchor="middle" fill="var(--warn)" font-size="9.5" font-family=${PLEX_MONO}>your clicks (with confirmation)</text>
  </svg>`;
}

/** Body of the "Why Agent Maestro?" dialog. */
function WhyContent() {
  const p = (text) => html`<p class="dlg-text">${text}</p>`;
  return html`
    ${p('Agent Maestro is a local control panel for the spec-driven loop on top of Claude Code. It runs on your machine and uses your own Claude subscription through the claude CLI: no API key. The panel makes no request outside your computer.')}
    ${p('A loop produces dozens of files: the roadmap, the state, decisions to validate, your answers, one spec per feature, the turns and the transcript of every agent. Opening them one by one does not show the whole. The app gathers them in one place: the timeline of features and turns, the specs, the questions from the loop, what to validate, the cost and the limits.')}
    <${FlowDiagram} />
    <ul class="why-list">
      <li><strong>Act on a specific point. </strong>Select a feature on the timeline, a turn or an agent and work on exactly that point: inspect it, open its page, ask the agent about it or answer its question.</li>
      <li><strong>Commands become buttons. </strong>Check, start, pause, answer, rewind and deliver are buttons with confirmation: fewer commands to remember, fewer copy-and-paste mistakes.</li>
      <li><strong>Guardrails. </strong>In every agent the loop starts, the guard blocks git push, commits outside the checkpoint, writes outside the feature's paths, secrets in commands and hosts that are not on the project's network list.</li>
      <li><strong>Any starting point. </strong>A new project (creates the folder and .specs), an existing repository without .specs (maps it and generates the roadmap) or a loop stopped midway (opens it and shows where it stopped).</li>
      <li><strong>Who does what. </strong>The orchestrator skill is the brain: the prompt each turn runs. The harness engine runs the turns and keeps the state. This app only shows what happened and triggers what you click.</li>
    </ul>
    <div class="small-label">Mini glossary</div>
    <dl class="why-glossary">${GLOSSARY.map(([t, d]) => [html`<dt key=${'t-' + t}>${t}</dt>`, html`<dd key=${'d-' + t}>${d}</dd>`])}</dl>`;
}

export function openWhy() {
  writeStored(SEEN_KEY, true);
  openDialog({
    title: 'Why Agent Maestro?', width: 720, cls: 'dlg-why',
    content: html`<${WhyContent} />`,
    actions: [{ text: 'Got it', cls: 'btn-primary' }],
  });
}

/** Opens by itself only on the first visit (without storage it opens every time it cannot remember: acceptable). */
export function openWhyOnFirstVisit() {
  if (!readStored(SEEN_KEY, false)) openWhy();
}

/** The "?" button that opens the dialog. */
export function WhyButton() {
  return html`<button type="button" class="btn btn-ghost btn-help" aria-label="Why Agent Maestro?" title="Why Agent Maestro?" onClick=${openWhy}>?</button>`;
}
