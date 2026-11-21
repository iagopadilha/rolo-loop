/* Root of the project panel (/p/<id>/): the whole page as one Preact tree inside the v2 shell (components/shell.js): the sidebar, then a
   column with the offline banner, the header (52px, breadcrumb + run status + actions) and ONE centre page. Before the first /state only
   the frame shows (a splash covers it).
   The page comes from the hash route (router.js) through the PAGES registry: loop (#/loop, default), features, feature (#/feature/<slug>),
   decisions, files, errors, settings. Every sidebar item is a route; there is no side zone and never a second column.
   Two modes (MODES): loop (roadmap approved: header + the page of the route; the status dialog shows what needs the user and where the
   project stands) and planning (state.planning_required: the PlanningScreen takes the place of the page; Settings still shows over it).
   Everything lives inside `.shell-col > .pg | main#loop-zone | .fp | #planning`; they scroll inside themselves, the page never scrolls. */
import { html } from '../../util/preact.js';
import { disconnectedDetail } from '../../api/api_client.js';
import { openWhy } from '../../components/why_panel.js';
import { Sidebar, ShellHeader, OverflowMenu, Splash, useSplash, dotKind } from '../../components/shell.js';
import { NAVBAR_STATUS, navbarStatusKey } from '../../components/navbar_status.js';
import { ctx, ui, status, projectName } from './context.js';
import { HeaderStatus, PrimaryAction, OverflowActions } from './header.js';
import { errorCount, openErrors } from './errors.js';
import { idleText, StatusDialog, statusHasDialog, openStatusDialog } from './alert.js';
import { PlanningScreen, planningRequired } from './planning.js';
import { LoopPage } from './loop_zone.js';
import { FeaturesPage, DecisionsPage, FilesPage, ErrorsPage } from './pages.js';
import { accountChip } from './claude_account_chip.js';
import { FeaturePage } from './feature_page.js';
import { SettingsPage } from '../settings_page.js';
import { currentRoute, navigate } from './router.js';

/** Content of #closed: "App shut down" (header) or "Project not found" (polling 404). */
function ClosedCard({ c }) {
  if (c.kind === 'shutdown') {
    return html`<div class="closed-card" role="alert"><div class="closed-title">App shut down</div>
      <div class="muted">The server was shut down. You can close this tab. Loops already started keep running.</div></div>`;
  }
  return html`<div class="closed-card" role="alert">
    <div class="closed-title">Project not found</div>
    <div class="muted">${c.message || 'This address does not match a project registered in the app (it may have been forgotten).'}</div>
    <a class="btn btn-primary" href="/">Back to the projects</a>
  </div>`;
}

function MenuHelp() { return html`<button type="button" class="btn btn-ghost" id="btn-help" onClick=${openWhy}>Why Rolo Loop?</button>`; }

// ---------- pages and sidebar (real state only) ----------
/** Registry route name -> page. `feature` keys on the slug so another feature remounts the page. */
const PAGES = {
  loop: () => html`<${LoopPage} />`,
  features: () => html`<${FeaturesPage} />`,
  feature: (route) => html`<${FeaturePage} key=${route.slug} slug=${route.slug} />`,
  decisions: () => html`<${DecisionsPage} />`,
  files: () => html`<${FilesPage} />`,
  errors: () => html`<${ErrorsPage} />`,
  settings: () => html`<${SettingsPage} />`,
};
const PAGE_LABEL = { loop: 'Loop', features: 'Features', feature: 'Features', decisions: 'Decisions', files: 'Files', errors: 'Errors', settings: 'Settings' };
/** Sidebar item highlighted for a route (the Feature page highlights Features). */
const navItemOf = (route) => (route.name === 'feature' ? 'features' : route.name);

function PanelSidebar({ has, planning }) {
  const name = (has && projectName()) || 'Project';
  const counts = (ctx.state && ctx.state.counts) || {};
  const errors = errorCount();
  const on = has && !planning;
  const route = currentRoute();
  const items = planning
    ? [{ id: 'planning', icon: 'plan', label: 'Planning', onClick: () => navigate('/loop') }]
    : on ? [
      { id: 'loop', icon: 'loop', label: 'Loop', onClick: () => navigate('/loop') },
      { id: 'features', icon: 'features', label: 'Features', count: counts.total || '', onClick: () => navigate('/features') },
      { id: 'decisions', icon: 'decisions', label: 'Decisions', onClick: () => navigate('/decisions') },
      { id: 'files', icon: 'file', label: 'Files', onClick: () => navigate('/files') },
      { id: 'errors', icon: 'alert', label: 'Errors', count: errors || '', onClick: openErrors },
    ] : [];
  const current = planning ? (route.name === 'settings' ? null : 'planning') : navItemOf(route);
  const project = has ? { id: ctx.projectId, name, dot: dotKind(status()), items, current } : null;
  const attention = on && NAVBAR_STATUS[navbarStatusKey(status())].attention;
  const activity = on && statusHasDialog() ? { count: attention ? 1 : 0, onClick: openStatusDialog } : null;
  const commands = items.filter((item) => item.onClick).map((item) => ({ label: item.label, hint: item.id === 'files' ? 'plan, specs and decisions (read only)' : '', run: item.onClick }));
  return html`<${Sidebar} page="panel" project=${project} account=${accountChip()} activity=${activity} commands=${commands} />`;
}

