/* Feature data for the Feature page (pages/panel/feature_page.js): `GET feature/<slug>`, read now and refreshed every 3 s while the loop runs
   (the page unmounts, which stops the poll). When the route or some fields are missing (older back end) the page falls back to the feature
   in /state. `openFeatureDialog` is the one entry point of the old dialog: it goes to the page #/feature/<slug>. */
import { useState, useEffect } from '../../../util/preact.js';
import { list } from '../../../util/format.js';
import { ctx } from '../context.js';
import { navigate } from '../router.js';

const POLL_MS = 3000;
const REQUEST_TIMEOUT_MS = 15000;

export const stateFeature = (slug) => list(ctx.state && ctx.state.features).find((item) => item && item.id === slug) || null;
const loopIsRunning = () => !!(ctx.state && ctx.state.loop && ctx.state.loop.running);

/** Reads the feature endpoint now and every POLL_MS while the loop runs. A failed read keeps the last data and shows the error. */
export function useFeatureDetail(slug) {
  const [detail, setDetail] = useState({ data: null, error: null, loading: true });
  const running = loopIsRunning();
  useEffect(() => {
    if (!slug) { setDetail({ data: null, error: null, loading: false }); return undefined; } // no slug: nothing to read (unknown feature)
    let alive = true;
    async function readFeature() {
      try {
        const data = await ctx.api.get('feature/' + encodeURIComponent(slug), REQUEST_TIMEOUT_MS);
        if (alive) setDetail({ data, error: null, loading: false });
      } catch (error) {
        if (alive) setDetail((previous) => ({ data: previous.data, error: error.message, loading: false }));
      }
    }
    readFeature();
    const timer = running ? setInterval(readFeature, POLL_MS) : null;
    return () => { alive = false; if (timer) clearInterval(timer); };
  }, [slug, running]);
  return detail;
}

/** Opens the feature: since UI v2 it is a page (#/feature/<slug>, see ../feature_page.js); every old entry point lands here. */
export function openFeatureDialog({ feature }) { navigate('/feature/' + encodeURIComponent(feature)); }
