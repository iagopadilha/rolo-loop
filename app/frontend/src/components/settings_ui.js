/* Primitives of the Settings page: ONE implementation of the inset-grouped look (group = title + rounded card, row = label and
   description on the left, control on the right, hairline between rows). Every section uses these; styles/v2/settings.css. */
import { html, useState, useRef, useEffect } from '../util/preact.js';

/** A titled card holding rows. `desc` is one calm line under the title. */
export function SettingsGroup({ title, desc, id, children }) {
  return html`<section class="st-group" id=${id} aria-labelledby=${id && title ? id + '-h' : undefined}>
    ${title ? html`<h3 class="st-group-h" id=${id ? id + '-h' : undefined}>${title}</h3>` : null}
    ${desc ? html`<p class="st-group-d">${desc}</p>` : null}
    <div class="st-card">${children}</div>
  </section>`;
}

/** One row: label + description left, control right (`children`). `stack` puts the control under the text, full width.
    `tags` = [{text, tone, title}] small inline notes after the label; `extra` = content under the row text (hints, errors). */
export function SettingsRow({ label, desc, labelFor, labelId, tags, stack = false, extra, rowKey, children }) {
  return html`<div class=${'st-row' + (stack ? ' is-stack' : '')} data-key=${rowKey}>
    <div class="st-text">
      <div class="st-label-line"><label class="st-label" id=${labelId} for=${labelFor}>${label}</label>
        ${(tags || []).map((t) => html`<span key=${t.text} class=${'st-tag' + (t.tone ? ' is-' + t.tone : '')} title=${t.title}>${t.text}</span>`)}</div>
      ${desc ? html`<p class="st-desc">${desc}</p>` : null}
      ${extra}
    </div>
    ${children ? html`<div class="st-control">${children}</div>` : null}
  </div>`;
}

/** A row whose content is free (a table, a list): same padding and separators, no label column. */
export function SettingsBlock({ children, className = '' }) {
  return html`<div class=${'st-row st-block ' + className}>${children}</div>`;
}

/** On/off switch (a checkbox with role=switch, so the keyboard and screen readers behave). */
export function Toggle({ id, checked, disabled, onChange, label }) {
  return html`<label class="st-switch"><input type="checkbox" role="switch" id=${id} checked=${!!checked} disabled=${disabled} aria-label=${label}
    onChange=${(e) => onChange(e.currentTarget.checked)} /><span class="st-switch-track" aria-hidden="true"></span></label>`;
}

export function SelectField({ id, value, options, disabled, onChange, label, className = '' }) {
  return html`<select id=${id} class=${'st-input ' + className} value=${value} disabled=${disabled} aria-label=${label} onChange=${(e) => onChange(e.currentTarget.value)}>
    ${options.map((o) => html`<option key=${o.value} value=${o.value} selected=${o.value === value}>${o.label}</option>`)}</select>`;
}

export function TextField({ id, value, disabled, onInput, placeholder, mono = false, type = 'text', numeric = false, step, label, className = '' }) {
  return html`<input id=${id} class=${'st-input' + (mono ? ' is-mono' : '') + ' ' + className} type=${numeric ? 'number' : type} min=${numeric ? '0' : undefined}
    step=${step} autocomplete="off" spellcheck="false" disabled=${disabled} value=${value} placeholder=${placeholder} aria-label=${label}
    onInput=${(e) => onInput(e.currentTarget.value)} />`;
}

export function TextArea({ id, value, disabled, onInput, rows = 3, placeholder, label }) {
  return html`<textarea id=${id} class="st-input st-area is-mono" rows=${rows} spellcheck="false" disabled=${disabled} value=${value} placeholder=${placeholder}
    aria-label=${label} onInput=${(e) => onInput(e.currentTarget.value)} />`;
}

/** Small inline message under a row: kind = hint | warn | err. */
export function Note({ kind = 'hint', id, children }) {
  return html`<div class=${'st-note is-' + kind} id=${id} role=${kind === 'err' ? 'alert' : undefined}>${children}</div>`;
}

/** Inline "Saved" confirmation: `const [saved, flash] = useSavedFlash(); flash()` shows "Saved" for 2 s. */
export function useSavedFlash() {
  const [on, setOn] = useState(false);
  const timer = useRef(0);
  useEffect(() => () => clearTimeout(timer.current), []);
  const flash = () => { setOn(true); clearTimeout(timer.current); timer.current = setTimeout(() => setOn(false), 2000); };
  return [on, flash];
}
export function SavedMark({ show }) {
  return html`<span class=${'st-saved' + (show ? ' is-on' : '')} role="status" aria-live="polite">${show ? 'Saved' : ''}</span>`;
}

export function Loading({ text }) {
  return html`<div class="st-loading" role="status"><div class="spinner" aria-hidden="true"></div><span>${text}</span></div>`;
}
