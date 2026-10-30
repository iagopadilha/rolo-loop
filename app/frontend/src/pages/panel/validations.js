/* "Validations" dialog: what the loop checks for each verified feature (state.validations) and what stays with the user and why
   (state.manual_checks). Read only: the engine decides, this only shows. Paths of evidence files are shown as written; no content is read. */
import { html } from '../../util/preact.js';
import { list, txt } from '../../util/format.js';
import { openDialog } from '../../components/dialog.js';
import { ctx } from './context.js';
import { FeatureLabel } from './feature_label.js';

const STATUS_TEXT = { ok: 'checked', failed: 'failed', pending: 'not run yet', unproven: 'no proof', 'not-covered': 'no surface to check', rejected: 'refused', blocked: 'blocked', 'skipped-by-user': 'skipped by you' };
const BAD = new Set(['failed', 'unproven', 'rejected', 'blocked']);
const SOURCE_TEXT = { by_design: 'by design', not_covered: 'not covered', skipped: 'you skipped it', checkpoint: 'checkpoint' };

/** How many things wait for the user's own check (the button badge). */
export function manualCount(state = ctx.state) { return list(state && state.manual_checks).length; }
/** True when the state carries any validation data at all (the button hides itself otherwise). */
export function hasValidations(state = ctx.state) { return list(state && state.validations).length > 0 || manualCount(state) > 0; }

function Body() {
  const state = ctx.state || {}, rows = list(state.validations), manual = list(state.manual_checks);
  return html`<div class="val">
    <p class="dl-explain">What the loop checks for every verified feature, layer by layer, and what only you can check. A check counts only with proof (a file the run left behind).</p>
    ${rows.length ? html`<table class="val-table"><thead><tr><th>Feature</th><th>Layer</th><th>What it checks</th><th>Result</th></tr></thead><tbody>${rows.map((row, index) => html`<tr key=${index}>
      <td><${FeatureLabel} slug=${row.feature} /></td><td class="mono">${txt(row.layer)}</td><td>${txt(row.what)}${list(row.evidence).length ? html`<div class="val-proof mono">${list(row.evidence).map(txt).join(', ')}</div>` : null}</td>
      <td><span class=${'val-status mono' + (BAD.has(row.status) ? ' is-bad' : '')}>${STATUS_TEXT[row.status] || txt(row.status)}</span></td></tr>`)}</tbody></table>`
      : html`<div class="dl-empty">No validation recorded yet.</div>`}
    <h3 class="dl-turn mono">Only you can check</h3>
    ${manual.length ? html`<ul class="val-manual">${manual.map((item, index) => html`<li key=${index}><div>${item.feature ? html`<${FeatureLabel} slug=${item.feature} />` : null} ${txt(item.what)}</div>
      <div class="hint">${txt(item.reason)}${item.source ? ' · ' + (SOURCE_TEXT[item.source] || txt(item.source)) : ''}</div></li>`)}</ul>` : html`<div class="dl-empty">Nothing is left for you to check.</div>`}
  </div>`;
}

export function openValidationsDialog() {
  openDialog({ title: 'Validations', width: 820, cls: 'dlg-validations', content: html`<${Body} />`, actions: [{ text: 'Close' }] });
}
