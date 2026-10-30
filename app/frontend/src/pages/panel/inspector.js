/* Inspector (right column of the LOOP page, 300 px, quiet): "Selected" (the feature or node picked in the pipeline, as it was at the selected turn)
   and "Now - turn <last>" (the orchestrator and the agents running right now: always the live loop). It only summarizes: talking to an agent
   (its history and the chat) happens in the agent dialog, deciding a pause in the pause dialog. */
import { html } from '../../util/preact.js';
import { Markdown, inlineNodes } from '../../components/markdown.js';
import { txt, list, clip, DASH, fmtRelative } from '../../util/format.js';
import { ctx, ui, ROLE_DESC, status, loop, agentById } from './context.js';
import { renderAll } from '../project_panel.js';
import { steps, turnsList, liveN, curN, selectTurn, selectNode, stateAt, activeNode, turnAt, loopStatusAt, featuresWithWaves, featureName, workedTurns, defaultStepOf, selectedFeature, selectFeature, pipe } from './pipeline_model.js';
import { stateWord, turnRangeText, stepWord } from './loop_timeline.js';
import { ClaudeBadge } from './claude_badge.js';
import { openAgentDialog, openPauseDialog } from './dialogs/index.js';
import { FeatureLabel } from './feature_label.js';
import { navigate } from './router.js';
import { StatusDot, dotKind } from '../../components/shell.js';
import { NAVBAR_STATUS, navbarStatusKey } from '../../components/navbar_status.js';

const ORCHESTRATOR_ID = 'orch';

/** Reads the full log of an agent into ui.agentLog (the chat of that agent and the feature history open it). */
export async function fetchAgentLog(id) {
  const mine = () => ui.sel && ui.sel.kind === 'agent' && ui.sel.id === id;
  try {
    const r = await ctx.api.get('agent/' + encodeURIComponent(id) + '/log', 20000);
    if (mine()) { ui.agentLog = { id, log: list(r.log), truncated: !!r.truncated }; ui.logLoading = null; renderAll(); }
  } catch (e) {
    if (mine()) { ui.agentLog = { id, log: [], err: e.message }; ui.logLoading = null; renderAll(); }
  }
}

// ---------- helpers ----------
/** Index of the pipeline step an agent role belongs to (executor-back -> executor, verifier-flow -> verifier); -1 when none. */
function stepIndexOfRole(role) {
  const wanted = String(role || '');
  return steps().findIndex((step) => wanted === step.agent || wanted.startsWith(step.agent + '-'));
}
const firstFeature = (feature) => String(feature || '').split(',')[0].trim();
const isQuarantined = (slug) => list(ctx.state && ctx.state.quarantine).some((q) => q.id === slug);
const NOTE_CLIP = 240;

/** A read-only data field (label + recorded text, a few lines at most): not a chat. The full text and the chat live in the dialog. */
function OutputField({ label, empty = 'No decision recorded for this turn.', children }) {
  const has = children != null && children !== false;
  return html`<div class="in-field"><div class="in-k">${label}</div>
    <div class=${'in-text' + (has ? '' : ' is-none')}>${has ? children : empty}</div></div>`;
}

function KeyValue({ k, children }) { return html`<dt>${k}</dt><dd>${children}</dd>`; }
const Heading = ({ children }) => html`<h3 class="in-h">${children}</h3>`;
/** Name on the left (status dot + text), state in words on the right: the head of every node of the inspector. */
const NodeHead = ({ name, state, tone }) => html`<div class="in-head"><span class="in-name"><${StatusDot} kind=${dotKind(tone || state)} />${name}</span><span class="in-state">${state}</span></div>`;

// ---------- NOW ----------
function OrchestratorCard() {
  const orch = agentById(ORCHESTRATOR_ID), info = NAVBAR_STATUS[navbarStatusKey(status())];
  const stateText = orch ? txt(orch.state) : info.label.toLowerCase();
  const note = orch ? (orch.decision || orch.summary || (orch.last_action && orch.last_action.text)) : null;
  const waiting = status() === 'awaiting_user' ? loop().reason : null;
  const text = clip(note || waiting, NOTE_CLIP);
  return html`<button type="button" class="in-card" title="select the orchestrator" onClick=${() => { selectTurn(null); selectNode({ orch: true }); }}>
    <${NodeHead} name="orchestrator" state=${stateText} tone=${orch ? orch.state : status()} />
    ${text ? html`<span class="in-note">${inlineNodes(text)}</span>` : null}
  </button>`;
}

