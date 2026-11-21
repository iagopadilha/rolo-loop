/* Timeline view of the LOOP zone (spec ui-v2 04, L-02): one row per feature (stage tag + name), one column per turn T1..Tn plus
   "next". The axis ticks are the turn selector. A bar spans the turns the feature was worked on, taken from the events of the pipeline model (nothing invented):
   verified (check), running (solid, chip = the step running), open/blocked (between steps or stopped) and queued (dashed, in the
   "next" column). Dependency connectors come from the real deps, "Now" marks the shown turn and the columns after it are hatched.
   The grid scrolls horizontally INSIDE its card; at <= 480 px the same data is shown as one card per feature. */
import { html, useLayoutEffect, useRef } from '../../util/preact.js';
import { stepName, pipe, steps, turnsList, liveN, curN, selectTurn, stateAt, workedTurns, loopStatusAt, featuresWithWaves, featureName, selectFeature, selectedFeature, emptyReason, STEP_COLOR } from './pipeline_model.js';

const ROW_H = 48;
const STATE_WORD = { DONE: 'verified', RUNNING: 'running', QUARANTINE: 'blocked', ATTENTION: 'needs attention', 'IN PROGRESS': 'in progress', 'NOT DISPATCHED': 'queued' };
const BAR_KIND = { DONE: 'verified', RUNNING: 'running', QUARANTINE: 'blocked', ATTENTION: 'blocked', 'IN PROGRESS': 'open', 'NOT DISPATCHED': 'queued' };

/** "build", "verify"...: the plain word of a step index (server key, else the agent role). */
export function stepWord(index) {
  const step = steps()[index];
  if (!step) return '';
  return stepName(step);
}
export const stateWord = (status) => STATE_WORD[status] || String(status || '').toLowerCase();
/** "T2", "T2 – T5" or null when the feature never ran. */
export function turnRangeText(worked) {
  if (!worked || !worked.length) return null;
  const first = worked[0], last = worked[worked.length - 1];
  return first === last ? 'T' + first : `T${first} – T${last}`;
}

/** Rows of the timeline at turn `at`: roadmap features (stage, wave, order) plus any slug the events mention that the roadmap does not know. */
function timelineRows(at) {
  const known = featuresWithWaves(), state = stateAt(at), worked = workedTurns(at), ids = new Set(known.map((feature) => feature.id));
  const extra = Object.keys(state).filter((id) => !ids.has(id)).map((id) => ({ id, name: id, stage: null, wave: 1, deps: [] }));
  const ordered = [...known, ...extra].map((feature, index) => ({ feature, index }))
    .sort((a, b) => (a.feature.stage == null ? 1e9 : a.feature.stage) - (b.feature.stage == null ? 1e9 : b.feature.stage) || a.feature.wave - b.feature.wave || a.index - b.index);
  return ordered.map(({ feature }) => {
    const status = loopStatusAt(feature.id, at), cells = state[feature.id] || null, turns = worked[feature.id] || [];
    const runningStep = cells ? cells.findIndex((cell) => cell.st === 'RUNNING') : -1;
    const pendingStep = cells ? cells.findIndex((cell) => cell.st !== 'DONE' && cell.st !== 'SKIPPED') : -1;
    return { id: feature.id, name: featureName(feature.id), stage: feature.stage, deps: feature.deps.filter((dep) => !String(dep).startsWith('dir:')), status, kind: BAR_KIND[status] || 'open',
      untested: !!feature.untested,
      cells, turns, chip: runningStep >= 0 ? stepWord(runningStep) : status === 'IN PROGRESS' && pendingStep >= 0 ? stepWord(pendingStep) : status === 'DONE' ? (feature.untested ? 'not tested' : '✓') : status === 'QUARANTINE' ? 'blocked' : status === 'ATTENTION' ? 'needs you' : '' };
  });
}

/** Column span (1-based, [start, end]) of each row: worked turns for dispatched features, the "next" column for queued ones. */
function placeBars(rows, columnOf, atColumn) {
  return rows.map((row) => {
    if (row.kind === 'queued' || !row.turns.length) return { ...row, kind: row.turns.length ? row.kind : 'queued', start: atColumn + 1, end: atColumn + 1 };
    const first = columnOf(row.turns[0]), last = columnOf(row.turns[row.turns.length - 1]);
    return { ...row, start: first, end: row.kind === 'verified' ? last : Math.max(last, atColumn) };
  });
}

