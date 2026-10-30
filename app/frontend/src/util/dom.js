/* DOM helpers that never parse HTML strings: every external text goes in through textContent. The panel itself is Preact;
   these serve the imperative widgets (pan/zoom transforms) and the tablist keys. */

export const $ = (id) => document.getElementById(id);
const SVGNS = 'http://www.w3.org/2000/svg';

function applyProps(el, props) {
  for (const [k, v] of Object.entries(props || {})) {
    if (v == null || v === false) continue;
    if (k === 'class') el.setAttribute('class', v);
    else if (k === 'text') el.textContent = v;
    else if (k === 'value') el.value = v;
    else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? '' : String(v));
  }
}

function addKids(el, kids) {
  for (const k of kids.flat(Infinity)) {
    if (k == null || k === false) continue;
    el.appendChild(typeof k === 'object' ? k : document.createTextNode(String(k)));
  }
}

/** Creates an SVG element: sv('path', {d, class}, ...children). */
export const sv = (tag, props, ...kids) => { const el = document.createElementNS(SVGNS, tag); applyProps(el, props); addKids(el, kids); return el; };

