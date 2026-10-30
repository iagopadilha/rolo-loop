/* Files page (PLAN-02..05, BACK-02): the plan and the files of the project, READ-ONLY. Two panes that scroll inside themselves.
   Left (260px): one merged tree: the plan (stages -> features -> their spec / design / tasks / contract), the project files, the codebase maps
   (docs/codebase and, in a monorepo, each subproject's map) and the loop files worth reading. Right: the selected file as Markdown with its purpose
   in one plain sentence above it. There is no Edit, Refresh or Change with AI here: the plan only changes through Back to planning. The data is
   read again when the page opens and whenever the loop state changes. "Ask about the plan" (scope `plan`) opens a read-only chat in a dialog.
   Reads: GET roadmap, GET specs/tree, GET codebase/tree and GET specs/file?path= (a map file outside .specs is also tried on codebase/file). */
import { html, useEffect } from '../../util/preact.js';
import { list, txt, maskHome, baseName, BADGE_INFO } from '../../util/format.js';
import { openBigDialog } from '../../components/big_dialog.js';
import { Icon, StatusDot } from '../../components/shell.js';
import { Markdown } from '../../components/markdown.js';
import { ctx, status, loop, lastN } from './context.js';
import { renderAll, requestRender } from '../project_panel.js';
import { featureName } from './pipeline_model.js';
import { explainFile } from './file_explanations.js';
import { navigate } from './router.js';

const STALE_MS = 8000;
const CHAT = {
  title: 'Ask about the plan', scope: { type: 'plan' }, placeholder: 'Ask about the plan, the specs, the maps or the decisions…',
  greeting: 'I can answer questions about the plan, the specs, the code maps, the AI decisions and the progress of the loop. I only read: I never change anything.',
  suggestions: ['What is each stage for?', 'Which features are not done yet?', 'What did the AI decide on its own?'],
};

/** Plain labels of the files (PLAN-03): what the file is, then its name. */
const FILE_LABELS = {
  'spec.md': 'Spec — what the feature must do', 'design.md': 'Design — how it is built', 'tasks.md': 'Tasks — the steps',
  'contract.md': 'Contract — what the other side can rely on', 'FLOWS.md': 'Flows — what must keep working',
  'PROJECT.md': 'Project — what is being built', 'ROADMAP.md': 'Roadmap — the plan in stages', 'RULES.md': 'Rules — the limits',
  'STATE.md': 'State — the memory of the loop', 'TEST-ENVIRONMENT.md': 'Test environment — where tests may run',
  'VALIDATE.md': 'AI decisions — what the AI chose alone', 'ANSWERS.md': 'Answers — what you decided',
  'STACK.md': 'Stack — languages and frameworks', 'ARCHITECTURE.md': 'Architecture — layers and data flow', 'CONVENTIONS.md': 'Conventions — naming and style',
  'STRUCTURE.md': 'Structure — where code lives', 'TESTING.md': 'Testing — how tests run', 'INTEGRATIONS.md': 'Integrations — what the code talks to',
  'CONCERNS.md': 'Concerns — debt and risks', 'SYSTEM.md': 'System — how the subprojects fit together',
};
const FEATURE_FILE_ORDER = ['spec.md', 'design.md', 'tasks.md', 'contract.md'];
const PROJECT_FILE_ORDER = ['PROJECT.md', 'ROADMAP.md', 'FLOWS.md', 'RULES.md', 'STATE.md', 'TEST-ENVIRONMENT.md', 'VALIDATE.md', 'ANSWERS.md'];
const PLAIN_STATUS = {
  pending: 'waiting its turn', speccing: 'spec being written', specced: 'spec written', approved: 'ready to build',
  building: 'being built', verified: 'done', blocked: 'needs attention',
};

const store = { roadmap: null, specs: null, codebase: null, files: {}, loading: false, error: null, loadedAt: 0, loopKey: null, selected: null, open: {}, handle: null, wantName: null };

const fileLabel = (name) => FILE_LABELS[name] || FILE_LABELS[Object.keys(FILE_LABELS).find((key) => key.toLowerCase() === String(name).toLowerCase())] || '';
const rank = (order, name) => { const at = order.findIndex((key) => key.toLowerCase() === String(name).toLowerCase()); return at < 0 ? order.length : at; };
const loopKey = () => `${status()}|${lastN()}|${loop().at || ''}`;

