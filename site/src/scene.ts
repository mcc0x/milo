// Release captures: tidebook outline 56×18, history 56×13, usage 90×6,
// and `view src/weather.ts` 82×17. Data is fictional. The earlier committed
// edit is assigned to format.ts so weather.ts starts clean in this scene.
export type Change = { kind: 'new' | 'edited'; added: number; removed?: number };
type Pending = 'commit' | 'push';
export type DemoFile = { path: string; change?: Change; pending?: Pending };
export type Commit = { title: string; age: string; files?: readonly DemoFile[] };

// What the person asks the agent for; it is typed into the composer first.
export const request = 'Add wind speed to the weekend forecast.';

// One pass of the demo, in milliseconds from its start. Each step is caused
// by the one before: the request is typed and sent, the agent edits a file
// (the outline marks it), tests, commits (the history gains a commit), and
// the usage figure rises.
export const scene = [
  { at: 0, phase: 'compose' },
  { at: 1900, phase: 'prompt' },
  { at: 2900, phase: 'plan' },
  { at: 4300, phase: 'edit' },
  { at: 6100, phase: 'test' },
  { at: 7400, phase: 'commit' },
  { at: 8700, phase: 'usage' },
] as const;

// When typing starts and how long it takes, inside the compose step.
export const typing = { start: 350, length: 1250 };
// The finished state is held, then the scene starts again.
export const loopLength = 13200;

export type Phase = typeof scene[number]['phase'];
type Usage = { five: number; week: number; age: string };
export type Frame = {
  phase: Phase;
  files: readonly DemoFile[];
  toCommit: number;
  toPush: number;
  ahead: readonly Commit[];
  usage: Record<'codex' | 'claude', Usage>;
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
  { path: '.gitignore' },
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

export function frameAt(phase: Phase): Frame {
  const edited = phase === 'edit' || phase === 'test';
  const committed = phase === 'commit' || phase === 'usage';
  const files = tree.map((file): DemoFile => file.path === edit.path && (edited || committed)
    ? { ...edit, pending: committed ? 'push' : 'commit' }
    : file);
  return {
    phase,
    files,
    toCommit: files.filter(file => file.pending === 'commit').length,
    toPush: files.filter(file => file.pending === 'push').length,
    ahead: committed ? [newCommit, ...ahead] : ahead,
    usage: {
      codex: { five: phase === 'usage' ? 19 : 17, week: 69, age: phase === 'usage' ? 'just now' : '4 m ago' },
      claude: { five: 31, week: 58, age: '6 m ago' },
    },
  };
}
