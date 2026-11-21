import { memo, type ReactNode } from 'react';
import { edit, newCommit, pushed, request, scene, weatherDiff, type Change, type Commit, type DemoFile, type DiffLine, type Frame, type Harness, type Phase } from '../../scene';

// React keeps each expression in its own text node, and the browser shapes
// each node separately. A phrase is built as one string unless its parts are
// spaced apart as separate words, which is how the window was first drawn.

function Row({ className, children }: { className: string; children?: ReactNode }) {
  return <div className={`term-line ${className}`}>{children}</div>;
}

const symbols = {
  folder: ['\uf07c', 'blue'],
  ts: ['\ue628', 'cyan'],
  md: ['\ue73e', 'slate'],
  json: ['\ue60b', 'yellow'],
} as const;

function FileName({ name, tree, color, bold = false, folder = false, dir }: { name: string; tree: string; color: string; bold?: boolean; folder?: boolean; dir?: string }) {
  const kind = folder ? 'folder'
    : name.endsWith('.ts') ? 'ts' : name.endsWith('.md') ? 'md' : 'json';
  const [glyph, tint] = symbols[kind];
  return (
    <span className="term-file">
      <span className="term-muted tree-branch">{tree}</span>
      <span className={`term-icon term-${tint}`} aria-hidden="true">{glyph}</span>
      <span className={`${color}${bold ? ' term-bold' : ''}`}>{name}</span>
      {dir && <span className="term-muted history-dir">{dir}</span>}
    </span>
  );
}

function Project() {
  return <span className="term-project"><span className="term-bold">tidebook</span><span className="term-purple">main</span></span>;
}

/** A name with what changed beside it. A folder shows totals, not an action. */
function DetailRow({ lead, change, folder = false, className = '' }: { lead: ReactNode; change?: Change; folder?: boolean; className?: string }) {
  const action = <span className={`term-action ${change?.kind === 'new' ? 'term-green' : 'term-amber'}`}>{folder ? '' : change?.kind ?? ''}</span>;
  const added = <span className="term-number term-green">{change?.added ? `+${change.added}` : ''}</span>;
  return change?.removed
    ? <Row className={`term-detail ${className}`}>{lead}{action}{added}<span className="term-number term-red">{`−${change.removed}`}</span></Row>
    : <Row className={`term-detail no-removals ${className}`}>{lead}{action}{added}</Row>;
}

const changeColor = (change?: Change) => change?.kind === 'edited' ? 'term-amber' : change ? 'term-green' : 'term-muted';

// The panes are memoized: the composer types a letter thirty times a second,
// and a frame keeps the same file and commit lists until they change.
export const Outline = memo(function Outline({ frame }: { frame: Frame }) {
  const folders = new Set<string>();
  const rows: ReactNode[] = [];
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
      rows.push(<DetailRow key={path} folder change={{ kind: 'edited', added, removed }}
        lead={<FileName name={dir} tree={'│ '.repeat(depth)} color={color} bold={changes.some(entry => entry.pending === 'commit')} folder />} />);
    }
    const weather = file.path === edit.path && !!file.change;
    rows.push(<DetailRow key={file.path} change={file.change}
      className={weather ? `term-selected${frame.phase === 'edit' ? ' term-lit' : ''}` : ''}
      lead={<FileName name={name} tree={'│ '.repeat(components.length)} color={changeColor(file.change)} bold={file.pending === 'commit'} />} />);
  }
  return (
    <>
      <Row className="term-header">
        <Project />
        <span className="term-summary"><span className="term-bold">{`${frame.toCommit} to commit`}</span><span className="term-muted"> · </span>{`${frame.toPush} to push`}</span>
      </Row>
      {rows}
      <Row className="term-hint term-muted">n next change · c changed only · q quit · ? help</Row>
    </>
  );
});

function CommitRows({ commit, open = false, remote = false, lit = false }: { commit: Commit; open?: boolean; remote?: boolean; lit?: boolean }) {
  const files = open ? commit.files ?? [] : [];
  const expanded = files.length > 0;
  return (
    <>
      <Row className={`term-commit${expanded ? ' term-selected' : ''}${remote ? ' term-muted' : ''}${lit ? ' term-lit' : ''}`}>
        <span className="commit-title">{`${expanded ? '●▾' : '● '}${commit.title}`}</span>
        <span className="term-muted">{expanded ? `${files.length} ${files.length === 1 ? 'file' : 'files'}` : commit.age}</span>
      </Row>
      {files.map((file: DemoFile) => {
        const [dir, name] = file.path.split('/');
        return <DetailRow key={file.path} change={file.change}
          lead={<FileName name={name} tree="  │ " color={file.change?.kind === 'new' ? 'term-green' : 'term-amber'} bold dir={dir} />} />;
      })}
    </>
  );
}

