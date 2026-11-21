/* Hub (/): the v2 projects page inside the shell (components/shell.js): a table of the projects (name + masked path, status, verified, cost,
   opened), a system strip with the diagnostics summary, open a folder (the system folder window), create a project from scratch,
   forget a project, reset its specs (GET .../reset-specs/preview, then POST .../reset-specs; deletes the loop worktree too), app diagnostics
   (GET /api/app/info), test notification and shut down the app. Every value comes from the API; every displayed path goes through maskHome. */
import { $ } from '../util/dom.js';
import { html, render, useState, useRef } from '../util/preact.js';
import { DASH, list, txt, fmtRelative, maskHome } from '../util/format.js';
import { appApi, createPolling, showDisconnectedBanner } from '../api/api_client.js';
import { toast } from '../components/toast.js';
import { openDialog } from '../components/dialog.js';
import { pickFolderNativeFirst } from '../components/folder_browser.js';
import { Conformance, conformanceDefault } from '../components/conformance.js';
import { openWhy, WhyButton } from '../components/why_panel.js';
import { currentRoute, onRoute, navigate } from './panel/router.js';
import { SettingsPage } from './settings_page.js';
import { checksOf, failText, maskIn, sendTestNotification } from './settings_system.js';
import { maybeStartTour } from '../components/tour.js';
import { Sidebar, ShellHeader, OverflowMenu, Splash, useSplash, Icon, dotKind, setShellProjects, StatusDot } from '../components/shell.js';

