/* UI v2 shell (spec 02): the sidebar (248px; icons only from 900 to 1179px; a drawer under 900px), the header frame with breadcrumb and
   theme toggle, the Ctrl+K quick switcher, and the splash. Presentational: the page (hub.js, panel/app.js) passes the real data
   (project, nav items, account, actions) and the handlers; nothing here is a demo string.
   Used by the hub (/) and the project panel (/p/<id>/). Styles: styles/v2/shell.css. */
import { html, Fragment, useState, useEffect, useRef } from '../util/preact.js';
import { list, maskHome, txt } from '../util/format.js';
import { appApi } from '../api/api_client.js';
import { getTheme, setTheme, onThemeChange } from '../util/theme.js';
import { navigate, currentRoute } from '../pages/panel/router.js';

const openSettings = () => navigate('/settings');

// ---------- tiny store shared by every shell component of the page (drawer, switcher, projects) ----------
const bus = { drawer: false, switcher: false, projects: null, subs: new Set(), opener: null };
const emit = () => { for (const fn of [...bus.subs]) fn(); };
function useBus() {
  const [, setTick] = useState(0);
  useEffect(() => { const fn = () => setTick((n) => n + 1); bus.subs.add(fn); return () => bus.subs.delete(fn); }, []);
  return bus;
}

function toggleDrawer() { bus.drawer = !bus.drawer; emit(); }
function closeDrawer() { if (bus.drawer) { bus.drawer = false; emit(); } }
function openSwitcher() {
  if (bus.switcher) return;
  bus.opener = document.activeElement;
  bus.switcher = true; emit();
  refreshProjects();
}
function closeSwitcher() {
  if (!bus.switcher) return;
  bus.switcher = false; emit();
  const back = bus.opener; bus.opener = null;
  if (back && back.focus && document.contains(back)) back.focus();
}

/** The hub feeds the list it already polls; the panel lets the sidebar read GET /api/app/projects itself. */
export function setShellProjects(projects) { bus.projects = list(projects); emit(); }
async function refreshProjects() {
  try { const r = await appApi.get('projects', 10000); setShellProjects(r && r.projects); } catch (_) { /* the list stays as it was */ }
}

// ---------- icons (16px grid, stroke follows currentColor) ----------
const ICONS = {
  folder: ['M2 4.5h4l1.5 1.5H14v6.5H2z'],
  bell: ['M4 11V7a4 4 0 0 1 8 0v4l1 1.5H3z', 'M6.5 14h3'],
  search: [{ c: [7, 7, 4.5] }, 'M10.5 10.5 L14 14'],
  loop: ['M2 4h7', 'M5 8h8', 'M3 12h6'],
  features: ['M6 4h8', 'M6 8h8', 'M6 12h8', 'M2.5 4h.01', 'M2.5 8h.01', 'M2.5 12h.01'],
  decisions: [{ c: [8, 8, 6] }, 'M5.5 8.2l1.7 1.7 3.3-3.6'],
  file: ['M4 2h5l3 3v9H4z', 'M9 2v3h3'],
  alert: ['M8 2.5l6 11H2z', 'M8 7v3', 'M8 12h.01'],
  settings: ['M2 4h12', 'M2 8h12', 'M2 12h12', { c: [5, 4, 1.6], fill: true }, { c: [10, 8, 1.6], fill: true }, { c: [7, 12, 1.6], fill: true }],
  plan: ['M3 13l1-3 7-7 2 2-7 7z', 'M10 4l2 2'],
  panel: [{ r: [1.5, 2.5, 13, 11, 2.5] }, 'M10 2.5v11'],
  menu: ['M2 4h12', 'M2 8h12', 'M2 12h12'],
  sun: [{ c: [8, 8, 3] }, 'M8 1.5v1.5', 'M8 13v1.5', 'M1.5 8H3', 'M13 8h1.5', 'M3.4 3.4l1 1', 'M11.6 11.6l1 1', 'M3.4 12.6l1-1', 'M11.6 4.4l1-1'],
  moon: ['M13 9.5A5.5 5.5 0 0 1 6.5 3a5.5 5.5 0 1 0 6.5 6.5z'],
  system: [{ r: [1.5, 2.5, 13, 8.5, 1.5] }, 'M5.5 14h5', 'M8 11v3'],
  dots: [{ c: [3.5, 8, 0.8], fill: true }, { c: [8, 8, 0.8], fill: true }, { c: [12.5, 8, 0.8], fill: true }],
};
export function Icon({ name, size = 16 }) {
  const parts = (ICONS[name] || []).map((p, i) => {
    if (typeof p === 'string') return html`<path key=${i} d=${p} />`;
    if (p.c) return html`<circle key=${i} cx=${p.c[0]} cy=${p.c[1]} r=${p.c[2]} style=${p.fill ? { fill: 'currentColor' } : null} />`;
    return html`<rect key=${i} x=${p.r[0]} y=${p.r[1]} width=${p.r[2]} height=${p.r[3]} rx=${p.r[4]} />`;
  });
  return html`<svg class="sh-icon" width=${size} height=${size} viewBox="0 0 16 16" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">${parts}</svg>`;
}

