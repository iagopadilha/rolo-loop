/* The Rolo Loop mark: Rolo, the hamster in the wheel. Static everywhere the brand shows (sidebar, dialogs, favicon); `running` on the loading
   screen: the spokes turn in discrete steps (pixel-art motion), Rolo bounces and the two pairs of feet alternate. Runs with SVG animation (no
   timers); with "reduce motion" on, the mark stays still. Art: rolo_art.js. */
import { html } from '../util/preact.js';
import { ROLO_LAYERS, ROLO_TILE, ROLO_HUB } from './rolo_art.js';

const PAD = 16;
const STEPS = 12;
const reduceMotion = () => typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;

/** `<rect>`s of one layer (built once per layer). */
const cache = new Map();
function rectsOf(name) {
  if (!cache.has(name)) {
    cache.set(name, Object.entries(ROLO_LAYERS[name]).flatMap(([fill, list]) => list.split(',').map((item, index) => {
      const [x, y, w, h] = item.split(' ');
      return html`<rect key=${fill + index} x=${x} y=${y} width=${w} height=${h} fill=${fill} />`;
    })));
  }
  return cache.get(name);
}

const turn = Array.from({ length: STEPS + 1 }, (_, i) => `${(360 / STEPS) * i} ${ROLO_HUB[0]} ${ROLO_HUB[1]}`).join(';');

/** size in px; `tile` = the dark rounded square behind Rolo (the app icon look); `label` makes it an image with a name. */
export function RoloMark({ size = 32, running = false, tile = true, label, className = '' }) {
  const animate = running && !reduceMotion();
  const box = tile ? `${-PAD} ${-PAD} ${200 + 2 * PAD} ${200 + 2 * PAD}` : '0 0 200 200';
  return html`<svg class=${'rolo ' + className} viewBox=${box} width=${size} height=${size} shape-rendering="crispEdges" focusable="false"
      role=${label ? 'img' : null} aria-label=${label || null} aria-hidden=${label ? null : 'true'}>
    ${tile ? html`<rect x=${-PAD} y=${-PAD} width=${200 + 2 * PAD} height=${200 + 2 * PAD} rx="52" fill=${ROLO_TILE} />` : null}
    <g>${rectsOf('rim')}</g>
    <g>${rectsOf('spokes')}${animate ? html`<animateTransform attributeName="transform" type="rotate" values=${turn} dur="1.1s" calcMode="discrete" repeatCount="indefinite" />` : null}</g>
    <g>
      ${animate ? html`<animateTransform attributeName="transform" type="translate" values="0 0;0 -4;0 0;0 -2" dur="0.44s" calcMode="discrete" repeatCount="indefinite" />` : null}
      ${rectsOf('body')}
      <g>${rectsOf('feetA')}${animate ? html`<animate attributeName="opacity" values="1;0" dur="0.22s" calcMode="discrete" repeatCount="indefinite" />` : null}</g>
      <g opacity=${animate ? 0 : 1}>${rectsOf('feetB')}${animate ? html`<animate attributeName="opacity" values="0;1" dur="0.22s" calcMode="discrete" repeatCount="indefinite" />` : null}</g>
    </g>
  </svg>`;
}
