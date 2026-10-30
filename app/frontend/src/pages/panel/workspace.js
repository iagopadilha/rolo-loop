/* Workspace data of the project panel: GET workspace (per repo the folder, the worktree, loop branch, base branch and a state), the creation job
   the loop starts by itself (GET workspace/job/<id>, followed until it ends) and the notices that explain an uncommitted change BEFORE the
   worktree is born. The repositories are defined in the repository manager (a dialog); the back end decides every state and refusal and this
   module only shows them. */
import { html } from '../../util/preact.js';
import { list, txt, maskHome } from '../../util/format.js';
import { toast } from '../../components/toast.js';

import { ctx, status } from './context.js';
import { renderAll, schedulePoll } from '../project_panel.js';
import { reloadProgress } from './progress_trail.js';

import { ROLE_TEXT } from '../repo_scope.js';
const FAST_MS = 1000, CHECK_MS = 15000, WATCH_MS = 90000;

const STATE = {
  not_created: ['not created', 'muted'], ready: ['ready', 'ok'], loop_running: ['loop running', 'warn'],
  dirty: ['uncommitted changes', 'warn'], missing: ['folder missing', 'bad'], read_only: ['read only — no worktree', 'muted'],
};

/** Screen state (survives renders). job = the latest create job seen (from workspace or from the job route). */
const ws = {
  data: null, error: null, loading: false, again: false, loadedAt: 0, job: null, following: null, dismissed: null, watchUntil: 0,
  check: null, checkErr: null, checking: false, checkedAt: 0, closing: false, result: null, saving: null,
};

const running = () => status() === 'running';
const repoList = () => list(ws.data && ws.data.repos);

// ---------- reading ----------
/** GET workspace (overlapping calls collapse into one more read). Also follows a create job started by the loop itself. */
export async function loadWorkspace() {
  if (ws.loading) { ws.again = true; return; }
  ws.loading = true;
  try {
    const d = await ctx.api.get('workspace', 15000);
    ws.data = d; ws.error = null;
    if (d.job && d.job.id) {
      if (!ws.job || ws.job.id !== d.job.id || (d.job.steps || []).length >= list(ws.job.steps).length) ws.job = d.job;
      if (d.job.state === 'running') follow(d.job.id);
    }
    if (repoList().some((r) => r.worktree_exists) && !running() && Date.now() - ws.checkedAt > CHECK_MS) loadCheck();
  } catch (e) {
    ws.error = e.status === 404 && !/project/i.test(e.message) ? 'The server does not offer the workspace yet.' : e.message;
  } finally {
    ws.loading = false; ws.loadedAt = Date.now();
    renderAll();
    if (ws.again) { ws.again = false; loadWorkspace(); }
  }
}
/** Called right after a loop start: reads the workspace every second for a while so the steps of a creation at start show up. */
export function watchWorkspace() { ws.watchUntil = Date.now() + WATCH_MS; loadWorkspace(); }

/** Follows a create job (GET workspace/job/<id> every second): steps are append-only, the job ends with state ok or error. */
function follow(id) {
  if (ws.following === id) return;
  ws.following = id;
  const step = async () => {
    if (ws.following !== id) return;
    try {
      const r = await ctx.api.get('workspace/job/' + encodeURIComponent(id), 10000);
      if (ws.following !== id) return;
      if (r.job) {
        ws.job = r.job; renderAll();
        if (r.job.state !== 'running') { ws.following = null; finished(r.job); return; }
      }
    } catch (e) {
      if (e.status === 404) { ws.following = null; loadWorkspace(); return; }
    }
    setTimeout(step, FAST_MS);
  };
  step();
}
function finished(job) {
  toast(job.state === 'ok' ? 'Workspace ready.' : 'Workspace not created' + (job.error ? ': ' + job.error : '.'), job.state === 'ok' ? 'ok' : 'err');
  ws.checkedAt = 0;
  loadWorkspace(); reloadProgress(); schedulePoll(300);
}

async function loadCheck() {
  if (ws.checking) return;
  ws.checking = true; ws.checkedAt = Date.now();
  try { ws.check = await ctx.api.get('close-loop/check', 30000); ws.checkErr = null; }
  catch (e) { ws.check = null; ws.checkErr = e.status === 404 ? null : e.message; }
  finally { ws.checking = false; renderAll(); }
}


/** Role of a repository: a missing value reads as edit. */
const roleOf = (r) => (r && r.role === 'reference' ? 'reference' : 'edit');
/** A reference repository is read by the agents only: no worktree, no carried files, nothing to commit for the loop. */
const READ_ONLY_TEXT = 'read only — no worktree';
const isReadOnly = (r) => !!r && (r.read_only === true || r.state === 'read_only' || roleOf(r) === 'reference');





const UNCOMMITTED_LIST_LIMIT = 12;

/** The context files (maps, CLAUDE.md, SYSTEM.md) a not-yet-created worktree will miss; [] once the worktree exists. */
function uncommittedContextOf(r) {
  return r && r.state === 'not_created' && !isReadOnly(r) ? list(r.uncommitted_context).filter((item) => item && item.path) : [];
}


