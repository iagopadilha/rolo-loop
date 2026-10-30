/* Plain-language explanation of each file the loop keeps in .specs (static table; no value of any file is read here).
   explainFile(path) -> { what, purpose, changes } or null. Used by the Files page reader. */

const BY_NAME = {
  'PROJECT.md': ['The vision of the project.', 'Says what is being built, for whom, and the limits of the idea. Every agent reads it first.', 'You and the planning chat. Rarely, once the plan is approved.'],
  'ROADMAP.md': ['The plan: stages and the features of each one.', 'One line per feature with status, size, paths and what it waits on. It is the source of the diagram and of the order of work.', 'The loop updates the status of each feature; the plan itself only changes by going back to planning.'],
  'RULES.md': ['The limits: what the AI may never do.', 'Overrides any spec. A rule here wins over everything else; a conflict makes the loop ask you.', 'Only you (or the orchestrator on your request).'],
  'STATE.md': ['The memory of the loop between turns.', 'Decisions taken, blockers and lessons, so a new session continues where the last one stopped.', 'The loop, on every turn.'],
  'FLOWS.md': ['The user flows the project must keep working.', 'Used by the flow validation phase: each flow becomes a route/screen check.', 'Planning; the loop only reads it.'],
  'TEST-ENVIRONMENT.md': ['How tests may run here (local URLs, services, accounts by name).', 'Tells the verifier where it can test safely. Never production.', 'You, in the test environment dialog.'],
  'VALIDATE.md': ['Decisions the AI took alone and that you should confirm later.', 'Each line has the doubt, what it chose and how to revert. Nothing blocks on it; you validate when you can.', 'The loop appends lines; you read and revert what you disagree with.'],
  'ANSWERS.md': ['Your answers to the questions the loop asked.', 'Saves the decision so the AI does not ask again.', 'The loop, when you answer in the panel.'],
  'GRAPH.md': ['Text version of the dependency graph.', 'What waits on what, wave by wave. Read-only picture of the current order of work.', 'The loop, on every turn.'],
  'LOOP.json': ['Technical state of the loop (status, turn, reason).', 'Used by the panel to show running, paused or waiting for you.', 'The loop.'],
  'spec.md': ['The specification of one feature.', 'Requirements with IDs (what must be true when the feature is done). The reviewer checks the code against it.', 'The spec-writer; you can change it with "Change with AI".'],
  'design.md': ['How the feature will be built.', 'Chosen approach, files to touch and the reasons. Written for features that are not small.', 'The spec-writer.'],
  'tasks.md': ['The feature broken into small tasks.', 'Each task has a check that proves it is done; the executor ticks them one by one.', 'The spec-writer and the executor.'],
  '.env.test': ['Values used only by tests (SECRET).', 'Only the NAMES of the variables are ever shown here, never the values.', 'You.'],
  'STACK.md': ['Languages, frameworks and versions of the code.', 'A map of the code, not the truth: when it disagrees with the code, the code wins.', 'The codebase mapper.'],
  'ARCHITECTURE.md': ['How the code is organized in layers and how data flows.', 'Helps the AI put new code where it belongs.', 'The codebase mapper.'],
  'CONVENTIONS.md': ['Naming, code style and error handling in use.', 'New code copies these habits.', 'The codebase mapper.'],
  'STRUCTURE.md': ['Where each kind of code lives in the folders.', 'Shortcut to find things without searching the whole repo.', 'The codebase mapper.'],
  'TESTING.md': ['How tests are written and run here.', 'The verifier uses it to know which commands to run.', 'The codebase mapper.'],
  'INTEGRATIONS.md': ['External APIs, databases, queues and authentication.', 'Tells the AI what the code talks to.', 'The codebase mapper.'],
  'CONCERNS.md': ['Technical debt, risks and fragile areas.', 'Warns the AI where a change is dangerous.', 'The codebase mapper.'],
};

const toExplanation = (a) => ({ what: a[0], purpose: a[1], changes: a[2] });

export function explainFile(path) {
  const p = String(path == null ? '' : path).replace(/\\/g, '/');
  const name = p.split('/').pop();
  if (/(^|\/)phases\/stage-\d+\.md$/i.test(p)) return { what: 'The plan of one stage.', purpose: 'Description of the stage, its waves and what closes it. Read it to see why the features are grouped this way.', changes: 'The orchestrator when the stage starts and when it closes.' };
  if (/(^|\/)features\/[^/]+\/?$/i.test(p)) return { what: 'Folder of one feature.', purpose: 'Holds spec.md, design.md and tasks.md of that feature.', changes: 'The spec-writer.' };
  const hit = BY_NAME[name] || BY_NAME[Object.keys(BY_NAME).find((k) => k.toLowerCase() === String(name).toLowerCase())];
  return hit ? toExplanation(hit) : null;
}