function RunningList() {
  const running = list(ctx.state && ctx.state.agents).filter((agent) => agent.state === 'running' && agent.id !== ORCHESTRATOR_ID && agent.role !== 'orchestrator');
  const pick = (agent) => { // selects it in the pipeline and opens its dialog (summary, runs, chat)
    selectTurn(null);
    const step = stepIndexOfRole(agent.role), feature = firstFeature(agent.feature);
    if (step >= 0 && feature) { selectNode({ f: feature, s: step }); openAgentDialog({ feature, step }); } else selectNode({ orch: true });
  };
  return html`<${Heading}>${'Running · ' + running.length}<//>
    <div class="in-list">${running.length ? running.map((agent) => html`<button type="button" class="in-row" key=${agent.id} onClick=${() => pick(agent)}>
        <${StatusDot} kind="live" /><span class="in-row-text"><span class="in-row-main">${txt(agent.role)}</span><span class="in-row-sub">${firstFeature(agent.feature) ? html`<${FeatureLabel} slug=${firstFeature(agent.feature)} stacked=${true} />` : txt(agent.feature)}</span></span></button>`)
      : html`<div class="in-none">No agent running.</div>`}</div>`;
}

// ---------- SELECTED ----------
function OrchestratorSelected({ turnNumber }) {
  const turn = turnsList().find((item) => item.n === turnNumber) || null, quarantined = list(ctx.state && ctx.state.quarantine)[0];
  return html`<${NodeHead} name="orchestrator" state=${turn ? txt(turn.status) : DASH} tone=${turn && turn.status} />
    <div class="in-sub">${ROLE_DESC.orchestrator}</div>
    <dl class="insp-dl"><${KeyValue} k="Turn">${turn ? `${turn.n} · ${txt(turn.label)}` : DASH}<//>
      <${KeyValue} k="Calls">${steps().map((step) => step.agent).join(', ') || DASH}<//>
      <${KeyValue} k="Ran with">${turn && turn.claude ? html`<${ClaudeBadge} source=${turn} />` : DASH}<//></dl>
    <${OutputField} label=${'Decision · turn ' + turnNumber}>${turn && turn.decision ? html`<div class="md-doc"><${Markdown} source=${String(turn.decision)} /></div>` : null}<//>
    ${quarantined ? html`<div class="insp-actions"><button type="button" class="btn btn-primary" onClick=${() => openPauseDialog(quarantined.id)}>Understand and resolve</button></div>` : null}`;
}

/** "Open feature": goes to the feature page (#/feature/<slug>). */
const openFeaturePage = (slug) => navigate('/feature/' + encodeURIComponent(slug));

