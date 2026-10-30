/* "thinking… N s" line shown while an AI answer is pending (planning chat). Own 1 s tick: the rest of the screen does not re-render. */
import { html, useState, useEffect } from '../util/preact.js';

export function Think({ job }) {
  const [, tick] = useState(0);
  useEffect(() => {
    if (!job) return undefined;
    const t = setInterval(() => tick((x) => x + 1), 1000);
    return () => clearInterval(t);
  }, [job]);
  return html`<div class="ch-think" role="status" hidden=${!job}>${job ? `thinking… ${Math.floor((Date.now() - job.start) / 1000)} s` : ''}</div>`;
}
