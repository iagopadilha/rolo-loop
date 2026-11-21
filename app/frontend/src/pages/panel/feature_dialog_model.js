/* Pure helpers of the Feature page and of the compact task waves in the pipeline lanes: no DOM, no Preact, no stores.
   Input is the `tasks` object of a feature (GET feature/<slug> or /state `features[].tasks`):
   { tasks: [{ id, title, status, wave, deps?, executors?, ... }], max_parallel, waves?: [{ n, task_ids, split_by_limit }] }.
   Older states have no deps / waves / executors: waves are then derived from `task.wave`, no arrow is drawn and no agent chip claims
   an agent that the data does not show. Unknown values stay null, never invented. */

const DEFAULT_WAVE = 1;
const SECONDS_PER_MINUTE = 60;

const asList = (value) => (Array.isArray(value) ? value : []);

/** Task as the views use it: every optional field present, deps and executors always lists. */
function normalizeTask(task) {
  return {
    id: String(task.id), title: task.title == null ? '' : String(task.title), status: String(task.status || 'pending'),
    wave: Number.isFinite(task.wave) ? task.wave : DEFAULT_WAVE, deps: asList(task.deps).map(String), files: asList(task.files).map(String),
    parallel: !!task.parallel, note: task.note == null ? '' : String(task.note), executors: asList(task.executors),
  };
}

/** One wave: its tasks (in the order of `task_ids`, else of the task list), the parallel flag and the split-by-limit flag. */
function buildWave(waveNumber, tasks, serverWave) {
  const byId = Object.fromEntries(tasks.map((task) => [task.id, task]));
  const fromServer = serverWave ? asList(serverWave.task_ids).map((id) => byId[String(id)]).filter(Boolean) : [];
  const members = fromServer.length ? fromServer : tasks.filter((task) => task.wave === waveNumber);
  return { n: waveNumber, tasks: members, parallel: members.length > 1, splitByLimit: !!(serverWave && serverWave.split_by_limit) };
}

/** { tasks, waves, maxParallel, hasDeps } of a `tasks` payload; an empty or missing payload gives no tasks and no waves. */
export function taskGraphOf(payload) {
  const tasks = asList(payload && payload.tasks).filter((task) => task && task.id != null).map(normalizeTask);
  const serverWaves = asList(payload && payload.waves);
  const numbers = [...new Set(tasks.map((task) => task.wave).concat(serverWaves.map((wave) => wave.n).filter(Number.isFinite)))].sort((a, b) => a - b);
  const waves = numbers.map((waveNumber) => buildWave(waveNumber, tasks, serverWaves.find((wave) => wave.n === waveNumber)));
  const maxParallel = Number.isFinite(payload && payload.max_parallel) ? payload.max_parallel : null;
  return { tasks, waves: waves.filter((wave) => wave.tasks.length), maxParallel, hasDeps: tasks.some((task) => task.deps.length > 0) };
}

/** Label of a wave: "sequential" (one task) or "parallel ×N". */
export const waveModeLabel = (wave) => (wave.parallel ? `parallel ×${wave.tasks.length}` : 'sequential');

/** Note shown under a wave that the parallel limit cut short (null when it was not cut). */
export const splitNoteOf = (wave, maxParallel) => (wave.splitByLimit ? `split by the limit of ${maxParallel == null ? '?' : maxParallel}` : null);

/** Short duration: "42s", "3m 05s", "1h 02m". null/NaN gives ''. */
export function shortDuration(seconds) {
  if (!Number.isFinite(seconds)) return '';
  const whole = Math.floor(seconds);
  if (whole < SECONDS_PER_MINUTE) return `${whole}s`;
  const minutes = Math.floor(whole / SECONDS_PER_MINUTE);
  if (minutes < SECONDS_PER_MINUTE) return `${minutes}m ${String(whole % SECONDS_PER_MINUTE).padStart(2, '0')}s`;
  return `${Math.floor(minutes / SECONDS_PER_MINUTE)}h ${String(minutes % SECONDS_PER_MINUTE).padStart(2, '0')}m`;
}

/** Rows of agent calls of a feature for the history: the grouped calls of `featureCalls` flattened with their turn, else the
    agents of the feature endpoint (older panels without history). */
export function agentRowsOf(turns, detailAgents) {
  const fromTurns = asList(turns).flatMap((turn) => asList(turn.agents).map((agent) => ({
    id: agent.id, role: agent.role, turn: turn.n, state: agent.state, return_status: agent.return_status, duration_s: agent.duration_s,
    summary: agent.decision || agent.summary || null,
    claude: agent.claude, claude_account: agent.claude_account, auth_method: agent.auth_method, auth_email: agent.auth_email, auth_plan: agent.auth_plan,
    cc_account: agent.cc_account, // inherited from the turn that spawned the agent
  })));
  if (fromTurns.length) return fromTurns;
  return asList(detailAgents).map((agent) => ({ id: agent.agent_id, role: agent.role, turn: agent.turn, state: agent.state,
    return_status: agent.return_status, duration_s: agent.duration_s, summary: agent.summary || null,
    claude: agent.claude, auth_method: agent.auth_method, auth_email: agent.auth_email, cc_account: agent.cc_account }));
}

/** The six steps of the timeline when the endpoint did not send them (older back end): derived from the roadmap status. */
export function fallbackSteps(status) {
  const order = ['spec', 'review', 'build', 'verify', 'quality', 'commit'];
  const reached = { pending: 0, speccing: 0, specced: 1, approved: 1, building: 2, verified: 6, blocked: 2 }[status];
  if (reached == null) return order.map((step) => ({ step, state: 'pending' }));
  return order.map((step, index) => ({ step, state: index < reached ? 'done' : index === reached ? (status === 'blocked' ? 'failed' : 'running') : 'pending' }));
}
