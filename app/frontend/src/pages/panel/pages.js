/* Centre pages of the project panel next to the Loop and the Feature page (routes in router.js, registry in app.js):
   Features (#/features), Decisions (#/decisions), Files (#/files), Errors (#/errors).
   All four are built from the same primitives (components/page.js: Page, Segmented, Section, List, Row, State, Chip, EmptyState). */
import { html, useEffect } from '../../util/preact.js';
import { list, maskHome } from '../../util/format.js';
import { Markdown } from '../../components/markdown.js';
import { Page, Segmented, Section, List, Row, State, EmptyState } from '../../components/page.js';
import { renderAll } from '../project_panel.js';
import { features } from './context.js';
import { pipe, loadPipeline, steps, curN, liveN, turnAt, emptyReason, workedTurns } from './pipeline_model.js';
import { groupStages, featureStatusInfo, QuarantineList } from './roadmap_sidebar.js';
import { PlanActions } from './progress_trail.js';
import { PlanFilesBody, openPlanFilesDialog } from './plan_files_dialog.js';
import { openAgentDialog } from './dialogs/index.js';
import { DependencyDiagram } from './dependency_diagram.js';
import { chipKind } from './feature_page.js';
import { featureParts, FeatureLabel } from './feature_label.js';
import { TurnDecisions, AnswersList } from './decisions.js';
import { ProblemsView } from './problems.js';
import { loadErrors, errorCount } from './errors.js';
import { TurnPicker, turnRangeText } from './loop_timeline.js';
import { navigate } from './router.js';

const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;

// ---------- Features ----------
const fp = { view: 'list' }; // list | deps
const FEATURE_VIEWS = [['list', 'List'], ['deps', 'Dependencies']];
const FEATURE_COLS = 'minmax(0,1fr) 110px 110px 130px 20px', FEATURE_COLS_NARROW = 'minmax(0,1fr) 96px 20px';

const featureHref = (slug) => '#/feature/' + encodeURIComponent(slug);
const openFeature = (slug) => navigate('/feature/' + encodeURIComponent(slug));

function FeatureRow({ feature, worked }) {
  const info = featureStatusInfo(feature), kind = chipKind(feature.id, feature.status), parts = featureParts(feature.id);
  const deps = list(feature.deps).filter((dep) => !String(dep).startsWith('dir:'));
  return html`<${Row} href=${featureHref(feature.id)} onOpen=${() => openFeature(feature.id)} title=${parts.tip}>
    <span class="ls-main"><span class="ls-line"><span class="ls-title">${parts.name}</span>${parts.same ? null : html`<span class="ls-slug">${parts.slug}</span>`}</span></span>
    <span title=${info.legend}><${State} kind=${kind}>${info.label}<//></span>
    <span class="ls-meta ls-opt">${turnRangeText(worked[feature.id]) || '—'}</span>
    <span class="ls-meta ls-opt" title=${deps.map((dep) => featureParts(dep).name).join(', ')}>${deps.length ? deps.length === 1 ? '1 dependency' : deps.length + ' dependencies' : 'No dependencies'}</span><//>`;
}

function FeatureList() {
  const groups = groupStages(), worked = workedTurns(liveN());
  if (!groups.length) return html`<${EmptyState} text="The roadmap has no feature yet." action=${{ label: 'Open the plan files', onClick: () => navigate('/files') }} />`;
  return html`<div class="pg-stack">
    <${QuarantineList} />
    ${groups.map((group) => html`<${Section} key=${String(group.n)} title=${group.title} note=${`${group.items.filter((f) => f.status === 'verified').length} of ${group.items.length} done`}>
      <${List} label=${group.title} cols=${FEATURE_COLS} narrow=${FEATURE_COLS_NARROW}>
        ${group.items.map((feature) => html`<${FeatureRow} key=${feature.id} feature=${feature} worked=${worked} />`)}<//>
    <//>`)}
  </div>`;
}

export function FeaturesPage() {
  const total = features().length, done = features().filter((f) => f.status === 'verified').length, deps = fp.view === 'deps';
  const tools = html`<${PlanActions} />
    <${Segmented} items=${FEATURE_VIEWS} value=${fp.view} label="Features view" onPick=${(key) => { fp.view = key; renderAll(); }} />`;
  return html`<${Page} name="features" title="Features" sub=${total ? `${done} of ${total} verified` : 'The roadmap has no feature yet.'} tools=${tools} fill=${deps} wide=${deps}>
    ${deps ? html`<div class="pg-fill-inner"><${DependencyDiagram} /></div>` : html`<${FeatureList} />`}
  <//>`;
}

