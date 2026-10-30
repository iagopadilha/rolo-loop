/* LOOP page (route #/loop, the default; main#loop-zone). Everything here depends on the selected turn, which the timeline axis ticks pick
   (loop_timeline.js; the Graph and List views get the same ticks in a row above the view). From top to bottom: the wrap-up (loop done or
   stopped with a worktree), the inline notice (needs you / paused / done), the page header (title "Turn N <step>", ONE summary line - the
   only place with verified count, cost and running agents -, the quiet facts line of that turn, the view switch Timeline | Graph | List and the
   inspector toggle) and a row with the view plus the inspector. The page scrolls vertically inside itself. */
import { html, useLayoutEffect } from '../../util/preact.js';
import { isNum, cost, dur, list } from '../../util/format.js';
import { renderAll } from '../project_panel.js';
import { ctx } from './context.js';
import { pipe, turnsList, curN, liveN, selectTurn, emptyReason, loopSummary } from './pipeline_model.js';
import { PipelineView } from './pipeline_view.js';
import { PauseBanner } from './pause_banner.js';
import { Inspector } from './inspector.js';
import { WrapUp, wrapUpVisible } from './wrap_up.js';
import { TurnPicker } from './loop_timeline.js';
import { Icon } from '../../components/shell.js';

const VIEWS = [['timeline', 'Timeline'], ['diagram', 'Graph'], ['list', 'List']];
const INSPECTOR_COLLAPSE_BELOW_PX = 960; // the content column (next to the sidebar) is too narrow for the 300 px inspector beside the view

/** Number of steps of a turn: `changes` when the model gives a number, else the events it has. */
function stepsInTurn(turn) {
  if (isNum(turn.changes)) return turn.changes;
  return Array.isArray(turn.events) ? turn.events.length : null;
}

function pickView(view) { pipe.view = view; renderAll(); }
const columnWidth = () => { const col = document.getElementById('shell-col'); return col ? col.clientWidth : Infinity; };
const inspectorCollapsed = () => columnWidth() < INSPECTOR_COLLAPSE_BELOW_PX;
/** The inspector button: toggles the column (wide) or the overlay / bottom sheet (when the column is too narrow or on a phone). */
function toggleInspector() { if (inspectorCollapsed()) pipe.over = !pipe.over; else pipe.insp = !pipe.insp; renderAll(); }
let wasCollapsed = null;
if (typeof window !== 'undefined') { // crossing the threshold changes what the inspector button means: redraw once
  window.addEventListener('resize', () => { const now = inspectorCollapsed(); if (wasCollapsed !== null && now !== wasCollapsed) { pipe.over = false; renderAll(); } wasCollapsed = now; });
}

/** The page header: "Turn N <step name>", the summary line (verified, cost, agents running) and the facts of the shown turn. */
function TurnHeader({ turn }) {
  if (!turn) {
    return html`<div class="lp-title-block"><h1 class="lp-title">${pipe.loading ? 'Reading the turns…' : 'No turn yet'}</h1>
      <div class="lp-summary">The turns appear here when the loop runs its first turn.</div></div>`;
  }
  const sum = loopSummary(turn.n), live = turn.n === liveN();
  const meta = ctx.state && ctx.state.meta, spent = live && meta && isNum(meta.cost_usd) ? meta.cost_usd : sum.spent, max = meta && isNum(meta.max_cost_usd) ? meta.max_cost_usd : null;
  const agents = live ? Math.max(sum.running, list(ctx.state && ctx.state.agents).filter((a) => a.state === 'running' && a.role !== 'orchestrator' && a.id !== 'orch').length) : sum.running;
  const steps = stepsInTurn(turn);
  const facts = [isNum(turn.duration_s) ? 'took ' + dur(turn.duration_s) : '', isNum(turn.cost_usd) ? cost(turn.cost_usd) : '', steps == null ? '' : `${steps} step${steps === 1 ? '' : 's'}`].filter(Boolean);
  return html`<div class="lp-title-block">
    <h1 class="lp-title"><span>${'Turn ' + turn.n}</span>${turn.label ? html` <span class="lp-step">${turn.label}</span>` : null}</h1>
    <div class="lp-summary" aria-live="polite">${`${sum.verified} of ${sum.total} verified · ${cost(spent)}${max != null ? ' of ' + cost(max) : ''} · ${agents} ${agents === 1 ? 'agent' : 'agents'} running`}</div>
    <div class="lp-facts" aria-label=${'Facts of turn ' + turn.n}>${facts.length ? `This turn ${facts.join(' · ')}` : 'No figures recorded for this turn.'}</div>
  </div>`;
}

/** Content of the page: the selected view of the pipeline (the pipeline agent owns each view). */
function ViewContent() {
  const unavailable = emptyReason();
  if (unavailable && !pipe.data) return html`<div class="lp-empty" role="alert">${unavailable}</div>`;
  return html`<${PipelineView} />`;
}

/** The whole <main>. */
export function LoopPage() {
  const turn = turnsList().find((item) => item.n === curN()) || null;
  const view = VIEWS.some(([key]) => key === pipe.view) ? pipe.view : 'timeline';
  const collapsed = inspectorCollapsed(), inspectorOn = collapsed ? pipe.over : pipe.insp, wrap = wrapUpVisible();
  if (wasCollapsed === null) wasCollapsed = collapsed;
  useLayoutEffect(() => { const app = document.getElementById('app'); if (app) app.classList.toggle('is-inspector-collapsed', collapsed); });
  return html`<main class="loop" id="loop-zone" aria-label="Loop">
    ${wrap ? html`<${WrapUp} />` : null}
    <${PauseBanner} wrapUp=${wrap} />
    <div class="lp-head"><${TurnHeader} turn=${turn} />
      <div class="lp-tools">
        ${pipe.sel != null ? html`<button type="button" class="btn btn-sm btn-ghost" title="back to the latest turn (live)" onClick=${() => selectTurn(null)}>Back to latest</button>` : null}
        <div class="sh-seg" role="group" aria-label="Pipeline view">${VIEWS.map(([key, label]) => html`<button key=${key} type="button"
          class=${'sh-seg-btn is-text' + (view === key ? ' is-on' : '')} aria-pressed=${String(view === key)} onClick=${() => pickView(key)}>${label}</button>`)}</div>
        <button type="button" class=${'lp-icon-btn' + (inspectorOn ? ' is-on' : '')} aria-pressed=${String(!!inspectorOn)} aria-label="Show or hide the inspector" title="Show or hide the inspector" onClick=${toggleInspector}>
          <${Icon} name="panel" /></button>
      </div></div>
    ${view === 'timeline' ? null : html`<div class="lp-turns"><${TurnPicker} /></div>`}
    <div class="z-row">
      <div class="z-content"><${ViewContent} /></div>
      <${Inspector} on=${true} />
    </div>
  </main>`;
}
