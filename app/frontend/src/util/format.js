/* Formatting: null becomes a dash, never an invented number. */

export const DASH = '—';
const HOUR_S = 60 * 60;
export const isNum = (v) => typeof v === 'number' && Number.isFinite(v);
export const num = (v, d = 2) => (isNum(v) ? (Number.isInteger(v) ? String(v) : v.toFixed(d)) : DASH);
export const cost = (v) => (isNum(v) ? '$' + v.toFixed(2) : DASH);
export function dur(s) {
  if (!isNum(s)) return DASH;
  s = Math.floor(s);
  const hh = Math.floor(s / HOUR_S), mm = Math.floor((s % HOUR_S) / 60), ss = s % 60;
  const p = (n) => String(n).padStart(2, '0');
  return hh ? `${hh}:${p(mm)}:${p(ss)}` : `${p(mm)}:${p(ss)}`;
}
export const txt = (v) => (v == null || v === '' ? DASH : String(v));
export const list = (v) => (Array.isArray(v) ? v : []);
/** One line of text cut to `max` characters with an ellipsis (whitespace collapsed). */
export const clip = (text, max) => { const flat = String(text == null ? '' : text).replace(/\s+/g, ' ').trim(); return flat.length > max ? flat.slice(0, max - 1) + '…' : flat; };
/** Money as `$1.23`; a dash when the value is not a number (the one money formatter: `cost`). */
export const money = cost;
export const firstLine = (t) => String(t == null ? '' : t).split('\n').map((x) => x.trim()).find(Boolean) || DASH;
export const httpUrl = (u) => (typeof u === 'string' && /^https?:\/\//i.test(u) ? u : null);
/** Long path cut in the middle at folder boundaries (keeps the drive and the last folders); the full value goes in a title. */
export const midPath = (p, max = 48) => {
  const v = String(p == null ? '' : p);
  if (v.length <= max) return v;
  const sep = v.includes('\\') ? '\\' : '/', parts = v.split(sep), head = parts[0] + sep + '...' + sep;
  let tail = parts[parts.length - 1];
  for (let i = parts.length - 2; i > 0 && (head + parts[i] + sep + tail).length <= max; i -= 1) tail = parts[i] + sep + tail;
  return parts.length > 2 && (head + tail).length <= max ? head + tail : v.slice(0, Math.ceil(max / 3)) + '...' + v.slice(-(max - Math.ceil(max / 3) - 1));
};
/** Masks the user's home folder in a path that is going on screen (v2 rule R3): `C:\Users\<name>\x` and `/home/<name>/x` and `/Users/<name>/x`
    become `~\x` / `~/x`. The real path stays available through the copy actions only. */
export const maskHome = (p) => String(p == null ? '' : p).replace(/^[A-Za-z]:[\\/]+Users[\\/]+[^\\/]+(?=[\\/]|$)/i, '~').replace(/^\/(?:home|Users)\/[^/]+(?=\/|$)/, '~');

export const fmtHour = (iso) => { const d = new Date(iso); return iso && !Number.isNaN(d.getTime()) ? d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' }) : ''; };
export const fmtDay = (iso) => { const d = new Date(iso); return iso && !Number.isNaN(d.getTime()) ? d.toLocaleString(undefined, { day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit' }) : DASH; };

/** "3 min ago", "2 h ago", "yesterday"... for ISO dates; a dash when invalid. */
export function fmtRelative(iso) {
  const d = new Date(iso);
  if (!iso || Number.isNaN(d.getTime())) return DASH;
  const s = Math.round((Date.now() - d.getTime()) / 1000);
  if (s < 60) return 'just now';
  if (s < HOUR_S) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / HOUR_S)} h ago`;
  if (s < 172800) return 'yesterday';
  return d.toLocaleDateString(undefined, { day: '2-digit', month: '2-digit', year: 'numeric' });
}

// ---- shared by the error list / dialog and the big dialogs (one copy each) ----
/** Colour of an error-center severity. */
export const SEVERITY_COLOR = { error: 'var(--danger)', warning: 'var(--warn)', info: 'var(--info)' };
/** Plain words of an error-center item kind (and of a failed agent step). */
export const ERROR_KIND_LABEL = { loop: 'loop stopped', turn: 'turn', guard: 'guard', feature: 'feature', launch: 'launch', step: 'agent step' };
/** Badge colour of the big dialogs that carry no state of their own. */
export const BADGE_INFO = 'var(--info)';
/** Last segment of a path (either slash), without a trailing slash. */
export const baseName = (path) => String(path || '').replace(/[\\/]+$/, '').split(/[\\/]/).pop() || '';
/** Seconds between two ISO dates (null when either is invalid, never negative). */
export const secondsBetween = (from, to) => { const a = Date.parse(from), b = Date.parse(to); return Number.isFinite(a) && Number.isFinite(b) ? Math.max(0, (b - a) / 1000) : null; };
