/* Panel header (loop and planning): quiet. Breadcrumb (app.js), ONE status pill (click opens the status dialog), the primary action (Pause after the
   turn while running, else Start loop), the "..." menu with the rest (PR, Back to planning, Finish, Workspace, Copy repo path, Shut down app, Why) and the
   theme toggle. Cost, verified count, repo and problem counts are NOT here: the Loop page and the sidebar own them (spec 09, dedupe).
   Shutting down belongs to the app: the button calls POST /api/app/shutdown (the project shutdown is a no-op).
   The header frame lives in the Panel (app.js); each dynamic part is a small component here, each action its own component. */
import { $ } from '../../util/dom.js';
import { html } from '../../util/preact.js';
import { isNum, firstLine, httpUrl, maskHome } from '../../util/format.js';
import { appApi } from '../../api/api_client.js';
import { toast } from '../../components/toast.js';
import { copyText } from '../../components/copy.js';
import { NavbarStatus, NAVBAR_STATUS, navbarStatusKey } from '../../components/navbar_status.js';
import { ctx, ui, status, loop, pauseRequested, projectName } from './context.js';
import { renderAll, schedulePoll, stopPanel } from '../project_panel.js';
import { costCapPause, openNewLoop, openStatusDialog, statusHasDialog } from './alert.js';
import { openReopenPlanning } from './reopen_planning.js';
import { openFinishDialog } from './wrap_up.js';
import { openWorkspaceDialog } from './workspace_dialog.js';

const meta = () => (ctx.state && ctx.state.meta) || {};
const turnNow = () => { const t = loop().turn; return isNum(t) ? t : null; };

/** Repo folder the panel works on and, when a loop worktree exists, that folder + the branch (both from /state). */
export function repoInfo() {
  const state = ctx.state || {}, ref = state.project_ref || {};
  const repo = ref.repo_path || state.repo || '', work = state.work || '';
  const norm = (v) => String(v).replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
  const same = (a, b) => norm(a) === norm(b);
  return { repo, worktree: work && repo && !same(work, repo) ? work : '', branch: state.branch || meta().branch || '' };
}

/** Status of the header pill: a cost-cap pause counts as paused; planning mode has its own key (one vocabulary: components/navbar_status.js). */
function statusKey() {
  if (ctx.state && ctx.state.planning_required) return 'planning';
  return costCapPause() ? 'paused' : navbarStatusKey(status());
}

/** The ONE status pill of the header (click opens the status dialog when it has something to show). */
export function HeaderStatus() {
  const key = statusKey(), l = loop(), clickable = statusHasDialog();
  const detail = key === 'awaiting_user' && !clickable ? 'Answer it in the LOOP panel.' : ['paused', 'error', 'awaiting_user', 'stopped'].includes(key) && l.reason ? firstLine(l.reason) : '';
  return html`<${NavbarStatus} statusKey=${key} turn=${key === 'planning' ? null : turnNow()} detail=${detail} onClick=${clickable ? openStatusDialog : null} />`;
}

/** "Copy repo path" (overflow menu): the label shows the masked path, the clipboard gets the full one. */
function CopyPathAction() {
  const r = repoInfo(), path = r.worktree || r.repo;
  if (!path) return null;
  return html`<button type="button" class="btn btn-ghost" id="btn-copy-path" title=${'copy the full path: ' + maskHome(path)} onClick=${() => copyText(path, 'Path copied.')}>Copy repo path</button>`;
}

