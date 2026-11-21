/* Settings > AI providers: the providers a loop runs with, in priority order (the first enabled one runs; on a usage limit or an account that
   cannot sign in the loop moves to the next). GET/POST /api/app/settings/providers, POST .../{id}/secret, POST .../{id}/test.
   Secrets (tokens, keys) are typed here, saved encrypted by the server and only ever shown masked. */
import { html, useState, useEffect } from '../util/preact.js';
import { appApi } from '../api/api_client.js';
import { toast } from '../components/toast.js';
import { maskHome } from '../util/format.js';
import { SettingsGroup, SettingsRow, SettingsBlock, Toggle, SelectField, TextField, Note, Loading } from '../components/settings_ui.js';

const KIND_INFO = {
  'claude-subscription': { label: 'Claude subscription', desc: 'The claude CLI signed in with your plan (Pro, Max, Team). No key: run `claude` once and log in.' },
  'claude-oauth-token': { label: 'Claude token', desc: 'A long-lived token from `claude setup-token`: your subscription, without a browser login on this machine.' },
  'anthropic-api-key': { label: 'Anthropic API key', desc: 'Pay per use with an API key from the Anthropic Console (billed per token).' },
  'anthropic-gateway': { label: 'Gateway (Anthropic compatible)', desc: 'A base URL that speaks the Anthropic API: LiteLLM, a Bedrock or Vertex proxy, or local models behind a gateway.' },
  'custom-cli': { label: 'Local agent (Claude Code compatible)', desc: 'Another command that accepts Claude Code\'s headless flags, with its own arguments (e.g. --dangerously-skip-permissions). Use Test to check it.' },
};
const MODEL_LABEL = { '': 'Default', opus: 'Opus', sonnet: 'Sonnet', haiku: 'Haiku' };
const newId = (kind, taken) => { let n = 1; const base = kind.split('-')[0]; while (taken.includes(base + '-' + n)) n += 1; return base + '-' + n; };

