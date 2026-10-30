/* Sections of the Settings page that come from GET /api/app/settings: every app setting of ~/.loop-harness/config.json.
   The API lists them (value, default, type, restart_required, caveman availability and cost comparison); POST saves one
   `{key, value, confirm: true}` or resets it `{key, reset: true, confirm: true}` (the key leaves config.json). A bool saves when toggled;
   the other types show Save once the draft differs. The page loads the list once (SettingsData) and each section shows its own keys. */
import { html, useState, useEffect, Fragment } from '../util/preact.js';
import { maskHome } from '../util/format.js';
import { appApi } from '../api/api_client.js';
import { toast } from '../components/toast.js';
import { getTheme, setTheme, onThemeChange } from '../util/theme.js';
import { ctx } from './panel/context.js';
import { ClaudeTotals } from './panel/claude_badge.js';
import { SettingsGroup, SettingsRow, SettingsBlock, Toggle, SelectField, TextField, TextArea, Note, SavedMark, useSavedFlash, Loading } from '../components/settings_ui.js';
import { ClaudeAccountGroup } from './settings_system.js';

const SAVE_TIMEOUT_MS = 15000;
const RESTART_TEXT = 'Takes effect after restarting the app.';
const NEXT_RUN_TEXT = 'Applies to the next claude run.';
const sameValue = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const toDraft = (item) => (item.type === 'list' ? (item.value || []).join('\n') : item.type === 'bool' ? !!item.value : item.type === 'text' ? maskHome(item.value == null ? '' : item.value) : String(item.value == null ? '' : item.value));
const money = (n) => (typeof n === 'number' ? '$' + n.toFixed(2) : '—');

/** The value to send for a draft, or an `{ error }` the user can fix before any call. */
function fromDraft(item, draft) {
  if (item.type === 'text') return { value: String(draft) === maskHome(item.value == null ? '' : item.value) ? item.value : String(draft) }; // the field shows the home masked; untouched = keep the real value
  if (item.type === 'bool' || item.type === 'choice') return { value: item.type === 'bool' ? !!draft : String(draft) };
  if (item.type === 'list') return { value: String(draft).split('\n').map((line) => line.trim()).filter(Boolean) };
  const text = String(draft).trim(), number = Number(text);
  if (!text || !Number.isFinite(number)) return { error: 'Type a number.' };
  if (item.type === 'int' && !Number.isInteger(number)) return { error: 'Type a whole number.' };
  return { value: number };
}

/** Cost per turn with and without caveman over the recorded turns of every project (CAV-04). */
function CavemanComparison({ caveman }) {
  if (!caveman) return null;
  return html`<${SettingsBlock} className="st-caveman"><div id="set-caveman-info" class="st-caveman-in">
    <p class="st-desc">${caveman.recommendation}</p>
    ${caveman.enough_data
      ? html`<table class="st-table"><caption class="sr-only">Average cost per turn with and without caveman</caption>
          <thead><tr><th scope="col"></th><th scope="col">Turns</th><th scope="col">Avg cost per turn</th></tr></thead>
          <tbody><tr><th scope="row">With caveman</th><td class="mono">${caveman.on.turns}</td><td class="mono">${money(caveman.on.average_cost_usd)}</td></tr>
            <tr><th scope="row">Without caveman</th><td class="mono">${caveman.off.turns}</td><td class="mono">${money(caveman.off.average_cost_usd)}</td></tr></tbody></table>`
      : html`<div class="st-empty mono" id="set-caveman-nodata">${caveman.message}</div>`}
  </div><//>`;
}

