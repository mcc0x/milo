import { edit, newCommit, pushed, request, scene, weatherDiff, type harnesses, type Change, type Commit, type DiffLine, type Frame, type Phase } from './scene';
import './agent.css';
import './usage.css';

export const element = (id: string) => document.getElementById(id)!;
const demo = element('demo');
const outline = element('outline-pane');
const history = element('history-pane');
const reader = element('reader-content');
const agentPane = element('agent-pane');
const agentSession = element('agent-session');
const agentBanner = element('agent-banner');
const transcript = element('agent-transcript');
const activity = element('agent-activity');
const composerModel = element('composer-model');
const composerPrompt = element('composer-prompt');
const composerPlaceholder = element('composer-placeholder');
const composerHint = element('composer-hint');
let agentSteps: { at: number; node: HTMLElement }[] = [];
let followTranscript = false;
let previousFrame: Frame | undefined;

function span(text: string, className = '') {
  const node = document.createElement('span');
  node.className = className;
  node.textContent = text;
  return node;
}

function row(className: string, ...contents: (string | Node)[]) {
  const div = document.createElement('div');
  div.className = `term-line ${className}`;
  div.append(...contents);
  return div;
}

const originalTab = demo.querySelector('.rex-tab')!;
const shellTab = span('zsh', 'rex-tab rex-tab-new');
shellTab.hidden = true;
originalTab.after(shellTab);

const shellPane = document.createElement('div');
shellPane.className = 'term-pane shell-pane';
shellPane.append(row('shell-prompt', 'tidebook ', span('main', 'term-blue'), ' › ', span('', 'composer-caret')));
agentPane.after(shellPane);

for (const [provider, countdown] of [['codex', '38m'], ['claude', '1h 24m']]) {
  const detail = document.createElement('small');
  detail.className = 'usage-reset term-muted';
  detail.textContent = `Resets in ${countdown}`;
  element(`${provider}-usage`).after(detail);
}

const symbols = {
  folder: ['\uf07c', 'blue'],
  ts: ['\ue628', 'cyan'],
  md: ['\ue73e', 'slate'],
  json: ['\ue60b', 'yellow'],
} as const;

function icon(name: string, folder = false) {
  const kind = folder ? 'folder'
    : name.endsWith('.ts') ? 'ts' : name.endsWith('.md') ? 'md' : 'json';
  const [glyph, color] = symbols[kind];
  const node = span(glyph, `term-icon term-${color}`);
  node.setAttribute('aria-hidden', 'true');
  return node;
}

function fileName(name: string, tree: string, color: string, bold = false, folder = false) {
  const label = span('', 'term-file');
  label.append(span(tree, 'term-muted tree-branch'), icon(name, folder), span(name, `${color}${bold ? ' term-bold' : ''}`));
  return label;
}

function project() {
  const label = span('', 'term-project');
  label.append(span('tidebook', 'term-bold'), span('main', 'term-purple'));
  return label;
}

function tallies(change?: Change, folder = false) {
  return [
    span(!folder ? change?.kind ?? '' : '', `term-action ${change?.kind === 'new' ? 'term-green' : 'term-amber'}`),
    span(change?.added ? `+${change.added}` : '', 'term-number term-green'),
    span(change?.removed ? `−${change.removed}` : '', 'term-number term-red'),
  ];
}

function detailRow(lead: HTMLElement, change?: Change, folder = false, emphasis = '') {
  const [action, added, removed] = tallies(change, folder);
  return removed.textContent
    ? row(`term-detail ${emphasis}`, lead, action, added, removed)
    : row(`term-detail no-removals ${emphasis}`, lead, action, added);
}

function renderOutline(frame: Frame) {
  const summary = span('', 'term-summary');
  summary.append(span(`${frame.toCommit} to commit`, 'term-bold'), span(' · ', 'term-muted'), `${frame.toPush} to push`);
  const lines = [row('term-header', project(), summary)];
  const folders = new Set<string>();
  for (const file of frame.files) {
    const components = file.path.split('/');
    const name = components.pop()!;
    for (const [depth, dir] of components.entries()) {
      const path = components.slice(0, depth + 1).join('/');
      if (folders.has(path)) continue;
      folders.add(path);
      const changes = frame.files.filter(entry => entry.path.startsWith(`${path}/`) && entry.change);
      const added = changes.reduce((total, entry) => total + entry.change!.added, 0);
      const removed = changes.reduce((total, entry) => total + (entry.change!.removed ?? 0), 0);
      const color = removed ? 'term-amber' : added ? 'term-green' : 'term-muted';
      lines.push(detailRow(fileName(dir, '│ '.repeat(depth), color, changes.some(entry => entry.pending === 'commit'), true), { kind: 'edited', added, removed }, true));
    }
    const color = file.change?.kind === 'edited' ? 'term-amber' : file.change ? 'term-green' : 'term-muted';
    const weather = file.path === edit.path && !!file.change;
    const emphasis = weather ? `term-selected${frame.phase === 'edit' ? ' term-lit' : ''}` : '';
    lines.push(detailRow(fileName(name, '│ '.repeat(components.length), color, file.pending === 'commit'), file.change, false, emphasis));
  }
  lines.push(row('term-hint term-muted', 'n next change · c changed only · q quit · ? help'));
  outline.replaceChildren(...lines);
}