/** Turn selector: click = the loop as it was at that turn, the latest turn = live. aria-current marks the shown one. */
function selectOrLive(n) { selectTurn(n === liveN() ? null : n); }
function tickTitle(turn, live) { return `Turn ${turn.n}${live ? ' (latest, live)' : ''}${turn.label ? ' · ' + turn.label : ''}`; }
/** A row of buttons T1..Tn (used by the views that have no timeline axis, and by the Decisions page). */
export function TurnPicker({ label = 'Turn' }) {
  const turns = turnsList(), at = curN(), latest = liveN();
  if (!turns.length) return null;
  return html`<div class="tp" role="group" aria-label=${label}>${turns.map((turn) => html`<button key=${turn.n} type="button" class=${'tp-btn' + (turn.n === at ? ' is-on' : '')}
    aria-current=${turn.n === at ? 'true' : null} title=${tickTitle(turn, turn.n === latest)} onClick=${() => selectOrLive(turn.n)}>${'T' + turn.n}${turn.n === latest ? html`<span class="tp-live" aria-hidden="true"></span>` : null}</button>`)}</div>`;
}

/** Picks a feature and shows the inspector (the column when there is room, the overlay / bottom sheet when LAY-01 collapsed it). */
function pick(slug) {
  const app = document.getElementById('app');
  if (app && app.classList.contains('is-inspector-collapsed')) pipe.over = true; else pipe.insp = true;
  selectFeature(slug);
}

function Legend() {
  return html`<div class="lt-legend">
    <span class="lt-key"><span class="lt-swatch is-verified"></span>verified</span>
    <span class="lt-key"><span class="lt-chip is-untested">not tested</span>verified without running tests</span>
    <span class="lt-key"><span class="lt-swatch is-running"></span>running now</span>
    <span class="lt-key"><span class="lt-swatch is-open"></span>in progress</span>
    <span class="lt-key"><span class="lt-swatch is-queued"></span>queued</span>
    <span class="lt-key"><span class="lt-line"></span>depends on</span>
  </div>`;
}

function Bar({ row, selected, columns }) {
  const text = row.kind === 'queued' ? 'queued' : row.name;
  const turnText = turnRangeText(row.turns);
  const tip = `${row.name} · ${stateWord(row.status)}${row.untested ? ' · not tested: no test or build tool ran' : ''}${turnText ? ' · ' + turnText : ''}${row.deps.length ? ' · after ' + row.deps.map(featureName).join(', ') : ''}`;
  return html`<button type="button" class=${'lt-bar is-' + row.kind + (selected ? ' is-selected' : '')} style=${{ gridColumn: `${row.start} / ${row.end + 1}` }}
    aria-label=${'Inspect ' + row.name} aria-pressed=${String(selected)} title=${tip} onClick=${() => pick(row.id)}>
    <span class="lt-bar-text">${text}</span>${row.chip ? html`<span class=${'lt-chip' + (row.kind === 'verified' ? (row.untested ? ' is-untested' : ' is-check') : '')}>${row.chip}</span>` : null}
  </button>`;
}

/** Elbow connectors from the end of each dependency bar to the start of the bar that waits on it (x in % of the plot, y in px). */
function Connectors({ bars, columns }) {
  const byId = Object.fromEntries(bars.map((bar, index) => [bar.id, { bar, index }])), paths = [];
  for (const { id, deps, kind, start } of bars) {
    const mine = byId[id];
    for (const dep of deps) {
      const from = byId[dep];
      if (!from) continue;
      const x1 = (from.bar.end / columns) * 100, x2 = ((start - 1) / columns) * 100, y1 = from.index * ROW_H + ROW_H / 2, y2 = mine.index * ROW_H + ROW_H / 2;
      const mid = x2 > x1 ? (x1 + x2) / 2 : x1 + 0.6;
      paths.push(html`<path key=${dep + '>' + id} d=${`M${x1} ${y1} L${mid} ${y1} L${mid} ${y2} L${Math.max(x2, mid)} ${y2}`} class=${'lt-conn' + (kind === 'queued' ? ' is-queued' : '')} vector-effect="non-scaling-stroke" />`);
    }
  }
  return html`<svg class="lt-svg" viewBox=${`0 0 100 ${bars.length * ROW_H}`} preserveAspectRatio="none" aria-hidden="true">${paths}</svg>`;
}