// ---------- actions, one component each ----------
function PrChip() {
  const m = meta(), prU = httpUrl(m.pr), n = prU && prU.match(/\/pull\/(\d+)/);
  if (!prU) return null;
  return html`<a class="btn btn-sm pr-chip" href=${prU} target="_blank" rel="noopener noreferrer"
    title=${'open pull request' + (m.pr_status ? ' · ' + m.pr_status : '')}>${'PR ' + (n ? '#' + n[1] : '↗') + (m.pr_status ? ' · ' + m.pr_status : '')}</a>`;
}
/** Planning mode (WS-03): opens the Workspace dialog (repositories, roles, branches, setup, commit of the setup files). */
function WorkspaceButton() {
  return html`<button type="button" class="btn btn-ghost" id="btn-workspace" title="repositories, branches, setup state and the plan folder of this project"
    onClick=${openWorkspaceDialog}>Workspace</button>`;
}
function PauseButton() {
  if (status() !== 'running') return null;
  return pauseRequested()
    ? html`<button type="button" class="btn btn-pending" disabled id="btn-pause">Pausing after the turn…</button>`
    : html`<button type="button" class="btn btn-primary btn-icon" id="btn-pause" disabled=${!!ui.busy.pause} onClick=${doPause}><svg width="10" height="10" viewBox="0 0 12 12" aria-hidden="true"><rect x="2" y="1.5" width="2.6" height="9" fill="currentColor" /><rect x="7.4" y="1.5" width="2.6" height="9" fill="currentColor" /></svg>Pause after the turn</button>`;
}
function StartLoopButton() {
  if (status() === 'running') return null;
  return html`<button type="button" class="btn btn-primary" id="btn-start-loop" title="Opens the Start dialog: what will run and how to start it." onClick=${openNewLoop}>Start loop</button>`;
}

/** "Back to planning" state from GET planning/reopen-check, fetched again when the loop status changes (BACK-01). */
const reopenGate = { key: null, check: null };
function ensureReopenGate() {
  const key = `${status()}|${loop().at}`;
  if (reopenGate.key === key || !ctx.api) return;
  reopenGate.key = key;
  ctx.api.get('planning/reopen-check', 30000).then((c) => { reopenGate.check = c || null; }).catch(() => { reopenGate.check = null; }).finally(renderAll);
}
function reopenBlockReason() {
  const c = reopenGate.check;
  if (status() === 'running' || (c && c.blocked_by === 'loop_running')) return 'The loop is running: pause it and wait for it to stop before going back to planning.';
  return '';
}
function BackToPlanningButton() {
  ensureReopenGate();
  const why = reopenBlockReason();
  return html`<button type="button" class="btn" id="btn-back-planning" disabled=${!!why} aria-disabled=${why ? 'true' : 'false'}
    title=${why || 'Change the roadmap or the repositories: goes back to the planning screen.'} onClick=${openReopenPlanning}>Back to planning</button>`;
}
function FinishButton() {
  const s = status(), r = repoInfo();
  if (!(s === 'done' || s === 'stopped') || !r.worktree) return null;
  return html`<button type="button" class="btn" id="btn-finish" title="Wrap up this loop: deliver the branch, close it and remove the worktree."
    onClick=${() => openFinishDialog({ state: ctx.state, project: ctx.projectId, onDone: () => { renderAll(); schedulePoll(0); } })}>Finish</button>`;
}
function ShutdownControl() {
  return ui.confirmShutdown
    ? html`<div class="confirm-inline" role="group" aria-label="Confirm shutting down the app">
        <span>Shut down the app? Loops already started keep running as separate processes.</span>
        <button type="button" class="btn btn-danger btn-sm" id="btn-shutdown-yes" disabled=${!!ui.busy.shutdown} onClick=${doShutdown}>Yes, shut down</button>
        <button type="button" class="btn btn-sm" id="btn-shutdown-no" onClick=${cancelShutdown}>Cancel</button>
      </div>`
    : html`<button type="button" class="more-item" id="btn-shutdown" role="menuitem" title="shuts down the whole app (every project)" onClick=${askShutdown}>Shut down app</button>`;
}

