/* The ONE inline notice of the LOOP page: a calm row (dot, one sentence, one action) for "needs you" (a feature in quarantine or a question),
   "paused" and "done". The decision (try again or skip) is never taken here: deciding needs context, and the context lives in the pause dialog
   or the status dialog. The wrap-up of a finished loop with a worktree (WrapUp) keeps its own view and replaces the "done" row. */
import { html } from '../../util/preact.js';
import { list, firstLine, clip, DASH } from '../../util/format.js';
import { StatusDot } from '../../components/shell.js';
import { ctx, ui, status, loop, resumingNow } from './context.js';
import { pipe } from './pipeline_model.js';
import { openPauseDialog } from './dialogs/index.js';
import { costCapPause, openStatusDialog, sendResume, statusHasDialog } from './alert.js';
import { FeatureLabel } from './feature_label.js';

const REASON_CLIP = 140;

/** The row itself: tone 'wait' | 'idle' | 'ok' picks the dot, `action` = { label, onClick } or null. */
export function Notice({ tone = 'idle', children, action }) {
  return html`<div class="lp-notice" role="status"><${StatusDot} kind=${tone} /><div class="lp-notice-text">${children}</div>
    ${action ? html`<button type="button" class="btn btn-sm" id=${action.id} disabled=${!!action.disabled} title=${action.title} onClick=${action.onClick}>${action.label}</button>` : null}</div>`;
}

/** P6. A stopped loop (error, paused, or its process gone) always has the same "Continue" button here, whatever the label of the status chip. */
const STOPPED_STATUSES = ['error', 'paused', 'stopped'];
export const canContinue = () => !!ctx.state && STOPPED_STATUSES.includes(status()) && !resumingNow();

/** I5. The install the guard refused, with the feature that waits for it ("waiting for: npm install x (feature y)"); '' when there is none. */
const waitingLine = () => { const w = list(ctx.state && ctx.state.guard_waiting)[0]; return w && w.text ? ' · ' + clip(w.text, 220) : ''; };

/** True when the notice has something to say. */
function pauseBannerVisible() {
  return !!ctx.state && (list(ctx.state.quarantine).length > 0 || ['awaiting_user', 'paused', 'done'].includes(status()) || costCapPause() || canContinue());
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
      <strong>Needs you.</strong><span class="lp-notice-dim">${' ' + clip(firstLine(loop().reason), REASON_CLIP)}${waitingLine()}</span><//>`;
  }
  if (canContinue()) {
    return html`<${Notice} tone="wait" action=${{ label: 'Continue', id: 'btn-continue', disabled: !!ui.busy.answer, title: 'relaunches the loop with the same command, from where it stopped', onClick: () => sendResume('resume request') }}>
      <strong>Stopped.</strong><span class="lp-notice-dim">${clip(firstLine(loop().reason), REASON_CLIP) ? ' ' + clip(firstLine(loop().reason), REASON_CLIP) : ' The loop is stopped.'}${waitingLine() || ' · fix what stopped it, then continue'}</span><//>`;
  }
  if (status() === 'paused' || costCapPause()) {
    return html`<${Notice} tone="idle" action=${dialog && { label: 'See why', onClick: dialog }}>
      <strong>Paused.</strong><span class="lp-notice-dim">${loop().reason ? ' ' + clip(firstLine(loop().reason), REASON_CLIP) : ' The loop continues when you start it again.'}${waitingLine()}</span><//>`;
  }
  if (status() === 'done' && !wrapUp) {
    return html`<${Notice} tone="ok" action=${dialog && { label: 'Summary', onClick: dialog }}>
      <strong>Done.</strong><span class="lp-notice-dim"> The loop finished.</span><//>`;
  }
  return null;
}