/** Status words of a project that has no loop (progress_summary.mode) and of a loop (progress_summary.loop_status). */
const MODE_LABEL = {
  new: 'New project', no_specs: 'No .specs', planning: 'Planning', ready_for_loop: 'Ready for the loop', in_loop: 'In loop', done: 'Done',
};
const NEXT_ACTION = {
  structure: 'initialize .specs', conformance: 'choose conformance', map: 'map the code', roadmap: 'generate the roadmap',
  approve: 'approve the roadmap', test_environment: 'describe the test environment', check: 'check the loop', start_loop: 'start the loop', follow: 'follow the loop',
};
const LOOP_LABEL = {
  running: 'Running', awaiting_user: 'Needs you', continue: 'Between turns', done: 'Done', error: 'Error', stopped: 'Stopped',
};
const INVALID_NAME = /[\\/:*?"<>|]/;
const INVALID_NAME_MSG = 'The name cannot contain \\ / : * ? " < > |';
const capital = (text) => (text ? text.charAt(0).toUpperCase() + text.slice(1) : text);

// store of the page: handlers mutate it and call draw() (Preact only touches what changed, so the focus stays)
const hb = {
  projects: null, loops: {}, error: null, info: null, infoError: null, forgetting: null, resetting: null, resetConfirm: '', resetPreview: null, busy: {}, opening: null, confirmShutdown: false, native: null,
  testing: false, shuttingDown: false, closed: false,
};
let hubRoot = null;
const draw = () => { if (hubRoot) render(html`<${Hub} />`, hubRoot); };
const focusId = (id) => { const el = document.getElementById(id); if (el) el.focus(); };
const closeMenus = () => { for (const menu of document.querySelectorAll('details.hub-menu[open]')) menu.open = false; };

// ---------- projects ----------
// A project whose progress the server is still reading (a huge folder) comes as progress_pending: ask again shortly, once.
const PENDING_RETRY_MS = 2000;
let pendingRetry = null;
async function loadProjects() {
  const r = await appApi.get('projects', 10000);
  hb.projects = list(r && r.projects); hb.error = null;
  setShellProjects(hb.projects);
  draw();
  if (!pendingRetry && hb.projects.some((p) => p.progress_pending)) {
    pendingRetry = setTimeout(() => { pendingRetry = null; loadProjects().catch(() => {}); }, PENDING_RETRY_MS);
  }
}

/** What the Status cell says: { label, kind, title }. A running loop adds its turn (from GET overview). */
function statusOf(p) {
  const ps = p.progress_summary || {}, lp = hb.loops[p.id];
  const turn = lp && lp.turn != null ? ` · T${lp.turn}` : '';
  const feats = lp ? list(lp.feature).join(', ') : '';
  if (p.folder_exists === false) return { label: 'Folder missing', kind: 'bad', title: 'The project folder is not on disk any more.' };
  if (p.progress_pending && !lp) return { label: 'Reading…', kind: 'idle', title: 'The app is still reading this project folder.' };
  const loopStatus = lp && lp.question ? 'awaiting_user' : (lp && lp.status) || ps.loop_status;
  if (LOOP_LABEL[loopStatus]) return { label: LOOP_LABEL[loopStatus] + (loopStatus === 'running' || loopStatus === 'awaiting_user' ? turn : ''), kind: dotKind(loopStatus), title: feats };
  return { label: MODE_LABEL[ps.mode] || 'No loop', kind: dotKind(ps.mode === 'done' ? 'done' : ''), title: '' };
}

/** The question a running loop is waiting on (GET /api/app/overview): the answer happens in the panel. */
function AskBlock({ p, lp }) {
  const q = lp.question, url = lp.url || p.url || `/p/${encodeURIComponent(p.id)}/`, opts = list(q && q.options);
  return html`<div class="hub-ask" role="group" aria-label=${'Question waiting in ' + txt(p.name || p.id)}>
    <div class="hub-ask-t"><strong>Waiting for you</strong><span class="dim">${q.since ? ' · ' + fmtRelative(q.since) : ''}</span></div>
    <div class="hub-ask-q">${txt(q.reason)}</div>
    ${opts.length ? html`<ul class="hub-ask-o" aria-label="Options">${opts.map((o, i) => html`<li key=${i}>${String(o)}</li>`)}</ul>` : null}
    <a class="btn btn-primary btn-sm hub-ask-a" href=${url}>Answer in the panel</a></div>`;
}
/** What the reset does to the loop worktree(s), read BEFORE confirming (GET .../reset-specs/preview). Text only: no options. */
function ResetWorktreeNotice({ preview }) {
  if (preview.state === 'loading') return html`<span class="reset-worktree dim">checking for a loop worktree…</span>`;
  if (preview.state === 'error') {
    return html`<span class="reset-worktree">Could not check for a loop worktree (${txt(preview.error)}). If one exists, it will be deleted too, with its local branch.</span>`;
  }
  if (!preview.worktrees.length) return null;
  return html`<span class="reset-worktree" role="note">${preview.worktrees.map((w) => html`<span class="reset-worktree-item" key=${w.path}>
      A loop worktree exists at <span class="mono" title=${maskHome(w.path)}>${maskHome(txt(w.path))}</span> (branch <span class="mono">${txt(w.branch)}</span>${preview.worktrees.length > 1 ? `, repo ${w.repo}` : ''}).
      ${typeof w.unpushed_commits === 'number' && w.unpushed_commits > 0 ? html` It has <strong>${w.unpushed_commits} commit${w.unpushed_commits === 1 ? '' : 's'} not pushed yet</strong>.` : null}
    </span>`)}
    <span class="reset-worktree-item">It will be deleted too, with its local branch. To keep its changes, finish the loop (Close loop) or push the branch before resetting; otherwise everything is deleted and cleaned.</span></span>`;
}

function ProjectRow({ p }) {
  const ps = p.progress_summary || {}, c = ps.counts || {}, lp = hb.loops[p.id];
  const url = p.url || `/p/${encodeURIComponent(p.id)}/`;
  const st = statusOf(p);
  const total = typeof c.total === 'number' ? c.total : 0, verified = c.verified != null ? c.verified : 0;
  const pct = total > 0 ? Math.max(0, Math.min(100, (verified / total) * 100)) : 0;
  const repo = maskHome(p.repo_path || '');
  const closeMenu = (e) => { const menu = e.currentTarget.closest('details'); if (menu) menu.open = false; };
  const askForget = (e) => { closeMenu(e); hb.forgetting = p.id; draw(); focusId('forget-no-' + p.id); };
  const cancelForget = () => { hb.forgetting = null; draw(); focusId('menu-' + p.id); };
  const askReset = (e) => { closeMenu(e); hb.resetting = p.id; hb.resetConfirm = ''; hb.resetPreview = { id: p.id, state: 'loading' }; draw(); focusId('reset-name-' + p.id); loadResetPreview(p); };
  const cancelReset = () => { hb.resetting = null; hb.resetConfirm = ''; hb.resetPreview = null; draw(); focusId('menu-' + p.id); };
  const typedName = hb.resetConfirm.trim();
  const nameMatches = typedName === String(p.name || '').trim() && typedName !== '';
  const preview = hb.resetPreview && hb.resetPreview.id === p.id ? hb.resetPreview : { state: 'loading' };
  const worktrees = preview.state === 'ready' ? preview.worktrees : [];
  const confirming = hb.resetting === p.id || hb.forgetting === p.id;
  return html`<li class="hub-item">
    <div class="hub-row" role="row">
      <span role="cell" class="hub-cell hub-cell-name">
        <span class="hub-name-line">${p.favorite ? html`<span class="hub-fav" title="favorite" aria-label="favorite">★</span>` : null}<a class="hub-name" href=${url}>${txt(p.name || p.id)}</a></span>
        <span class="hub-path mono" title=${repo}>${repo || DASH}</span>
      </span>
      <span role="cell" class="hub-cell hub-cell-status" title=${[st.title, ps.next_action ? 'Next: ' + (NEXT_ACTION[ps.next_action] || ps.next_action) : ''].filter(Boolean).join(' · ') || null}>
        <${StatusDot} kind=${st.kind} /><span class="hub-status-text">${st.label}</span></span>
      <span role="cell" class="hub-cell hub-cell-ver">${total > 0
        ? html`<span class="hub-bar" role="img" aria-label=${`${verified} of ${total} features verified`}><span style=${{ width: pct + '%' }}></span></span><span class="mono hub-num">${verified}/${total}</span>`
        : html`<span class="dim" title="no features yet">${DASH}</span>`}</span>
      <span role="cell" class="hub-cell hub-cell-cost mono">${lp && typeof lp.cost_usd === 'number' ? '$' + lp.cost_usd.toFixed(2) : DASH}</span>
      <span role="cell" class="hub-cell hub-cell-opened dim">${capital(fmtRelative(p.last_opened_at))}</span>
      <span role="cell" class="hub-cell hub-cell-menu">
        <details class="hub-menu">
          <summary id=${'menu-' + p.id} aria-label=${'Actions for ' + txt(p.name || p.id)} title="Actions"><${Icon} name="dots" /></summary>
          <div class="hub-menu-pop">
            <a href=${url}>Open panel</a>
            <button type="button" id=${'reset-' + p.id} onClick=${askReset} title="Move the plan and loop state to .specs/.trash, delete the loop worktree and its local loop branch, and start from zero">Reset specs</button>
            <button type="button" id=${'forget-' + p.id} class="is-danger" onClick=${askForget}>Forget</button>
          </div>
        </details>
      </span>
    </div>
    ${lp && lp.question ? html`<${AskBlock} p=${p} lp=${lp} />` : null}
    ${hb.resetting === p.id
      ? html`<form class="confirm-inline confirm-reset" role="group" aria-label="Confirm resetting the specs" onSubmit=${(e) => { e.preventDefault(); if (nameMatches && !hb.busy[p.id]) resetSpecs(p); }}>
          <span id=${'reset-help-' + p.id}>Moves the plan, roadmap, flows, features and loop state to .specs/.trash/ (${worktrees.length ? 'the spec files are kept there' : 'nothing is deleted'}).<br />
            RULES, PROJECT, the test environment and the codebase map stay.</span>
          <${ResetWorktreeNotice} preview=${preview} />
          <label class="sr-only" for=${'reset-name-' + p.id}>Type the project name to confirm</label>
          <input type="text" id=${'reset-name-' + p.id} class="dlg-input mono reset-name" autocomplete="off" spellcheck="false"
            placeholder=${'type "' + txt(p.name || p.id) + '"'} aria-describedby=${'reset-help-' + p.id} value=${hb.resetConfirm}
            onInput=${(e) => { hb.resetConfirm = e.currentTarget.value; draw(); }} />
          <button type="submit" class="btn btn-danger btn-sm" id=${'reset-yes-' + p.id} disabled=${!nameMatches || !!hb.busy[p.id] || preview.state === 'loading'}>Reset specs</button>
          <button type="button" class="btn btn-sm" id=${'reset-no-' + p.id} onClick=${cancelReset}>Cancel</button>
        </form>`
      : hb.forgetting === p.id
      ? html`<div class="confirm-inline" role="group" aria-label="Confirm forgetting the project">
          <span>Remove it from the list? No file is deleted.</span>
          <button type="button" class="btn btn-danger btn-sm" id=${'forget-yes-' + p.id} disabled=${!!hb.busy[p.id]} onClick=${() => forget(p)}>Yes, forget</button>
          <button type="button" class="btn btn-sm" id=${'forget-no-' + p.id} onClick=${cancelForget}>Cancel</button>
        </div>`
      : null}
  </li>`;
}

const COLUMNS = ['Name', 'Status', 'Verified', 'Cost', 'Opened'];
/** Content of #hub-list: the table (loading skeleton, error, empty and filled). */
function ProjectTable() {
  let body;
  if (hb.projects == null) {
    body = hb.error
      ? html`<div class="hub-state hub-state-error" role="alert"><div class="hub-state-t">Could not read the projects</div>
          <div class="muted">${maskIn(hb.error)}</div><button type="button" class="btn btn-sm" id="hub-retry" onClick=${() => loadProjects().catch((e) => { hb.error = e.message; draw(); })}>Try again</button></div>`
      : html`<div class="hub-skel" aria-hidden="true">${[0, 1, 2].map((i) => html`<div class="hub-skel-row" key=${i}><span></span><span></span><span></span></div>`)}</div>
          <span class="sr-only" role="status">Loading projects…</span>`;
  } else if (!hb.projects.length) {
    body = html`<div class="hub-state"><div class="hub-state-t">No projects yet</div>
      <div class="muted">Open the folder of an existing project (with or without .specs) or create one from scratch. The app remembers the projects you open.</div></div>`;
  } else {
    const ord = [...hb.projects].sort((a, b) => (b.favorite ? 1 : 0) - (a.favorite ? 1 : 0) || String(b.last_opened_at || '').localeCompare(String(a.last_opened_at || '')));
    body = html`<ul class="hub-rows" role="rowgroup">${ord.map((p) => html`<${ProjectRow} key=${p.id} p=${p} />`)}</ul>`;
  }
  return html`<div class="hub-table" role="table" aria-label="Projects">
    <div class="hub-row hub-head" role="row">${COLUMNS.map((name, i) => html`<span key=${name} role="columnheader" class=${'hub-cell hub-cell-' + ['name', 'status', 'ver', 'cost', 'opened'][i]}>${name}</span>`)}<span class="hub-cell hub-cell-menu" aria-hidden="true"></span></div>
    <div id="hub-list">${body}</div>
  </div>`;
}
/** GET overview: every running loop of every project. A back end without it leaves the table as it was. */
async function loadOverview() {
  try {
    const r = await appApi.get('overview', 10000);
    hb.loops = Object.fromEntries(list(r && r.loops).filter((l) => l && l.project_id).map((l) => [l.project_id, l]));
  } catch (_) { /* no overview route: no live data */ }
}
async function loadResetPreview(p) {
  let next;
  try {
    const r = await appApi.get('projects/' + encodeURIComponent(p.id) + '/reset-specs/preview', 15000);
    next = { id: p.id, state: 'ready', worktrees: list(r && r.worktrees) };
  } catch (e) { next = { id: p.id, state: 'error', error: e.message }; }
  if (hb.resetting !== p.id) return; // cancelled or reset meanwhile
  hb.resetPreview = next; draw();
}
async function resetSpecs(p) {
  hb.busy[p.id] = true; draw();
  try {
    const r = await appApi.post('projects/' + encodeURIComponent(p.id) + '/reset-specs', { confirm: hb.resetConfirm.trim() });
    const n = list(r && r.moved).length;
    const removed = list(r && r.worktrees_removed).length, failed = list(r && r.worktree_failures);
    hb.resetting = null; hb.resetConfirm = ''; hb.resetPreview = null;
    toast((n ? `Specs reset: ${n} item${n === 1 ? '' : 's'} moved to ${maskHome(r.trash)}.` : 'Nothing to reset: the specs were already clean.')
      + (removed ? ` Loop worktree${removed === 1 ? '' : 's'} deleted.` : ''), 'ok');
    if (failed.length) toast('Not everything was deleted: ' + failed.map((f) => `${f.repo}: ${maskIn(f.error)}`).join('; '), 'err');
    draw();
    focusId('menu-' + p.id);
    loadProjects().catch(() => {});
  } catch (e) { toast('Specs not reset: ' + e.message, 'err'); }
  finally { delete hb.busy[p.id]; draw(); }
}
async function forget(p) {
  hb.busy[p.id] = true; draw();
  try {
    await appApi.post('projects/' + encodeURIComponent(p.id) + '/forget', {});
    hb.projects = hb.projects.filter((x) => x.id !== p.id); hb.forgetting = null;
    setShellProjects(hb.projects);
    toast(`${p.name || p.id} left the list (the files stay where they were).`, 'ok');
  } catch (e) { toast('Not forgotten: ' + e.message, 'err'); }
  finally { delete hb.busy[p.id]; draw(); }
}

// ---------- open / create ----------
function goToProject(r) {
  const url = (r && r.url) || (r && r.project && r.project.url) || (r && r.project && r.project.id ? `/p/${encodeURIComponent(r.project.id)}/` : null);
  if (!url) { toast('The app opened the project but did not return the panel address.', 'err'); loadProjects().catch(() => {}); return; }
  location.href = url;
}
async function openPath(path) {
  if (!path) return;
  setOpening({ text: 'opening ' + maskHome(path) + '…' });
  try { goToProject(await appApi.post('projects/open', { path }, 60000)); }
  catch (e) { setOpening({ error: e.status === 400 ? `Not opened: ${maskIn(e.message)}. Check that the path is a folder.` : 'Not opened: ' + maskIn(e.message) }); }
}
async function openFolder() {
  if (hb.opening && hb.opening.native) return;
  const mine = {};
  hb.native = mine;
  setOpening({ native: true, text: 'Pick the folder in the system window that opened (it may be behind the browser).' });
  try {
    const picked = await pickFolderNativeFirst({ title: 'Open a project folder', buttonText: 'Open this folder' });
    if (hb.native !== mine) return;
    hb.native = null;
    if (picked) openPath(picked);
    else setOpening(null); // dialog cancelled
  } catch (e) {
    if (hb.native !== mine) return;
    hb.native = null;
    setOpening(null);
    toast('The system window did not open (' + e.message + '); paste the folder path instead.', 'err');
  }
}
function setOpening(v) { hb.opening = v; draw(); }
/** Content of #hub-opening (hidden when nothing is opening). */
function Opening({ v }) {
  if (!v) return null;
  if (v.error) return html`<div class="nl-box nl-bad" role="alert">${v.error}</div>`;
  return html`<div class="nl-msg" role="status"><div class="spinner" aria-hidden="true" /><span>${v.text}</span></div>`;
}

/** Body of the "Create a project from scratch" dialog; handle.submit(dialogApi) is the main action. */
function CreateProjectForm({ handle }) {
  const [parent, setParent] = useState('');
  const [name, setName] = useState('');
  const [git, setGit] = useState(true);
  const [conf, setConf] = useState(() => conformanceDefault(null, true));
  const [submitError, setSubmitError] = useState(''); // cleared by any edit, like the live name check
  const parentRef = useRef(null), nameRef = useRef(null);
  const p = parent.trim(), n = name.trim();
  const error = submitError || (n && INVALID_NAME.test(n) ? INVALID_NAME_MSG : '');
  const editParent = (v) => { setParent(v); setSubmitError(''); };
  const editName = (v) => { setName(v); setSubmitError(''); };
  const sep = p.includes('/') && !p.includes('\\') ? '/' : '\\';
  const choose = async () => {
    // the system folder window first (Windows Explorer); the in-app browser only when the system window is unavailable
    const c = await pickFolderNativeFirst({ title: 'Parent folder of the new project', buttonText: 'Use this folder', initial: p });
    if (c) editParent(c);
    if (nameRef.current) nameRef.current.focus();
  };
  handle.submit = async (d) => {
    if (!p) { setSubmitError('Choose the parent folder.'); parentRef.current.focus(); return; }
    if (!n || INVALID_NAME.test(n)) { setSubmitError(n ? INVALID_NAME_MSG : 'Give the project a name.'); nameRef.current.focus(); return; }
    d.busy(true);
    try { const r = await appApi.post('projects/create', { parent_path: p, name: n, init_git: !!git, conformance: conf }, 60000); d.close(); toast('Project created.', 'ok'); goToProject(r); }
    catch (e) { setSubmitError('Not created: ' + maskIn(e.message)); d.busy(false); }
  };
  return html`
    <p class="dlg-text">Creates the project folder, the template .specs and (if checked) git with an initial commit. Refuses if the folder already exists with content.</p>
    <div class="dlg-field"><label class="nl-label" for="cr-parent">Parent folder</label>
      <div class="fb-bar">
        <input type="text" id="cr-parent" class="dlg-input mono" autocomplete="off" spellcheck="false" placeholder="~/projects" ref=${parentRef}
          value=${parent} onInput=${(e) => editParent(e.currentTarget.value)} />
        <button type="button" class="btn btn-sm" onClick=${choose}>Choose…</button>
      </div>
    </div>
    <div class="dlg-field"><label class="nl-label" for="cr-name">Project name (becomes the folder name)</label>
      <input type="text" id="cr-name" class="dlg-input mono" autocomplete="off" spellcheck="false" placeholder="my-app" ref=${nameRef}
        value=${name} onInput=${(e) => editName(e.currentTarget.value)} />
      <div class="nl-err" role="alert">${error}</div>
    </div>
    <div class="hint mono" aria-live="polite">${p && n ? 'Will create: ' + maskHome(p.replace(/[\\/]+$/, '')) + sep + n : ''}</div>
    <label class="nl-check" for="cr-git"><input type="checkbox" id="cr-git" checked=${git} onChange=${(e) => setGit(e.currentTarget.checked)} /><span>Initialize git (git init + initial commit)</span></label>
    <${Conformance} allowLater=${true} onChange=${setConf} />`;
}
function createDialog() {
  const handle = {};
  openDialog({
    title: 'Create a project from scratch', width: 600,
    content: html`<${CreateProjectForm} handle=${handle} />`,
    actions: [
      { text: 'Cancel' },
      { text: 'Create project', cls: 'btn-primary', onClick: (d) => handle.submit(d) },
    ],
  });
}

// ---------- diagnostics (the checks list lives in settings_system.js; "Details" opens Settings -> System) ----------
/** The system strip: "N checks passed", every failing check as danger text, Details, the app version. */
function SystemStrip() {
  const r = hb.info;
  const checks = r ? checksOf(r) : [];
  const passed = checks.filter((c) => c.ok === true).length;
  const failing = checks.filter((c) => c.ok === false);
  const version = r && r.version ? (/^v/i.test(String(r.version)) ? String(r.version) : 'v' + r.version) : '';
  return html`<section class="hub-system" id="hub-diag" aria-label="System">
    <span class="hub-system-t">System</span>
    ${r ? html`<span class="muted">${passed} check${passed === 1 ? '' : 's'} passed</span>
        ${failing.map((c) => html`<span class="hub-fail" key=${c.key}><svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M8 2.5l6 11H2z" /><path d="M8 7v3" /><path d="M8 12h.01" /></svg>${failText(c)}</span>`)}`
      : html`<span class="muted">${hb.infoError ? 'Diagnostics unavailable: ' + maskIn(hb.infoError) : 'Reading diagnostics…'}</span>`}
    <span class="sh-spacer"></span>
    <button type="button" class="hub-link" id="hub-test" disabled=${hb.testing} onClick=${testNotification}>${hb.testing ? 'Sending…' : 'Test notification'}</button>
    <button type="button" class="hub-link" id="hub-details" onClick=${openDetails}>Details</button>
    ${version ? html`<span class="mono dim hub-version">${version}</span>` : null}
  </section>`;
}
async function loadInfo() {
  try { hb.info = await appApi.get('info', 15000); hb.infoError = null; }
  catch (e) { hb.infoError = e.message; }
  draw();
}
async function testNotification() {
  hb.testing = true; draw();
  try { await sendTestNotification(); } finally { hb.testing = false; draw(); }
}
const openDetails = () => navigate('/settings/system');
const askShutdown = () => { hb.confirmShutdown = true; draw(); focusId('shut-no'); };
const cancelShutdown = () => { hb.confirmShutdown = false; draw(); focusId('shut-open'); };
/** Content of #hub-shutdown. */
function Shutdown() {
  if (!hb.confirmShutdown) return html`<button type="button" class="btn btn-ghost" id="shut-open" onClick=${askShutdown}>Shut down app</button>`;
  return html`<div class="confirm-inline" role="group" aria-label="Confirm shutting down the app">
    <span>Shut down the app? Open panels stop updating. Loops already started keep running as separate processes.</span>
    <button type="button" class="btn btn-danger btn-sm" id="shut-yes" disabled=${hb.shuttingDown} onClick=${shutDown}>Yes, shut down</button>
    <button type="button" class="btn btn-sm" id="shut-no" onClick=${cancelShutdown}>Cancel</button>
  </div>`;
}
let hubPolling = null;
async function shutDown() {
  hb.shuttingDown = true; draw();
  try {
    await appApi.post('shutdown', {}, 15000);
    if (hubPolling) hubPolling.stop();
    hb.closed = true; draw();
    document.title = 'app shut down · Rolo Loop';
  } catch (e) { toast('Could not shut down: ' + e.message, 'err'); hb.confirmShutdown = false; hb.shuttingDown = false; draw(); focusId('shut-open'); }
}

/** The Claude account of the main command (diagnostics), for the sidebar chip: e-mail or the command name, and the sign-in when known. */
function hubAccount() {
  const main = list(hb.info && hb.info.claude_commands).find((c) => c.role === 'main' && c.found) || list(hb.info && hb.info.claude_commands).find((c) => c.found);
  if (!main) return null;
  const auth = main.auth || {};
  const base = String(main.name || '').split(/[\\/]/).pop().replace(/\.(cmd|bat|exe|py)$/i, '');
  const method = auth.method && auth.method !== '?' ? auth.method : '';
  return { name: auth.email || base, sub: [auth.plan, method].filter(Boolean).join(' · ') || 'Claude command' };
}

/** The whole hub page (#hub). The offline banner is driven by showDisconnectedBanner(); its props never change, so Preact leaves it alone. */
export function Hub() {
  const splash = useSplash(hb.projects != null || !!hb.error);
  return html`<div id="hub" class="hub shell">
    <${Sidebar} page="hub" project=${null} account=${hubAccount()} activity=${null}
      commands=${[{ label: 'New project', hint: 'create a project from scratch', run: createDialog }, { label: 'Open folder', hint: 'open the folder of a project', run: openFolder }]} />
    <div class="shell-col" id="shell-col">
      <div id="banner-offline" class="banner banner-offline" role="alert" hidden>
        <span class="dot dot-red" />
        <strong>App disconnected</strong>
        <span id="banner-offline-detail" />
      </div>
      <${ShellHeader} cls="hub-header" crumbs=${currentRoute().name === 'settings' ? [{ label: 'Projects', href: '#/loop' }, { label: 'Settings' }] : [{ label: 'Projects' }]}
        end=${(compact) => compact
          ? html`<div class="header-actions" id="hub-actions"><button type="button" class="btn btn-primary" id="hub-open" onClick=${openFolder}>Open folder</button>
              ${hb.confirmShutdown ? html`<div id="hub-shutdown"><${Shutdown} /></div>` : null}</div>
            <${OverflowMenu}>
              <button type="button" class="btn" id="hub-create" onClick=${createDialog}>New project</button>
              ${hb.confirmShutdown ? null : html`<div id="hub-shutdown"><${Shutdown} /></div>`}
              <button type="button" class="btn btn-ghost" id="btn-help" onClick=${openWhy}>Why Rolo Loop?</button>
            <//>`
          : html`<div class="header-actions" id="hub-actions">
            <button type="button" class="btn" id="hub-create" onClick=${createDialog}>New project</button>
            <button type="button" class="btn btn-primary" id="hub-open" onClick=${openFolder}>Open folder</button>
            <div id="hub-shutdown"><${Shutdown} /></div>
          </div><div class="header-actions"><${WhyButton} /></div>`} />

      ${currentRoute().name === 'settings' ? html`<${SettingsPage} />` : html`<main class="hub-main" id="hub-main">
        <div class="hub-inner">
          <section class="hub-start" aria-labelledby="hub-title">
            <h1 id="hub-title" class="hub-h1">Projects</h1>
            <p class="hub-lead">Open a folder (new, existing, or a loop stopped midway). Rolo Loop runs the spec-driven loop of the <span class="mono">/orchestrator</span> skill and keeps every step in files you can read.</p>
            <div id="hub-opening" class="hub-opening" hidden=${!hb.opening}><${Opening} v=${hb.opening} /></div>
          </section>
          <${ProjectTable} />
          <${SystemStrip} />
        </div>
      </main>`}
    </div>

    <div id="closed" class="closed" hidden=${!hb.closed}>
      <div class="closed-card" role="alert">
        <div class="closed-title">App shut down</div>
        <div class="muted">The server was shut down. You can close this tab. Loops already started keep running.</div>
      </div>
    </div>
    ${splash ? html`<${Splash} text=${hb.error ? 'Waiting for the app…' : 'Loading projects…'} />` : null}
  </div>`;
}

export function startHub(root) {
  hubRoot = root;
  root.replaceChildren();
  onRoute(() => draw());
  document.title = 'projects · Rolo Loop';
  maybeStartTour(); // first run: the guided tour (it waits for the splash)
  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    if (document.querySelector('.sh-sw') || document.querySelector('dialog[open]')) return; // the switcher and the dialogs own Esc
    if (document.querySelector('details.hub-menu[open]')) { const menu = document.querySelector('details.hub-menu[open] summary'); closeMenus(); if (menu) menu.focus(); return; }
    if (hb.confirmShutdown) cancelShutdown();
    else if (hb.resetting) { const id = hb.resetting; hb.resetting = null; hb.resetConfirm = ''; draw(); focusId('menu-' + id); }
    else if (hb.forgetting) { const id = hb.forgetting; hb.forgetting = null; draw(); focusId('menu-' + id); }
  });
  document.addEventListener('pointerdown', (e) => { for (const menu of document.querySelectorAll('details.hub-menu[open]')) if (!menu.contains(e.target)) menu.open = false; });
  draw();
  hubPolling = createPolling({
    intervalMs: 5000,
    task: async () => {
      await loadOverview();
      try { await loadProjects(); } catch (e) { hb.error = e.message; if (hb.projects == null) draw(); throw e; }
    },
    onConnectionChange: (disc, msg) => showDisconnectedBanner($('banner-offline'), $('banner-offline-detail'), disc, msg),
  });
  hubPolling.now(0);
  loadInfo();
  // the first-run guide is the tour (maybeStartTour); "Why Rolo Loop?" stays one click away (the ? button)
}