/** The "more" menu of the header: what you rarely need while following a loop (shut down the app, help). */
const more = { open: false, bound: false };
function closeMore() { if (!more.open) return; more.open = false; renderAll(); }
function bindMoreMenu() {
  if (more.bound) return;
  more.bound = true;
  document.addEventListener('pointerdown', (e) => { if (more.open && !e.target.closest('.more-wrap')) closeMore(); });
  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape' || !more.open) return;
    if (ui.confirmShutdown) return;
    closeMore(); const b = $('btn-more'); if (b) b.focus();
  });
}
function MoreMenu() {
  bindMoreMenu();
  const open = more.open || ui.confirmShutdown;
  return html`<div class="more-wrap">
    <button type="button" class="btn btn-ghost more-btn" id="btn-more" aria-haspopup="menu" aria-expanded=${String(open)} aria-label="More: shut down app, help" title="More"
      onClick=${() => { more.open = !more.open; renderAll(); }}><svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true" focusable="false"><circle cx="5" cy="12" r="1.8" fill="currentColor" /><circle cx="12" cy="12" r="1.8" fill="currentColor" /><circle cx="19" cy="12" r="1.8" fill="currentColor" /></svg></button>
    ${open ? html`<div class="more-menu" role="menu" aria-label="More">
      <button type="button" class="more-item" role="menuitem" id="btn-why" onClick=${() => { more.open = false; renderAll(); openWhy(); }}>Why use this panel</button>
      <${ShutdownControl} />
    </div>` : null}
  </div>`;
}

function askShutdown() {
  ui.confirmShutdown = true; renderAll();
  const b = $('btn-shutdown-no'); if (b) b.focus();
}

/** Primary action of the header, always in the bar (wide and compact): Pause while running, else Start loop (loop mode) / Pause (planning).
    The shutdown confirmation also stays in the bar (it is a popover under the header, not a menu entry). */
export function PrimaryAction({ mode = 'loop' }) {
  const confirm = ui.confirmShutdown ? html`<${ShutdownControl} />` : null;
  return mode === 'planning' ? html`<${PauseButton} />${confirm}` : html`<${PauseButton} /><${StartLoopButton} />${confirm}`;
}
/** Everything else, as the real components (same ids and handlers), for the "⋯" menu. */
export function OverflowActions({ mode = 'loop' }) {
  const shutdown = ui.confirmShutdown ? null : html`<${ShutdownControl} />`;
  return mode === 'planning'
    ? html`<${PrChip} /><${WorkspaceButton} /><${CopyPathAction} />${shutdown}`
    : html`<${PrChip} /><${BackToPlanningButton} /><${FinishButton} /><${CopyPathAction} />${shutdown}`;
}

export function cancelShutdown() {
  if (!ui.confirmShutdown) return;
  ui.confirmShutdown = false; renderAll();
  const b = $('btn-shutdown'); if (b) b.focus();
}

const CLOSED_TITLE = { notfound: 'project not found · Rolo Loop', shutdown: 'app shut down · Rolo Loop' };
export function updateTitle() {
  const state = ctx.state;
  if (ui.closed) { document.title = CLOSED_TITLE[ui.closed.kind] || 'Rolo Loop'; return; }
  if (!state) { document.title = (ctx.offline ? 'disconnected' : 'loading') + ' · Rolo Loop'; return; }
  const label = NAVBAR_STATUS[statusKey()].label, v = loop().turn;
  document.title = `${ctx.offline ? 'disconnected · ' : ''}${label}${isNum(v) ? ' · turn ' + v : ''} · ${projectName() || 'project'}`;
}

async function doPause() {
  ui.busy.pause = true; renderAll();
  try { await ctx.api.post('pause', {}); ui.pausePending = true; toast('Pause requested: the loop stops at the end of this turn.', 'ok'); schedulePoll(0); }
  catch (e) { toast('Could not pause: ' + e.message, 'err'); }
  finally { ui.busy.pause = false; renderAll(); }
}

async function doShutdown() {
  ui.busy.shutdown = true; renderAll();
  try {
    await appApi.post('shutdown', {}, 15000);
    stopPanel();
    ui.closed = { kind: 'shutdown' };
  } catch (e) { toast('Could not shut down: ' + e.message, 'err'); ui.confirmShutdown = false; }
  finally { ui.busy.shutdown = false; renderAll(); }
}