// ---------- Decisions ----------
const agentNameOf = (event) => event.agent || (steps()[event.step] && steps()[event.step].agent) || 'agent';
const openEvent = (event) => (event.feature && event.step >= 0 ? () => openAgentDialog({ feature: event.feature, step: event.step }) : null);
const EVENT_KIND = { DONE: 'ok', RUNNING: 'live', FAIL: 'bad', BLOCKED: 'bad', QUEUED: 'idle', PENDING: 'idle' };
const EVENT_COLS = 'minmax(0,1fr) 150px 110px 20px', EVENT_COLS_NARROW = 'minmax(0,1fr) 96px 20px';
const FILE_COLS = 'minmax(0,1fr) 90px', FILE_COLS_NARROW = 'minmax(0,1fr) 70px';

/** The orchestrator decision of the turn, the events it produced, the AI decisions to review and the files its commits changed. */
function TurnLog({ turn }) {
  return html`<div class="pg-stack">
    <${Section} title="What the loop decided">
      ${turn.decision ? html`<div class="pg-card md-doc"><${Markdown} source=${String(turn.decision)} /></div>` : html`<${EmptyState} text="No decision text recorded for this turn." />`}
    <//>
    <${Section} title="Events" note=${turn.events.length ? String(turn.events.length) : null}>
      ${turn.events.length ? html`<${List} label="Events of the turn" cols=${EVENT_COLS} narrow=${EVENT_COLS_NARROW}>
        ${turn.events.map((event, index) => html`<${Row} key=${index} onOpen=${openEvent(event)} title=${event.out || null}>
          <span class="ls-main"><span class="ls-title">${event.feature ? html`<${FeatureLabel} slug=${event.feature} />` : '—'}</span>${event.out ? html`<span class="ls-sub" title=${event.out}>${event.out}</span>` : null}</span>
          <span class="ls-meta ls-opt">${agentNameOf(event)}</span>
          <span><${State} kind=${EVENT_KIND[event.status] || 'idle'}>${String(event.status).toLowerCase()}<//></span><//>`)}<//>`
        : html`<${EmptyState} text="No events recorded in this turn." />`}
    <//>
    <${Section} title="To review"><${TurnDecisions} turn=${turn.n} /><//>
    <${Section} title="Files changed" note=${turn.files.length ? String(turn.files.length) : null}>
      ${turn.files.length ? html`<${List} label="Files changed in the turn" cols=${FILE_COLS} narrow=${FILE_COLS_NARROW}>
        ${turn.files.map((file) => html`<${Row} key=${file.path} title=${maskHome(file.path)}>
          <span class="ls-main"><span class="ls-title ls-path">${maskHome(file.path)}</span></span>
          <span class="ls-meta ls-end"><span class="ls-add">+${file.add == null ? 0 : file.add}</span> <span class="ls-del">−${file.del == null ? 0 : file.del}</span></span><//>`)}<//>`
        : html`<${EmptyState} text="No file changes recorded for this turn." />`}
    <//>
    <${AnswersList} />
  </div>`;
}

export function DecisionsPage() {
  const reason = emptyReason(), turn = reason ? null : turnAt(curN());
  return html`<${Page} name="decisions" title="Decisions" sub=${turn ? `Turn ${turn.n}${turn.label ? ' · ' + turn.label : ''}` : 'What the loop decided and did, turn by turn.'} tools=${html`<${TurnPicker} />`}>
    ${turn ? html`<${TurnLog} key=${turn.n} turn=${turn} />`
      : html`<${EmptyState} text=${reason || 'No turn to show.'} action=${pipe.err && !pipe.loading ? { label: 'Retry', onClick: () => loadPipeline(true) } : null} />`}
  <//>`;
}

// ---------- Files ----------
export function FilesPage() {
  const tools = html`<button type="button" class="btn btn-sm" id="btn-ask-plan" title="Ask a question about the plan, the specs and the decisions (read only)" onClick=${openPlanFilesDialog}>Ask about the plan</button>`;
  return html`<${Page} name="files" title="Files" sub="The plan, the specs, the code maps and the loop files. Read only." tools=${tools} fill=${true} wide=${true}>
    <div class="pg-fill-inner"><${PlanFilesBody} /></div>
  <//>`;
}

// ---------- Errors ----------
const ep = { scope: 'all' }; // all | turn
export function ErrorsPage() {
  useEffect(() => { loadErrors(true); }, []);
  const at = curN(), scope = ep.scope === 'turn' && at != null ? 'turn' : 'all', count = errorCount();
  const scopes = [['all', 'Every turn'], ...(at != null ? [['turn', 'Turn ' + at]] : [])];
  const tools = html`<${Segmented} items=${scopes} value=${scope} label="Scope of the list" onPick=${(key) => { ep.scope = key; renderAll(); }} />`;
  return html`<${Page} name="errors" title="Errors" sub=${count ? plural(count, 'problem') + ' to look at' : 'Nothing to fix. Errors and guard blocks show here.'} tools=${tools}>
    <${ProblemsView} turn=${scope === 'turn' ? at : null} />
  <//>`;
}
