/* Brand mark: the conductor on the violet tile. Sized by the .logo box (38px). */
import { html } from '../util/preact.js';

export function LogoMark() {
  return html`<svg viewBox="0 0 100 100" width="38" height="38" aria-hidden="true" focusable="false">
    <path d="M14 100 C14 82 26 72 42 70 L58 70 C66 71 72 74 76 80 L76 100 Z" fill="#0B0A10" />
    <path d="M44 54 L56 54 L57 71 L43 71 Z" fill="#0B0A10" />
    <path d="M32 36 C30 20 42 10 56 11 C68 12 74 22 73 32 L82 39 L73 42 C72 52 64 58 54 58 C41 58 33 49 32 36 Z" fill="#0B0A10" />
    <path d="M33 30 C26 16 40 2 60 7 C50 8 44 13 42 21 Z" fill="#0B0A10" />
    <path d="M60 31 Q64 34 68 31" stroke="#7C6CF6" stroke-width="2" fill="none" stroke-linecap="round" />
    <path d="M43 70 L50 74 L43 78 Z M57 70 L50 74 L57 78 Z" fill="#7C6CF6" />
    <path d="M70 82 L86 56" stroke="#0B0A10" stroke-width="10" stroke-linecap="round" />
    <path d="M87 53 L97 20" stroke="#FFFFFF" stroke-width="2.4" stroke-linecap="round" />
    <circle cx="97" cy="20" r="3" fill="#FFFFFF" />
  </svg>`;
}
