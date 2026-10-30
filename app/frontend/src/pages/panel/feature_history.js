/* Every agent call that worked on a feature, turn by turn (oldest first). Past turns come from GET history, the running turn from the live state. */
import { list } from '../../util/format.js';
import { ctx, ui } from './context.js';

const sameFeature = (a, id) => String(a.feature || '').split(',').map((s) => s.trim()).includes(id);

/** [{ n, live, agents }] for the feature, oldest turn first. */
export function featureCalls(id) {
  const state = ctx.state || {}, running = !!(state.loop && state.loop.running), liveTurn = running && state.loop ? state.loop.turn : null;
  const turns = list(ui.hist.data).filter((t) => t.n !== liveTurn).map((t) => ({ n: t.n, live: false, agents: list(t.agents).filter((a) => sameFeature(a, id)) }));
  if (liveTurn != null) turns.push({ n: liveTurn, live: true, agents: list(state.agents).filter((a) => sameFeature(a, id)) });
  return turns.filter((t) => t.agents.length).sort((a, b) => a.n - b.n);
}
