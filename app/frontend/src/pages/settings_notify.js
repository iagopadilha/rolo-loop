/* Settings > Notifications: where and when the app tells you about a loop. Channels (Windows notification, e-mail, webhook), the events
   that notify, the e-mail sender (SMTP, Amazon SES over SMTP, or an HTTP e-mail API) and the webhook (URL, method, headers, a preset or your
   own JSON body template). GET/POST /api/app/settings/notifications; POST /api/app/notify/test sends a real test per channel.
   Passwords, tokens, the webhook URL and its headers are saved encrypted and only shown masked. */
import { html, useState, useEffect } from '../util/preact.js';
import { appApi } from '../api/api_client.js';
import { toast } from '../components/toast.js';
import { SettingsGroup, SettingsRow, SettingsBlock, Toggle, SelectField, TextField, TextArea, Note, Loading } from '../components/settings_ui.js';

const CHANNELS = [['toast', 'Windows notification', 'A notification in the corner of the screen.'], ['email', 'E-mail', 'One message per event to the addresses below.'],
  ['webhook', 'Webhook', 'An HTTP request to Slack, Teams, Discord or any URL.'], ['file', 'File in the project', 'NOTICE.txt in the loop folder.']];
const SES_REGIONS = ['us-east-1', 'us-east-2', 'us-west-2', 'eu-west-1', 'eu-central-1', 'sa-east-1', 'ap-southeast-1'];
const PRESETS = [['generic', 'Generic JSON'], ['slack', 'Slack'], ['teams', 'Microsoft Teams'], ['discord', 'Discord']];
const PLACEHOLDERS = '{{event}} {{project}} {{subject}} {{body}} {{url}} {{turn}} {{cost}} {{at}}';
const listOf = (text) => String(text || '').split(',').map((s) => s.trim()).filter(Boolean);

function SecretField({ id, field, value, onInput, placeholder }) {
  return html`<${TextField} id=${id} type="password" mono=${true} value=${value} onInput=${onInput}
    placeholder=${field && field.set ? field.masked + ' (saved)' : placeholder} />`;
}

