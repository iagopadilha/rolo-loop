/* The one set of page primitives (styles: styles/v2/pages.css). A centre page is built from these and nothing else:
   Page / PageHead (title block + tools), Segmented, Section (sentence-case label), List + Row (hairline rows, hover fill, trailing chevron),
   State (status dot + word), Chip, EmptyState. The status dot itself is the shell's (StatusDot); the turn picker is the loop's (TurnPicker). */
import { html } from '../util/preact.js';
import { StatusDot } from './shell.js';

/** Header of a page: 28px title (h1), one muted line under it and the tools (buttons, switches) on the right. */
function PageHead({ title, sub, children }) {
  return html`<header class="pg-head">
    <div class="pg-titles"><h1 class="pg-title">${title}</h1>${sub ? html`<p class="pg-sub">${sub}</p>` : null}</div>
    ${children ? html`<div class="pg-tools">${children}</div>` : null}
  </header>`;
}

/** A page: <main> with the header and the body. `fill` = the body does not scroll (a child fills it and scrolls inside); `wide` = no reading-width cap. */
export function Page({ name, title, sub, tools, fill = false, wide = false, children }) {
  return html`<main class=${'pg pg-' + name + (wide ? ' is-wide' : '')} aria-label=${title}>
    <${PageHead} title=${title} sub=${sub}>${tools}<//>
    <div class=${'pg-body' + (fill ? ' is-fill' : '')}>${children}</div>
  </main>`;
}

/** Two-way switch (role=group, aria-pressed) like Timeline | Graph | List. items = [[key, label]]. */
export function Segmented({ items, value, onPick, label }) {
  return html`<div class="seg" role="group" aria-label=${label}>${items.map(([key, text]) => html`<button key=${key} type="button"
    class=${'seg-btn' + (value === key ? ' is-on' : '')} aria-pressed=${String(value === key)} onClick=${() => onPick(key)}>${text}</button>`)}</div>`;
}

/** Group label in sentence case: "Foundations", with an optional muted note on the right ("2 of 3 done"). */
export function Section({ title, note, children }) {
  return html`<section class="pg-sec"><h2 class="pg-sec-h"><span>${title}</span>${note ? html`<span class="pg-sec-note">${note}</span>` : null}</h2>${children}</section>`;
}

/** Quiet list: rows with hairline separators. `cols` = CSS grid columns of every row, `narrow` = the same under 900px (cells with class ls-opt hide there). */
export function List({ label, cols, narrow, head, children }) {
  return html`<ul class="ls" aria-label=${label} style=${{ '--cols': cols, '--cols-narrow': narrow || cols }}>
    ${head ? html`<li class="ls-head" aria-hidden="true">${head}</li>` : null}${children}</ul>`;
}

const CHEVRON = html`<svg class="ls-chev" width="14" height="14" viewBox="0 0 16 16" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M6 3 L11 8 L6 13" /></svg>`;

/** One 44px row. `href` = a link row (Enter and middle click work natively; a plain click goes through onOpen), `onOpen` alone = a button row, neither = a static row. */
export function Row({ href, onOpen, selected = false, chevron = true, title, children }) {
  const cls = 'ls-row' + (selected ? ' is-sel' : '') + (href || onOpen ? '' : ' is-static');
  const inner = html`${children}${chevron && (href || onOpen) ? CHEVRON : null}`;
  if (!href && !onOpen) return html`<li><div class=${cls} title=${title}>${inner}</div></li>`;
  return html`<li>${href
    ? html`<a class=${cls} href=${href} title=${title} aria-current=${selected ? 'true' : null}
      onClick=${(event) => { if (event.defaultPrevented || event.button || event.ctrlKey || event.metaKey || event.shiftKey) return; event.preventDefault(); onOpen(); }}>${inner}</a>`
    : html`<button type="button" class=${cls} title=${title} aria-current=${selected ? 'true' : null} onClick=${onOpen}>${inner}</button>`}</li>`;
}

/** Status of a feature / step in one vocabulary: kind = idle | live | wait | ok | bad (the shell's dot kinds). */
const STATE_DOT = { verified: 'ok', running: 'live', blocked: 'bad', queued: 'idle' };
export function State({ kind, children }) {
  return html`<span class="pg-state"><${StatusDot} kind=${STATE_DOT[kind] || kind} />${children}</span>`;
}

/** Small neutral label (a turn, a kind). tone = '' | 'accent' | 'bad'. */
export function Chip({ tone = '', title, href, onOpen, children }) {
  const cls = 'pg-chip' + (tone ? ' is-' + tone : '') + (href ? ' is-link' : '');
  return href ? html`<a class=${cls} href=${href} title=${title} onClick=${(event) => { if (onOpen && !event.defaultPrevented && !event.button && !event.ctrlKey && !event.metaKey && !event.shiftKey) { event.preventDefault(); onOpen(); } }}>${children}</a>`
    : html`<span class=${cls} title=${title}>${children}</span>`;
}

/** One calm line and, at most, one action. */
export function EmptyState({ text, action }) {
  return html`<div class="pg-empty"><p>${text}</p>${action ? html`<button type="button" class="btn btn-sm" onClick=${action.onClick}>${action.label}</button>` : null}</div>`;
}