/** Details of one step of the selected feature at the shown turn: who ran it, the runs, the last output. */
function StepDetails({ slug, index, turnNumber }) {
  const stepList = steps(), step = stepList[index];
  if (!step) return null;
  const cells = (turnNumber != null && stateAt(turnNumber)[slug]) || [], cell = cells[index] || { st: 'PENDING', runs: [] };
  const runs = list(cell.runs), lastRun = runs[runs.length - 1] || null;
  const calledBy = index === 0 ? 'orchestrator' : stepList[index - 1].agent, calls = index + 1 < stepList.length ? stepList[index + 1].agent : DASH;
  return html`<section class="in-sec" aria-label="Selected agent">
    <${Heading}>${'Agent · turn ' + (turnNumber == null ? DASH : turnNumber)}<//>
    <${NodeHead} name=${(lastRun && lastRun.agent) || step.agent} state=${cell.st.toLowerCase()} tone=${cell.st} />
    <div class="in-sub">${txt(step.role)}</div>
    <dl class="insp-dl"><${KeyValue} k="Called by">${calledBy}<//><${KeyValue} k="Calls">${calls}<//><${KeyValue} k="Runs">${runs.length}<//>
      ${lastRun && turnAt(lastRun.turn) && turnAt(lastRun.turn).claude ? html`<${KeyValue} k="Ran with"><${ClaudeBadge} source=${turnAt(lastRun.turn)} inherited /><//>` : null}</dl>
    <${OutputField} label=${lastRun ? 'Last output · turn ' + lastRun.turn : 'Last output'} empty="Not run in this state yet.">${lastRun && lastRun.out ? inlineNodes(String(lastRun.out)) : null}<//>
  </section>`;
}

const GLYPH = { DONE: '✓', FAIL: '!', BLOCKED: '!' };
/** The selected feature (spec L-04): name, state, turns, depends on, the steps with small check dots, the two buttons. */
function FeatureSelected({ slug }) {
  const at = curN(), cells = stateAt(at)[slug] || null, status = loopStatusAt(slug, at), worked = workedTurns(at)[slug] || [];
  const feature = featuresWithWaves().find((item) => item.id === slug), deps = feature ? feature.deps.filter((dep) => !String(dep).startsWith('dir:')) : [];
  const node = activeNode(), focus = node && !node.orch && node.f === slug ? node.s : defaultStepOf(slug, at);
  const stepList = steps();
  return html`<section class="in-sec" aria-label="Selected feature">
    <div class="in-name-lg">${featureName(slug)}</div>
    <div class="in-sub">${stateWord(status)} · <span class="mono">${slug}</span></div>
    <dl class="insp-dl"><dt>Turns</dt><dd>${turnRangeText(worked) || 'not dispatched yet'}</dd>
      <dt>After</dt><dd>${deps.length ? deps.map((dep, i) => html`<span key=${dep}>${i ? ', ' : ''}<button type="button" class="insp-dep" onClick=${() => selectFeature(dep)}>${featureName(dep)}</button></span>`) : DASH}</dd></dl>
    <ol class="insp-steps" aria-label="Steps">${stepList.map((step, index) => {
      const cell = cells ? cells[index] : null, st = cell ? cell.st : 'PENDING', runs = cell ? list(cell.runs) : [], lastRun = runs[runs.length - 1] || null;
      const state = st === 'DONE' ? 'done' : st === 'RUNNING' ? 'run' : st === 'FAIL' || st === 'BLOCKED' ? 'bad' : 'todo';
      return html`<li key=${index}><button type="button" class=${'insp-step-row is-' + state + (focus === index ? ' is-focus' : '')} disabled=${!cells} aria-pressed=${String(focus === index)}
        title=${`${stepWord(index)} · ${st}`} onClick=${() => selectNode({ f: slug, s: index })}>
        <span class="insp-dot" aria-hidden="true">${GLYPH[st] || ''}</span><span class="insp-step-name">${stepWord(index)}</span><span class="insp-step-agent">${(lastRun && lastRun.agent) || step.agent}</span></button></li>`;
    })}</ol>
    <div class="insp-actions">
      ${isQuarantined(slug) ? html`<button type="button" class="btn" onClick=${() => openPauseDialog(slug)}>Understand and resolve</button>` : null}
      <button type="button" class="btn btn-primary" onClick=${() => openAgentDialog({ feature: slug, step: Math.max(0, focus) })}>Ask the agent</button>
      <button type="button" class="btn" onClick=${() => openFeaturePage(slug)}>Open feature</button>
    </div>
  </section>
  ${cells && focus >= 0 ? html`<${StepDetails} slug=${slug} index=${focus} turnNumber=${at} />` : null}`;
}

function SelectedCard() {
  const node = activeNode(), turnNumber = curN(), slug = selectedFeature();
  if (node && node.orch) return html`<section class="in-sec" aria-label="Selected node"><${Heading}>${'Selected · turn ' + (turnNumber == null ? DASH : turnNumber)}<//><${OrchestratorSelected} turnNumber=${turnNumber} /></section>`;
  if (slug) return html`<${FeatureSelected} slug=${slug} />`;
  return html`<section class="in-sec" aria-label="Selected node"><${Heading}>Selected<//><div class="in-none">Pick a feature on the timeline to inspect it at this turn.</div></section>`;
}

/** Processes the engine started and has running right now (state.processes): what, which turn, pid, since when, which account. They run
    hidden, so this card is the only place they show: nothing opens on the user's screen. */
function ProcessesCard() {
  const items = list(ctx.state && ctx.state.processes);
  return html`<section class="in-sec" aria-label="Running processes">
    <${Heading}>${'Processes · ' + items.length}<//>
    ${items.length ? items.map((item) => html`<div class="in-proc" key=${item.id}>
      <div class="in-proc-head"><span class="mono">${txt(item.claude || item.kind)}</span><span class="mono">${item.turn != null ? 'T' + item.turn : ''}</span></div>
      <div class="in-proc-sub mono">${['pid ' + txt(item.pid), 'started ' + fmtRelative(item.started_at), item.claude_account ? 'account ' + txt(item.claude_account) : ''].filter(Boolean).join(' · ')}</div>
      ${list(item.lanes).length ? html`<div class="in-proc-sub">${list(item.lanes).map((lane) => txt(lane.feature) + (lane.step ? ' · ' + txt(lane.step) : '')).join(' ; ')}</div>` : null}</div>`)
      : html`<div class="in-none">Nothing is running.</div>`}
    <div class="in-proc-sub">They run hidden: no window opens on your screen.</div>
  </section>`;
}

/** The whole inspector. `on` = the loop panel shows (state arrived and the project is not in planning mode). */
export function Inspector({ on }) {
  const latest = liveN();
  const close = () => { pipe.over = false; renderAll(); };
  return html`<aside class=${'insp' + (pipe.insp ? ' is-open' : '') + (pipe.over ? ' is-over' : '')} id="inspector" aria-label="Inspector">
    <button type="button" class="insp-close" aria-label="Close the inspector" onClick=${close}>×</button>
    <div class="insp-body" id="insp-body">
      ${!on ? null : html`<${ProcessesCard} /><${SelectedCard} />
        <section class="in-sec" aria-label="Loop now">
          <${Heading}>${'Now · turn ' + (latest == null ? DASH : latest)}<//>
          <${OrchestratorCard} /><${RunningList} />
        </section>`}
    </div>
  </aside>`;
}