// ---------- reading ----------
async function loadAll(force) {
  if (store.loading || (!force && Date.now() - store.loadedAt < STALE_MS)) return;
  store.loading = true; store.error = null; requestRender();
  try {
    const [roadmap, specs, codebase] = await Promise.all([
      ctx.api.get('roadmap', 15000).catch(() => null),
      ctx.api.get('specs/tree', 15000).catch((e) => { store.error = e.message; return null; }),
      ctx.api.get('codebase/tree', 15000).catch(() => null),
    ]);
    store.roadmap = roadmap; store.specs = specs; store.codebase = codebase; store.loadedAt = Date.now();
    if (force) store.files = {};
    selectWanted();
  } finally { store.loading = false; renderAll(); }
}

/** Reads the plan and the files again (a structure or a roadmap job just changed them). */
export function reloadPlanFiles() { return loadAll(true); }

/** Opens the Files page on a project file by name (ROADMAP.md, FLOWS.md...): picks it once the tree is read. */
export function openProjectFile(name) {
  if (ctx.state && ctx.state.planning_required) return; // planning has its own screen: the file pages belong to the loop
  store.wantName = String(name || '');
  navigate('/files');
  loadAll(true);
}
function selectWanted() {
  if (!store.wantName) return;
  const wanted = store.wantName.toLowerCase(), node = projectSection(store.specs).find((item) => item.name.toLowerCase() === wanted);
  store.wantName = null;
  if (node) { store.selected = node; loadFile(node); }
}

/** Reads the text of a file node: .specs files on specs/file; a map outside .specs falls back to codebase/file. */
async function readNodeText(node) {
  const routes = node.source === 'map' ? ['specs/file', 'codebase/file'] : ['specs/file'];
  let firstError = null;
  for (const route of routes) {
    try {
      const response = await ctx.api.get(route + '?path=' + encodeURIComponent(node.path), 15000);
      return response.text != null ? String(response.text) : String(response.content || '');
    } catch (e) { firstError = firstError || e; }
  }
  throw firstError;
}

async function loadFile(node) {
  if (!node || store.files[node.path]) return;
  store.files[node.path] = { loading: true };
  try { store.files[node.path] = { text: await readNodeText(node) }; }
  catch (e) { store.files[node.path] = { error: e.message }; }
  renderAll();
}

// ---------- the merged tree (pure: data in, sections out) ----------
const fileNode = (item, source) => ({ kind: 'file', path: item.path, name: item.name || baseName(item.path), label: fileLabel(item.name || baseName(item.path)), source });

/** Plan section: stages -> features (title, slug, status in plain words, files). `liveStatus(slug)` overrides the ROADMAP status. */
function planSection(roadmap, specs, liveStatus, titleOf) {
  const filesOf = new Map(list(specs && specs.features).map((feature) => [feature.slug, list(feature.files)]));
  const features = list(roadmap && roadmap.features).map((feature) => ({
    kind: 'feature', slug: feature.slug, stage: feature.stage == null || feature.stage === '' ? null : String(feature.stage),
    title: titleOf(feature.slug), state: liveStatus(feature.slug) || feature.status || 'pending', status: PLAIN_STATUS[liveStatus(feature.slug) || feature.status || 'pending'] || txt(feature.status),
    files: list(filesOf.get(feature.slug)).map((item) => fileNode(item, 'specs')).sort((a, b) => rank(FEATURE_FILE_ORDER, a.name) - rank(FEATURE_FILE_ORDER, b.name)),
  }));
  const stages = list(roadmap && roadmap.stages).map((stage) => ({ kind: 'stage', n: String(stage.n), title: stage.title || `Stage ${stage.n}`, features: features.filter((f) => f.stage === String(stage.n)) }));
  const loose = features.filter((feature) => !stages.some((stage) => stage.n === feature.stage));
  if (loose.length) stages.push({ kind: 'stage', n: 'none', title: 'No stage', features: loose });
  return stages;
}

