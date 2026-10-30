/* Which Claude account the loop uses: the sidebar account chip (data below) and the dialog it opens ("Claude account and usage").
   The command (`claude` = main, `cc` = wrapper on another account), the sign-in in words ("subscription, browser login", "subscription,
   sign-in token"; an API key is billed per token and is drawn as a warning), the e-mail (organization and plan when known) and, for `cc`,
   the account it picked. The data is `state.meta.claude_account_info`; the 5-hour and weekly usage of every command/account
   (`state.meta.claude_usage_view`, exhausted windows in the warning colour) and the usage per sign-in method and per command are listed in the dialog. */
import { html } from '../../util/preact.js';
import { openBigDialog } from '../../components/big_dialog.js';
import { ctx } from './context.js';
import { ClaudeTotals, ClaudeUsageWindows, METHOD_WORDS, methodCodeOf, methodSentence } from './claude_badge.js';

const infoOf = () => (ctx.state && ctx.state.meta && ctx.state.meta.claude_account_info) || {};

/** The Claude account of the running loop, in plain words for the sidebar chip; null before there is one. */
export function accountChip() {
  const info = infoOf();
  const name = info.email || (info.cc_account ? 'cc account ' + info.cc_account : '') || info.command || '';
  if (!name) return null;
  const words = METHOD_WORDS[methodCodeOf(info)] || '';
  return { name, sub: words ? words.charAt(0).toUpperCase() + words.slice(1) : '', onClick: openAccountDialog };
}

/** "claude · dev@example.com · subscription, browser login (Acme, team)": every known field, one per line. */
function accountLines(info, method) {
  const extra = [info.org, info.plan].filter(Boolean).join(', ');
  const switches = Number(info.switches) || 0;
  return [
    ['Command', (info.command || '—') + (info.role ? ' (' + (info.role === 'main' ? 'main' : 'fallback') + ')' : '')],
    method ? ['Sign-in', methodSentence(method)] : null,
    info.email ? ['E-mail', info.email] : null,
    info.org ? ['Organization', info.org] : null,
    info.plan ? ['Plan', info.plan] : (extra ? ['Plan', extra] : null),
    info.cc_account ? ['cc account', info.cc_account] : null,
    switches ? ['Account switches in this run', String(switches)] : null,
  ].filter(Boolean);
}

function AccountBody() {
  const info = infoOf(), method = methodCodeOf(info);
  const totals = ctx.state && ctx.state.meta ? ctx.state.meta.claude_totals : null;
  const rows = (ctx.state && ctx.state.meta && ctx.state.meta.claude_usage_view) || [];
  const hasUsage = (!!totals && (totals.turns > 0 || totals.calls > 0)) || rows.length > 0;
  return html`<div class="acct">
    ${method === 'api_key' ? html`<div class="nl-box nl-warn" role="alert">This loop uses an API key: every token is billed.</div>` : null}
    <dl class="acct-list">${accountLines(info, method).map(([key, value]) => html`<div key=${key} class="acct-row"><dt>${key}</dt><dd>${value}</dd></div>`)}</dl>
    ${hasUsage ? html`<div class="acct-usage"><div class="small-label">Usage (5-hour and weekly) and totals</div><${ClaudeUsageWindows} /><${ClaudeTotals} /></div>`
      : html`<div class="empty-note">No usage recorded yet.</div>`}
  </div>`;
}

/** Opens the Claude account and usage dialog (from the sidebar account chip). */
function openAccountDialog() {
  return openBigDialog({ badge: 'CLAUDE', title: 'Claude account and usage', body: () => html`<${AccountBody} />`, chat: null });
}
