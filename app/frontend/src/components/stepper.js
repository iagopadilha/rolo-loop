/* Stepper: the one step-by-step list every guided flow uses (finish a loop, set something up, start a loop). Generic and data driven:
   the caller passes `steps` = [{ id, title, what, status, result, action }] and nothing else.
   status: 'done' | 'active' | 'todo' | 'error' | 'skipped'. Only the first step that is not done/skipped is "now": it is the only one with a
   button, so the order never has two things to press. `what` says in one line what the step does, `result` what happened (the real reason
   when it failed), `action` = { label, onClick, disabled, hint }. Never put a secret in `result`: the text is shown as is. */
import { html } from '../util/preact.js';

const MARK = { done: '✓', error: '!', skipped: '–', active: '', todo: '' };
const WORD = { done: 'done', error: 'needs attention', skipped: 'skipped', active: 'now', todo: 'waiting' };

/** Index of the step that is "now" (the first one not done or skipped), or -1 when every step is finished. */
export function currentStep(steps) {
  return steps.findIndex((step) => step.status !== 'done' && step.status !== 'skipped');
}

export function Stepper({ steps, label = 'Steps' }) {
  const now = currentStep(steps);
  return html`<ol class="stp" aria-label=${label}>${steps.map((step, index) => {
    const state = index === now && step.status === 'todo' ? 'active' : step.status;
    const isNow = index === now;
    return html`<li key=${step.id || index} class=${'stp-step is-' + state + (isNow ? ' is-now' : '')} aria-current=${isNow ? 'step' : undefined}>
      <span class="stp-mark mono" aria-hidden="true">${MARK[state] || index + 1}</span>
      <div class="stp-body">
        <div class="stp-head"><span class="stp-title">${step.title}</span><span class="stp-state mono">${WORD[state] || state}</span></div>
        ${step.what ? html`<div class="stp-what">${step.what}</div>` : null}
        ${step.result ? html`<div class=${'stp-result' + (state === 'error' ? ' is-bad' : '')} role=${state === 'error' ? 'alert' : undefined}>${step.result}</div>` : null}
        ${isNow && step.action ? html`<div class="stp-action"><button type="button" class="btn btn-sm btn-primary" disabled=${!!step.action.disabled}
          onClick=${step.action.onClick}>${step.action.label}</button>${step.action.hint ? html`<span class="hint">${step.action.hint}</span>` : null}</div>` : null}
      </div></li>`;
  })}</ol>`;
}