/** "a, b, ... and N more": up to 12 names. */
function uncommittedNames(items) {
  const names = items.map((i) => txt(i.path));
  return names.slice(0, UNCOMMITTED_LIST_LIMIT).join(', ') + (names.length > UNCOMMITTED_LIST_LIMIT ? ` and ${names.length - UNCOMMITTED_LIST_LIMIT} more` : '');
}

/** Warning for the Start dialog: uncommitted context files the new worktree will not carry. Never blocks Start. */
export function UncommittedContextNotice() {
  const items = repoList().flatMap(uncommittedContextOf);
  if (!items.length) return null;
  return html`<div class="nl-box nl-warn ws-note" role="note" id="nl-uncommitted"><strong>Setup files not committed: </strong>they are carried into the worktree when the loop starts (or commit them from Workspace).
    <div class="mono ws-notice-l">${uncommittedNames(items)}</div></div>`;
}

/** One short notice for the Start dialog: which repositories are in scope and which worktrees Start creates by itself. */
export function WorkspaceNotice() {
  const repos = repoList();
  const todo = repos.filter((r) => r.state === 'not_created' && !isReadOnly(r));
  // RD-01: the live `other_changes` of each repository (tracked files changed in the folder that are not setup files), listed by name; a payload
  // without that key falls back to the bare `dirty` flag.
  const othersOf = (r) => (Array.isArray(r.other_changes) ? list(r.other_changes) : null);
  const withOthers = todo.filter((r) => (othersOf(r) ? othersOf(r).length : r.dirty));
  if (!todo.length && !withOthers.length) return null;
  return html`<div class="nl-box nl-warn" role="note"><strong>${'Repositories in scope: ' + repos.map((r) => r.name).join(', ') + '. '}</strong>
    ${todo.length ? html`Start creates ${todo.length === 1 ? 'the missing worktree' : 'the ' + todo.length + ' missing worktrees'} by itself (each on a new loop branch from its last commit; your folder and branch are never touched).
      <div class="mono ws-notice-l">${todo.map((r) => `${r.name} → ${maskHome(r.worktree)}`).join(String.fromCharCode(10))}</div>` : null}
    ${withOthers.map((r) => { const files = othersOf(r); return html`<div class="ws-note" role="note" key=${r.name}>${files ? `${files.length} other changed file${files.length === 1 ? '' : 's'} in ${r.name} will NOT be carried: the worktree is born from a commit.` : `Other changed files in ${r.name} will NOT be carried: the worktree is born from a commit.`}
      ${files ? html`<div class="mono ws-notice-l">${uncommittedNames(files.map((path) => ({ path })))}</div>` : null}</div>`; })}</div>`;
}

/** Workspace state for the Start dialog (START-04): one row per repository with its role, whether the worktree exists and the branch. */
export function WorkspaceState({ reattachBranch = null } = {}) {
  const repos = repoList(), d = ws.data;
  if (!d) return html`<div class="sl-muted">${ws.error ? 'Workspace unavailable: ' + ws.error : 'Reading the workspace…'}</div>`;
  if (!repos.length) return html`<div class="sl-muted">No repository is defined yet.</div>`;
  return html`<ul class="sl-repos" aria-label="Repositories and worktrees">${repos.map((r) => {
    const [stateText, tone] = STATE[r.state] || [String(r.state || ''), 'muted'];
    if (isReadOnly(r)) {
      return html`<li key=${r.name} class="sl-repo"><span class="sl-repo-name mono">${r.name}</span><span class="rp-tag mono" title=${ROLE_TEXT.reference.hover} tabIndex="0">${ROLE_TEXT.reference.label}</span>
        <span class="ws-chip mono tone-muted"><span class="dot"></span>${READ_ONLY_TEXT}</span></li>`;
    }
    const worktree = r.worktree_exists ? 'worktree created' : 'worktree not created yet (Start creates it)';
    return html`<li key=${r.name} class="sl-repo"><span class="sl-repo-name mono">${r.name}</span><span class="rp-tag mono" title=${ROLE_TEXT[roleOf(r)].hover} tabIndex="0">${ROLE_TEXT[roleOf(r)].label}</span>
      <span class=${'ws-chip mono tone-' + tone}><span class="dot"></span>${stateText}</span>
      <span class="sl-muted">${worktree}${r.branch ? ' · branch ' : reattachBranch && r.primary ? ' · continues the branch ' : ' · a new loop branch from '}<span class="mono">${r.branch || (r.primary && reattachBranch) || txt(r.base_branch || r.base)}</span>
        ${!r.branch && !(reattachBranch && r.primary) && roleOf(r) === 'edit' ? html` · starts from <span class="mono">${txt(r.base_branch || r.base)}${r.base_sha ? ` (${String(r.base_sha).slice(0, 7)})` : ''}</span>${r.current_branch && r.current_branch !== (r.base_branch || r.base) ? html`, your folder is on <span class="mono">${r.current_branch}</span>` : null}` : null}
        ${roleOf(r) === 'edit' && d.integration_branch ? html` · PR to <span class="mono">${d.integration_branch}</span>` : null}</span></li>`;
  })}</ul>`;
}