function commitRows(commit: Commit, open: boolean, remote = false, lit = false) {
  const expanded = open && !!commit.files?.length;
  const title = span(`${expanded ? '●▾' : '● '}${commit.title}`, 'commit-title');
  const caption = expanded ? `${commit.files!.length} ${commit.files!.length === 1 ? 'file' : 'files'}` : commit.age;
  const lines = [row(`term-commit${expanded ? ' term-selected' : ''}${remote ? ' term-muted' : ''}${lit ? ' term-lit' : ''}`, title, span(caption, 'term-muted'))];
  if (expanded) {
    for (const file of commit.files!) {
      const [dir, name] = file.path.split('/');
      const label = fileName(name, '  │ ', file.change?.kind === 'new' ? 'term-green' : 'term-amber', true);
      label.append(span(dir, 'term-muted history-dir'));
      lines.push(detailRow(label, file.change));
    }
  }
  return lines;
}

function renderHistory(frame: Frame) {
  const progress = row('history-dots', span(`○ ${'●  '.repeat(frame.ahead.length)}`, 'term-bold'), span(`│${'  ●'.repeat(pushed.length)}`, 'term-muted'));
  const items = [
    row('term-header', project(), span(`${frame.ahead.length} commits to push`, 'term-summary')),
    progress,
    row('term-bold', `○ ${frame.toCommit} files to commit`),
    ...frame.ahead.flatMap((commit, index) => commitRows(commit, index === 0, false, frame.phase === 'commit' && commit === newCommit)),
    row('history-divider term-muted', '── on origin/main'),
    ...pushed.flatMap(commit => commitRows(commit, false, true)),
    row('term-hint term-muted', 'Enter hide files · r reload · q quit · ? help'),
  ];
  history.replaceChildren(...items);
}

function renderUsage({ usage }: Frame) {
  for (const provider of ['codex', 'claude'] as const) {
    const left = usage[provider];
    const figure = element(`${provider}-usage`);
    figure.textContent = `${left}%`;
    figure.className = left <= 10 ? 'term-red' : left <= 30 ? 'term-amber' : 'term-green';
  }
}

function diffRow(line: DiffLine) {
  const code = span('', 'diff-code');
  code.append(...line.tokens.map(token => typeof token === 'string' ? token : span(token[0], `term-${token[1]}`)));
  const marker = line.change === 'added' ? '+' : '-';
  return row(`diff-line diff-${line.change}`, span(String(line.number ?? ''), 'diff-number'), span(marker, 'diff-marker'), code);
}

function renderReader() {
  const heading = detailRow(fileName('weather.ts', '', '', true), edit.change);
  heading.classList.add('reader-heading');
  heading.firstElementChild?.append(span('src', 'term-muted history-dir'));
  const lines = weatherDiff.map(diffRow);
  reader.replaceChildren(heading, ...lines, row('term-hint term-muted', '↑↓ scroll · q close · ? help'));
}

const order = scene.map(step => step.phase);
const finish = order.indexOf('commit');
const resize = order.indexOf('resize');

function agentLine(kind: string, glyph: string, ...contents: (string | Node)[]) {
  const body = span('');
  body.append(...contents);
  return glyph ? row(`agent-${kind} agent-gutter`, span(glyph, 'agent-glyph'), body) : row(`agent-${kind}`, body);
}