export function NotificationsV3Section() {
  const [data, setData] = useState(null);
  const [draft, setDraft] = useState({});
  const [error, setError] = useState('');
  const [testing, setTesting] = useState('');
  const load = async () => { try { setData(await appApi.get('settings/notifications')); setDraft({}); setError(''); } catch (e) { setError(e.message); } };
  useEffect(() => { load(); }, []);
  if (error) return html`<${Note} kind="err">${error}<//>`;
  if (!data) return html`<${Loading} text="Reading the notifications…" />`;
  const plain = (name) => (name in draft ? draft[name] : ((data.fields[name] || {}).value || ''));
  const secret = (name) => (name in draft ? draft[name] : '');
  const set = (name, value) => setDraft({ ...draft, [name]: value });
  const channels = listOf(plain('LOOP_NOTIFY_CHANNELS'));
  const events = listOf(plain('LOOP_NOTIFY_EVENTS'));
  const toggleIn = (name, current, item, on) => set(name, (on ? [...current, item] : current.filter((x) => x !== item)).join(','));
  const provider = plain('LOOP_EMAIL_PROVIDER') || 'smtp';
  const dirty = Object.keys(draft).length > 0;
  const save = async () => {
    try { setData(await appApi.post('settings/notifications', { fields: draft })); setDraft({}); toast('Notifications saved.', 'ok'); } catch (e) { toast(e.message, 'err'); }
  };
  const test = async (channel) => {
    setTesting(channel);
    try {
      const r = await appApi.post('notify/test', { channels: [channel] }, 90000);
      const line = Object.entries(r.results || {}).map(([name, text]) => name + ': ' + text).join(' · ') || 'sent';
      toast(line, /FAILED|SKIPPED/.test(line) ? 'err' : 'ok');
    } catch (e) { toast(e.message, 'err'); } finally { setTesting(''); }
  };
  const ses = (region) => setDraft({ ...draft, LOOP_EMAIL_PROVIDER: 'smtp', LOOP_SMTP_HOST: `email-smtp.${region}.amazonaws.com`, LOOP_SMTP_PORT: '587', LOOP_SMTP_SECURITY: 'starttls' });
  const missing = (name) => ((data.channels || []).find((c) => c.name === name) || {}).missing || [];
  const testButton = (name) => html`<button type="button" class="btn btn-sm" disabled=${!!testing || dirty} title=${dirty ? 'Save first' : ''} onClick=${() => test(name)}>${testing === name ? 'Sending…' : 'Send test'}</button>`;
  return html`
    <${SettingsGroup} id="nt-channels" title="Channels" desc="Where the app tells you a loop needs you or finished. None selected = Windows notification and the file.">
      ${CHANNELS.map(([name, label, desc]) => html`<${SettingsRow} key=${name} label=${label} desc=${desc}
        tags=${channels.includes(name) && missing(name).length ? [{ text: 'needs setup', tone: 'warn', title: missing(name).join(', ') }] : []}>
        <div class="st-actions">${channels.includes(name) ? testButton(name) : null}
          <${Toggle} id=${'nt-ch-' + name} label=${label} checked=${channels.includes(name)} onChange=${(on) => toggleIn('LOOP_NOTIFY_CHANNELS', channels, name, on)} /></div><//>`)}
    <//>
    <${SettingsGroup} id="nt-events" title="Events" desc="Which events notify. None selected = all of them.">
      ${data.events.map((ev) => html`<${SettingsRow} key=${ev.id} label=${ev.label}>
        <${Toggle} id=${'nt-ev-' + ev.id} label=${ev.label} checked=${events.length === 0 || events.includes(ev.id)}
          onChange=${(on) => { const all = data.events.map((e) => e.id); const current = events.length ? events : all; const next = on ? [...current, ev.id] : current.filter((x) => x !== ev.id); set('LOOP_NOTIFY_EVENTS', next.length === all.length ? '' : next.join(',')); }} /><//>`)}
    <//>
    <${SettingsGroup} id="nt-email" title="E-mail" desc="SMTP works with any provider (Gmail, Outlook, Amazon SES, SendGrid, Mailgun). An HTTP e-mail API is the alternative.">
      <${SettingsRow} label="Send with" labelFor="nt-provider">
        <${SelectField} id="nt-provider" value=${provider} options=${[{ value: 'smtp', label: 'SMTP' }, { value: 'http', label: 'HTTP e-mail API' }]} onChange=${(v) => set('LOOP_EMAIL_PROVIDER', v)} /><//>
      <${SettingsRow} label="Recipients" desc="Comma-separated addresses." labelFor="nt-to"><${TextField} id="nt-to" value=${plain('LOOP_NOTIFY_TO')} placeholder="you@example.com" onInput=${(v) => set('LOOP_NOTIFY_TO', v)} /><//>
      ${provider === 'smtp' ? html`
        <${SettingsRow} label="Amazon SES" desc="Fills host, port and security for the SES SMTP endpoint of a region (use SES SMTP credentials below)." labelFor="nt-ses">
          <${SelectField} id="nt-ses" value="" options=${[{ value: '', label: 'Choose a region…' }, ...SES_REGIONS.map((r) => ({ value: r, label: r }))]} onChange=${(v) => v && ses(v)} /><//>
        <${SettingsRow} label="Server" labelFor="nt-host"><div class="st-actions">
          <${TextField} id="nt-host" mono=${true} value=${plain('LOOP_SMTP_HOST')} placeholder="smtp.example.com" onInput=${(v) => set('LOOP_SMTP_HOST', v)} />
          <${TextField} id="nt-port" label="Port" numeric=${true} className="is-short" value=${plain('LOOP_SMTP_PORT')} placeholder="587" onInput=${(v) => set('LOOP_SMTP_PORT', v)} />
          <${SelectField} id="nt-sec" label="Security" value=${plain('LOOP_SMTP_SECURITY') || 'starttls'} options=${[{ value: 'starttls', label: 'STARTTLS' }, { value: 'tls', label: 'TLS' }, { value: 'none', label: 'None' }]} onChange=${(v) => set('LOOP_SMTP_SECURITY', v)} /></div><//>
        <${SettingsRow} label="User" labelFor="nt-user"><${TextField} id="nt-user" value=${plain('LOOP_SMTP_USER')} onInput=${(v) => set('LOOP_SMTP_USER', v)} /><//>
        <${SettingsRow} label="Password" labelFor="nt-pass"><${SecretField} id="nt-pass" field=${data.fields.LOOP_SMTP_PASSWORD} value=${secret('LOOP_SMTP_PASSWORD')} onInput=${(v) => set('LOOP_SMTP_PASSWORD', v)} placeholder="app password" /><//>
        <${SettingsRow} label="From" labelFor="nt-from"><${TextField} id="nt-from" value=${plain('LOOP_SMTP_FROM')} placeholder="Agent Maestro <maestro@example.com>" onInput=${(v) => set('LOOP_SMTP_FROM', v)} /><//>` : html`
        <${SettingsRow} label="API URL" desc="POST endpoint of your e-mail service." labelFor="nt-http-url"><${TextField} id="nt-http-url" mono=${true} value=${plain('LOOP_EMAIL_HTTP_URL')} onInput=${(v) => set('LOOP_EMAIL_HTTP_URL', v)} /><//>
        <${SettingsRow} label="API token" labelFor="nt-http-token"><${SecretField} id="nt-http-token" field=${data.fields.LOOP_EMAIL_HTTP_TOKEN} value=${secret('LOOP_EMAIL_HTTP_TOKEN')} onInput=${(v) => set('LOOP_EMAIL_HTTP_TOKEN', v)} placeholder="token" /><//>
        <${SettingsRow} label="From" labelFor="nt-http-from"><${TextField} id="nt-http-from" value=${plain('LOOP_EMAIL_HTTP_FROM')} onInput=${(v) => set('LOOP_EMAIL_HTTP_FROM', v)} /><//>`}
    <//>
    <${SettingsGroup} id="nt-webhook" title="Webhook" desc=${'A JSON request per event. Pick a preset or write your own body with ' + PLACEHOLDERS + '.'}>
      <${SettingsRow} label="URL" desc="Saved encrypted (webhook URLs usually carry a token)." labelFor="nt-wh-url">
        <${SecretField} id="nt-wh-url" field=${data.fields.LOOP_WEBHOOK_URL} value=${secret('LOOP_WEBHOOK_URL')} onInput=${(v) => set('LOOP_WEBHOOK_URL', v)} placeholder="https://hooks.example.com/…" /><//>
      <${SettingsRow} label="Format" labelFor="nt-wh-preset"><div class="st-actions">
        <${SelectField} id="nt-wh-preset" value=${plain('LOOP_WEBHOOK_PRESET') || 'generic'} options=${PRESETS.map(([v, l]) => ({ value: v, label: l }))} onChange=${(v) => set('LOOP_WEBHOOK_PRESET', v)} />
        <${SelectField} id="nt-wh-method" label="Method" value=${plain('LOOP_WEBHOOK_METHOD') || 'POST'} options=${[{ value: 'POST', label: 'POST' }, { value: 'PUT', label: 'PUT' }]} onChange=${(v) => set('LOOP_WEBHOOK_METHOD', v)} /></div><//>
      <${SettingsRow} label="Custom body" desc="Optional. Replaces the format above." stack=${true} labelFor="nt-wh-body">
        <${TextArea} id="nt-wh-body" rows=${4} value=${plain('LOOP_WEBHOOK_BODY')} placeholder='{"text": "{{project}}: {{subject}}"}' onInput=${(v) => set('LOOP_WEBHOOK_BODY', v)} /><//>
      <${SettingsRow} label="Headers" desc='Optional JSON object, saved encrypted. E.g. {"Authorization": "Bearer …"}' stack=${true} labelFor="nt-wh-headers">
        <${SecretField} id="nt-wh-headers" field=${data.fields.LOOP_WEBHOOK_HEADERS} value=${secret('LOOP_WEBHOOK_HEADERS')} onInput=${(v) => set('LOOP_WEBHOOK_HEADERS', v)} placeholder="{}" /><//>
    <//>
    <div class="st-savebar" hidden=${!dirty}><span>Unsaved changes to the notifications.</span>
      <button type="button" class="btn btn-sm" onClick=${() => setDraft({})}>Discard</button>
      <button type="button" class="btn btn-sm btn-primary" id="nt-save" onClick=${save}>Save notifications</button></div>`;
}
