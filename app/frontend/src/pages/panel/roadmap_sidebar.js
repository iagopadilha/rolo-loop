/* Plan helpers of the Features page: the stage grouping, the status words of a feature and the quarantined features that need you.
   The plan files are read in the Files page (plan_files_dialog.js); the decisions in decisions.js. */
import { html } from '../../util/preact.js';
import { isNum, txt, list } from '../../util/format.js';
import { List, Row, Section, State } from '../../components/page.js';
import { ctx, ui, live, features } from './context.js';
import { openPauseDialog } from './dialogs/index.js';

export function groupStages() {
  const fs = features(), titles = {};
  for (const e of list(ctx.state && ctx.state.stages)) titles[e.n] = e.title;
  const groups = new Map();
  for (const f of fs) { const n = isNum(f.stage) ? f.stage : null; if (!groups.has(n)) groups.set(n, []); groups.get(n).push(f); }
  return [...groups.entries()].sort((a, b) => (a[0] == null) - (b[0] == null) || a[0] - b[0])
    .map(([n, items]) => ({ n, title: n == null ? 'No stage' : titles[n] || `Stage ${String(n).padStart(2, '0')}`, items }));
}

/** Short reader-facing status of a feature: [label, legend]. */
const FEATURE_STATUS = {
  pending: ['pending', 'waiting its turn'], speccing: ['spec', 'spec being written'], specced: ['spec ok', 'spec written, awaiting approval'],
  approved: ['ready', 'spec approved, ready to run'], building: ['running', 'being implemented'], verified: ['done', 'implemented and verified'],
  blocked: ['failed', 'blocked or failed, needs attention'],
};
/** Status of a feature: a building feature with a running verifier agent shows as "verifying". */
export function featureStatusInfo(feature) {
  const verifying = feature.status === 'building' && list(ctx.state && ctx.state.agents).some((a) => a.feature === feature.id && /verifier/.test(a.role || '') && a.state === 'running');
  if (verifying) return { label: 'verifying', legend: 'being verified (tests, build)', key: 'queued' };
  const [label, legend] = FEATURE_STATUS[feature.status] || [txt(feature.status), ''];
  return { label, legend, key: feature.status };
}

/** Features in quarantine: one row each that opens the pause dialog (the decision and its context live there, not here). */
export function QuarantineList() {
  const quarantined = ctx.state && live() ? list(ctx.state.quarantine) : [];
  if (!quarantined.length) return null;
  return html`<${Section} title="Needs you" note=${String(quarantined.length)}>
    <${List} label="Features in quarantine" cols="minmax(0,1fr) 120px 20px" narrow="minmax(0,1fr) 20px">
      ${quarantined.map((item) => html`<${Row} key=${item.id} onOpen=${() => openPauseDialog(item.id)} title="Understand and resolve">
        <span class="ls-main"><span class="ls-title">${item.id}</span><span class="ls-sub" title=${txt(item.reason)}>${txt(item.reason)}</span></span>
        <span class="ls-meta ls-opt"><${State} kind="bad">quarantine<//></span><//>`)}<//><//>`;
}
