/* Settings section "Skills and tools": which skills and MCP tools every agent of the loop receives, saved as the default for every loop.
   Skills group: GET /api/app/settings/skills lists every skill found on this machine (name, one line, source) and the saved choice; Save writes the
   config key `loop_extra_skills` through POST /api/app/settings. MCP group: McpServersSection (servers, and a box per tool). Names only, no secret. */
import { html, useState, useEffect } from '../util/preact.js';
import { appApi, projectIdFromUrl } from '../api/api_client.js';
import { toast } from './toast.js';
import { McpServersSection } from './mcp_servers_section.js';

const TIMEOUT_MS = 15000;
const KEY = 'loop_extra_skills';
const sameSet = (a, b) => a.size === b.size && [...a].every((x) => b.has(x));

function SkillsGroup() {
  const [data, setData] = useState(null);
  const [chosen, setChosen] = useState(new Set());
  const [saved, setSaved] = useState(new Set());
  const [query, setQuery] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const load = (d) => { setData(d); const s = new Set(d.released || []); setChosen(s); setSaved(s); };
  useEffect(() => {
    const project = projectIdFromUrl();
    appApi.get('settings/skills' + (project ? '?project=' + encodeURIComponent(project) : ''), TIMEOUT_MS).then(load).catch((e) => setError(e.message));
  }, []);
  if (error && !data) return html`<div class="nl-err" role="alert">Could not read the skills: ${error}</div>`;
  if (!data) return html`<div class="nl-msg" role="status"><div class="spinner" aria-hidden="true" /><span>loading skills…</span></div>`;
  const skills = data.skills || [];
  const needle = query.trim().toLowerCase();
  const shown = skills.filter((s) => !needle || (s.name + ' ' + s.description + ' ' + s.source).toLowerCase().includes(needle));
  const listed = new Set(skills.map((s) => s.name));
  const hidden = [...chosen].filter((n) => !listed.has(n)); // saved earlier, not found on this machine now: kept
  const toggle = (name, on) => setChosen((old) => { const next = new Set(old); if (on) next.add(name); else next.delete(name); return next; });
  const setAll = (on) => setChosen(on ? new Set([...hidden, ...skills.filter((s) => !s.default).map((s) => s.name)]) : new Set(hidden));
  const changed = !sameSet(chosen, saved);
  const save = async () => {
    setBusy(true); setError('');
    try {
      await appApi.post('settings', { key: KEY, value: [...chosen], confirm: true }, TIMEOUT_MS);
      setSaved(new Set(chosen)); toast('Skills saved. Applies to the next claude run.', 'ok');
    } catch (e) { setError(e.message); } finally { setBusy(false); }
  };
  return html`<div class="sk-group" id="st-skills">
    <h4 class="sk-title">Skills</h4>
    <p class="set-desc">Received by every agent of the loop, in every project. The ones marked <em>always</em> are included anyway; tick more to release them. More skills means more text in every call.</p>
    <div class="set-edit sk-bar">
      <label class="sr-only" for="st-search">Search skills</label>
      <input id="st-search" class="dlg-input set-input" type="search" autocomplete="off" spellcheck="false" placeholder="search by name, description or source"
        value=${query} onInput=${(e) => setQuery(e.currentTarget.value)} />
      <button type="button" class="btn btn-sm" id="st-all" disabled=${busy} onClick=${() => setAll(true)}>All</button>
      <button type="button" class="btn btn-sm" id="st-none" disabled=${busy} onClick=${() => setAll(false)}>None</button>
    </div>
    <div class="hint" id="st-count">${chosen.size} released on top of the defaults · ${skills.length} found${data.truncated ? ' (list cut at ' + skills.length + ' of ' + data.total + ')' : ''}</div>
    ${data.standards_warning ? html`<div class="nl-box sk-warn" id="st-standards-warning" role="status">${data.standards_warning}</div>` : null}
    ${skills.length ? html`<ul class="sk-list" aria-label="Skills">${shown.map((s) => html`<li class="sk-row" key=${s.name}>
        <label class="nl-check" for=${'st-' + s.name}><input type="checkbox" id=${'st-' + s.name} checked=${s.default || chosen.has(s.name)} disabled=${busy || s.default}
          onChange=${(e) => toggle(s.name, e.currentTarget.checked)} /><span class="mono">${s.name}</span></label>
        <span class="hint sk-src">${s.default ? 'always · ' : ''}${s.source}${s.description ? ' — ' + s.description : ''}</span></li>`)}</ul>
      ${shown.length ? null : html`<div class="hint">No skill matches the search.</div>`}`
      : html`<div class="hint">No skill was found on this machine.</div>`}
    ${error ? html`<div class="nl-err" role="alert">${error}</div>` : null}
    <div class="set-edit"><button type="button" class="btn btn-sm btn-primary" id="st-save" disabled=${busy || !changed} onClick=${save}>Save skills</button></div>
  </div>`;
}

export function SkillsToolsSection() {
  return html`<section class="set-row sk-section" id="set-skills-tools" aria-labelledby="set-sk-title">
    <div class="set-head"><h3 class="set-label" id="set-sk-title">Skills and tools</h3></div>
    <p class="set-desc">What the agents of a loop can use. The choice saved here is the default for every loop.</p>
    <${SkillsGroup} />
    <${McpServersSection} />
  </section>`;
}