function ProviderCard({ p, index, count, saved, onChange, onMove, onRemove, onSecretSaved }) {
  const [secret, setSecret] = useState('');
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState(null);
  const info = KIND_INFO[p.kind] || { label: p.kind, desc: '' };
  const set = (patch) => onChange({ ...p, ...patch });
  const models = p.models || { orchestrator: '', agents: '' };
  const saveSecret = async () => {
    try { await appApi.post('settings/providers/' + encodeURIComponent(p.id) + '/secret', { value: secret }); setSecret(''); toast(secret ? 'Key saved (encrypted).' : 'Key removed.', 'ok'); onSecretSaved(); }
    catch (e) { toast(e.message, 'err'); }
  };
  const test = async () => {
    setTesting(true); setResult(null);
    try { setResult(await appApi.post('settings/providers/' + encodeURIComponent(p.id) + '/test', {}, 150000)); }
    catch (e) { setResult({ ok: false, error: e.message }); }
    finally { setTesting(false); }
  };
  const status = !p.enabled ? 'Off' : (p.problems || []).length ? 'Needs setup' : 'Ready';
  return html`<${SettingsGroup} id=${'pv-' + p.id} title=${(index + 1) + '. ' + (p.name || info.label)} desc=${info.desc}>
    <${SettingsRow} label="Status" desc=${(p.problems || []).join(' · ') || (p.path ? 'Command found: ' + maskHome(p.path) : '')}
      tags=${[{ text: status, tone: status === 'Ready' ? 'ok' : status === 'Off' ? '' : 'warn' }]}>
      <div class="st-actions">
        <button type="button" class="btn btn-sm" disabled=${index === 0} aria-label="Move up" title="Run earlier" onClick=${() => onMove(-1)}>↑</button>
        <button type="button" class="btn btn-sm" disabled=${index === count - 1} aria-label="Move down" title="Run later" onClick=${() => onMove(1)}>↓</button>
        <${Toggle} id=${'pv-on-' + p.id} checked=${p.enabled} label="Enabled" onChange=${(v) => set({ enabled: v })} />
      </div>
    <//>
    <${SettingsRow} label="Name" labelFor=${'pv-name-' + p.id}><${TextField} id=${'pv-name-' + p.id} value=${p.name} onInput=${(v) => set({ name: v })} /><//>
    <${SettingsRow} label="Type" labelFor=${'pv-kind-' + p.id}>
      <${SelectField} id=${'pv-kind-' + p.id} value=${p.kind} options=${Object.keys(KIND_INFO).map((k) => ({ value: k, label: KIND_INFO[k].label }))} onChange=${(v) => set({ kind: v })} /><//>
    ${p.kind === 'custom-cli' ? html`
      <${SettingsRow} label="Command" desc="Name on the PATH or full path." labelFor=${'pv-cmd-' + p.id}><${TextField} id=${'pv-cmd-' + p.id} mono=${true} value=${p.command} placeholder="agy" onInput=${(v) => set({ command: v })} /><//>
      <${SettingsRow} label="Arguments" desc="Added right after the command, separated by spaces." labelFor=${'pv-args-' + p.id}>
        <${TextField} id=${'pv-args-' + p.id} mono=${true} value=${(p.args || []).join(' ')} placeholder="--dangerously-skip-permissions" onInput=${(v) => set({ args: v.split(/\s+/).filter(Boolean) })} /><//>` : null}
    ${p.kind === 'anthropic-gateway' ? html`
      <${SettingsRow} label="Base URL" desc="ANTHROPIC_BASE_URL of the gateway." labelFor=${'pv-url-' + p.id}><${TextField} id=${'pv-url-' + p.id} mono=${true} value=${p.base_url} placeholder="http://localhost:4000" onInput=${(v) => set({ base_url: v })} /><//>` : null}
    ${!saved ? html`<${SettingsBlock}><${Note}>Save the providers to add its key and test it.<//><//>` : null}
    ${saved && p.kind !== 'claude-subscription' && p.kind !== 'custom-cli' ? html`
      <${SettingsRow} label=${p.kind === 'claude-oauth-token' ? 'Token' : p.kind === 'anthropic-gateway' ? 'Gateway key (optional)' : 'API key'}
        desc=${p.secret && p.secret.set ? 'Saved: ' + p.secret.masked + '. Type a new one to replace it.' : 'Saved encrypted for your Windows user; never shown again.'} labelFor=${'pv-secret-' + p.id}>
        <div class="st-actions"><${TextField} id=${'pv-secret-' + p.id} type="password" mono=${true} value=${secret} placeholder=${p.secret && p.secret.set ? p.secret.masked : 'paste here'} onInput=${setSecret} />
          <button type="button" class="btn btn-sm" disabled=${!secret && !(p.secret && p.secret.set)} onClick=${saveSecret}>${secret ? 'Save key' : 'Remove'}</button></div><//>` : null}
    <${SettingsRow} label="Models" desc="Orchestrator turn and subagents. Default keeps the app's choice." labelFor=${'pv-mo-' + p.id}>
      <div class="st-actions">
        <${SelectField} id=${'pv-mo-' + p.id} label="Orchestrator model" value=${models.orchestrator || ''} options=${Object.keys(MODEL_LABEL).map((m) => ({ value: m, label: 'Orchestrator: ' + MODEL_LABEL[m] }))}
          onChange=${(v) => set({ models: { ...models, orchestrator: v } })} />
        <${SelectField} id=${'pv-ma-' + p.id} label="Agents model" value=${models.agents || ''} options=${Object.keys(MODEL_LABEL).map((m) => ({ value: m, label: 'Agents: ' + MODEL_LABEL[m] }))}
          onChange=${(v) => set({ models: { ...models, agents: v } })} /></div><//>
    <${SettingsBlock}>
      <div class="st-actions st-actions-end">
        ${result ? html`<span class=${'st-test ' + (result.ok ? 'is-ok' : 'is-err')} role="status">${result.ok ? `OK in ${result.seconds}s${result.model ? ' · ' + result.model : ''}` : 'Failed: ' + (result.error || 'no answer')}</span>` : null}
        <button type="button" class="btn btn-sm" disabled=${testing || !saved} onClick=${test}>${testing ? 'Testing…' : 'Test'}</button>
        <button type="button" class="btn btn-sm btn-danger" disabled=${count === 1} onClick=${onRemove}>Remove</button>
      </div><//>
  <//>`;
}