/** The conductor mark: a tile in the foreground color with the bust cut out in the background color. */
function Logo({ size = 32 }) {
  return html`<span class="sh-logo" style=${{ width: size + 'px', height: size + 'px', flexBasis: size + 'px', borderRadius: Math.round(size / 4) + 'px' }}>
    <svg viewBox="0 0 100 100" width=${size} height=${size} aria-hidden="true" focusable="false">
      <path d="M14 100 C14 82 26 72 42 70 L58 70 C66 71 72 74 76 80 L76 100 Z" class="sh-logo-ink" />
      <path d="M44 54 L56 54 L57 71 L43 71 Z" class="sh-logo-ink" />
      <path d="M32 36 C30 20 42 10 56 11 C68 12 74 22 73 32 L82 39 L73 42 C72 52 64 58 54 58 C41 58 33 49 32 36 Z" class="sh-logo-ink" />
      <path d="M33 30 C26 16 40 2 60 7 C50 8 44 13 42 21 Z" class="sh-logo-ink" />
      <path d="M70 82 L86 56" class="sh-logo-stroke" stroke-width="10" stroke-linecap="round" fill="none" />
      <path d="M87 53 L97 20" class="sh-logo-stroke" stroke-width="2.4" stroke-linecap="round" fill="none" />
      <circle cx="97" cy="20" r="4" class="sh-logo-dot" />
    </svg></span>`;
}

// ---------- status dot of a loop status (hub rows, sidebar) ----------
/** Loop status or agent/step state -> dot kind. 'live' = running; 'wait' = it needs the user; 'ok' | 'bad' | 'idle'. */
export function dotKind(value) {
  const state = String(value || '').toLowerCase();
  if (state === 'running' || state === 'continue') return 'live';
  if (state === 'awaiting_user') return 'wait';
  if (state === 'done' || state === 'verified') return 'ok';
  if (state === 'error' || state === 'failed' || state === 'fail' || state === 'blocked') return 'bad';
  return 'idle';
}
export const StatusDot = ({ kind }) => html`<span class=${'sh-dot is-' + (kind || 'idle')} aria-hidden="true"></span>`;

// ---------- sidebar ----------
const isMac = /Mac|iPhone|iPad/i.test(navigator.platform || '');
const SHORTCUT = isMac ? '⌘K' : 'Ctrl K';

function NavItem({ icon, label, count, current, href, onClick, id, tone, dot }) {
  const cls = 'sh-item' + (current ? ' is-current' : '') + (tone ? ' is-' + tone : '');
  const inner = html`${dot ? html`<${StatusDot} kind=${dot} />` : icon ? html`<${Icon} name=${icon} />` : null}
    <span class="sh-label sh-grow">${label}</span>${count != null && count !== '' ? html`<span class=${'sh-count' + (tone === 'alert' ? ' is-alert' : '')}>${count}</span>` : null}`;
  const common = { class: cls, id, title: label, 'aria-current': current ? 'page' : null };
  if (href) return html`<a ...${common} href=${href} onClick=${closeDrawer}>${inner}</a>`;
  return html`<button type="button" ...${common} onClick=${(e) => { closeDrawer(); if (onClick) onClick(e); }}>${inner}</button>`;
}

