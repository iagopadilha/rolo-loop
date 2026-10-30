/* The ONE inline notice of the LOOP page: a calm row (dot, one sentence, one action) for "needs you" (a feature in quarantine or a question),
   "paused" and "done". The decision (try again or skip) is never taken here: deciding needs context, and the context lives in the pause dialog
   or the status dialog. The wrap-up of a finished loop with a worktree (WrapUp) keeps its own view and replaces the "done" row. */
import { html } from '../../util/preact.js';
import { list, firstLine, clip, DASH } from '../../util/format.js';
import { StatusDot } from '../../components/shell.js';
import { ctx, status, loop } from './context.js';
import { pipe } from './pipeline_model.js';
import { openPauseDialog } from './dialogs/index.js';
import { costCapPause, openStatusDialog, statusHasDialog } from './alert.js';
import { FeatureLabel } from './feature_label.js';

const REASON_CLIP = 140;

/** The row itself: tone 'wait' | 'idle' | 'ok' picks the dot, `action` = { label, onClick } or null. */
export function Notice({ tone = 'idle', children, action }) {
  return html`<div class="lp-notice" role="status"><${StatusDot} kind=${tone} /><div class="lp-notice-text">${children}</div>
    ${action ? html`<button type="button" class="btn btn-sm" onClick=${action.onClick}>${action.label}</button>` : null}</div>`;
}

/** True when the notice has something to say. */
function pauseBannerVisible() {
  return !!ctx.state && (list(ctx.state.quarantine).length > 0 || ['awaiting_user', 'paused', 'done'].includes(status()) || costCapPause());
}

export function PauseBanner({ wrapUp = false }) {
  if (!pauseBannerVisible()) return null;
  const quarantined = list(ctx.state.quarantine), first = quarantined[0], awaiting = status() === 'awaiting_user';
  const paused = awaiting || !!(pipe.data && pipe.data.paused);
  const dialog = statusHasDialog() ? openStatusDialog : null;
  if (first) {
    const more = quarantined.length - 1;
    return html`<${Notice} tone="wait" action=${{ label: 'Understand and resolve', onClick: () => openPauseDialog(first.id) }}>
      <strong>Needs you.</strong> <${FeatureLabel} slug=${first.id} className="lp-notice-slug" /> is in quarantine<span class="lp-notice-dim">${' · ' + (clip(first.reason, REASON_CLIP) || DASH)}${paused ? ' · loop paused' : ''}${more > 0 ? ` · +${more} more` : ''}</span><//>`;
  }
  if (awaiting) {
    return html`<${Notice} tone="wait" action=${dialog && { label: 'Answer', onClick: dialog }}>
      <strong>Needs you.</strong><span class="lp-notice-dim">${' ' + clip(firstLine(loop().reason), REASON_CLIP)}</span><//>`;
  }
  if (status() === 'paused' || costCapPause()) {
    return html`<${Notice} tone="idle" action=${dialog && { label: 'See why', onClick: dialog }}>
      <strong>Paused.</strong><span class="lp-notice-dim">${loop().reason ? ' ' + clip(firstLine(loop().reason), REASON_CLIP) : ' The loop continues when you start it again.'}</span><//>`;
  }
  if (status() === 'done' && !wrapUp) {
    return html`<${Notice} tone="ok" action=${dialog && { label: 'Summary', onClick: dialog }}>
      <strong>Done.</strong><span class="lp-notice-dim"> The loop finished.</span><//>`;
  }
  return null;
}
