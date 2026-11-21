/* macOS-style window controls of the desktop app: the three traffic lights (close, minimize, zoom) and the sidebar toggle.
   Rendered only inside the desktop app (util/desktop.js); null in a browser. Where they show (sidebar band or page header) is decided by
   styles/v2/desktop.css from the window width and the sidebar state, so both places render them and only one is visible. */
import { html, useState, useEffect } from '../util/preact.js';
import { isDesktop, minimizeWindow, toggleZoom, closeWindow, toggleSidebar, sidebarState, onSidebarChange } from '../util/desktop.js';

const GLYPHS = {
  close: html`<path d="M2.6 2.6l2.8 2.8M5.4 2.6L2.6 5.4" stroke-width="1.1" stroke-linecap="round" />`,
  min: html`<path d="M2.2 4h3.6" stroke-width="1.1" stroke-linecap="round" />`,
  zoom: html`<path d="M2.3 5.7V2.3h3.4z M5.7 2.3v3.4H2.3z" stroke="none" fill="currentColor" />`,
};
const LIGHTS = [['close', 'Close window', closeWindow], ['min', 'Minimize', minimizeWindow], ['zoom', 'Zoom', toggleZoom]];

/** where: 'side' | 'head' (a class for the CSS placement rules). */
export function TrafficLights({ where }) {
  if (!isDesktop()) return null;
  return html`<div class=${'tl-group tl-in-' + where} role="group" aria-label="Window">
    ${LIGHTS.map(([kind, label, action]) => html`<button key=${kind} type="button" class=${'tl tl-' + kind} aria-label=${label} title=${label} onClick=${action}>
      <svg width="8" height="8" viewBox="0 0 8 8" aria-hidden="true" focusable="false" fill="none" stroke="currentColor">${GLYPHS[kind]}</svg></button>`)}
  </div>`;
}

export function SidebarToggle({ where }) {
  const [state, setState] = useState(sidebarState());
  useEffect(() => onSidebarChange(setState), []);
  if (!isDesktop()) return null;
  const label = state === 'collapsed' ? 'Show the sidebar' : 'Hide the sidebar';
  return html`<button type="button" class=${'tl-toggle tl-toggle-' + where} aria-label=${label} title=${label + ' (Ctrl+Alt+S)'} aria-pressed=${String(state === 'open')}
    aria-controls="shell-sidebar" aria-keyshortcuts="Control+Alt+S" onClick=${toggleSidebar}>
    <svg class="sh-icon" width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round">
      <rect x="1.5" y="2.5" width="13" height="11" rx="2.5" /><path d="M6 2.5v11" /></svg></button>`;
}
