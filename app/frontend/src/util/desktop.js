/* Desktop app (Tauri) window chrome. In the desktop app the window has no title bar: the page draws the macOS-style traffic lights,
   moves the window when the top band is dragged ([data-drag] areas, outside controls), zooms it on a double-click there, greys the
   lights while the window is inactive and lets the sidebar collapse (Ctrl+Alt+S, kept in localStorage `maestro.sidebar`).
   In a normal browser (`harness serve`) every export is inert and nothing changes. The only IPC used is the window API the desktop
   capability grants (crates/desktop/capabilities/panel.json): minimize, toggle maximize, close, start dragging. */

const SIDEBAR_KEY = 'maestro.sidebar';
const INTERACTIVE = 'button, a[href], input, select, textarea, label, summary, [role="button"], [role="menu"], [contenteditable], [data-no-drag]';
const subscribers = new Set();

function currentWindow() {
  const api = window.__TAURI__ && window.__TAURI__.window;
  return api && typeof api.getCurrentWindow === 'function' ? api.getCurrentWindow() : null;
}

/** True inside the desktop app. */
export const isDesktop = () => !!currentWindow();

const run = (action) => { const w = currentWindow(); if (w) w[action]().catch(() => { /* the window is going away */ }); };
export const minimizeWindow = () => run('minimize');
export const toggleZoom = () => run('toggleMaximize');
export const closeWindow = () => run('close');

function readSidebar() {
  try { return localStorage.getItem(SIDEBAR_KEY) === 'collapsed' ? 'collapsed' : 'open'; } catch (_) { return 'open'; }
}
/** 'open' | 'collapsed' (desktop only; the browser panel always shows its sidebar). */
export const sidebarState = () => document.documentElement.getAttribute('data-side') || 'open';
export function toggleSidebar() {
  const next = sidebarState() === 'collapsed' ? 'open' : 'collapsed';
  document.documentElement.setAttribute('data-side', next);
  try { localStorage.setItem(SIDEBAR_KEY, next); } catch (_) { /* private mode: the choice lasts for this page */ }
  for (const fn of [...subscribers]) fn(next);
}
export function onSidebarChange(fn) { subscribers.add(fn); return () => subscribers.delete(fn); }

function onPointerDown(e) {
  if (e.button !== 0 || !e.target.closest || !e.target.closest('[data-drag]') || e.target.closest(INTERACTIVE)) return;
  const w = currentWindow();
  if (!w) return;
  e.preventDefault();
  if (e.detail === 2) w.toggleMaximize().catch(() => {});
  else w.startDragging().catch(() => {});
}

/** Marks the page as desktop (html[data-desktop]) and wires the window chrome. Call once, before the first render. */
export function initDesktop() {
  if (!isDesktop()) return;
  const root = document.documentElement;
  root.setAttribute('data-desktop', '');
  root.setAttribute('data-side', readSidebar());
  const active = (on) => { if (on) root.removeAttribute('data-window-inactive'); else root.setAttribute('data-window-inactive', ''); };
  window.addEventListener('focus', () => active(true));
  window.addEventListener('blur', () => active(false));
  active(document.hasFocus());
  document.addEventListener('mousedown', onPointerDown);
  document.addEventListener('keydown', (e) => {
    if (e.ctrlKey && e.altKey && !e.metaKey && e.key.toLowerCase() === 's') { e.preventDefault(); toggleSidebar(); }
  });
}
