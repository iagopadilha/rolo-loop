/* "MCP servers" section of the Settings page: which MCP servers a loop may use. Closed by default: until a server is ticked, loops cannot use any.
   GET /api/app/settings/mcp-servers lists the servers found on this machine (+ the saved ones and the ones added by name) with their options;
   POST saves the whole list `{servers: {name: {allow, kind, production, read_only}}, extra: [patterns], confirm: true}` (config.json
   `mcp_servers`, `mcp_allow_extra`; the guard reads the same keys). Nothing is saved until the user presses Save. */
import { html, useState, useEffect } from '../util/preact.js';
import { appApi, projectIdFromUrl } from '../api/api_client.js';
import { toast } from './toast.js';
import { SettingsGroup, SettingsRow, SettingsBlock, Toggle, SelectField, TextField, TextArea, Note, Loading } from './settings_ui.js';

const TIMEOUT_MS = 15000;
const NONE_ALLOWED_HINT = 'Loops cannot use any MCP server until you allow one.';
const SOURCE_TEXT = { user: 'your Claude config', project: 'this project', '.mcp.json': 'project .mcp.json', plugin: 'a plugin', permissions: 'Claude permissions', manual: 'added by name' };
const sourceText = (sources) => (sources || []).map((s) => SOURCE_TEXT[s] || s).join(', ');
const toRow = (r) => ({ name: r.name, sources: r.sources || [], defined: !!r.defined, saved: !!r.saved, allow: !!r.allow, kind: r.kind === 'sql' ? 'sql' : 'other', production: !!r.production, read_only: !!r.read_only, prefix: r.prefix || '', tools: r.tools || [] });

const NO_TOOLS_TEXT = 'Tools cannot be listed: the app does not start servers, it only knows tool names from your Claude permissions and the patterns below.';
const patternOf = (row, tool) => 'mcp__' + row.prefix + '__' + tool;

/** One box per known tool: ticks add or remove the pattern `mcp__<server>__<tool>` in the extra patterns. With the whole server allowed they are all on. */
function ToolList({ row, index, disabled, patterns, onTool }) {
  if (!row.tools.length) return html`<div class="hint sk-notools">${NO_TOOLS_TEXT}</div>`;
  return html`<ul class="sk-tools" aria-label=${'Tools of ' + row.name}>${row.tools.map((tool) => html`<li key=${tool}><label class="nl-check" for=${'mcp-' + index + '-t-' + tool}>
      <input type="checkbox" id=${'mcp-' + index + '-t-' + tool} checked=${row.allow || patterns.has(patternOf(row, tool))} disabled=${disabled || row.allow}
        onChange=${(e) => onTool(patternOf(row, tool), e.currentTarget.checked)} /><span class="mono">${tool}</span></label></li>`)}</ul>`;
}
const cleanKey = (row) => ({ allow: row.allow, kind: row.kind, production: row.production, read_only: row.read_only });
const isDefault = (row) => !row.allow && row.kind === 'other' && !row.production && !row.read_only;
const extraLines = (text) => String(text).split('\n').map((line) => line.trim()).filter(Boolean);
const snapshot = (rows, extra) => JSON.stringify([rows.filter((r) => r.saved || !isDefault(r)).map((r) => [r.name, cleanKey(r)]), extraLines(extra)]);

function ServerRow({ row, index, disabled, patterns, onTool, onChange }) {
  const id = 'mcp-' + index;
  const set = (patch) => onChange(Object.assign({}, row, patch));
  return html`<${SettingsRow} rowKey=${row.name} labelFor=${id + '-allow'} label=${html`<span class="mono">${row.name}</span>`}
    desc=${sourceText(row.sources) + (row.defined ? '' : ' — no definition found on this machine')}
    extra=${html`<div class="st-opts">
      <span class="st-opt"><label for=${id + '-kind'}>Type</label><${SelectField} id=${id + '-kind'} value=${row.kind} disabled=${disabled} onChange=${(v) => set({ kind: v })}
        options=${[{ value: 'other', label: 'Other' }, { value: 'sql', label: 'Database' }]} /></span>
      <span class="st-opt"><${Toggle} id=${id + '-prod'} checked=${row.production} disabled=${disabled} label="Production" onChange=${(v) => set({ production: v })} /><label for=${id + '-prod'}>Production</label></span>
      <${ToolList} row=${row} index=${index} disabled=${disabled} patterns=${patterns} onTool=${onTool} />
      <span class="st-opt"><${Toggle} id=${id + '-ro'} checked=${row.read_only} disabled=${disabled} label="Read-only" onChange=${(v) => set({ read_only: v })} /><label for=${id + '-ro'}>Read-only</label></span>
    </div>`}>
    <${Toggle} id=${id + '-allow'} checked=${row.allow} disabled=${disabled} label=${'Allow ' + row.name} onChange=${(v) => set({ allow: v })} />
  <//>`;
}