export function renderHarness({ id, name, version, model }: typeof harnesses[number]) {
  const claude = id === 'claude';
  const codex = id === 'codex';
  const omp = id === 'omp';
  const directory = '~/dev/tidebook';
  const prompt = claude ? '❯' : codex ? '›' : '';
  const assistant = claude ? '⏺' : codex ? '•' : '';
  agentPane.dataset.harness = id;
  const title = row('agent-product',
    span(codex ? 'OpenAI Codex' : name, 'term-bold'),
    span(codex ? ` (v${version})` : ` v${version}`, 'agent-version'),
  );
  agentBanner.replaceChildren(title, ...(claude ? [
    row('agent-model', `${model} · Claude Max`),
    row('agent-directory', directory),
  ] : codex ? [
    row('agent-model', span('model:     ', 'agent-label'), model),
    row('agent-directory', span('directory: ', 'agent-label'), directory),
  ] : []));
  composerPrompt.textContent = prompt;
  composerPlaceholder.textContent = codex ? 'Ask Codex to do anything' : '';
  composerModel.replaceChildren(...(claude ? [] : codex ? [
    `${model} · ${directory}`,
  ] : [
    span('π', 'omp-model-mark'), span(model, 'omp-model-name'), span('tidebook'), span('main', 'omp-model-branch'),
  ]));
  composerHint.textContent = omp ? '' : '? for shortcuts';
  activity.replaceChildren(
    span(claude ? '✻' : codex ? '•' : '⠿', 'activity-glyph'),
    claude ? 'Thinking…' : 'Working', span('esc to interrupt', 'activity-hint'),
  );

  const path = edit.path;
  const { added, removed } = edit.change;
  const summary = `(+${added} -${removed})`;
  const tool = (name: string, detail: string) => agentLine('tool', assistant || '✓',
    span(name, 'agent-tool-name'),
    claude ? `(${detail})` : codex ? ` ${detail}` : `: ${detail}`,
  );
  const shell = (description: string, command: string, ...output: (string | Node)[]) => omp ? [
    agentLine('tool agent-shell-title', '›_', span('Bash', 'agent-tool-name'), ' · ', description),
    agentLine('shell-command', '$', command),
    agentLine('result', '└', ...output),
  ] : [
    tool(codex ? 'Ran' : 'Bash', command),
    agentLine('result', claude ? '⎿' : '└', ...output),
  ];
  const step = (phase: Phase, ...children: Node[]) => {
    const node = document.createElement('div');
    node.className = 'agent-step';
    node.dataset.step = phase;
    node.hidden = true;
    node.append(...children);
    return { at: order.indexOf(phase), node };
  };
  const diff = row('agent-diff', ...weatherDiff.map(line => diffRow(line.number ? line : { ...line, number: 1 })));
  agentSteps = [
    step('prompt', agentLine('request', prompt, request)),
    step('plan', agentLine('response', assistant, 'I’ll add wind speed in km/h and check the forecast output.')),
    step('editing',
      tool(claude ? 'Update' : codex ? 'Edited' : 'Edit', claude ? path : `${path} ${summary}`),
      ...(claude ? [agentLine('result', '⎿', `Added ${added} lines, removed ${removed} line`)] : []),
      diff,
    ),
    step('test', ...shell('Check forecast output', 'bun test tests/weather.test.ts', span('1 pass', 'term-green'), '  ', '0 fail')),
    step('committing', ...shell('Commit forecast change', `git add ${path} && git commit -m "${newCommit.title}"`,
      span(`[main ${newCommit.id}]`, 'agent-commit-id'), ' ', newCommit.title)),
    step('commit',
      agentLine('response', assistant, 'The weekend forecast now includes wind speed in km/h. The test passed.'),
      ...(claude ? [row('agent-turn-time', '✻ Cooked for 12s')] : codex ? [row('agent-turn-time', 'Worked for 12s')] : []),
    ),
  ];
  transcript.replaceChildren(...agentSteps.map(step => step.node));
  agentSession.scrollTop = 0;
  followTranscript = false;
}

export function renderFrame(frame: Frame) {
  if (frame.files !== previousFrame?.files || frame.phase === 'edit') renderOutline(frame);
  if (!previousFrame || frame.ahead !== previousFrame.ahead || frame.toCommit !== previousFrame.toCommit || frame.phase === 'commit') {
    renderHistory(frame);
  }
  if (frame.usage !== previousFrame?.usage) renderUsage(frame);
  previousFrame = frame;
  const stage = order.indexOf(frame.phase);
  const newTab = frame.phase === 'new-tab';
  demo.toggleAttribute('data-expanded', frame.phase === 'usage');
  demo.toggleAttribute('data-resized', stage >= resize);
  demo.toggleAttribute('data-shell', newTab);
  shellTab.hidden = !newTab;
  if (frame.phase === 'read' && !reader.hasChildNodes()) renderReader();
  for (const { at, node } of agentSteps) {
    const hidden = stage < at;
    if (node.hidden !== hidden) {
      node.hidden = hidden;
      followTranscript = true;
    }
  }
  activity.hidden = stage === 0 || stage >= finish;
  // A phone may hide the agent while another pane is in focus. Follow the
  // newly printed output when it becomes visible again, not the page itself.
  if (followTranscript && agentSession.clientHeight) {
    agentSession.scrollTop = agentSession.scrollHeight;
    followTranscript = false;
  }
}
