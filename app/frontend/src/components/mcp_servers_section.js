/* "MCP servers" section of the Settings dialog: which MCP servers a loop may use. Closed by default: until a server is ticked, loops cannot use any.
   GET /api/app/settings/mcp-servers lists the servers found on this machine (+ the saved ones and the ones added by name) with their options;
   POST saves the whole list `{servers: {name: {allow, kind, production, read_only}}, extra: [patterns], confirm: true}` (config.json
   `mcp_servers`, `mcp_allow_extra`; the guard reads the same keys). Nothing is saved until the user presses Save. */
import { html, useState, useEffect } from '../util/preact.js';
import { appApi, projectIdFromUrl } from '../api/api_client.js';
import { toast } from './toast.js';

const TIMEOUT_MS = 15000;
const NONE_ALLOWED_HINT = 'Loops cannot use any MCP server until you allow one.';
const SOURCE_TEXT = { user: 'your Claude config', project: 'this project', '.mcp.json': 'project .mcp.json', plugin: 'a plugin', permissions: 'Claude permissions', manual: 'added by name' };
const sourceText = (sources) => (sources || []).map((s) => SOURCE_TEXT[s] || s).join(', ');
const toRow = (r) => ({ name: r.name, sources: r.sources || [], defined: !!r.defined, saved: !!r.saved, allow: !!r.allow, kind: r.kind === 'sql' ? 'sql' : 'other', production: !!r.production, read_only: !!r.read_only, prefix: r.prefix || '', tools: r.tools || [] });
const cleanKey = (row) => ({ allow: row.allow, kind: row.kind, production: row.production, read_only: row.read_only });
const isDefault = (row) => !row.allow && row.kind === 'other' && !row.production && !row.read_only;
const extraLines = (text) => String(text).split('\n').map((line) => line.trim()).filter(Boolean);
const snapshot = (rows, extra) => JSON.stringify([rows.filter((r) => r.saved || !isDefault(r)).map((r) => [r.name, cleanKey(r)]), extraLines(extra)]);

const NO_TOOLS_TEXT = 'Tools cannot be listed: the app does not start servers, it only knows tool names from your Claude permissions and the patterns below.';
const patternOf = (row, tool) => 'mcp__' + row.prefix + '__' + tool;

/** One box per known tool: ticks add or remove the pattern `mcp__<server>__<tool>` in the extra patterns. With the whole server allowed they are all on. */
function ToolList({ row, index, disabled, patterns, onTool }) {
  if (!row.tools.length) return html`<div class="hint sk-notools">${NO_TOOLS_TEXT}</div>`;
  return html`<ul class="sk-tools" aria-label=${'Tools of ' + row.name}>${row.tools.map((tool) => html`<li key=${tool}><label class="nl-check" for=${'mcp-' + index + '-t-' + tool}>
      <input type="checkbox" id=${'mcp-' + index + '-t-' + tool} checked=${row.allow || patterns.has(patternOf(row, tool))} disabled=${disabled || row.allow}
        onChange=${(e) => onTool(patternOf(row, tool), e.currentTarget.checked)} /><span class="mono">${tool}</span></label></li>`)}</ul>`;
}

