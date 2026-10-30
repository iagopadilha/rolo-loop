/* Theme: the user's choice is 'light' | 'dark' | 'system' (localStorage 'maestro.theme', stored raw); the applied theme is always
   'light' or 'dark' on <html data-theme>. index.html applies it before the first paint with the same rule (no flash). */
const KEY = 'maestro.theme';
const MODES = ['light', 'dark', 'system'];
const listeners = new Set();
const query = typeof window.matchMedia === 'function' ? window.matchMedia('(prefers-color-scheme: dark)') : null;

function readMode() {
  try { const v = localStorage.getItem(KEY); return MODES.includes(v) ? v : 'system'; } catch (_) { return 'system'; }
}
let mode = readMode();

const resolve = (m) => (m === 'system' ? (query && query.matches ? 'dark' : 'light') : m);

function apply() {
  document.documentElement.setAttribute('data-theme', resolve(mode));
  for (const fn of [...listeners]) { try { fn(mode); } catch (_) { listeners.delete(fn); } }
}

/** The user's choice: 'light' | 'dark' | 'system'. */
export const getTheme = () => mode;
export function setTheme(next) {
  if (!MODES.includes(next)) return;
  mode = next;
  try { localStorage.setItem(KEY, next); } catch (_) { /* storage blocked: the choice lasts until reload */ }
  apply();
}

/** Calls `fn(mode)` after every change (including the OS changing while the mode is 'system'). Returns the unsubscribe. */
export function onThemeChange(fn) { listeners.add(fn); return () => listeners.delete(fn); }

if (query) {
  const follow = () => { if (mode === 'system') apply(); };
  if (query.addEventListener) query.addEventListener('change', follow); else if (query.addListener) query.addListener(follow);
}
apply();