export function ProvidersSection() {
  const [data, setData] = useState(null);
  const [draft, setDraft] = useState(null);
  const [error, setError] = useState('');
  const [adding, setAdding] = useState('anthropic-api-key');
  const load = async () => { try { const r = await appApi.get('settings/providers'); setData(r); setDraft(r.providers); setError(''); } catch (e) { setError(e.message); } };
  useEffect(() => { load(); }, []);
  if (error) return html`<${Note} kind="err">${error}<//>`;
  if (!draft) return html`<${Loading} text="Reading the providers…" />`;
  const dirty = JSON.stringify(draft.map(strip)) !== JSON.stringify(data.providers.map(strip));
  const update = (i, p) => setDraft(draft.map((x, j) => (j === i ? p : x)));
  const move = (i, step) => { const next = draft.slice(); const [item] = next.splice(i, 1); next.splice(i + step, 0, item); setDraft(next); };
  const save = async () => {
    try { const r = await appApi.post('settings/providers', { providers: draft.map(strip) }); setData(r); setDraft(r.providers); toast('Providers saved. The next loop uses this order.', 'ok'); }
    catch (e) { toast(e.message, 'err'); }
  };
  const add = () => setDraft([...draft, { id: newId(adding, draft.map((p) => p.id)), name: KIND_INFO[adding].label, kind: adding, enabled: true, command: '', args: [], base_url: '', models: { orchestrator: '', agents: '' }, problems: [] }]);
  return html`
    <${SettingsGroup} id="pv-order" title="Run order" desc="The loop uses the first enabled provider. When it hits a usage limit or cannot sign in, the next one takes the turn automatically.">
      <${SettingsBlock}><ol class="st-order">${draft.map((p, i) => html`<li key=${p.id} class=${p.enabled ? '' : 'is-off'}>${p.name || p.id}<span class="st-order-kind">${(KIND_INFO[p.kind] || {}).label || p.kind}</span></li>`)}</ol><//>
      <${SettingsRow} label="Add a provider" labelFor="pv-add-kind">
        <div class="st-actions"><${SelectField} id="pv-add-kind" value=${adding} options=${Object.keys(KIND_INFO).map((k) => ({ value: k, label: KIND_INFO[k].label }))} onChange=${setAdding} />
          <button type="button" class="btn btn-sm" onClick=${add}>Add</button></div><//>
    <//>
    ${draft.map((p, i) => html`<${ProviderCard} key=${p.id} p=${p} index=${i} count=${draft.length} saved=${data.providers.some((x) => x.id === p.id)} onChange=${(next) => update(i, next)} onMove=${(step) => move(i, step)}
      onRemove=${() => setDraft(draft.filter((_, j) => j !== i))} onSecretSaved=${load} />`)}
    <div class="st-savebar" hidden=${!dirty}><span>Unsaved changes to the providers.</span>
      <button type="button" class="btn btn-sm" onClick=${() => setDraft(data.providers)}>Discard</button>
      <button type="button" class="btn btn-sm btn-primary" id="pv-save" onClick=${save}>Save providers</button></div>`;
}

/** The fields the server stores (no status, secret or path). */
function strip(p) {
  return { id: p.id, name: p.name, kind: p.kind, enabled: !!p.enabled, command: p.command || '', args: p.args || [], base_url: p.base_url || '', models: p.models || { orchestrator: '', agents: '' } };
}