function ServerRow({ row, index, disabled, patterns, onTool, onChange }) {
  const id = 'mcp-' + index;
  const set = (patch) => onChange(Object.assign({}, row, patch));
  return html`<li class="mcp-row" data-server=${row.name}>
    <div class="mcp-name"><label class="nl-check" for=${id + '-allow'}><input type="checkbox" id=${id + '-allow'} checked=${row.allow} disabled=${disabled}
        onChange=${(e) => set({ allow: e.currentTarget.checked })} /><span class="mono">${row.name}</span></label>
      <span class="hint mcp-src">${sourceText(row.sources)}${row.defined ? '' : ' — no definition found on this machine'}</span></div>
    <div class="mcp-opts">
      <label class="mcp-kind" for=${id + '-kind'}><span>Type</span><select id=${id + '-kind'} class="dlg-input" value=${row.kind} disabled=${disabled}
        onChange=${(e) => set({ kind: e.currentTarget.value })}><option value="other" selected=${row.kind === 'other'}>Other</option>
        <option value="sql" selected=${row.kind === 'sql'}>Database</option></select></label>
      <label class="nl-check" for=${id + '-prod'}><input type="checkbox" id=${id + '-prod'} checked=${row.production} disabled=${disabled}
        onChange=${(e) => set({ production: e.currentTarget.checked })} /><span>Production</span></label>
      <label class="nl-check" for=${id + '-ro'}><input type="checkbox" id=${id + '-ro'} checked=${row.read_only} disabled=${disabled}
        onChange=${(e) => set({ read_only: e.currentTarget.checked })} /><span>Read-only</span></label>
    </div>
    <${ToolList} row=${row} index=${index} disabled=${disabled} patterns=${patterns} onTool=${onTool} /></li>`;
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
  if (error && !rows) return html`<div class="sk-group"><div class="nl-err" role="alert">Could not read the MCP servers: ${error}</div></div>`;
  if (!rows) return html`<div class="sk-group"><div class="nl-msg" role="status"><div class="spinner" aria-hidden="true" /><span>loading MCP servers…</span></div></div>`;
  const changed = snapshot(rows, extra) !== saved;
  const nothingAllowed = !rows.some((r) => r.allow) && extraLines(extra).length === 0;
  const patterns = new Set(extraLines(extra));
  const onTool = (pattern, on) => { const lines = extraLines(extra).filter((l) => l !== pattern); setExtra((on ? lines.concat([pattern]) : lines).join('\n')); };
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
  return html`<div class="sk-group mcp-section" id="set-mcp">
    <h4 class="sk-title" id="set-mcp-title">MCP servers and tools</h4>
    <p class="set-desc">Received by every agent of the loop. Tick the servers a loop may use; a ticked server gives all its tools, otherwise tick single tools. Type <em>Database</em> makes the guard inspect every statement; <em>Production</em> means writes are
      never allowed and only the orchestrator and planning reach it; <em>Read-only</em> blocks write tools. Found on this machine: your Claude config, the project
      <span class="mono">.mcp.json</span> and enabled plugins. A claude.ai connector cannot be found: add it by name below.</p>
    ${nothingAllowed ? html`<div class="nl-box mcp-none" id="mcp-none" role="status">${NONE_ALLOWED_HINT}</div>` : null}
    ${rows.length ? html`<ul class="mcp-list" aria-label="MCP servers">${rows.map((row, i) => html`<${ServerRow} key=${row.name} row=${row} index=${i} disabled=${busy} patterns=${patterns} onTool=${onTool} onChange=${(r) => update(i, r)} />`)}</ul>`
      : html`<div class="hint">No MCP server was found on this machine.</div>`}
    <form class="set-edit mcp-add" onSubmit=${add}>
      <label class="sr-only" for="mcp-add-name">Server name</label>
      <input id="mcp-add-name" class="dlg-input set-input mono" type="text" autocomplete="off" spellcheck="false" placeholder="add a server by name, e.g. db-dev or plugin:name:server"
        value=${newName} onInput=${(e) => setNewName(e.currentTarget.value)} />
      <button type="submit" class="btn btn-sm" id="mcp-add-btn" disabled=${busy || !newName.trim()}>Add</button>
    </form>
    <label class="set-label" for="mcp-extra">Extra tool patterns</label>
    <p class="set-desc">One full tool name per line, <span class="mono">mcp__server__tool</span>; a trailing <span class="mono">*</span> is the only wildcard.</p>
    <textarea id="mcp-extra" class="dlg-area mono set-input" rows="3" spellcheck="false" disabled=${busy} value=${extra}
      onInput=${(e) => setExtra(e.currentTarget.value)} />
    ${notes.map((n, i) => html`<div class="hint set-note" key=${i}>${n}</div>`)}
    ${error ? html`<div class="nl-err" role="alert">${error}</div>` : null}
    <div class="set-edit"><button type="button" class="btn btn-sm btn-primary" id="mcp-save" disabled=${busy || !changed} onClick=${save}>Save MCP servers</button></div>
  </div>`;
}
