/* Settings page (route #/settings[/<section>]), the same in the hub and in every project: a system-settings layout with a sticky list of
   sections on the left (a select above the content under 900px) and inset-grouped cards on the right. Every row is built with
   components/settings_ui.js. Sections: appearance, claude, network, mcp, notifications, projects, system. */
import { html } from '../util/preact.js';
import { Page } from '../components/page.js';
import { SkillsToolsSection } from '../components/skills_tools_section.js';
import { currentRoute, navigate } from './panel/router.js';
import { AppearanceSection, ClaudeSection, NetworkSettings, ProjectsSection, LoopRunGroup } from './settings_sections.js';
import { SystemSection } from './settings_system.js';
import { ProvidersSection } from './settings_providers.js';
import { AgentsSection, QualitySection } from './settings_agents.js';
import { NotificationsV3Section } from './settings_notify.js';
import { startTour } from '../components/tour.js';
import { SettingsGroup, SettingsRow } from '../components/settings_ui.js';

/** General: the look of the app and the guided tour. */
function GeneralSection() {
  return html`<${AppearanceSection} />
    <${LoopRunGroup} />
    <${SettingsGroup} id="st-tour" title="Guided tour" desc="A step-by-step walk through the screens: what each setting is for and how to start your first loop.">
      <${SettingsRow} label="Start the tour again" desc="It opens on the projects screen and moves through Settings with you.">
        <button type="button" class="btn btn-sm" id="st-tour-restart" onClick=${() => startTour(0)}>Start tour</button><//>
    <//>`;
}

const SECTIONS = [
  { id: 'general', label: 'General', sub: 'Theme, how the loop runs and the guided tour.', view: GeneralSection },
  { id: 'providers', label: 'AI providers', sub: 'Which AI runs the loop, in what order, with which models.', view: ProvidersSection },
  { id: 'agents', label: 'Agents', sub: 'The agents of the pipeline, their models and what a loop runs.', view: AgentsSection },
  { id: 'quality', label: 'Quality gate', sub: 'What the last review of every feature checks.', view: QualitySection },
  { id: 'mcp', label: 'Skills and MCP', sub: 'Skills and MCP tools every agent of the loop may use.', view: SkillsToolsSection },
  { id: 'notifications', label: 'Notifications', sub: 'Windows, e-mail and webhook notices, and which events send them.', view: NotificationsV3Section },
  { id: 'network', label: 'Network', sub: 'Hosts the agents may reach.', view: NetworkSettings },
  { id: 'claude', label: 'Claude account', sub: 'Account, usage, command and the orchestrator model.', view: ClaudeSection },
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
