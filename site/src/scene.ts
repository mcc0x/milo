// Release captures: tidebook outline 56×18, history 56×13, usage 90×6,
// and `view src/weather.ts` 82×17. Data is fictional. The earlier committed
// edit is assigned to format.ts so weather.ts starts clean in this scene.
export type Change = { kind: 'new' | 'edited'; added: number; removed?: number };
type Pending = 'commit' | 'push';
export type DemoFile = { path: string; change?: Change; pending?: Pending };
export type Commit = { title: string; age: string; files?: readonly DemoFile[] };

// What the person asks the agent for; it is typed into the composer first.
export const request = 'Add wind speed to the weekend forecast.';

// What the whole window is, for someone who cannot see it. A phone that shows
// one pane at a time swaps this for that pane's name.
export const demoLabel = 'A Rex window with milo’s outline, history and usage panes beside a coding agent at work';

export type Pane = 'outline' | 'history' | 'usage' | 'reader' | 'agent';

// Each pass shows a different coding agent in the same Rex layout.
export const harnesses = [
  { id: 'claude', name: 'Claude Code', version: '2.1.309', model: 'Opus 4.7' },
  { id: 'codex', name: 'Codex', version: '0.170.0', model: 'GPT-5.6 Sol high' },
  { id: 'omp', name: 'omp', version: '19.0.0', model: 'GPT-5.6 Sol high' },
] as const;

// One pass of the demo, in milliseconds from its start. Each effect follows
// its cause by a beat: the agent's edit appears, then the outline marks the
// file; the agent commits, then the history gains the commit. `focus` is the
// pane the window draws the eye to. Each caption emphasizes one phrase;
// {agent} stands for the harness in this pass.
export const still = { at: 16200, phase: 'rest', focus: null, say: 'All of it stays in view, **beside your work**.' } as const;

export const scene = [
  { at: 0, phase: 'compose', focus: 'agent', say: 'You ask **{agent}**, as usual.' },
  { at: 2000, phase: 'prompt', focus: 'agent', say: 'You ask **{agent}**, as usual.' },
  { at: 3100, phase: 'plan', focus: 'agent', say: 'You ask **{agent}**, as usual.' },
  { at: 4700, phase: 'editing', focus: 'agent', say: 'It edits **weather.ts**.' },
  { at: 5500, phase: 'edit', focus: 'outline', say: '**Outline** marks the file the moment it changes.' },
  { at: 8600, phase: 'read', focus: 'reader', say: 'Reader shows **what changed**, line by line.' },
  { at: 12000, phase: 'test', focus: 'agent', say: '**{agent}** runs the forecast test.' },
  { at: 13500, phase: 'committing', focus: 'agent', say: 'It **commits the change**.' },
  { at: 14300, phase: 'commit', focus: 'history', say: 'History gains **the new commit**.' },
  still,
  { at: 17300, phase: 'usage', focus: 'usage', say: 'Usage shows **how much is left** for the five-hour window.' },
  { at: 19900, phase: 'resize', focus: 'usage', say: 'Make Usage smaller. **Milo remembers.**' },
  { at: 22000, phase: 'new-tab', focus: null, say: 'New tab. **Same layout.**' },
] as const satisfies readonly { at: number; phase: string; focus: Pane | null; say: `${string}**${string}**${string}` }[];

// When typing starts and how long it takes, inside the compose step.
export const typing = { start: 400, length: 1300 };
// The finished state is held, then the scene starts again.
export const loopLength = 25200;

export type Phase = typeof scene[number]['phase'];
export type Frame = {
  phase: Phase;
  files: readonly DemoFile[];
  toCommit: number;
  toPush: number;
  ahead: readonly Commit[];
  usage: Record<'codex' | 'claude', number>;
};

