/* Pause banner of the LOOP zone: one line and ONE action. It shows while a feature is in quarantine or the loop waits for the
   user. The decision (try again or skip) is never taken here: deciding needs context, and the context lives in the pause dialog.
   A loop that waits for an answer without any quarantine keeps the existing answer UI (the Alert band at the top of the page). */
import { html } from '../../util/preact.js';
import { list, firstLine, DASH } from '../../util/format.js';
import { ctx, ui, status, loop, resumingNow } from './context.js';
import { pipe } from './pipeline_model.js';
import { openPauseDialog } from './dialogs/index.js';
import { openStatusDialog, sendResume } from './alert.js';
import { FeatureLabel } from './feature_label.js';

const REASON_CLIP = 140;
const clip = (text, max) => { const flat = String(text == null ? '' : text).replace(/\s+/g, ' ').trim(); return flat.length > max ? flat.slice(0, max - 1) + '…' : flat; };

/** True when the banner has something to say. */
export function pauseBannerVisible() {
  return !!ctx.state && (list(ctx.state.quarantine).length > 0 || status() === 'awaiting_user' || canContinue());
}

/** P6. A stopped loop (error, paused, or its process gone) always has the same "Continue" button here, whatever the label of the status chip. */
const STOPPED_STATUSES = ['error', 'paused', 'stopped'];
export const canContinue = () => !!ctx.state && STOPPED_STATUSES.includes(status()) && !resumingNow();

/** Opens the question dialog (the same one the "waiting for you" chip opens); it used to flip a flag nobody read and scroll to an element that does not exist. */
const showAnswer = openStatusDialog;

/** I5. The install the guard refused, with the feature that waits for it ("waiting for: npm install x (feature y)"); '' when there is none. */
const waitingLine = () => { const w = list(ctx.state && ctx.state.guard_waiting)[0]; return w && w.text ? ' · ' + clip(w.text, 220) : ''; };

export function PauseBanner() {
  if (!pauseBannerVisible()) return null;
  const quarantined = list(ctx.state.quarantine), first = quarantined[0], awaiting = status() === 'awaiting_user';
  const paused = awaiting || !!(pipe.data && pipe.data.paused);
  if (!first && !awaiting) {
    return html`<div class="z-pause" role="status">
      <span class="z-pause-kick mono">STOPPED</span>
      <div class="z-pause-text"><span>${clip(firstLine(loop().reason), REASON_CLIP) || 'The loop is stopped.'}</span><span class="z-pause-dim">${waitingLine() || ' · fix what stopped it, then continue'}</span></div>
      <button type="button" class="z-pause-btn" id="btn-continue" disabled=${!!ui.busy.answer} title="relaunches the loop with the same command, from where it stopped" onClick=${() => sendResume('resume request')}>Continue</button></div>`;
  }
  if (!first) {
    return html`<div class="z-pause" role="status">
      <span class="z-pause-kick mono">NEEDS YOU</span>
      <div class="z-pause-text"><span>${clip(firstLine(loop().reason), REASON_CLIP)}</span><span class="z-pause-dim">${waitingLine() || ' · loop paused'}</span></div>
      <button type="button" class="z-pause-btn" onClick=${showAnswer}>Answer</button></div>`;
  }
  const more = quarantined.length - 1;
  return html`<div class="z-pause" role="status">
    <span class="z-pause-kick mono">NEEDS YOU</span>
    <div class="z-pause-text"><${FeatureLabel} slug=${first.id} className="z-pause-slug" /> in quarantine<span class="z-pause-dim">${' · ' + (clip(first.reason, REASON_CLIP) || DASH)}${paused ? ' · loop paused' : ''}${more > 0 ? ` · +${more} more` : ''}</span></div>
    <button type="button" class="z-pause-btn" onClick=${() => openPauseDialog(first.id)}>Understand and resolve</button>
  </div>`;
}
