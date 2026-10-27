/* Network section of the Test environment tab: the hosts the agents may reach inside the loop. GET network (effective list with
   origins, project entries, suggestions); PUT network saves the project entries (400 invalid_network_entry names the row);
   POST network/suggestion applies or dismisses what the planning AI suggested. Only a click here writes NETWORK.json.
   One text input per row holds the entry in the NET-01 grammar; the server parses and normalises it (there is no note field in the API). */
import { html, useState, useEffect, useRef } from '../../util/preact.js';
import { list } from '../../util/format.js';
import { toast } from '../../components/toast.js';
import { ctx } from './context.js';

const REFRESH_MS = 6000, CALL_TIMEOUT_MS = 15000;
const GRAMMAR = 'host[:port][/path] [METHODS] [cred] [if-missing=NAME]';
const EXAMPLES = 'api.example.com/v1 GET,POST cred  ·  *.example.com  ·  db.example.com:3306 tcp';
const ORIGIN_TEXT = { local: 'always allowed', builtin: 'built-in read', global: 'global (Settings)', project: 'this project' };
const ORIGIN_TITLE = {
  local: 'This machine: any port, any method, credentials allowed.',
  builtin: 'Maestro built-in read list: GET and HEAD only, never credentials. Turn it off in Settings.',
  global: 'Allowed for every project in Settings (egress_hosts).',
  project: 'Saved in .specs/project/NETWORK.json.',
};

const isBlank = (text) => !String(text).trim();

function InheritedRows({ effective, deny }) {
  const rows = effective.filter((x) => x.origin !== 'project');
  if (!rows.length && !deny.length) return null;
  return html`<details class="plan-net-inh" open>
    <summary>Inherited, read only (${rows.length + deny.length})</summary>
    <ul class="plan-net-list" aria-label="Inherited network entries">
      ${rows.map((x, i) => html`<li key=${'a' + i} class="plan-net-row ro" data-origin=${x.origin}><code>${x.entry}</code>
        <span class=${'plan-net-chip ' + x.origin} title=${ORIGIN_TITLE[x.origin] || ''}>${ORIGIN_TEXT[x.origin] || x.origin}</span></li>`)}
      ${deny.map((x, i) => html`<li key=${'d' + i} class="plan-net-row ro deny" data-origin="deny"><code>${x}</code>
        <span class="plan-net-chip deny" title="Never reachable, whatever any list allows (egress_never in Settings).">global deny</span></li>`)}
    </ul></details>`;
}

function SuggestionBanner({ suggestions, dirty, busy, onAct }) {
  return html`<div class="plan-net-sug" role="status" id="plan-net-sug">
    <b>The planning AI suggests ${suggestions.length ? suggestions.length + (suggestions.length === 1 ? ' host' : ' hosts') : 'hosts'} for this project.</b>
    ${suggestions.length ? html`<ul>${suggestions.map((x, i) => html`<li key=${i}><code>${x}</code></li>`)}</ul>` : html`<p>The suggestion file has no valid entry.</p>`}
    <p class="plan-net-hint">Review them. Nothing is allowed until you apply.${dirty ? ' Save your edits first to apply.' : ''}</p>
    <div class="plan-net-act">
      <button type="button" class="btn btn-sm btn-primary" id="plan-net-apply" disabled=${!!busy || dirty || !suggestions.length} onClick=${() => onAct('apply')}>${busy === 'apply' ? 'Applying…' : 'Apply'}</button>
      <button type="button" class="btn btn-sm" id="plan-net-dismiss" disabled=${!!busy} onClick=${() => onAct('dismiss')}>${busy === 'dismiss' ? 'Dismissing…' : 'Dismiss'}</button>
    </div></div>`;
}

