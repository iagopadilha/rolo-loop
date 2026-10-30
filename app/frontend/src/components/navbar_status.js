/* Presentational piece of the panel header: the status indicator (one table of texts for every loop status).
   No fetch and no panel state here: the page (pages/panel/header.js) decides the status key, the detail and what a click does. */
import { html } from '../util/preact.js';
import { isNum } from '../util/format.js';

/** The only table of indicator texts. tone picks the colors (css .nb-tone-*); attention = the user has something to do. */
export const NAVBAR_STATUS = {
  no_loop: { label: 'No loop', tone: 'muted', attention: false, tip: 'Nothing is running. Start a loop when ready.' },
  running: { label: 'Running', tone: 'accent', attention: false, pulse: true, tip: 'The loop is running. You can pause it after the current turn.' },
  paused: { label: 'Paused', tone: 'accent', attention: true, tip: 'The loop is paused and can continue. Click to see why and what to do.' },
  awaiting_user: { label: 'Waiting for you', tone: 'accent', attention: true, pulse: true, tip: 'The loop has a question for you. Click to answer it.' },
  stopped: { label: 'Stopped', tone: 'muted', attention: true, tip: 'The loop process is no longer running, but there was still work to do. Click to resume it.' },
  error: { label: 'Error', tone: 'red', attention: true, tip: 'The loop stopped with an error. Click to see it and try again.' },
  planning: { label: 'Planning', tone: 'accent', attention: false, tip: 'The roadmap is being planned. The loop starts after you approve it.' },
  done: { label: 'Done', tone: 'green', attention: false, tip: 'The loop finished. Click for the summary, or use Finish to wrap up.' },
};

/** Loop status (as in /state) -> key of NAVBAR_STATUS. `continue` is a loop that goes on, shown as running. */
export function navbarStatusKey(loopStatus) {
  if (loopStatus === 'continue') return 'running';
  return NAVBAR_STATUS[loopStatus] ? loopStatus : 'no_loop';
}

/** Status indicator. statusKey: key of NAVBAR_STATUS; detail: extra sentence (the reason) appended to the tooltip;
    turn: current turn or null; tip: replaces the table sentence; onClick: given means the indicator is a button. */
export function NavbarStatus({ statusKey, turn, detail, tip, onClick }) {
  const st = NAVBAR_STATUS[statusKey] || NAVBAR_STATUS.no_loop;
  const title = [tip || st.tip, detail].filter(Boolean).join(' ');
  const cls = `nb-status nb-tone-${st.tone}${st.attention ? ' is-attention' : ''}${onClick ? ' is-click' : ''}`;
  const inner = html`<span class=${'dot' + (st.pulse ? ' pulse' : '')} aria-hidden="true"></span>
    <span class="nb-status-text">${st.label + (isNum(turn) ? ` · Turn ${turn}` : '')}</span>`;
  return onClick
    ? html`<button type="button" class=${cls} id="navbar-status" title=${title} aria-label=${`Loop status: ${st.label}. ${title}`} onClick=${onClick}>${inner}</button>`
    : html`<div class=${cls} id="navbar-status" role="status" aria-live="polite" title=${title}>${inner}</div>`;
}