/** Project files (PROJECT, ROADMAP, FLOWS, RULES, STATE, TEST-ENVIRONMENT, then the decisions and answers of the loop folder). */
function projectSection(specs) {
  const items = [...list(specs && specs.project), ...list(specs && specs.loop).filter((item) => /^(VALIDATE|ANSWERS)\.md$/i.test(item.name || baseName(item.path)))];
  return items.map((item) => fileNode(item, 'specs')).sort((a, b) => rank(PROJECT_FILE_ORDER, a.name) - rank(PROJECT_FILE_ORDER, b.name));
}

/** Code maps: `.specs/codebase` plus the groups of GET codebase/tree (root docs/codebase and one per subproject). */
function mapSection(specs, codebase) {
  const groups = [];
  const inSpecs = list(specs && specs.codebase);
  if (inSpecs.length) groups.push({ id: 'specs-codebase', label: 'Code map in .specs', files: inSpecs.map((item) => fileNode(item, 'specs')) });
  for (const group of list(codebase && codebase.groups)) {
    const files = list(group.files).map((item) => fileNode(item, 'map'));
    if (files.length) groups.push({ id: 'map:' + (group.folder || group.label), label: group.label || group.folder || 'Code map', files });
  }
  return groups;
}

/** Loop files worth reading (the summary); VALIDATE/ANSWERS are already under the project files. */
function loopSection(specs) {
  return list(specs && specs.loop).filter((item) => !/^(VALIDATE|ANSWERS)\.md$/i.test(item.name || baseName(item.path))).map((item) => fileNode(item, 'specs'));
}

// ---------- drawing ----------
const DOT_OF = { verified: 'ok', building: 'live', speccing: 'live', blocked: 'bad' };
const isOpen = (key, fallback = true) => (key in store.open ? store.open[key] : fallback);
const toggle = (key, fallback = true) => { store.open[key] = !isOpen(key, fallback); renderAll(); };
const select = (node) => { store.selected = node; loadFile(node); renderAll(); };
/** "Spec — what the feature must do" -> "Spec": the short half of a plain label. */
const shortLabel = (node) => (node.label ? node.label.split(' — ')[0] : node.name);

function FileRow({ node, depth = 0 }) {
  const selected = store.selected && store.selected.path === node.path;
  return html`<button type="button" class=${'fx-row fx-file' + (selected ? ' is-sel' : '')} style=${{ '--depth': depth }} aria-current=${selected ? 'true' : null}
    title=${(node.label ? node.label + ' · ' : '') + maskHome(node.path)} onClick=${() => select(node)}>
    <${Icon} name="file" size=${14} /><span class="fx-name">${shortLabel(node)}</span>${node.label && shortLabel(node) !== node.name ? html`<span class="fx-ext">${node.name}</span>` : null}</button>`;
}

function Fold({ open, onToggle, depth = 0, lead, name, note, children }) {
  return html`<div class="fx-fold"><button type="button" class="fx-row fx-head" style=${{ '--depth': depth }} aria-expanded=${String(open)} onClick=${onToggle}>
    <span class=${'fx-caret' + (open ? ' is-open' : '')} aria-hidden="true"></span>${lead}<span class="fx-name">${name}</span>${note ? html`<span class="fx-note">${note}</span>` : null}</button>
    ${open ? children : null}</div>`;
}

function Feature({ feature }) {
  const key = 'f:' + feature.slug, open = isOpen(key, false);
  return html`<${Fold} open=${open} onToggle=${() => toggle(key, false)} depth=${1} name=${feature.title}
    lead=${html`<span title=${feature.status}><${StatusDot} kind=${DOT_OF[feature.state] || 'idle'} /></span>`}>
    ${feature.files.length ? feature.files.map((node) => html`<${FileRow} key=${node.path} node=${node} depth=${2} />`)
      : html`<div class="fx-empty" style=${{ '--depth': 2 }}>No spec file yet.</div>`}<//>`;
}

function Stage({ stage, first }) {
  const key = 's:' + stage.n, open = isOpen(key, first);
  const done = stage.features.filter((feature) => feature.state === 'verified').length;
  return html`<${Fold} open=${open} onToggle=${() => toggle(key, first)} name=${stage.n === 'none' ? stage.title : `${stage.n} · ${stage.title}`} note=${`${done}/${stage.features.length}`}>
    ${stage.features.map((feature) => html`<${Feature} key=${feature.slug} feature=${feature} />`)}<//>`;
}