/** One card per feature (narrow screens). */
function FeatureCards({ bars, selected }) {
  return html`<div class="lt-cards">${bars.map((row) => html`<button type="button" key=${row.id} class=${'lt-fcard is-' + row.kind + (selected === row.id ? ' is-selected' : '')}
      aria-pressed=${String(selected === row.id)} aria-label=${'Inspect ' + row.name} onClick=${() => pick(row.id)}>
      <span class="lt-fcard-top"><span class="lt-stage">${row.stage == null ? 'S?' : 'S' + row.stage}</span><span class="lt-fcard-name">${row.name}</span><span class=${'lt-state is-' + row.kind}>${stateWord(row.status)}</span></span>
      <span class="lt-fcard-sub">${turnRangeText(row.turns) || 'not dispatched yet'}${row.deps.length ? ' · after ' + row.deps.map(featureName).join(', ') : ''}</span>
      <span class="lt-dots" aria-hidden="true">${steps().map((step, index) => html`<span key=${index} class="lt-dotstep" title=${stepWord(index)} style=${{ '--st': STEP_COLOR[(row.cells && row.cells[index] && row.cells[index].st) || 'PENDING'] || STEP_COLOR.PENDING }}></span>`)}</span>
    </button>`)}</div>`;
}

/** The Timeline view (the caller already handled "no data yet"). */
export function LoopTimeline() {
  const reason = emptyReason(), turns = turnsList(), latest = liveN(), cardRef = useRef(null), nowKey = `${turns.length}:${curN()}`;
  // wide timeline: bring the "Now" line into view when the turn changes (the user can scroll away afterwards)
  useLayoutEffect(() => {
    const card = cardRef.current, now = card && card.querySelector('.lt-now');
    if (!card || !now) return;
    const x = now.getBoundingClientRect().left - card.getBoundingClientRect().left + card.scrollLeft;
    card.scrollLeft = Math.max(0, x + 96 - card.clientWidth); // the Now line and the "next" column stay visible
  }, [nowKey]);
  if (reason || !turns.length) return html`<div class="pl-empty">${reason || 'No turn to show.'}</div>`;
  const at = curN(), columnOf = (n) => turns.findIndex((turn) => turn.n === n) + 1, atColumn = columnOf(at);
  const columns = turns.length + 1, rows = timelineRows(at), bars = placeBars(rows, columnOf, atColumn), selected = selectedFeature();
  if (!bars.length) return html`<div class="pl-empty">The roadmap has no feature yet.</div>`;
  const grid = { '--lt-cols': columns, '--lt-rows': bars.length, '--lt-row-h': ROW_H + 'px' };
  return html`<div class="lt">
    <div class="lt-card" ref=${cardRef} role="group" aria-label="Feature timeline">
      <div class="lt-grid" style=${grid}>
        <div class="lt-names">
          <div class="lt-corner">Feature</div>
          ${bars.map((row, index) => html`<div key=${row.id} class=${'lt-name' + (index > 0 && bars[index - 1].stage !== row.stage ? ' is-stage-top' : '') + (row.kind === 'verified' ? ' is-done' : '')}>
            <span class="lt-stage">${row.stage == null ? 'S?' : 'S' + row.stage}</span><span class="lt-name-text" title=${row.id}>${row.name}</span></div>`)}
        </div>
        <div class="lt-plot">
          <div class="lt-axis" role="group" aria-label="Turn">${turns.map((turn) => html`<button key=${turn.n} type="button" class=${'lt-tick' + (turn.n === at ? ' is-now' : '')}
            aria-current=${turn.n === at ? 'true' : null} title=${tickTitle(turn, turn.n === latest)} onClick=${() => selectOrLive(turn.n)}>${'T' + turn.n}</button>`)}<span class="lt-tick">next</span></div>
          <div class="lt-body">
            <div class="lt-cols" aria-hidden="true">${Array.from({ length: columns }, (_, index) => html`<span key=${index} class=${'lt-col' + (index + 1 > atColumn ? ' is-future' : '')}></span>`)}</div>
            <${Connectors} bars=${bars} columns=${columns} />
            <div class="lt-rows">${bars.map((row, index) => html`<div key=${row.id} class=${'lt-row' + (index > 0 && bars[index - 1].stage !== row.stage ? ' is-stage-top' : '')}>
              <${Bar} row=${row} selected=${selected === row.id} columns=${columns} /></div>`)}</div>
            <div class="lt-now" aria-hidden="true" style=${{ left: ((atColumn - 1 + 0.87) / columns) * 100 + '%' }}><span class="lt-now-pill">Now</span></div>
          </div>
        </div>
      </div>
    </div>
    <${FeatureCards} bars=${bars} selected=${selected} />
    <${Legend} />
  </div>`;
}
