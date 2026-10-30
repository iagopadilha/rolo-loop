/* Problems of the loop on the Errors page: "Errors" (what really went wrong: loop stops, failed turns, quarantines, launch problems and the
   FAIL/BLOCKED steps of the agents) apart from "Guard blocks" (the guard stopped an agent from breaking a rule: it worked, nothing to fix).
   The page shows every turn or only the selected one. Data: the error center (GET errors, store ui.errors of errors.js) and the turn events
   of the pipeline model. Built from the page primitives (components/page.js); a row opens the error dialog. */
import { html } from '../../util/preact.js';
import { txt, list, fmtRelative, ERROR_KIND_LABEL } from '../../util/format.js';
import { List, Row, Section, State, EmptyState } from '../../components/page.js';
import { StatusDot } from '../../components/shell.js';
import { ui } from './context.js';
import { loadErrors } from './errors.js';
import { steps, turnFailures } from './pipeline_model.js';
import { openErrorDialog, openAgentDialog } from './dialogs/index.js';
import { formatTurnRanges } from './dialogs/error_dialog.js';
import { FeatureLabel } from './feature_label.js';

const GUARD_NOTE = 'The guard stopped an agent from doing something the loop does not allow. It worked: nothing was changed. Many blocks of one rule mean an agent kept retrying.';
const COLS = 'minmax(0,1fr) 150px 110px 20px', COLS_NARROW = 'minmax(0,1fr) 20px';
const DOT_OF_SEVERITY = { error: 'bad', warning: 'wait', info: 'idle' };

const isGuard = (item) => item.kind === 'guard';
/** Turns an error-center item happened in: `turns` when the server sends it, else the single `turn`. */
function itemTurns(item) {
  const turns = list(item.turns).filter((n) => Number.isFinite(n));
  return turns.length ? turns : Number.isFinite(item.turn) ? [item.turn] : [];
}
/** How many times an item happened in turn n (null when unknown). */
function countInTurn(item, n) {
  const counts = item.turn_counts || {};
  if (counts[n] != null || counts[String(n)] != null) return Number(counts[n] != null ? counts[n] : counts[String(n)]);
  const turns = itemTurns(item);
  return turns.length === 1 && turns[0] === n && Number.isFinite(item.count) ? item.count : null;
}

/** { errors, guard, loaded } for the whole loop (turn null) or for one turn: errors = error-center problems (+ the failed agent steps of the
    turn), guard = guard items, as the server sorts them. */
function problemsAt(turn) {
  const store = ui.errors, items = list(store.data && store.data.items), loaded = !!store.data;
  const inTurn = (item) => turn == null || itemTurns(item).includes(turn);
  const errors = items.filter((item) => !isGuard(item) && inTurn(item)).map((item) => ({ source: 'center', item }));
  if (turn != null) turnFailures(turn).forEach((event) => errors.push({ source: 'step', event }));
  const guard = items.filter((item) => isGuard(item) && inTurn(item)).map((item) => ({ source: 'center', item })); // the "older history" line has no turn
  return { errors, guard, loaded };
}

const Head = () => html`<span class="ls-h">Problem</span><span class="ls-h ls-opt">Where</span><span class="ls-h ls-opt ls-end">When</span><span></span>`;

function CenterRow({ item, turn }) {
  const times = turn == null ? item.count : countInTurn(item, turn), turns = itemTurns(item);
  const title = isGuard(item) && item.rule ? `${item.rule}${item.profile ? ' (' + item.profile + ')' : ''}` : txt(item.title);
  const where = turns.length ? formatTurnRanges(turns) : isGuard(item) && item.profile ? item.profile : ERROR_KIND_LABEL[item.kind] || txt(item.kind);
  const sub = isGuard(item) ? (list(item.attempts).length ? 'tried: ' + list(item.attempts)[0] : item.detail) : ERROR_KIND_LABEL[item.kind] || '';
  return html`<${Row} onOpen=${() => openErrorDialog(item)} title="Open what happened and ask the AI">
    <span class="ls-main"><span class="ls-line"><${StatusDot} kind=${DOT_OF_SEVERITY[item.severity] || 'idle'} />
      <span class="ls-title">${title}</span>${Number.isFinite(times) && times > 1 ? html`<span class="ls-sub">${times + '×'}</span>` : null}</span>
      ${sub ? html`<span class="ls-sub" title=${sub}>${sub}</span>` : null}</span>
    <span class="ls-meta ls-opt">${where}</span><span class="ls-meta ls-opt ls-end">${item.when ? fmtRelative(item.when) : '—'}</span><//>`;
}

function StepRow({ event }) {
  const agent = event.agent || (steps()[event.step] && steps()[event.step].agent) || 'agent';
  const open = event.feature && event.step >= 0 ? () => openAgentDialog({ feature: event.feature, step: event.step }) : null;
  return html`<${Row} onOpen=${open} title="Open the agent: its runs and the chat">
    <span class="ls-main"><span class="ls-line"><${StatusDot} kind="bad" />
      <span class="ls-title">${agent}${event.feature ? html` · <${FeatureLabel} slug=${event.feature} />` : null}</span></span>
      ${event.out ? html`<span class="ls-sub" title=${event.out}>${event.out}</span>` : null}</span>
    <span class="ls-meta ls-opt">${ERROR_KIND_LABEL.step}</span><span class="ls-meta ls-opt ls-end"><${State} kind="bad">${String(event.status).toLowerCase()}<//></span><//>`;
}

function Rows({ entries, turn, label }) {
  return html`<${List} label=${label} cols=${COLS} narrow=${COLS_NARROW} head=${html`<${Head} />`}>
    ${entries.map((entry, index) => entry.source === 'step' ? html`<${StepRow} key=${'s' + index} event=${entry.event} />` : html`<${CenterRow} key=${entry.item.id} item=${entry.item} turn=${turn} />`)}<//>`;
}

/** The problems of the whole loop (turn null) or of one turn. */
export function ProblemsView({ turn = null }) {
  const store = ui.errors, { errors, guard, loaded } = problemsAt(turn);
  if (!loaded) return html`<${EmptyState} text=${store.loading || store.key === undefined ? 'Loading the errors…' : 'Errors unavailable: ' + (store.err || 'no response')}
    action=${store.loading ? null : { label: 'Try again', onClick: () => loadErrors(true) }} />`;
  const guardTimes = guard.reduce((sum, { item }) => sum + (turn == null ? (item.count || 0) : (countInTurn(item, turn) || 0)), 0);
  return html`<div class="pg-stack">
    <${Section} title="Errors" note=${errors.length ? String(errors.length) : null}>
      ${errors.length ? html`<${Rows} entries=${errors} turn=${turn} label="Errors" />`
        : html`<${EmptyState} text=${turn == null ? 'No errors. The loop only stops and notifies you for a real problem.' : 'No error in this turn.'} />`}
    <//>
    ${guard.length ? html`<${Section} title="Guard blocks" note=${`${guardTimes || guard.length} · not problems`}>
      <p class="pg-note">${GUARD_NOTE}</p><${Rows} entries=${guard} turn=${turn} label="Guard blocks" /><//>` : null}
  </div>`;
}
