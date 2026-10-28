/* "Permissions" dialog: the journal of what was released to the loop (state.permissions_log, newest first) and a small form for the same release as
   `harness grant` (POST access/grant {kind, value, reason}). The back end validates, refuses production and journals; values and reasons are
   redacted there before they are written, and nothing here shows more than the journal holds. */
import { html, useState } from '../../util/preact.js';
import { list, txt } from '../../util/format.js';
import { openDialog, confirmDialog } from '../../components/dialog.js';
import { toast } from '../../components/toast.js';
import { ctx } from './context.js';

const KINDS = [
  { value: 'host', label: 'Network host', hint: 'A development host and its methods, for example "dev.example.com GET,POST". Production is never released.' },
  { value: 'cost_limit', label: 'Cost limit (US$)', hint: 'The new limit for the whole run, for example 65.' },
];
const KIND_TEXT = { host: 'host', cost_limit: 'cost limit', baseline: 'baseline' };

/** Entries of the journal in the last state read (newest first). */
export const permissionsEntries = (state = ctx.state) => list(state && state.permissions_log && state.permissions_log.entries);

function Row({ entry }) {
  const bad = entry.outcome === 'refused';
  return html`<li class=${'perm-row' + (bad ? ' is-bad' : '')}>
    <div><span class="mono">${txt(entry.at)}</span> · <strong>${KIND_TEXT[entry.kind] || txt(entry.kind)}</strong> <span class="mono">${txt(entry.value)}</span>
      <span class=${'val-status mono' + (bad ? ' is-bad' : '')}>${bad ? 'refused' : 'granted'}</span></div>
    <div class="hint">${txt(entry.reason)}${entry.who ? ' · by ' + txt(entry.who) : ''}${entry.detail ? ' · ' + txt(entry.detail) : ''}</div>
  </li>`;
}

function Body() {
  const [log, setLog] = useState(() => (ctx.state && ctx.state.permissions_log) || null);
  const [kind, setKind] = useState('host'), [value, setValue] = useState(''), [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false), [msg, setMsg] = useState(null);
  const entries = list(log && log.entries);
  const hint = (KINDS.find((k) => k.value === kind) || {}).hint;
  const grant = async () => {
    const ok = await confirmDialog({
      title: 'Release this to the loop?', yesText: 'Release',
      text: `This adds ${KIND_TEXT[kind]} "${value.trim()}" for this project and writes it to the permissions journal with your reason.`,
    });
    if (!ok) return;
    setBusy(true); setMsg(null);
    try {
      const r = await ctx.api.post('access/grant', { kind, value: value.trim(), reason: reason.trim() }, 30000);
      setMsg({ ok: r.ok === true, text: r.message || (r.ok ? 'Released.' : 'Refused.') });
      if (r.ok) { setValue(''); setReason(''); toast('Released.', 'ok'); }
      const fresh = await ctx.api.get('state?visible=1', 15000);
      if (fresh && fresh.permissions_log) setLog(fresh.permissions_log);
    } catch (e) { setMsg({ ok: false, text: e.message }); }
    finally { setBusy(false); }
  };
  return html`<div class="perm">
    <p class="dl-explain">What was released to the loop (network hosts, the cost limit, the baseline) and why. Every release is written to the journal, refusals included.</p>
    <div class="perm-form" role="group" aria-label="Release access">
      <label class="perm-f">Kind <select value=${kind} onChange=${(e) => setKind(e.currentTarget.value)} disabled=${busy}>${KINDS.map((k) => html`<option key=${k.value} value=${k.value}>${k.label}</option>`)}</select></label>
      <label class="perm-f">Value <input type="text" spellcheck="false" value=${value} onInput=${(e) => setValue(e.currentTarget.value)} disabled=${busy} /></label>
      <label class="perm-f">Reason <input type="text" value=${reason} onInput=${(e) => setReason(e.currentTarget.value)} disabled=${busy} /></label>
      <div class="hint">${hint}</div>
      <div><button type="button" class="btn btn-sm btn-primary" id="perm-grant" disabled=${busy || !value.trim() || !reason.trim()} onClick=${grant}>${busy ? 'Releasing…' : 'Release'}</button></div>
      ${msg ? html`<div class=${'nl-box ' + (msg.ok ? 'nl-ok' : 'nl-bad')} role=${msg.ok ? 'status' : 'alert'}>${msg.text}</div>` : null}
    </div>
    <h3 class="dl-turn mono">Journal${log ? ` · ${log.granted || 0} granted · ${log.refused || 0} refused` : ''}</h3>
    ${entries.length ? html`<ul class="perm-list">${entries.map((entry, index) => html`<${Row} key=${entry.id || index} entry=${entry} />`)}</ul>` : html`<div class="dl-empty">Nothing was released yet.</div>`}
  </div>`;
}

export function openPermissionsDialog() {
  openDialog({ title: 'Permissions', width: 760, cls: 'dlg-permissions', content: html`<${Body} />`, actions: [{ text: 'Close' }] });
}
