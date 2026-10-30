/* Tiny hash router shared by the hub (/) and the project panel (/p/<id>/).
   Routes: #/loop (default) #/features #/feature/<slug> #/decisions #/files #/errors #/settings[/<section>].
   `#feature=<slug>` (the first feature page address) is an alias: it is rewritten to #/feature/<slug> when read.
   No imports from the pages: the panel and the hub subscribe with onRoute() and redraw themselves.
   History: every navigate() pushes an entry that remembers how deep it is (history.state.r), so back() returns to the previous
   page of the app and, when there is none (a deep link), goes to the default route instead of leaving the app. */

const NAMES = ['loop', 'features', 'feature', 'decisions', 'files', 'errors', 'settings'];
const DEFAULT = { name: 'loop', path: '/loop', slug: null, section: null };
const listeners = new Set();
let last = null; // path last announced

const decode = (text) => { try { return decodeURIComponent(text); } catch (_) { return text; } };

/** { name, path, slug, section } of a hash string (with or without '#'). Unknown or empty = the loop. */
function parseHash(hash) {
  let text = String(hash || '').replace(/^#/, '');
  const alias = /^feature=(.+)$/.exec(text);
  if (alias) text = '/feature/' + alias[1];
  const parts = text.split('/').filter(Boolean);
  if (!parts.length || !NAMES.includes(parts[0])) return DEFAULT;
  const name = parts[0];
  if (name === 'feature') return parts[1] ? { name, path: '/feature/' + encodeURIComponent(decode(parts[1])), slug: decode(parts[1]), section: null } : DEFAULT;
  if (name === 'settings') return { name, path: '/settings' + (parts[1] ? '/' + parts[1] : ''), slug: null, section: parts[1] || null };
  return { name, path: '/' + name, slug: null, section: null };
}

const depth = () => (history.state && Number.isInteger(history.state.r) ? history.state.r : 0);

/** The current route; rewrites the `#feature=<slug>` alias in place. */
export function currentRoute() {
  const hash = location.hash || '';
  const route = parseHash(hash);
  if (/^#feature=/.test(hash) && route !== DEFAULT) history.replaceState(history.state, '', location.pathname + location.search + '#' + route.path);
  return route;
}

function announce() {
  const route = currentRoute();
  if (last === route.path) return;
  last = route.path;
  for (const fn of [...listeners]) { try { fn(route); } catch (_) { /* a broken listener must not stop the others */ } }
}

/** Goes to `path` ('/errors', '/feature/<slug>'...). `replace: true` swaps the current history entry instead of adding one. */
export function navigate(path, { replace = false } = {}) {
  const route = parseHash(String(path || '').replace(/^#/, ''));
  const url = location.pathname + location.search + '#' + route.path;
  if (replace) history.replaceState({ r: depth() }, '', url);
  else if (currentRoute().path !== route.path) history.pushState({ r: depth() + 1 }, '', url);
  announce();
}

/** Subscribes to route changes (hashchange, back/forward, navigate). Returns the unsubscribe function. */
export function onRoute(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Previous page of the app, or the default route when this page was opened directly. */
export function back() {
  if (depth() > 0) history.back(); else navigate(DEFAULT.path, { replace: true });
}

let escapeGuard = null;
/** The page can say "Esc is mine right now" (an inline confirmation open): `fn()` true keeps Esc from going back. */
export function setEscapeGuard(fn) { escapeGuard = fn; }

/** True while something above the page owns Esc (dialog, quick switcher, open drawer or menu). */
function escapeTaken(event) {
  if (event.defaultPrevented || (escapeGuard && escapeGuard())) return true;
  return !!document.querySelector('dialog[open], .sh-sw, .sh-side.is-open, .sh-menu:not([hidden])');
}

window.addEventListener('hashchange', announce);
window.addEventListener('popstate', announce);
document.addEventListener('keydown', (event) => {
  if (event.key !== 'Escape' || escapeTaken(event) || currentRoute().name === DEFAULT.name) return;
  const target = event.target;
  if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT' || target.isContentEditable)) { target.blur(); return; }
  event.preventDefault();
  back();
});
last = currentRoute().path;
