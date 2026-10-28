/* List of AI decisions (presentational): grouped by turn, each one with its level, feature, doubt, choice, how to revert it and
   the two actions (Mark as reviewed / Ask for a change). The screen state and the API calls live in pages/panel/decisions.js. */
import { html } from '../util/preact.js';

export const EMPTY_DECISIONS_TEXT = 'No decision of the AI needs your review';
export const TURN_UNKNOWN_TEXT = 'Turn unknown';
const LEVEL_LABEL = { critical: 'CRITICAL', important: 'IMPORTANT' };

/** Groups items by turn: [{ turn: number|null, items }] with known turns ascending and "unknown" last. */
export function groupByTurn(items) {
  const groups = new Map();
  for (const item of items || []) {
    const key = Number.isFinite(item.turn) ? item.turn : null;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(item);
  }
  return [...groups.entries()].sort(([a], [b]) => (a === null) - (b === null) || (a || 0) - (b || 0)).map(([turn, group]) => ({ turn, items: group }));
}

/** One decision: `busy` disables its buttons while a call runs; `renderFeature(slug)` may return a nicer label than the slug. */
function DecisionItem({ item, busy, onReview, onChange, renderFeature }) {
  const level = item.level === 'critical' ? 'critical' : 'important';
  return html`<li class=${'dl-item is-' + level + (item.reviewed ? ' is-reviewed' : '')}>
    <div class="dl-top">
      <span class=${'dl-level mono is-' + level}>${LEVEL_LABEL[level]}</span>
      <span class="dl-feature">${renderFeature ? renderFeature(item.slug) : item.slug}</span>
      ${item.reviewed ? html`<span class="dl-state mono">reviewed</span>` : null}
      ${item.change_requested ? html`<span class="dl-state is-change mono">change asked</span>` : null}
    </div>
    <dl class="dl-fields">
      <dt>What was unclear</dt><dd>${item.doubt}</dd>
      <dt>What the AI chose</dt><dd>${item.chose}</dd>
      <dt>How to undo it</dt><dd>${item.revert}</dd>
    </dl>
    <div class="dl-actions">
      <button type="button" class="btn btn-sm" disabled=${busy || item.reviewed} title="You agree with this choice: it stops counting as waiting for review." onClick=${() => onReview(item)}>${item.reviewed ? 'Reviewed' : 'I agree'}</button>
      <button type="button" class="btn btn-sm" disabled=${busy || item.change_requested} title="You disagree: this is added to the follow-up draft, nothing runs until you send it." onClick=${() => onChange(item)}>${item.change_requested ? 'Change asked' : 'Ask to change it'}</button>
    </div>
    ${item.reviewed || item.change_requested ? null : html`<p class="dl-hint">${'"I agree" closes this item. "Ask to change it" adds it to the follow-up draft; nothing runs until you send it.'}</p>`}
  </li>`;
}

/** One pending approval: a command the loop wants to run on DEV and may not yet. The loop keeps working on the rest meanwhile. Production never
    gets a card, so there is no button for it. */
function ApprovalCard({ item, busy, onApprove, onDeny, renderFeature }) {
  return html`<li class="dl-item dl-approval">
    <div class="dl-top">
      <span class="dl-level mono">APPROVAL</span>
      ${item.slug ? html`<span class="dl-feature">${renderFeature ? renderFeature(item.slug) : item.slug}</span>` : null}
      <span class="dl-state mono">${'on ' + (item.env || 'DEV')}</span>
    </div>
    <p class="dl-ask">${'The loop wants to ' + (item.what || 'run a command') + ' on ' + (item.env || 'DEV') + '.'}</p>
    <code class="dl-raw-text">${item.command}</code>
    <div class="dl-actions">
      <button type="button" class="btn btn-sm btn-primary" disabled=${busy} title="Run it this once. The loop goes on by itself." onClick=${() => onApprove(item, false)}>Approve</button>
      <button type="button" class="btn btn-sm" disabled=${busy} title="Approve it and stop asking for this kind of command in this project (saved in the project's list)." onClick=${() => onApprove(item, true)}>Approve always for this project</button>
      <button type="button" class="btn btn-sm" disabled=${busy} title="Do not run it. The loop leaves that task undone and carries on." onClick=${() => onDeny(item)}>Deny</button>
    </div>
  </li>`;
}

/** <DecisionList items unparsed approvals busyIds onReview onChange onApprove onDeny renderFeature showTurnTitles emptyText /> */
export function DecisionList({ items = [], unparsed = [], approvals = [], busyIds = {}, onReview, onChange, onApprove, onDeny, renderFeature, showTurnTitles = true, emptyText = EMPTY_DECISIONS_TEXT }) {
  if (!items.length && !unparsed.length && !approvals.length) return html`<div class="dl-empty">${emptyText}</div>`;
  return html`<div class="dl">
    ${approvals.length ? html`<section class="dl-group"><h3 class="dl-turn mono">Waiting for your approval</h3>
      <ul class="dl-list">${approvals.map((item) => html`<${ApprovalCard} key=${item.id} item=${item} busy=${!!busyIds[item.id]} onApprove=${onApprove} onDeny=${onDeny} renderFeature=${renderFeature} />`)}</ul></section>` : null}
    ${groupByTurn(items).map((group) => html`<section key=${String(group.turn)} class="dl-group">
      ${showTurnTitles ? html`<h3 class="dl-turn mono">${group.turn == null ? TURN_UNKNOWN_TEXT : 'Turn ' + group.turn}</h3>` : null}
      <ul class="dl-list">${group.items.map((item) => html`<${DecisionItem} key=${item.id} item=${item} busy=${!!busyIds[item.id]}
        onReview=${onReview} onChange=${onChange} renderFeature=${renderFeature} />`)}</ul>
    </section>`)}
    ${unparsed.length ? html`<section class="dl-group dl-unparsed"><h3 class="dl-turn mono">Unparsed lines</h3>
      <p class="dl-hint">These lines of VALIDATE.md did not match the decision format; they are shown as written.</p>
      <ul class="dl-list">${unparsed.map((line) => html`<li key=${line.line_no} class="dl-item dl-raw"><span class="dl-raw-no mono">${'line ' + line.line_no}</span><code class="dl-raw-text">${line.raw}</code></li>`)}</ul></section>` : null}
  </div>`;
}