const initialsOf = (name) => (String(name || '').replace(/[^A-Za-z0-9 ._-]/g, '').split(/[ ._-]+/).filter(Boolean).slice(0, 2).map((w) => w[0]).join('') || '?').toUpperCase();

/** props: page 'hub' | 'panel'; project {id, name, kind, items:[{id, icon, label, count, onClick}], current}; account {name, sub};
    activity {count, onClick} | null; commands [{label, hint, run}] extra quick-switcher entries. */
export function Sidebar({ page, project, account, activity, commands }) {
  const state = useBus();
  useEffect(() => {
    if (bus.projects == null) refreshProjects();
    if (page !== 'panel') return undefined;
    const timer = setInterval(() => { if (document.visibilityState === 'visible') refreshProjects(); }, 15000);
    return () => clearInterval(timer);
  }, [page]);
  useEffect(() => {
    const onKey = (e) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 'k') { e.preventDefault(); if (bus.switcher) closeSwitcher(); else openSwitcher(); return; }
      if (e.key === 'Escape' && bus.drawer && !bus.switcher && !document.querySelector('dialog[open]')) closeDrawer();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);

  const projects = state.projects || [];
  const others = projects.filter((p) => !project || p.id !== project.id)
    .sort((a, b) => String(b.last_opened_at || '').localeCompare(String(a.last_opened_at || ''))).slice(0, 5);
  return html`<${Fragment}>
    <div class=${'sh-scrim' + (state.drawer ? ' is-on' : '')} onClick=${closeDrawer} aria-hidden="true"></div>
    <aside class=${'sh-side' + (state.drawer ? ' is-open' : '')} id="shell-sidebar" aria-label="Sidebar">
      <a class="sh-brand" href="/" title="Projects" aria-label="Agents Maestro: back to the projects">
        <${Logo} /><span class="sh-brand-text"><span class="sh-brand-name">Agents Maestro</span><span class="sh-brand-sub">Deterministic AI Harness</span></span></a>
      <button type="button" class="sh-search" id="btn-search" onClick=${openSwitcher} aria-keyshortcuts="Control+K Meta+K" title=${'Search projects and pages (' + SHORTCUT + ')'}>
        <${Icon} name="search" size=${14} /><span class="sh-label sh-grow">Search</span><kbd class="sh-kbd sh-label">${SHORTCUT}</kbd></button>
      <nav class="sh-nav" aria-label="Workspace">
        <${NavItem} icon="folder" label="Projects" href="/" current=${page === 'hub' && currentRoute().name !== 'settings'} />
        ${activity ? html`<${NavItem} icon="bell" label="Activity" count=${activity.count || ''} tone=${activity.count ? 'alert' : null} onClick=${activity.onClick} />` : null}
      </nav>
      ${project ? html`<nav class="sh-nav" aria-label=${project.name || 'Project'}>
        <span class="sh-group" title=${project.name}><${StatusDot} kind=${project.dot} /><span class="sh-label sh-group-name">${project.name}</span></span>
        ${list(project.items).map((it) => html`<${NavItem} key=${it.id} icon=${it.icon} label=${it.label} count=${it.count} current=${project.current === it.id} onClick=${it.onClick} />`)}
      </nav>` : null}
      ${others.length ? html`<nav class="sh-nav sh-recent" aria-label=${project ? 'Other projects' : 'Recent projects'}>
        <span class="sh-group"><span class="sh-label sh-group-name">${project ? 'Projects' : 'Recent'}</span></span>
        ${others.map((p) => html`<${NavItem} key=${p.id} dot=${dotKind((p.progress_summary || {}).loop_status)} label=${txt(p.name || p.id)} href=${p.url || `/p/${encodeURIComponent(p.id)}/`} />`)}
      </nav>` : null}
      <span class="sh-grow"></span>
      <${NavItem} icon="settings" label="Settings" id="btn-settings" current=${currentRoute().name === 'settings'} onClick=${openSettings} />
      ${account && account.name ? html`<${account.onClick ? 'button' : 'div'} type=${account.onClick ? 'button' : null} class=${'sh-account' + (account.onClick ? ' is-action' : '')} id="sh-account"
        title=${[account.name, account.sub].filter(Boolean).join(' · ') + (account.onClick ? ' (account and usage)' : '')} onClick=${account.onClick ? () => { closeDrawer(); account.onClick(); } : null}>
        <span class="sh-avatar" aria-hidden="true">${initialsOf(account.name)}</span>
        <span class="sh-account-text"><span class="sh-account-name">${account.name}</span>${account.sub ? html`<span class="sh-account-sub">${account.sub}</span>` : null}</span><//>` : null}
    </aside>
    ${state.switcher ? html`<${QuickSwitcher} projects=${projects} current=${project} commands=${list(commands)} />` : null}
  <//>`;
}

