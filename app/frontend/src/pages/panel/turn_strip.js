/* Turn strip of the LOOP zone (a slim bar at the bottom): the button that shows or hides the GLOBAL zone, one compact chip per
   turn (T<n>, dot = worst status of its events, label, cost), the AI decisions button and "latest". Each turn is a snapshot of the loop: picking one
   makes the whole LOOP zone show the state accumulated up to that turn. Data and selection come from pipeline_model.js. */
import { html, useEffect, useRef } from '../../util/preact.js';
import { isNum, cost } from '../../util/format.js';
import { pipe, turnsList, liveN, curN, selectTurn } from './pipeline_model.js';
import { toggleGlobalZone, globalZoneHidden, globalToggleArrow } from './layout.js';
import { describeClaude } from './claude_badge.js';
import { ctx } from './context.js';
import { decisionsCounts, openDecisionsDialog } from './decisions.js';

const FALLBACK_DOT = '#4A4664';

/** Number of steps of a turn: `changes` when the model gives a number, else the events it has. */
export function stepsInTurn(turn) {
  if (isNum(turn.changes)) return turn.changes;
  return Array.isArray(turn.events) ? turn.events.length : null;
}
const stepsText = (turn) => { const k = stepsInTurn(turn); return k == null ? '' : `${k} step${k === 1 ? '' : 's'}`; };

function TurnCard({ turn, on, isLive }) {
  const claudeTip = (describeClaude(turn) || {}).title;
  const tip = `Turn ${turn.n}${isLive ? ' (latest)' : ''} · ${turn.label || turn.status || ''}${claudeTip ? ' · ' + claudeTip : ''}`;
  return html`<button type="button" class=${'z-turn' + (on ? ' is-on' : '') + (isLive ? ' is-live' : '')} title=${tip} aria-pressed=${String(on)}
    onClick=${() => selectTurn(isLive ? null : turn.n)}>
    <span class="z-turn-n mono">${'T' + turn.n}</span><span class="z-turn-dot" style=${{ background: turn.dot || FALLBACK_DOT }}></span>
    <span class="z-turn-label">${turn.label || turn.status || '—'}</span>
    <span class="z-turn-foot mono">${isNum(turn.cost_usd) ? cost(turn.cost_usd) : stepsText(turn)}</span>
  </button>`;
}

/** The whole strip: ONE slim bar at the bottom of the loop zone (the caller shows it only when the panel is in loop mode).
    Left to right: global toggle, the turns (scrollable), the AI decisions button, "latest". */
export function TurnStrip() {
  const turns = turnsList(), latest = liveN(), current = curN();
  const cards = useRef(null);
  const open = decisionsCounts(), openTotal = open.critical + open.important;
  // a new turn scrolls the strip to the end so the latest card stays visible
  useEffect(() => { if (cards.current) cards.current.scrollLeft = cards.current.scrollWidth; }, [turns.length]);
  return html`<section class="z-strip" aria-label="Loop turns">
    <button type="button" class="z-global-toggle" title=${globalZoneHidden() ? 'Show the global context' : 'Hide the global context'}
      aria-label="Show or hide the global context" aria-expanded=${String(!globalZoneHidden())} aria-controls="global-zone" onClick=${toggleGlobalZone}>${globalToggleArrow()}</button>
    <div class="z-turns" ref=${cards}>${turns.length
      ? turns.map((turn) => html`<${TurnCard} key=${turn.n} turn=${turn} on=${turn.n === current} isLive=${turn.n === latest} />`)
      : html`<span class="empty-note">${pipe.loading ? 'Reading the turns…' : 'no turn recorded yet'}</span>`}</div>
    <button type="button" class=${'z-btn-mono z-strip-decisions' + (open.critical ? ' is-critical' : '')} title="What the AI decided on its own: review, or ask for a change"
      onClick=${() => openDecisionsDialog({ state: ctx.state })}>AI decisions${openTotal ? html` <span class="z-strip-count">${openTotal}</span>` : null}</button>
    <button type="button" class="z-btn-mono" disabled=${pipe.sel == null} title="back to the latest turn (live)" onClick=${() => selectTurn(null)}>● latest</button>
  </section>`;
}
