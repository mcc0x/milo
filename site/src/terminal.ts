import { edit, newCommit, pushed, request, scene, weatherDiff, type Change, type Commit, type DiffLine, type Frame, type Phase } from './scene';

export const element = (id: string) => document.getElementById(id)!;
const outline = element('outline-pane');
const history = element('history-pane');
const reader = element('reader-content');
const agentSteps = [...document.querySelectorAll<HTMLElement>('.agent-step')];
const status = element('agent-status');

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

function renderUsage(frame: Frame) {
  for (const name of ['codex', 'claude'] as const) {
    element(`${name}-age`).textContent = frame.usage[name].age;
    for (const slot of ['five', 'week'] as const) {
      const value = frame.usage[name][slot];
      const figure = element(`${name}-${slot}`);
      const percent = document.createElement('small');
      percent.textContent = '%';
      figure.className = value >= 90 ? 'level-red' : value >= 70 ? 'level-amber' : 'level-green';
      // A figure that has just risen counts up to its new value; the
      // stylesheet draws the number from these two properties.
      const from = slot === 'five' ? frame.usage[name].from : undefined;
      if (from === undefined) {
        figure.replaceChildren(String(value), percent);
        continue;
      }
      const count = span('', 'count');
      count.style.setProperty('--from', String(from));
      count.style.setProperty('--n', String(value));
      figure.replaceChildren(count, percent);
    }
  }
}

function diffRow(line: DiffLine, numbers: boolean) {
  const code = span('', 'diff-code');
  code.append(...line.tokens.map(token => typeof token === 'string' ? token : span(token[0], `term-${token[1]}`)));
  const marker = line.change === 'added' ? '+' : '-';
  return row(`diff-line diff-${line.change}`, ...(numbers ? [span(String(line.number ?? ''), 'diff-number')] : []), span(marker, 'diff-marker'), code);
}

function renderReader(edited: boolean) {
  const heading = detailRow(fileName('weather.ts', '', '', true), edited ? edit.change : undefined);
  heading.classList.add('reader-heading');
  heading.firstElementChild?.append(span('src', 'term-muted history-dir'));
  const lines = edited
    ? weatherDiff.map(line => diffRow(line, true))
    : [row('diff-line', span('1', 'diff-number term-muted'), span('', 'diff-marker'), span('export const outlook = "clear";', 'diff-code'))];
  reader.replaceChildren(heading, ...lines, row('term-hint term-muted', '↑↓ scroll · q close · ? help'));
}

const order = scene.map(step => step.phase);
export function renderFrame(frame: Frame) {
  renderOutline(frame);
  renderHistory(frame);
  renderUsage(frame);
  const stage = order.indexOf(frame.phase);
  for (const step of agentSteps) step.hidden = stage < order.indexOf(step.dataset.step as Phase);
  renderReader(stage >= order.indexOf('edit'));
  // Beside the composer: nothing while the request is typed, then working, then done.
  const state = frame.phase === 'compose' ? 'idle' : stage >= order.indexOf('usage') ? 'done' : 'working';
  status.dataset.state = state;
  status.textContent = state === 'idle' ? '' : state === 'done' ? '✓ done' : 'working';
}

// The agent's short excerpt highlights the change introduced by the edit.
element('agent-diff').replaceChildren(...[weatherDiff[0], weatherDiff[1], ...weatherDiff.slice(3)].map(line => diffRow(line, false)));
const [, added, removed] = tallies(edit.change);
element('agent-tally').append(added, ' ', removed);
element('agent-commit-id').textContent = `[main ${newCommit.id}]`;
element('agent-commit-title').textContent = newCommit.title;
element('agent-request-text').textContent = request;