// Tokens and line numbers reproduce the captured reader; the agent's excerpt
// and both Git panes use the same edit as the reader.
export type DiffLine = { change: 'added' | 'removed'; number?: number; tokens: readonly (string | readonly [string, string])[] };
export const weatherDiff: readonly DiffLine[] = [
  { change: 'removed', tokens: ['export const outlook = "clear";'] },
  { change: 'added', number: 1, tokens: [['export const', 'purple'], ' outlook = ', ["'clear'", 'green'], ';'] },
  { change: 'added', number: 2, tokens: [] },
  { change: 'added', number: 3, tokens: [['export function', 'purple'], ' ', ['weekendForecast', 'blue'], '() {'] },
  { change: 'added', number: 4, tokens: ['  ', ['return', 'purple'], ' { outlook, windKph: ', ['18', 'amber'], ' };'] },
  { change: 'added', number: 5, tokens: ['}'] },
];

export const edit: DemoFile & { change: Change } = {
  path: 'src/weather.ts',
  change: {
    kind: 'edited',
    added: weatherDiff.filter(line => line.change === 'added').length,
    removed: weatherDiff.filter(line => line.change === 'removed').length,
  },
};

export const newCommit = { id: 'f3c19ad', title: 'Add wind speed to the forecast', age: 'now', files: [edit] };

const tree: readonly DemoFile[] = [
  { path: 'docs/guide.md', change: { kind: 'edited', added: 1, removed: 1 }, pending: 'push' },
  { path: 'docs/weekend.md', change: { kind: 'new', added: 1 }, pending: 'push' },
  { path: 'src/format.ts', change: { kind: 'edited', added: 1, removed: 1 }, pending: 'push' },
  { path: 'src/stations.ts', change: { kind: 'new', added: 1 }, pending: 'commit' },
  { path: 'src/tides.ts', change: { kind: 'edited', added: 3, removed: 2 }, pending: 'commit' },
  { path: 'src/units.ts', change: { kind: 'edited', added: 1, removed: 1 }, pending: 'push' },
  { path: edit.path },
  { path: 'tests/fixtures/tides.json' },
  { path: 'tests/tides.test.ts', change: { kind: 'new', added: 4 }, pending: 'commit' },
  { path: 'tests/weather.test.ts', change: { kind: 'new', added: 2 }, pending: 'push' },
  { path: 'README.md' },
  { path: 'package.json' },
];

const forecastPaths = ['docs/weekend.md', 'src/format.ts', 'tests/weather.test.ts'];

const ahead: readonly Commit[] = [
  { title: 'Add weekend forecasts', age: '1h', files: tree.filter(file => forecastPaths.includes(file.path)) },
  { title: 'List the coastal stations', age: '3h' },
];

export const pushed: readonly Commit[] = [
  { title: 'Set the display units', age: '1d' },
  { title: 'Add tide examples', age: '2d' },
  { title: 'Start the shore guide', age: '4d' },
];

const order = scene.map(step => step.phase);
const editedAt = order.indexOf('edit');
const committedAt = order.indexOf('commit');
const usageAt = order.indexOf('usage');

function outlineAt(pending?: Pending) {
  const files = pending ? tree.map((file): DemoFile => file.path === edit.path ? { ...edit, pending } : file) : tree;
  return {
    files,
    toCommit: files.filter(file => file.pending === 'commit').length,
    toPush: files.filter(file => file.pending === 'push').length,
  };
}

// Reuse the unchanged pane data so the renderer only redraws affected panes.
const beforeEdit = outlineAt();
const afterEdit = outlineAt('commit');
const afterCommit = outlineAt('push');
const aheadAfterCommit = [newCommit, ...ahead];

// These are the latest local reports for each provider, not totals for the edit.
const usageReports = { codex: 17, claude: 31 };

const reportedAfter = {
  claude: { ...usageReports, claude: usageReports.claude - 2 },
  codex: { ...usageReports, codex: usageReports.codex - 2 },
};

export function frameAt(phase: Phase, harness: typeof harnesses[number]['id']): Frame {
  const stage = order.indexOf(phase);
  const committed = stage >= committedAt;
  // omp runs a Codex model in this scene, so it changes the Codex report.
  const provider = harness === 'claude' ? 'claude' : 'codex';
  return {
    phase,
    ...(committed ? afterCommit : stage >= editedAt ? afterEdit : beforeEdit),
    ahead: committed ? aheadAfterCommit : ahead,
    usage: stage >= usageAt ? reportedAfter[provider] : usageReports,
  };
}