// ---------- quick switcher (Ctrl+K) ----------
function QuickSwitcher({ projects, current, commands }) {
  const [query, setQuery] = useState('');
  const [index, setIndex] = useState(0);
  const input = useRef(null);
  useEffect(() => { if (input.current) input.current.focus(); }, []);

  const entries = [
    ...commands.map((c) => ({ key: 'c-' + c.label, group: current ? current.name || 'Project' : 'Go to', label: c.label, hint: c.hint || '', run: c.run })),
    ...projects.map((p) => ({ key: 'p-' + p.id, group: 'Projects', label: txt(p.name || p.id), hint: maskHome(p.repo_path || ''), href: p.url || `/p/${encodeURIComponent(p.id)}/` })),
    { key: 'a-settings', group: 'App', label: 'Settings', hint: 'open the settings', run: openSettings },
  ];
  const q = query.trim().toLowerCase();
  const shown = q ? entries.filter((e) => (e.label + ' ' + e.hint).toLowerCase().includes(q)) : entries;
  const at = Math.min(index, Math.max(0, shown.length - 1));

  const choose = (entry) => {
    if (!entry) return;
    closeSwitcher();
    if (entry.href) location.href = entry.href; else if (entry.run) setTimeout(entry.run, 0);
  };
  const onKey = (e) => {
    if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeSwitcher(); }
    else if (e.key === 'ArrowDown') { e.preventDefault(); setIndex((at + 1) % Math.max(1, shown.length)); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setIndex((at - 1 + shown.length) % Math.max(1, shown.length)); }
    else if (e.key === 'Home') { e.preventDefault(); setIndex(0); }
    else if (e.key === 'End') { e.preventDefault(); setIndex(Math.max(0, shown.length - 1)); }
    else if (e.key === 'Enter') { e.preventDefault(); choose(shown[at]); }
    else if (e.key === 'Tab') e.preventDefault(); // the field is the only stop: the list follows the arrows
  };
  useEffect(() => { const el = document.getElementById('sh-sw-opt-' + at); if (el && el.scrollIntoView) el.scrollIntoView({ block: 'nearest' }); }, [at, query]);
  let lastGroup = null;
  return html`<div class="sh-sw-back" onMouseDown=${(e) => { if (e.target === e.currentTarget) closeSwitcher(); }}>
    <div class="sh-sw" role="dialog" aria-modal="true" aria-label="Quick switcher" onKeyDown=${onKey}>
      <div class="sh-sw-bar"><${Icon} name="search" size=${14} />
        <input ref=${input} class="sh-sw-input" type="text" role="combobox" aria-expanded="true" aria-controls="sh-sw-list" aria-activedescendant=${shown.length ? 'sh-sw-opt-' + at : null}
          aria-label="Search projects and pages" placeholder="Search projects and pages…" autocomplete="off" spellcheck="false" value=${query}
          onInput=${(e) => { setQuery(e.currentTarget.value); setIndex(0); }} /><kbd class="sh-kbd">Esc</kbd></div>
      <ul class="sh-sw-list" id="sh-sw-list" role="listbox" aria-label="Results">
        ${shown.length ? shown.map((entry, i) => {
          const head = entry.group !== lastGroup ? html`<li role="presentation" class="sh-sw-group">${entry.group}</li>` : null;
          lastGroup = entry.group;
          return html`<${Fragment} key=${entry.key}>${head}<li role="option" id=${'sh-sw-opt-' + i} class=${'sh-sw-opt' + (i === at ? ' is-on' : '')} aria-selected=${String(i === at)}
            onMouseMove=${() => { if (i !== at) setIndex(i); }} onClick=${() => choose(entry)}>
            <span class="sh-sw-label">${entry.label}</span>${entry.hint ? html`<span class="sh-sw-hint" title=${entry.hint}>${entry.hint}</span>` : null}</li><//>`;
        }) : html`<li role="presentation" class="sh-sw-empty">Nothing matches "${query}".</li>`}
      </ul>
    </div></div>`;
}

// ---------- theme toggle ----------
const THEME_CHOICES = [['light', 'sun', 'Light theme'], ['dark', 'moon', 'Dark theme'], ['system', 'system', 'Follow the system theme']];
function ThemeToggle() {
  const [mode, setMode] = useState(getTheme());
  useEffect(() => onThemeChange(setMode), []);
  return html`<div class="sh-seg sh-theme" role="group" aria-label="Theme">${THEME_CHOICES.map(([value, icon, label]) => html`<button key=${value} type="button" class=${'sh-seg-btn' + (mode === value ? ' is-on' : '')}
    aria-pressed=${String(mode === value)} aria-label=${label} title=${label} data-theme-choice=${value} onClick=${() => setTheme(value)}><${Icon} name=${icon} size=${14} /></button>`)}</div>`;
}

// ---------- header ----------
const COMPACT_QUERY = '(max-width: 1179px)';
/** True under the 1180px breakpoint of shell.css: the header keeps the status and the primary action and moves the rest to the "⋯" menu. */
function useCompact() {
  const mq = typeof matchMedia === 'function' ? matchMedia(COMPACT_QUERY) : null;
  const [compact, setCompact] = useState(!!(mq && mq.matches));
  useEffect(() => {
    if (!mq) return undefined;
    const fn = () => setCompact(mq.matches);
    fn();
    if (mq.addEventListener) mq.addEventListener('change', fn); else mq.addListener(fn);
    return () => { if (mq.removeEventListener) mq.removeEventListener('change', fn); else mq.removeListener(fn); };
  }, []);
  return compact;
}

const MENU_ITEMS = 'button:not(:disabled):not([hidden]):not([data-theme-choice]), a[href]';
/** Overflow "⋯" menu of the compact header. The children are the real controls (same ids and handlers); the menu stays mounted
    (hidden while closed) so dialogs that remember their opener and lookups by id keep working. The theme row (compact only) closes the list. */
export function OverflowMenu({ children, label = 'More actions', alert = false, theme = true }) {
  const [open, setOpen] = useState(false);
  const wrap = useRef(null), trigger = useRef(null), menu = useRef(null), pending = useRef(null);
  const items = () => [...(menu.current ? menu.current.querySelectorAll(MENU_ITEMS) : [])];
  const close = (back = true) => { setOpen(false); if (back && trigger.current) trigger.current.focus(); };
  const show = (which) => { pending.current = which; setOpen(true); };
  useEffect(() => {
    if (!open) return undefined;
    const list_ = items();
    if (pending.current != null && list_.length) (pending.current === 'last' ? list_[list_.length - 1] : list_[0]).focus();
    pending.current = null;
    const away = (e) => { if (wrap.current && !wrap.current.contains(e.target)) setOpen(false); };
    document.addEventListener('pointerdown', away);
    return () => document.removeEventListener('pointerdown', away);
  }, [open]);
  const onTrigger = (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); show('first'); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); show('last'); }
    else if (e.key === 'Escape' && open) { e.preventDefault(); close(); }
  };
  const onMenu = (e) => {
    const list_ = items(), at = list_.indexOf(document.activeElement);
    if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); }
    else if (e.key === 'Tab') setOpen(false);
    else if (e.key === 'ArrowDown') { e.preventDefault(); if (list_.length) list_[(at + 1) % list_.length].focus(); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); if (list_.length) list_[(at - 1 + list_.length) % list_.length].focus(); }
    else if (e.key === 'Home') { e.preventDefault(); if (list_.length) list_[0].focus(); }
    else if (e.key === 'End') { e.preventDefault(); if (list_.length) list_[list_.length - 1].focus(); }
  };
  // Capture: the menu closes and focus goes back to "⋯" before the real handler runs, so a dialog it opens remembers a live opener.
  const onPick = (e) => { if (e.target.closest && e.target.closest('button,a[href]') && !e.target.closest('[data-theme-choice]')) close(); };
  return html`<div class="sh-more" ref=${wrap}>
    <button type="button" class=${'sh-more-btn' + (alert ? ' has-alert' : '')} id="btn-more" ref=${trigger} aria-haspopup="menu" aria-expanded=${String(open)} aria-controls="sh-menu"
      aria-label=${label} title=${label} onClick=${() => (open ? close() : show('first'))} onKeyDown=${onTrigger}><${Icon} name="dots" /></button>
    <div class="sh-menu" id="sh-menu" role="menu" aria-label=${label} hidden=${!open} ref=${menu} onKeyDown=${onMenu} onClickCapture=${onPick}>
      ${children}
      ${theme ? html`<div class="sh-menu-theme"><span class="sh-menu-theme-label">Theme</span><${ThemeToggle} /></div>` : null}
    </div></div>`;
}

/** crumbs: [{label, href?}] (the last one is the page); mid: status and meters; end: the actions. The theme toggle closes the row.
    mid and end may be functions of `compact` (viewport under 1180px): the compact form keeps the primary action and puts the rest in an
    <OverflowMenu>, which carries the theme toggle itself. */
export function ShellHeader({ id = 'header', cls = '', crumbs, mid, end }) {
  const state = useBus();
  const compact = useCompact();
  const items = list(crumbs);
  const midNode = typeof mid === 'function' ? mid(compact) : mid;
  const endNode = typeof end === 'function' ? end(compact) : end;
  return html`<header id=${id} class=${'sh-header ' + cls}>
    <button type="button" class="sh-burger" id="btn-menu" aria-label="Open the menu" aria-controls="shell-sidebar" aria-expanded=${String(!!state.drawer)} onClick=${toggleDrawer}><${Icon} name="menu" /></button>
    <nav class="sh-crumbs" aria-label="Breadcrumb">${items.map((c, i) => {
      const last = i === items.length - 1;
      const node = c.href && !last ? html`<a class="sh-crumb" href=${c.href} title=${c.label}>${c.label}</a>` : html`<span class=${'sh-crumb' + (last ? ' is-cur' : '')} title=${c.label} aria-current=${last ? 'page' : null}>${c.label}</span>`;
      return html`<${Fragment} key=${i}>${i ? html`<span class="sh-crumb-sep" aria-hidden="true">/</span>` : null}${node}<//>`;
    })}</nav>
    ${midNode}
    <div class="sh-end">${endNode}${compact ? null : html`<${ThemeToggle} />`}</div>
  </header>`;
}

// ---------- splash ----------
const SPLASH_MIN_MS = 300;
const splashStart = performance.now();
/** True while the first data is pending, and for at least SPLASH_MIN_MS after the page started (no flash). */
export function useSplash(ready) {
  const [, setTick] = useState(0);
  const wait = SPLASH_MIN_MS - (performance.now() - splashStart);
  useEffect(() => {
    if (!ready || wait <= 0) return undefined;
    const timer = setTimeout(() => setTick((n) => n + 1), wait + 10);
    return () => clearTimeout(timer);
  }, [ready]);
  return !ready || wait > 0;
}
/** Full-window loading screen over the page. `text` is the real phase ("Loading projects…"), never a made-up status. */
export function Splash({ text }) {
  return html`<div class="sh-splash" id="splash" role="status" aria-live="polite">
    <${Logo} size=${84} />
    <div class="sh-splash-titles"><span class="sh-splash-title">Agents Maestro</span><span class="sh-splash-sub">Deterministic AI Harness</span></div>
    <div class="sh-splash-bar" aria-hidden="true"><div></div></div>
    <span class="sh-splash-text">${text}</span></div>`;
}
