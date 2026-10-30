/* Settings page (route #/settings[/<section>]), the same in the hub and in every project: a system-settings layout with a sticky list of
   sections on the left (a select above the content under 900px) and inset-grouped cards on the right. Every row is built with
   components/settings_ui.js. Sections: appearance, claude, network, mcp, notifications, projects, system. */
import { html } from '../util/preact.js';
import { Page } from '../components/page.js';
import { McpServersSection } from '../components/mcp_servers_section.js';
import { currentRoute, navigate } from './panel/router.js';
import { AppearanceSection, ClaudeSection, NetworkSettings, ProjectsSection } from './settings_sections.js';
import { NotificationsSection, SystemSection } from './settings_system.js';

const SECTIONS = [
  { id: 'appearance', label: 'Appearance', sub: 'Theme of the app.', view: AppearanceSection },
  { id: 'claude', label: 'Claude', sub: 'Account, usage, command and model.', view: ClaudeSection },
  { id: 'network', label: 'Network', sub: 'Hosts the agents may reach.', view: NetworkSettings },
  { id: 'mcp', label: 'MCP servers', sub: 'Which MCP servers a loop may use.', view: McpServersSection },
  { id: 'notifications', label: 'Notifications', sub: 'Channels and a test message.', view: NotificationsSection },
  { id: 'projects', label: 'Projects and data', sub: 'Data folders and limits.', view: ProjectsSection },
  { id: 'system', label: 'System', sub: 'Checks, version and guard.', view: SystemSection },
];
const sectionOf = (id) => SECTIONS.find((s) => s.id === id) || SECTIONS[0];
// replace, not push: Esc / back leaves Settings instead of walking through the sections one by one
const go = (id) => navigate('/settings/' + id, { replace: true });

export function SettingsPage() {
  const current = sectionOf(currentRoute().section);
  const View = current.view;
  return html`<${Page} name="settings" title="Settings" fill=${true}>
    <div class="st">
      <nav class="st-nav" aria-label="Settings sections">
        ${SECTIONS.map((s) => html`<a key=${s.id} class=${'st-nav-item' + (s.id === current.id ? ' is-on' : '')} href=${'#/settings/' + s.id} id=${'st-nav-' + s.id}
          aria-current=${s.id === current.id ? 'page' : undefined} onClick=${(e) => { e.preventDefault(); go(s.id); }}>${s.label}</a>`)}
      </nav>
      <div class="st-pick">
        <label class="sr-only" for="st-pick-select">Settings section</label>
        <select id="st-pick-select" class="st-input" value=${current.id} onChange=${(e) => go(e.currentTarget.value)}>
          ${SECTIONS.map((s) => html`<option key=${s.id} value=${s.id} selected=${s.id === current.id}>${s.label}</option>`)}</select>
      </div>
      <div class="st-content" id="st-content" tabindex="-1">
        <div class="st-inner" key=${current.id} role="region" aria-label=${current.label}>
          <header class="st-sec-head"><h2 class="st-sec-title">${current.label}</h2><p class="st-sec-sub">${current.sub}</p></header>
          <${View} />
        </div>
      </div>
    </div>
  <//>`;
}