export function McpServersSection() {
  const [rows, setRows] = useState(null);
  const [extra, setExtra] = useState('');
  const [saved, setSaved] = useState('');
  const [notes, setNotes] = useState([]);
  const [newName, setNewName] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const load = (data) => {
    const next = (data.servers || []).map(toRow), text = (data.extra || []).join('\n');
    setRows(next); setExtra(text); setSaved(snapshot(next, text)); setNotes([].concat(data.notes || [], data.broken ? ['config.json is unreadable: ' + data.broken] : []));
  };
  useEffect(() => {
    const project = projectIdFromUrl();
    appApi.get('settings/mcp-servers' + (project ? '?project=' + encodeURIComponent(project) : ''), TIMEOUT_MS).then(load).catch((e) => setError(e.message));
  }, []);
  if (error && !rows) return html`<${Note} kind="err">Could not read the MCP servers: ${error}<//>`;
  if (!rows) return html`<${Loading} text="Loading MCP servers…" />`;
  const patterns = new Set(extraLines(extra));
  const onTool = (pattern, on) => { const lines = extraLines(extra).filter((l) => l !== pattern); setExtra((on ? lines.concat([pattern]) : lines).join(String.fromCharCode(10))); };
  const changed = snapshot(rows, extra) !== saved;
  const nothingAllowed = !rows.some((r) => r.allow) && extraLines(extra).length === 0;
  const update = (index, row) => setRows((old) => old.map((r, i) => (i === index ? row : r)));
  const add = (event) => {
    event.preventDefault();
    const name = newName.trim();
    if (!name) return;
    if (rows.some((r) => r.name.toLowerCase() === name.toLowerCase())) { setError('"' + name + '" is already in the list.'); return; }
    setError(''); setRows(rows.concat([toRow({ name, sources: ['manual'], allow: true })])); setNewName('');
  };
  const save = async () => {
    setBusy(true); setError('');
    const servers = {};
    rows.filter((r) => r.saved || !isDefault(r)).forEach((r) => { servers[r.name] = cleanKey(r); });
    try {
      load(await appApi.post('settings/mcp-servers', { servers, extra: extraLines(extra), confirm: true }, TIMEOUT_MS));
      toast('MCP servers saved. Applies to the next claude run.', 'ok');
    } catch (e) { setError(e.message); } finally { setBusy(false); }
  };
  return html`<div id="set-mcp" class="mcp-section">
    <${SettingsGroup} title="Servers" id="st-mcp-servers" desc=${html`Tick the servers a loop may use. <em>Database</em> makes the guard inspect every statement; <em>Production</em> means writes are never allowed and only the orchestrator and planning reach it; <em>Read-only</em> blocks write tools. Found: your Claude config, the project <span class="mono">.mcp.json</span> and enabled plugins.`}>
      ${nothingAllowed ? html`<${SettingsBlock}><${Note} kind="warn" id="mcp-none">${NONE_ALLOWED_HINT}<//><//>` : null}
      ${rows.length ? rows.map((row, i) => html`<${ServerRow} key=${row.name} row=${row} index=${i} disabled=${busy} patterns=${patterns} onTool=${onTool} onChange=${(r) => update(i, r)} />`)
        : html`<${SettingsBlock}><span class="st-empty">No MCP server was found on this machine.</span><//>`}
    <//>
    <${SettingsGroup} title="Add and extend" id="st-mcp-extra" desc="A claude.ai connector cannot be found on this machine: add it by name.">
      <${SettingsRow} label="Add a server by name" labelFor="mcp-add-name" desc="For example db-dev or plugin:name:server.">
        <form class="st-field" onSubmit=${add}>
          <${TextField} id="mcp-add-name" mono=${true} value=${newName} label="Server name" placeholder="server name" onInput=${setNewName} />
          <button type="submit" class="btn btn-sm" id="mcp-add-btn" disabled=${busy || !newName.trim()}>Add</button>
        </form><//>
      <${SettingsRow} stack label="Extra tool patterns" labelFor="mcp-extra" desc=${html`One full tool name per line, <span class="mono">mcp__server__tool</span>; a trailing <span class="mono">*</span> is the only wildcard.`}>
        <${TextArea} id="mcp-extra" value=${extra} disabled=${busy} label="Extra tool patterns" onInput=${setExtra} />
      <//>
    <//>
    ${notes.map((n, i) => html`<${Note} key=${i} kind="warn">${n}<//>`)}
    ${error ? html`<${Note} kind="err">${error}<//>` : null}
    <div class="st-savebar"><button type="button" class="btn btn-sm btn-primary" id="mcp-save" disabled=${busy || !changed} onClick=${save}>Save MCP servers</button></div>
  </div>`;
}