/** One setting as a row: the control picked by its type, Save when the draft differs, Reset when it was customised. */
function SettingRow({ item, onSaved }) {
  const [draft, setDraft] = useState(() => toDraft(item));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [saved, flash] = useSavedFlash();
  useEffect(() => { setDraft(toDraft(item)); }, [item.value]);
  const blocked = item.available === false && !item.value; // caveman not installed: cannot be turned on
  const parsed = fromDraft(item, draft);
  const changed = item.type !== 'bool' && !parsed.error && !sameValue(parsed.value, item.value);
  const send = async (body, done) => {
    setBusy(true); setError('');
    try {
      const r = await appApi.post('settings', Object.assign({ key: item.key, confirm: true }, body), SAVE_TIMEOUT_MS);
      onSaved(r.setting); toast(done, 'ok'); flash();
    } catch (e) { setError(e.message); setDraft(toDraft(item)); }
    finally { setBusy(false); }
  };
  const save = (value) => send({ value }, item.label + ' saved.');
  const reset = () => send({ reset: true }, item.label + ' back to its default.');
  const id = 'set-' + item.key;
  const wide = item.type === 'list';
  const control = item.type === 'bool'
    ? html`<${Toggle} id=${id} checked=${!!draft} disabled=${busy || blocked} label=${item.label} onChange=${(v) => { setDraft(v); save(v); }} />`
    : item.type === 'choice'
      ? html`<${SelectField} id=${id} value=${draft} disabled=${busy} options=${item.choices || []} label=${item.label} onChange=${(v) => { setDraft(v); setError(''); }} />`
      : item.type === 'list'
        ? html`<${TextArea} id=${id} value=${draft} disabled=${busy} label=${item.label} onInput=${(v) => { setDraft(v); setError(''); }} />`
        : html`<${TextField} id=${id} value=${draft} disabled=${busy} label=${item.label} numeric=${item.type === 'int' || item.type === 'number'} mono=${item.type === 'text'}
            step=${item.type === 'number' ? 'any' : undefined} placeholder=${item.default === '' ? 'default' : maskHome(String(item.default))} onInput=${(v) => { setDraft(v); setError(''); }} />`;
  const actions = html`<div class="st-actions">
    ${item.type !== 'bool' ? html`<button type="button" class="btn btn-sm btn-primary" id=${'set-save-' + item.key} disabled=${busy || !changed} onClick=${() => save(parsed.value)}>Save</button>` : null}
    ${item.customized ? html`<button type="button" class="btn btn-sm btn-ghost" id=${'set-reset-' + item.key} disabled=${busy} onClick=${reset}
      title=${'Back to the default: ' + (Array.isArray(item.default) ? (item.default.join(', ') || 'none') : item.default === '' ? 'empty' : maskHome(item.default))}>Reset to default</button>` : null}
    <${SavedMark} show=${saved} /></div>`;
  const tags = [item.restart_required ? { text: 'restart', tone: 'warn', title: RESTART_TEXT } : null, item.customized ? { text: 'changed', tone: 'info' } : null].filter(Boolean);
  const desc = html`${item.description}<span class="st-when"> ${item.restart_required ? RESTART_TEXT : NEXT_RUN_TEXT}</span>`;
  const extra = html`${parsed.error && item.type !== 'bool' && String(draft).trim() !== '' ? html`<${Note} kind="err">${parsed.error}<//>` : null}
    ${error ? html`<${Note} kind="err">${error}<//>` : null}${item.note ? html`<${Note} kind="warn" id=${'set-note-' + item.key}>${item.note}<//>` : null}`;
  return html`<${SettingsRow} rowKey=${item.key} label=${item.label} labelId=${'set-l-' + item.key} labelFor=${id} desc=${desc} tags=${tags} stack=${wide} extra=${extra}>
    <div class=${'st-field' + (wide ? ' is-wide' : '')}>${control}${item.type === 'bool' ? (item.customized ? actions : html`<${SavedMark} show=${saved} />`) : actions}</div><//>`;
}

/** Loads the settings list once for the whole page and hands it to the section (render prop). */
function SettingsData({ children }) {
  const [data, setData] = useState(null);
  const [error, setError] = useState('');
  useEffect(() => { appApi.get('settings', SAVE_TIMEOUT_MS).then(setData).catch((e) => setError(e.message)); }, []);
  if (error) return html`<${Note} kind="err">Could not read the settings: ${error}<//>`;
  if (!data) return html`<${Loading} text="Loading settings…" />`;
  const replace = (saved) => setData((old) => Object.assign({}, old, { settings: old.settings.map((s) => (s.key === saved.key ? saved : s)) }));
  return children(data, replace);
}

/** A card with the settings of `keys` (in that order). */
function KeyGroup({ data, onSaved, keys, title, desc, id, after }) {
  const items = keys.map((k) => data.settings.find((s) => s.key === k)).filter(Boolean);
  if (!items.length) return null;
  return html`<${SettingsGroup} title=${title} desc=${desc} id=${id}>
    ${items.map((item) => html`<${Fragment} key=${item.key}><${SettingRow} item=${item} onSaved=${onSaved} />${after && after[item.key] ? after[item.key] : null}<//>`)}
  <//>`;
}


