/* Error center store: GET errors (never in the 2 s polling). The Errors page draws it through problems.js; the sidebar and the header read the
   count. `errorCount` = real problems only (guard blocks and notes never count). */
import { ctx, ui } from './context.js';
import { renderAll } from '../project_panel.js';
import { navigate } from './router.js';

/** Real problems only (errors + warnings); guard blocks and notes never count. */
export const errorCount = () => { const d = ui.errors.data || {}, c = d.counts || {}; return d.problems != null ? d.problems : (c.error || 0) + (c.warning || 0); };

/** Reloads when the loop moved on (turn count, status, guard total) or when forced. */
export async function loadErrors(force) {
  const state = ctx.state, es = ui.errors;
  const key = [state && state.meta ? state.meta.turns : '', state && state.loop ? state.loop.status : '', Object.values((state && state.guard_counts) || {}).reduce((a, b) => a + b, 0)].join('|');
  if (es.loading || (!force && es.key === key)) return;
  es.loading = true; es.key = key; es.err = null; if (!es.data) renderAll();
  try { es.data = await ctx.api.get('errors', 30000); }
  catch (e) { es.err = e.status === 404 ? 'The server does not offer the error list yet.' : e.message; }
  finally { es.loading = false; renderAll(); }
}

/** Opens the Errors page (and reads the list again). */
export function openErrors() { navigate('/errors'); loadErrors(true); }
