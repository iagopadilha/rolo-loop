/* Settings > Agents and quality: the agents of the loop's pipeline (model, on/off for the optional ones), a preview of what one feature goes
   through, and the quality gate checks (block, warn, off). GET/POST /api/app/settings/agents and /api/app/settings/quality.
   The engine hands both to the orchestrator every turn (`.specs/project/loop/internal/policy.json`). */
import { html, useState, useEffect } from '../util/preact.js';
import { appApi } from '../api/api_client.js';
import { toast } from '../components/toast.js';
import { SettingsGroup, SettingsRow, SettingsBlock, Toggle, SelectField, Note, Loading } from '../components/settings_ui.js';

const MODEL_LABEL = { '': 'Default', opus: 'Opus', sonnet: 'Sonnet', haiku: 'Haiku' };
const ROLE = {
  'brownfield-mapper': 'Maps an existing codebase once (stack, architecture, conventions) so the other agents read it on demand.',
  'spec-writer': 'Writes the spec of one feature: requirements with IDs, acceptance criteria, design and tasks.',
  'spec-reviewer': 'Reviews the spec before any code (gate). Off: the spec is checked by code only (harness spec-lint).',
  executor: 'Implements the feature from the approved spec, inside its paths, with the frontend-dev / backend-dev skills.',
  verifier: 'Fallback verification when the toolkit cannot verify by code (tests, build, typecheck, lint).',
  'verifier-flow': 'Validates the user flow end to end (API with curl, UI with Playwright) when E2E is on.',
  'quality-gate': 'Reviews the finished diff: spec, security, standards, naming, size and patterns. Blocks or warns per check.',
};
const STEPS = [
  ['Spec', 'spec-writer'], ['Gate', 'spec-reviewer'], ['Build', 'executor'], ['Verify', 'verifier'], ['Quality', 'quality-gate'], ['Commit', null],
];
const LEVEL_LABEL = { block: 'Block', warn: 'Warn', off: 'Off' };

function Pipeline({ agents }) {
  const byName = Object.fromEntries(agents.map((a) => [a.name, a]));
  return html`<ol class="st-pipeline" aria-label="What one feature goes through">${STEPS.map(([step, name]) => {
    const agent = name ? byName[name] : null;
    const off = agent && !agent.enabled;
    const model = agent ? MODEL_LABEL[agent.model || ''] === 'Default' ? (agent.default_model || 'default') : MODEL_LABEL[agent.model] : 'the loop';
    return html`<li key=${step} class=${off ? 'is-off' : ''}><span class="st-pipeline-step">${step}</span>
      <span class="st-pipeline-who">${name ? (off ? 'skipped' : name) : 'engine'}</span><span class="st-pipeline-model">${off ? '' : model}</span></li>`;
  })}</ol>`;
}

export function AgentsSection() {
  const [data, setData] = useState(null);
  const [error, setError] = useState('');
  useEffect(() => { appApi.get('settings/agents').then(setData).catch((e) => setError(e.message)); }, []);
  if (error) return html`<${Note} kind="err">${error}<//>`;
  if (!data) return html`<${Loading} text="Reading the agents…" />`;
  const save = async (name, patch) => {
    const next = data.agents.map((a) => (a.name === name ? { ...a, ...patch } : a));
    const body = { agents: Object.fromEntries(next.map((a) => [a.name, { enabled: a.enabled, model: a.model || '' }])) };
    try { setData(await appApi.post('settings/agents', body)); toast('Saved. The next turn follows it.', 'ok'); } catch (e) { toast(e.message, 'err'); }
  };
  return html`
    <${SettingsGroup} id="ag-preview" title="What a loop runs" desc="Each feature goes through these steps, one lane per feature, several lanes at once.">
      <${SettingsBlock}><${Pipeline} agents=${data.agents} /><//>
    <//>
    <${SettingsGroup} id="ag-list" title="Agents" desc="The model each agent runs with and the optional ones you can turn off. Default keeps the agent's own model.">
      ${!data.folder_found ? html`<${SettingsBlock}><${Note} kind="warn">The plugin's agents folder was not found: install the plugin (Settings > System).<//><//>` : null}
      ${data.agents.map((a) => html`<${SettingsRow} key=${a.name} rowKey=${a.name} label=${a.name} desc=${ROLE[a.name] || a.description} labelFor=${'ag-model-' + a.name}
        tags=${a.optional ? [{ text: 'optional' }] : [{ text: 'pipeline' }]}>
        <div class="st-actions">
          <${SelectField} id=${'ag-model-' + a.name} label=${'Model of ' + a.name} value=${a.model || ''} disabled=${!a.enabled}
            options=${Object.keys(MODEL_LABEL).map((m) => ({ value: m, label: m ? MODEL_LABEL[m] : 'Default (' + (a.default_model || 'inherit') + ')' }))} onChange=${(v) => save(a.name, { model: v })} />
          ${a.optional ? html`<${Toggle} id=${'ag-on-' + a.name} label=${'Run ' + a.name} checked=${a.enabled} onChange=${(v) => save(a.name, { enabled: v })} />` : null}
        </div><//>`)}
    <//>`;
}

export function QualitySection() {
  const [data, setData] = useState(null);
  const [error, setError] = useState('');
  useEffect(() => { appApi.get('settings/quality').then(setData).catch((e) => setError(e.message)); }, []);
  if (error) return html`<${Note} kind="err">${error}<//>`;
  if (!data) return html`<${Loading} text="Reading the quality gate…" />`;
  const save = async (id, level) => {
    const checks = Object.fromEntries(data.checks.map((c) => [c.id, c.id === id ? level : c.level]));
    try { setData(await appApi.post('settings/quality', { checks })); toast('Saved. The next quality gate uses it.', 'ok'); } catch (e) { toast(e.message, 'err'); }
  };
  return html`<${SettingsGroup} id="qg-checks" title="Quality gate checks"
      desc="After lint and verification, the quality-gate agent reviews each finished feature once. Block sends the feature back to the executor; Warn only reports.">
    ${data.checks.map((c) => html`<${SettingsRow} key=${c.id} rowKey=${c.id} label=${c.id[0].toUpperCase() + c.id.slice(1)} desc=${c.text}>
      <div class="st-seg" role="radiogroup" aria-label=${'Level of the ' + c.id + ' check'}>${data.levels.map((level) => html`<button key=${level} type="button" role="radio"
        aria-checked=${String(c.level === level)} tabindex=${c.level === level ? 0 : -1} class=${'st-seg-opt' + (c.level === level ? ' is-on' : '')} onClick=${() => save(c.id, level)}>${LEVEL_LABEL[level]}</button>`)}</div><//>`)}
  <//>`;
}