const CLAUDE_KEYS = ['orchestrator_model', 'claude_binary', 'claude_fallback_binaries', 'loop_isolation', 'loop_caveman', 'loop_extra_skills'];
const NETWORK_KEYS = ['egress_hosts', 'egress_never', 'egress_builtin_read', 'env_sources'];
/** Keys no section above names go to Projects and data, so a setting added later is never lost. */
const otherKeys = (data) => data.settings.map((s) => s.key).filter((k) => !CLAUDE_KEYS.includes(k) && !NETWORK_KEYS.includes(k));

export function ClaudeSection() {
  const totals = ctx.state && ctx.state.meta ? ctx.state.meta.claude_totals : null;
  return html`<${ClaudeAccountGroup} />
    ${totals && (totals.turns > 0 || totals.calls > 0) ? html`<${SettingsGroup} title="Usage of this project" id="st-totals"><${SettingsBlock}><${ClaudeTotals} /><//><//>` : null}
    <${SettingsData}>${(data, onSaved) => html`<${KeyGroup} data=${data} onSaved=${onSaved} keys=${CLAUDE_KEYS} title="Claude and the loop" id="st-claude"
      desc="Which command and model the loop runs, and how isolated each run is." after=${{ loop_caveman: html`<${CavemanComparison} caveman=${data.caveman} />` }} />`}<//>`;
}

export function NetworkSettings() {
  return html`<${SettingsData}>${(data, onSaved) => html`<${KeyGroup} data=${data} onSaved=${onSaved} keys=${NETWORK_KEYS} title="Hosts the agents may reach" id="st-network"
    desc="Inside the loop the agents reach only these hosts. Each project adds its own list (Planning, Test environment) on top." />`}<//>`;
}

export function ProjectsSection() {
  return html`<${SettingsData}>${(data, onSaved) => html`<${KeyGroup} data=${data} onSaved=${onSaved} keys=${otherKeys(data)} title="Data and limits" id="st-projects"
      desc="Saved in config.json of the app data folder. A setting you never change stays out of the file and keeps its default." />
    ${data.info && data.info.length ? html`<${SettingsGroup} title="This installation" desc="Read only." id="st-install">
      ${data.info.map((row) => html`<${SettingsRow} key=${row.key} label=${row.label}><span class="st-value mono st-path" title=${maskHome(row.value)}>${maskHome(row.value)}</span><//>`)}
    <//>` : null}`}<//>`;
}

const THEME_CHOICES = [['light', 'Light'], ['dark', 'Dark'], ['system', 'System']];

/** Appearance: Light / Dark / System, a radio group (arrows move and select, Tab leaves the group). */
export function AppearanceSection() {
  const [mode, setMode] = useState(getTheme());
  useEffect(() => onThemeChange(setMode), []);
  const choose = (value) => { setTheme(value); setMode(value); };
  const onKey = (e) => {
    const keys = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 };
    if (!keys[e.key]) return;
    e.preventDefault();
    const i = THEME_CHOICES.findIndex(([v]) => v === mode);
    const next = THEME_CHOICES[(i + keys[e.key] + THEME_CHOICES.length) % THEME_CHOICES.length][0];
    choose(next);
    const el = e.currentTarget.querySelector('[data-theme-choice="' + next + '"]');
    if (el) el.focus();
  };
  return html`<${SettingsGroup} title="Theme" id="st-theme">
    <${SettingsRow} label="Appearance" labelId="set-l-appearance" desc="Light, dark, or follow the system. Saved on this device.">
      <div class="st-seg" role="radiogroup" aria-labelledby="set-l-appearance" onKeyDown=${onKey}>
        ${THEME_CHOICES.map(([value, label]) => html`<button key=${value} type="button" role="radio" class=${'st-seg-opt' + (mode === value ? ' is-on' : '')}
          aria-checked=${mode === value ? 'true' : 'false'} tabindex=${mode === value ? 0 : -1} data-theme-choice=${value} onClick=${() => choose(value)}>${label}</button>`)}
      </div><//>
  <//>`;
}