export const History = memo(function History({ frame }: { frame: Frame }) {
  return (
    <>
      <Row className="term-header"><Project /><span className="term-summary">{`${frame.ahead.length} commits to push`}</span></Row>
      <Row className="history-dots">
        <span className="term-bold">{`○ ${'●  '.repeat(frame.ahead.length)}`}</span>
        <span className="term-muted">{`│${'  ●'.repeat(pushed.length)}`}</span>
      </Row>
      <Row className="term-bold">{`○ ${frame.toCommit} files to commit`}</Row>
      {frame.ahead.map((commit, index) => (
        <CommitRows key={commit.title} commit={commit} open={index === 0} lit={frame.phase === 'commit' && commit === newCommit} />
      ))}
      <Row className="history-divider term-muted">── on origin/main</Row>
      {pushed.map(commit => <CommitRows key={commit.title} commit={commit} remote />)}
      <Row className="term-hint term-muted">Enter hide files · r reload · q quit · ? help</Row>
    </>
  );
});

/** How much of a provider's window is left, coloured by how close it is to running out. */
export function UsageFigure({ id, left }: { id: string; left: number }) {
  return <strong id={id} className={left <= 10 ? 'term-red' : left <= 30 ? 'term-amber' : 'term-green'}>{`${left}%`}</strong>;
}

function DiffRow({ line }: { line: DiffLine }) {
  return (
    <Row className={`diff-line diff-${line.change}`}>
      <span className="diff-number">{line.number ?? ''}</span>
      <span className="diff-marker">{line.change === 'added' ? '+' : '-'}</span>
      <span className="diff-code">
        {line.tokens.map((token, index) => typeof token === 'string' ? token : <span key={index} className={`term-${token[1]}`}>{token[0]}</span>)}
      </span>
    </Row>
  );
}

export function Reader() {
  return (
    <>
      <DetailRow className="reader-heading" change={edit.change} lead={<FileName name="weather.ts" tree="" color="" bold dir="src" />} />
      {weatherDiff.map((line, index) => <DiffRow key={index} line={line} />)}
      <Row className="term-hint term-muted">↑↓ scroll · q close · ? help</Row>
    </>
  );
}

const order = scene.map(step => step.phase);
/** Where a phase falls in the scene; later phases keep what earlier ones drew. */
export const stageOf = (phase: Phase) => order.indexOf(phase);
const finish = stageOf('commit');

function AgentLine({ kind, glyph, children }: { kind: string; glyph: string; children: ReactNode }) {
  return glyph
    ? <Row className={`agent-${kind} agent-gutter`}><span className="agent-glyph">{glyph}</span><span>{children}</span></Row>
    : <Row className={`agent-${kind}`}><span>{children}</span></Row>;
}

/** The banner each coding agent prints when it starts. */
export function AgentBanner({ harness: { id, name, version, model } }: { harness: Harness }) {
  const codex = id === 'codex';
  const directory = '~/dev/tidebook';
  return (
    <>
      <Row className="agent-product">
        <span className="term-bold">{codex ? 'OpenAI Codex' : name}</span>
        <span className="agent-version">{codex ? ` (v${version})` : ` v${version}`}</span>
      </Row>
      {id === 'claude' && <>
        <Row className="agent-model">{`${model} · Claude Max`}</Row>
        <Row className="agent-directory">{directory}</Row>
      </>}
      {codex && <>
        <Row className="agent-model"><span className="agent-label">{'model:     '}</span>{model}</Row>
        <Row className="agent-directory"><span className="agent-label">{'directory: '}</span>{directory}</Row>
      </>}
    </>
  );
}

