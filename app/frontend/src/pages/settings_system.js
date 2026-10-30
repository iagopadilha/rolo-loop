/* Diagnostics of the app (GET /api/app/info): the checks list shared by the hub system strip and Settings -> System, the Notifications
   section (channels + test) and the Claude commands/usage. One source: checksOf(). Every path shown goes through maskIn. */
import { html, useState, useEffect } from '../util/preact.js';
import { list, txt, DASH } from '../util/format.js';
import { appApi } from '../api/api_client.js';
import { toast } from '../components/toast.js';
import { SettingsGroup, SettingsRow, SettingsBlock, Loading, Note } from '../components/settings_ui.js';
import { windowText, ageText } from './panel/claude_badge.js';

const HOME_ANYWHERE = /[A-Za-z]:[\\/]+Users[\\/]+[^\\/:*?"<>|]+|\/(?:home|Users)\/[^/\s:]+/g;
/** maskHome for a text that may carry a path anywhere inside it. */
export const maskIn = (text) => String(text == null ? '' : text).replace(HOME_ANYWHERE, '~');

const bool = (v) => (typeof v === 'boolean' ? v : v == null ? null : !!v);
function enabledChannels(v) {
  if (Array.isArray(v)) return v.filter((x) => typeof x === 'string' || (x && x.enabled !== false)).map((x) => (typeof x === 'string' ? x : x && x.name)).filter(Boolean);
  if (v && typeof v === 'object') return Object.entries(v).filter(([, on]) => on).map(([k]) => k);
  return [];
}
/** One entry per configured claude command: role, found or "not found (skipped)", auth method (API key = pay per token), e-mail, org, plan. */
function claudeCommandEntry(command, index) {
  const auth = command.auth || {};
  const who = [auth.email, auth.org, auth.plan].filter(Boolean).join(' · ');
  const method = auth.method === 'API key' ? '⚠ API key (pay per token)' : auth.method;
  const detail = command.found ? [command.name, method, who, auth.error ? 'auth: ' + auth.error : ''].filter(Boolean).join(' · ') : command.name + ' · not found (skipped)';
  return { label: 'claude command (' + (command.role === 'main' ? 'main' : 'fallback') + ')', ok: command.found ? true : command.role === 'main' ? false : null, detail, key: 'claude-command-' + index };
}
const KNOWN_INFO = new Set(['skill_dir', 'guard', 'claude', 'notify_channels', 'data_dir', 'version', 'seconds_until_shutdown', 'problems', 'ok', 'pid', 'url']);

/** The checks of GET /api/app/info as [{label, ok, detail, key}] (ok: true | false | null = informative). */
export function checksOf(r) {
  const guard = r.guard || {}, claude = r.claude || {};
  const guardOk = bool(guard.blocks_git_push != null ? guard.blocks_git_push : guard.found);
  const channels = enabledChannels(r.notify_channels);
  return [
    { label: '/orchestrator skill', ok: r.skill_dir ? true : null, detail: r.skill_dir ? String(r.skill_dir) : '' },
    { label: 'guard blocking git push', ok: guardOk, detail: guard.path ? String(guard.path) : '' },
    { label: 'claude on PATH', ok: bool(claude.on_path), detail: claude.binary_configured ? String(claude.binary_configured) : '' },
    ...list(r.claude_commands).map(claudeCommandEntry),
    { label: 'notification channels', ok: channels.length ? true : null, detail: channels.length ? channels.join(', ') : 'none configured' },
    ...(r.data_dir ? [{ label: 'data folder', ok: null, detail: String(r.data_dir) }] : []),
    ...list(r.problems).map((p, i) => ({ label: 'problem', ok: false, detail: txt(p), key: 'problem-' + i })),
  ].map((c) => ({ key: c.label, ...c }));
}
const FAIL_TEXT = {
  'guard blocking git push': 'the guard is not blocking git push',
  'claude on PATH': 'claude was not found on PATH',
  '/orchestrator skill': 'the /orchestrator skill was not found',
};
export const failText = (c) => (c.label === 'problem' ? maskIn(c.detail) : FAIL_TEXT[c.label] || c.label + ' is missing');

/** POST notify/test with the toast the user sees (used by the hub strip and the Notifications section). */
export async function sendTestNotification() {
  try {
    const r = await appApi.post('notify/test', {}, 90000);
    const results = r.results && typeof r.results === 'object' ? Object.entries(r.results).map(([k, v]) => `${k}: ${v}`).join(' · ') : '';
    toast('Test notification sent' + (results ? ': ' + results.slice(0, 160) : '.'), 'ok');
  } catch (e) { toast('The test notification failed: ' + e.message, 'err'); }
}

/** GET info for a section: { info, error } (info is null while loading). */
function useAppInfo() {
  const [state, setState] = useState({ info: null, error: '' });
  useEffect(() => {
    let alive = true;
    appApi.get('info', 15000).then((info) => { if (alive) setState({ info, error: '' }); }).catch((e) => { if (alive) setState({ info: null, error: e.message }); });
    return () => { alive = false; };
  }, []);
  return state;
}

function StateMark({ ok }) {
  const [text, tone] = ok === true ? ['OK', 'ok'] : ok === false ? ['Missing', 'bad'] : [DASH, 'na'];
  return html`<span class=${'st-state is-' + tone}><span aria-hidden="true" class="st-state-ic">${ok === true ? '✓' : ok === false ? '✗' : '·'}</span>${text}</span>`;
}

function Unavailable({ error }) {
  return error ? html`<${Note} kind="err">Diagnostics unavailable: ${maskIn(error)}<//>` : html`<${Loading} text="Reading diagnostics…" />`;
}

/** Settings -> System: every check, the version, the guard and the extra fields. */
export function SystemSection() {
  const { info, error } = useAppInfo();
  if (!info) return html`<${SettingsGroup} title="Checks"><${SettingsBlock}><${Unavailable} error=${error} /><//><//>`;
  const rest = Object.entries(info).filter(([k, v]) => !KNOWN_INFO.has(k) && (v == null || typeof v !== 'object'));
  const shutdownIn = info.seconds_until_shutdown;
  const version = info.version ? (/^v/i.test(String(info.version)) ? String(info.version) : 'v' + info.version) : '';
  return html`<${SettingsGroup} title="Checks" desc="What the app needs on this machine to run a loop safely." id="st-checks">
      ${checksOf(info).map((c) => html`<${SettingsRow} key=${c.key} label=${c.label} desc=${c.detail ? html`<span class="st-path" title=${maskIn(c.detail)}>${maskIn(c.detail)}</span>` : null}><${StateMark} ok=${c.ok} /><//>`)}
    <//>
    <${SettingsGroup} title="About" id="st-about">
      <${SettingsRow} label="Version"><span class="st-value mono">${version || DASH}</span><//>
      ${typeof shutdownIn === 'number' ? html`<${SettingsRow} label="Idle shutdown" desc="The app shuts itself down after a while without use."><span class="st-value">in ${Math.round(shutdownIn / 60)} min</span><//>` : null}
    <//>
    ${rest.length ? html`<${SettingsGroup} title="More details" id="st-more">
      ${rest.map(([k, v]) => html`<${SettingsRow} key=${k} label=${k}>${typeof v === 'boolean' ? html`<${StateMark} ok=${v} />` : html`<span class="st-value mono" title=${maskIn(txt(v))}>${maskIn(txt(v))}</span>`}<//>`)}
    <//>` : null}`;
}

/** Settings -> Notifications: the channels (read only, from GET info) and the test notification. */
export function NotificationsSection() {
  const { info, error } = useAppInfo();
  const [busy, setBusy] = useState(false);
  const channels = info ? list(info.notify_channels) : [];
  const send = async () => { setBusy(true); try { await sendTestNotification(); } finally { setBusy(false); } };
  return html`<${SettingsGroup} title="Channels" desc="Where the app tells you a loop needs you. Channels are turned on in config.json." id="st-channels">
      ${!info ? html`<${SettingsBlock}><${Unavailable} error=${error} /><//>`
        : channels.length ? channels.map((c) => {
          const name = typeof c === 'string' ? c : c.name;
          const on = typeof c === 'string' ? true : c.enabled !== false;
          const note = [c.required ? 'required' : '', on && c.configured === false ? 'not configured' : ''].filter(Boolean).join(' · ');
          return html`<${SettingsRow} key=${name} label=${name} desc=${note || null}><span class=${'st-state is-' + (on ? 'ok' : 'na')}>${on ? 'On' : 'Off'}</span><//>`;
        }) : html`<${SettingsBlock}><span class="st-empty">No notification channel is configured.</span><//>`}
    <//>
    <${SettingsGroup} title="Test" id="st-test">
      <${SettingsRow} label="Send a test notification" desc="Sends one message through every enabled channel."><button type="button" class="btn btn-sm" id="st-test-notify" disabled=${busy} onClick=${send}>${busy ? 'Sending…' : 'Send test'}</button><//>
    <//>`;
}

/** Claude commands and their 5-hour / weekly usage (GET info). */
export function ClaudeAccountGroup() {
  const { info, error } = useAppInfo();
  if (!info) return html`<${SettingsGroup} title="Account and usage" id="st-account"><${SettingsBlock}><${Unavailable} error=${error} /><//><//>`;
  const commands = list(info.claude_commands);
  const usage = list(info.claude_usage);
  return html`<${SettingsGroup} title="Account and usage" desc="The claude commands the loop may run, the account each is signed in to and how much of its limits is used." id="st-account">
    ${commands.length ? commands.map((c, i) => {
      const auth = c.auth || {};
      const who = [auth.email, auth.org, auth.plan].filter(Boolean).join(' · ');
      const method = auth.method && auth.method !== '?' ? (auth.method === 'API key' ? 'API key (billed per token)' : auth.method) : '';
      const row = list(c.usage)[0] || usage.find((u) => u.command && String(c.name).toLowerCase().includes(String(u.command).toLowerCase()));
      const windows = row ? list(row.windows) : [];
      const usageText = windows.length ? windows.map(windowText).join(' · ') + (ageText(row.age_seconds) ? ' · ' + ageText(row.age_seconds) : '') : 'usage unknown';
      return html`<${SettingsRow} key=${i} stack label=${c.role === 'main' ? 'Main command' : 'Fallback command'} desc=${html`<span class="st-path" title=${maskIn(c.name)}>${maskIn(c.name)}</span>`}
        extra=${html`<div class="st-facts">
          <span>${c.found ? 'Found' : 'Not found (skipped)'}</span>${method ? html`<span>${method}</span>` : null}${who ? html`<span>${who}</span>` : null}
          <span class=${windows.some((w) => w.exhausted || w.pct >= 100) ? 'is-warn' : ''}>${usageText}</span></div>`} />`;
    }) : html`<${SettingsBlock}><span class="st-empty">No claude command found.</span><//>`}
  <//>`;
}