/** The Network section. Keeps its own state: a poll never overwrites rows the user is editing. */
export function NetworkSection() {
  const [data, setData] = useState(null);
  const [gone, setGone] = useState(false);
  const [error, setError] = useState('');
  const [draft, setDraft] = useState([]);
  const [dirty, setDirty] = useState(false);
  const [rowErr, setRowErr] = useState(null); // { index, reason }
  const [busy, setBusy] = useState('');
  const dirtyRef = useRef(false), busyRef = useRef('');
  const mark = (value) => { dirtyRef.current = value; setDirty(value); };

  const load = async () => {
    try {
      const r = await ctx.api.get('network', CALL_TIMEOUT_MS);
      setData(r); setError('');
      if (!dirtyRef.current) setDraft(list(r.project));
    } catch (e) { if (e.status === 404) setGone(true); else setError(e.message); }
  };
  useEffect(() => {
    load();
    const t = setInterval(() => { if (document.visibilityState === 'visible' && !busyRef.current) load(); }, REFRESH_MS);
    return () => clearInterval(t);
  }, []);
  const run = async (kind, call) => { busyRef.current = kind; setBusy(kind); try { await call(); } finally { busyRef.current = ''; setBusy(''); } };

  const edit = (index, text) => { setDraft((rows) => rows.map((x, i) => (i === index ? text : x))); mark(true); setRowErr(null); };
  const add = () => { setDraft((rows) => rows.concat([''])); mark(true); setRowErr(null); };
  const remove = (index) => { setDraft((rows) => rows.filter((_, i) => i !== index)); mark(true); setRowErr(null); };

  const save = () => run('save', async () => {
    const kept = draft.map((text, index) => ({ text: text.trim(), index })).filter((x) => x.text); // blank rows are dropped, the row index of an error maps back
    try {
      const r = await ctx.api.put('network', { allow: kept.map((x) => x.text), confirm: true }, CALL_TIMEOUT_MS);
      setDraft(list(r.project)); mark(false); setRowErr(null);
      toast('Network list saved.', 'ok');
      await load();
    } catch (e) {
      const body = e.body || {};
      if (body.code === 'invalid_network_entry' && Number.isInteger(body.index) && kept[body.index]) setRowErr({ index: kept[body.index].index, reason: body.reason || e.message });
      else toast('Not saved: ' + e.message, 'err');
    }
  });
  const discard = () => { setDraft(list(data && data.project)); mark(false); setRowErr(null); };
  const suggest = (action) => run(action, async () => {
    try {
      const r = await ctx.api.post('network/suggestion', { action, confirm: true }, CALL_TIMEOUT_MS);
      toast(action === 'apply' ? `${list(r.added).length} ${list(r.added).length === 1 ? 'entry' : 'entries'} added to the network list.` : 'Suggestion dismissed.', 'ok');
      await load();
    } catch (e) { toast('Not done: ' + e.message, 'err'); }
  });

  if (gone) return null; // an older back end has no network routes
  if (!data) return html`<div class="plan-env-sec plan-net" id="plan-net"><h3>Network</h3>${error ? html`<div class="nl-box nl-bad" role="alert">Could not read the network list: ${error}</div>` : html`<p class="plan-env-note">loading…</p>`}</div>`;
  const notes = list(data.notes), suggestions = list(data.suggestions);
  return html`<div class="plan-env-sec plan-net" id="plan-net"><h3 id="plan-net-h">Network</h3>
    <p class="plan-env-note">Inside the loop the agents reach only these hosts (a host can also carry a path, methods and permission to send credentials). A deny below always wins. Interactive sessions are not limited.</p>
    ${data.broken ? html`<div class="nl-box nl-bad" role="alert" id="plan-net-broken"><b>${data.broken}</b> cannot be read: while it stays broken, the loop blocks every request that is not to this machine. Fix the file or save the list again.</div>` : null}
    ${error ? html`<div class="nl-err" role="alert">${error}</div>` : null}
    ${data.suggestions_exists ? html`<${SuggestionBanner} suggestions=${suggestions} dirty=${dirty} busy=${busy} onAct=${suggest} />` : null}
    <ul class="plan-net-list" aria-labelledby="plan-net-h" id="plan-net-rows">
      ${draft.map((text, i) => html`<li key=${i} class="plan-net-row" data-row=${i}>
        <label class="sr-only" for=${'plan-net-in-' + i}>Network entry ${i + 1}</label>
        <input id=${'plan-net-in-' + i} type="text" class=${'plan-net-in' + (rowErr && rowErr.index === i ? ' bad' : '')} spellcheck="false" autocomplete="off"
          value=${text} placeholder=${GRAMMAR} aria-invalid=${String(!!rowErr && rowErr.index === i)} disabled=${!!busy}
          onInput=${(e) => edit(i, e.currentTarget.value)} onKeyDown=${(e) => { if (e.key === 'Enter') { e.preventDefault(); if (dirty && !isBlank(text)) save(); } }} />
        <button type="button" class="btn btn-sm btn-ghost" aria-label=${'Remove entry ' + (i + 1)} disabled=${!!busy} onClick=${() => remove(i)}>Remove</button>
        ${rowErr && rowErr.index === i ? html`<div class="nl-err plan-net-err" role="alert">${rowErr.reason}</div>` : null}</li>`)}
      ${draft.length ? null : html`<li class="plan-net-none">No entry for this project yet: only the inherited hosts below are reachable.</li>`}
    </ul>
    <div class="plan-net-act">
      <button type="button" class="btn btn-sm" id="plan-net-add" disabled=${!!busy} onClick=${add}>Add entry</button>
      <button type="button" class="btn btn-sm btn-primary" id="plan-net-save" disabled=${!!busy || !dirty} onClick=${save}>${busy === 'save' ? 'Saving…' : 'Save'}</button>
      ${dirty ? html`<button type="button" class="btn btn-sm btn-ghost" id="plan-net-discard" disabled=${!!busy} onClick=${discard}>Discard changes</button>` : null}
    </div>
    <p class="plan-net-hint">One entry per row: <code>${GRAMMAR}</code>. METHODS: a comma list of GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS, or <code>*</code>, or <code>tcp</code>; none means GET,HEAD. <code>cred</code> lets credentials go to that host. <code>if-missing=NAME</code> keeps the entry off once NAME has a value in .env.test. Example: <code>${EXAMPLES}</code></p>
    ${notes.length ? html`<details class="plan-net-notes"><summary>${notes.length} note${notes.length === 1 ? '' : 's'} from reading the lists</summary><ul>${notes.map((x, i) => html`<li key=${i}>${x}</li>`)}</ul></details>` : null}
    <${InheritedRows} effective=${list(data.effective)} deny=${list(data.deny)} />
  </div>`;
}