/** Everything the agent prints in one pass. Each step stays hidden until the scene reaches it. */
export const AgentTranscript = memo(function AgentTranscript({ harness: { id }, stage }: { harness: Harness; stage: number }) {
  const claude = id === 'claude';
  const codex = id === 'codex';
  const prompt = claude ? '❯' : codex ? '›' : '';
  const assistant = claude ? '⏺' : codex ? '•' : '';
  const result = claude ? '⎿' : '└';
  const path = edit.path;
  const { added, removed } = edit.change;

  const tool = (name: string, detail: string) => (
    <AgentLine kind="tool" glyph={assistant || '✓'}>
      <span className="agent-tool-name">{name}</span>{claude ? `(${detail})` : codex ? ` ${detail}` : `: ${detail}`}
    </AgentLine>
  );
  const shell = (description: string, command: string, output: ReactNode) => id === 'omp' ? <>
    <AgentLine kind="tool agent-shell-title" glyph="›_"><span className="agent-tool-name">Bash</span>{' · '}{description}</AgentLine>
    <AgentLine kind="shell-command" glyph="$">{command}</AgentLine>
    <AgentLine kind="result" glyph="└">{output}</AgentLine>
  </> : <>
    {tool(codex ? 'Ran' : 'Bash', command)}
    <AgentLine kind="result" glyph={result}>{output}</AgentLine>
  </>;
  const step = (phase: Phase, children: ReactNode) => (
    <div className="agent-step" data-step={phase} hidden={stage < stageOf(phase)}>{children}</div>
  );

  return (
    <>
      {step('prompt', <AgentLine kind="request" glyph={prompt}>{request}</AgentLine>)}
      {step('plan', <AgentLine kind="response" glyph={assistant}>I’ll add wind speed in km/h and check the forecast output.</AgentLine>)}
      {step('editing', <>
        {tool(claude ? 'Update' : codex ? 'Edited' : 'Edit', claude ? path : `${path} (+${added} -${removed})`)}
        {claude && <AgentLine kind="result" glyph="⎿">{`Added ${added} lines, removed ${removed} line`}</AgentLine>}
        <Row className="agent-diff">
          {weatherDiff.map((line, index) => <DiffRow key={index} line={line.number ? line : { ...line, number: 1 }} />)}
        </Row>
      </>)}
      {step('test', shell('Check forecast output', 'bun test tests/weather.test.ts', <><span className="term-green">1 pass</span>{'  '}{'0 fail'}</>))}
      {step('committing', shell('Commit forecast change', `git add ${path} && git commit -m "${newCommit.title}"`,
        <><span className="agent-commit-id">{`[main ${newCommit.id}]`}</span>{' '}{newCommit.title}</>))}
      {step('commit', <>
        <AgentLine kind="response" glyph={assistant}>The weekend forecast now includes wind speed in km/h. The test passed.</AgentLine>
        {claude && <Row className="agent-turn-time">✻ Cooked for 12s</Row>}
        {codex && <Row className="agent-turn-time">Worked for 12s</Row>}
      </>)}
    </>
  );
});

/** The agent's status line and input box, with whatever has been typed so far. */
export function AgentChrome({ harness: { id, model }, stage, typed }: { harness: Harness; stage: number; typed: string }) {
  const claude = id === 'claude';
  const codex = id === 'codex';
  return (
    <div className="agent-chrome">
      <div className="agent-activity" id="agent-activity" hidden={stage === 0 || stage >= finish}>
        <span className="activity-glyph">{claude ? '✻' : codex ? '•' : '⠿'}</span>
        {claude ? 'Thinking…' : 'Working'}
        <span className="activity-hint">esc to interrupt</span>
      </div>
      <div className="agent-composer">
        <div className="composer-model" id="composer-model">
          {codex && `${model} · ~/dev/tidebook`}
          {id === 'omp' && <>
            <span className="omp-model-mark">π</span><span className="omp-model-name">{model}</span><span>tidebook</span><span className="omp-model-branch">main</span>
          </>}
        </div>
        <div className="composer-line">
          <span className="composer-prompt" id="composer-prompt">{claude ? '❯' : codex ? '›' : ''}</span>
          <span><span className="composer-text" id="composer-text">{typed}</span><i className="composer-caret"></i><span className="composer-placeholder" id="composer-placeholder">{codex ? 'Ask Codex to do anything' : ''}</span></span>
        </div>
      </div>
      <div className="composer-hint" id="composer-hint">{id === 'omp' ? '' : '? for shortcuts'}</div>
    </div>
  );
}