function FileGroup({ id, label, files }) {
  const key = 'g:' + id, open = isOpen(key, true);
  return html`<${Fold} open=${open} onToggle=${() => toggle(key, true)} name=${label} note=${String(files.length)}>
    ${files.map((node) => html`<${FileRow} key=${node.path} node=${node} depth=${1} />`)}<//>`;
}

function Tree() {
  const live = Object.fromEntries(list(ctx.state && ctx.state.features).map((feature) => [feature.id, feature.status]));
  const stages = planSection(store.roadmap, store.specs, (slug) => live[slug], featureName);
  const project = projectSection(store.specs), maps = mapSection(store.specs, store.codebase), loopFiles = loopSection(store.specs);
  const firstOpen = stages.findIndex((stage) => stage.features.some((feature) => feature.state !== 'verified'));
  return html`<nav class="fx-tree" aria-label="Plan and files">
    <div class=${'fx-status' + (store.error ? ' is-bad' : '')} aria-live="polite">${store.loading ? 'Reading…' : store.error || ''}</div>
    <div class="fx-sec">Plan</div>
    ${stages.length ? stages.map((stage, index) => html`<${Stage} key=${stage.n} stage=${stage} first=${index === (firstOpen < 0 ? 0 : firstOpen)} />`)
      : html`<div class="fx-empty">${store.loading ? 'Reading…' : 'No roadmap yet.'}</div>`}
    ${project.length ? html`<div class="fx-sec">Project files</div>${project.map((node) => html`<${FileRow} key=${node.path} node=${node} />`)}` : null}
    ${maps.length ? html`<div class="fx-sec">Code maps</div>${maps.map((group) => html`<${FileGroup} key=${group.id} ...${group} />`)}` : null}
    ${loopFiles.length ? html`<div class="fx-sec">Loop</div>${loopFiles.map((node) => html`<${FileRow} key=${node.path} node=${node} />`)}` : null}
  </nav>`;
}

function Reader() {
  const node = store.selected;
  if (!node) return html`<div class="fx-reader"><div class="fx-blank">Pick a file on the left to read it. Everything here is read only; the plan changes through Back to planning.</div></div>`;
  const explanation = explainFile(node.path), entry = store.files[node.path];
  return html`<article class="fx-reader" aria-label=${node.label || node.name}>
    <header class="fx-doc-head"><h2 class="fx-doc-title">${shortLabel(node)}</h2>
      <p class="fx-doc-sub">${explanation ? explanation.what + ' ' + explanation.purpose : 'File kept by the loop or by planning.'}</p>
      <p class="fx-doc-path" title=${maskHome(node.path)}>${maskHome(node.path)}</p></header>
    ${!entry || entry.loading ? html`<div class="fx-blank">Reading…</div>` : entry.error ? html`<div class="fx-blank" role="alert">${'Could not read this file: ' + entry.error}</div>`
      : entry.text.trim() ? html`<div class="md-doc fx-doc"><${Markdown} source=${entry.text} /></div>` : html`<div class="fx-blank">The file is empty.</div>`}
  </article>`;
}

/** The two panes (tree + reader); fills the height its parent gives it. */
export function PlanFilesBody() {
  const key = loopKey();
  // reads when the page opens (loopKey starts null) and again each time the loop state changes
  useEffect(() => { const changed = store.loopKey !== key; store.loopKey = key; loadAll(changed); }, [key]);
  return html`<div class="fx"><${Tree} /><${Reader} /></div>`;
}

/** "Ask about the plan": a read-only chat in a dialog. */
export function openPlanFilesDialog() {
  if (store.handle) return store.handle;
  store.handle = openBigDialog({
    badge: 'Plan', badgeColor: BADGE_INFO, title: 'Ask about the plan', chat: CHAT,
    body: () => html`<div class="fx-blank"><p>Ask about the plan, the specs, the code maps, the AI decisions or the progress of the loop. The answers come from the files you see on the Files page; nothing is ever changed.</p></div>`,
    onClose: () => { store.handle = null; },
  });
  return store.handle;
}