/** The header of both modes: breadcrumb, ONE status pill, the primary action, the "..." menu with the rest, the theme toggle (inside the menu when compact).
    Same structure wide and compact, so nothing moves between breakpoints but the theme toggle. */
function PanelHeader({ has, mode }) {
  const planning = mode === 'planning';
  const crumbs = [{ label: has ? projectName() || 'Project' : 'Project' }, { label: planning ? 'Planning' : PAGE_LABEL[currentRoute().name] || 'Loop' }];
  const status_ = has ? html`<${HeaderStatus} />`
    : html`<div class="nb-status nb-tone-muted" id="navbar-status" role="status"><span class="dot"></span><span class="nb-status-text">Connecting…</span></div>`;
  const actions = html`<div class="header-actions" id="header-actions">${has ? html`<${PrimaryAction} mode=${mode} />` : null}</div>`;
  const mid = (compact) => (compact ? html`<div class="sh-row2">${status_}${actions}</div>` : status_);
  const end = (compact) => html`${compact ? null : actions}<${OverflowMenu} theme=${compact}>${has ? html`<${OverflowActions} mode=${mode} />` : null}<${MenuHelp} /><//>`;
  return html`<${ShellHeader} crumbs=${crumbs} mid=${mid} end=${end} cls=${'navbar' + (planning ? ' nb-planning' : '')} />`;
}
const LoopHeader = (props) => html`<${PanelHeader} ...${props} mode="loop" />`;
const PlanningHeader = (props) => html`<${PanelHeader} ...${props} mode="planning" />`;
/** Loop mode: only the status dialog (opens from the header indicator, the sidebar Activity item or by itself on a question). */
function LoopTop({ has }) { return has ? html`<${StatusDialog} />` : null; }
/** Planning mode: the PlanningScreen (hidden, not unmounted, while Settings shows over it). */
function PlanningTop({ hidden }) {
  return html`<div id="chrome-zones" class="chrome-zones is-open" hidden>
      <div id="workspace-zone" class="workspace-zone" hidden></div>
    </div>
    <section id="planning" class="planning-zone" aria-label="Planning" hidden=${hidden}><${PlanningScreen} /></section>`;
}

const MODES = {
  loop: { Header: LoopHeader, Top: LoopTop },
  planning: { Header: PlanningHeader, Top: PlanningTop },
};

export function Panel() {
  const has = !!ctx.state, planning = has && planningRequired(), on = has && !planning;
  const route = currentRoute();
  const { Header, Top } = MODES[planning ? 'planning' : 'loop'];
  const splash = useSplash(has || ctx.offline || !!ui.closed);
  const settings = has && route.name === 'settings';
  const page = settings ? PAGES.settings(route) : on ? (PAGES[route.name] || PAGES.loop)(route) : null;
  return html`<div id="app" class="shell" data-mode=${planning ? 'planning' : null}>
    <${PanelSidebar} has=${has} planning=${planning} />
    <div class="shell-col" id="shell-col">
      <div id="banner-offline" class="banner banner-offline" role="alert" hidden=${!ctx.offline}>
        <span class="dot dot-red"></span>
        <strong>App disconnected</strong>
        <span id="banner-offline-detail">${ctx.offline ? disconnectedDetail(ctx.offlineMsg) : ''}</span>
      </div>

      <${Header} has=${has} />
      <${Top} has=${has} hidden=${settings} />
      ${page}

      <div id="idle-note" class="idle-note mono">${on ? idleText() : ''}</div>
    </div>
    <div id="closed" class="closed" hidden=${!ui.closed}>${ui.closed ? html`<${ClosedCard} c=${ui.closed} />` : null}</div>
    ${splash ? html`<${Splash} text=${ctx.offline ? 'Waiting for the app…' : 'Loading the project…'} />` : null}
  </div>`;
}
