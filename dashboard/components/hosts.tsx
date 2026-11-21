'use client';

import {useRef, useState} from 'react';
import {useSession} from './session';
import type {HostState, TransferGroup, TransferPreview, TransferScope} from './session-state';
import './hosts.css';

const openCommand = 'milo web --host LABEL';
const groups: readonly (readonly [TransferGroup, string, string])[] = [
  ['appearance', 'Appearance', 'Icons and change marks'],
  ['workflow', 'Workflow', 'Project following and outline filters'],
  ['usage', 'Usage display', 'Which tools show, and figure size'],
  ['layout', 'Layout', 'Panes, side, order and proportions'],
];
const scopes: readonly (readonly [TransferScope, string])[] = [
  ['global', 'Global defaults'], ['worktree', 'This worktree'], ['all', 'Both'],
];

const compatibilityLabels = {
  checked: 'Checked', newer: 'Newer Rex', older: 'Older Rex', command_differs: 'Versions differ',
};
const viewerLabels = {
  here: 'A Rex app is open on this computer.',
  elsewhere: 'Rex apps are connected from another device only.',
  nobody: 'No Rex app is connected.',
};

/** Which computer these settings belong to, and whether its Rex is one Milo was checked with. */
export function HostSummary({host, headingId}: {host: HostState; headingId: string}) {
  const rex = host.rex;
  return (
    <div className="host-summary" role="group" aria-labelledby={headingId}>
      <span className="host-mark" aria-hidden="true">
        <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
          <rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8m-4-4v4"/>
        </svg>
      </span>
      <div className="setting-info">
        <span className="setting-label" id="host-name">{host.name}</span>
        <span className="setting-detail" id="host-rex">
          {rex ? `Rex ${rex.version} · API ${rex.api} · host ${rex.host_id}` : 'No Rex server answered. The panes work without it.'}
        </span>
        {rex ? <span className="setting-detail" id="host-note">{rex.note}</span> : null}
        {rex?.command && (rex.command.version !== rex.version || rex.command.api !== rex.api)
          ? <span className="setting-detail">The rex command is {rex.command.version}, API {rex.command.api}.</span> : null}
        {rex?.viewers ? <span className="setting-detail">{viewerLabels[rex.viewers]}</span> : null}
        {host.forwarded ? <span className="setting-detail">You opened these settings from another computer, through a Rex forward.</span> : null}
      </div>
      {rex ? <span className="integration-state" id="host-compatibility" data-state={rex.warning ? 'warning' : 'on'}>
        {compatibilityLabels[rex.compatibility]}</span> : null}
    </div>
  );
}

function OtherHost() {
  const {notify} = useSession();
  return (
    <section aria-labelledby="hosts-open-title" className="host-section">
      <h3 id="hosts-open-title" className="setting-subheading">Open another host’s settings</h3>
      <p className="setting-detail">
        Run this on the computer you are using. <code>LABEL</code> is the host’s name in <code>rex hosts</code>. Milo
        starts its editor on that host and carries it here through a Rex forward on <code>127.0.0.1</code>. Choosing
        Done, or pressing <kbd>Ctrl</kbd>‑<kbd>C</kbd>, closes the editor and the forward.
      </p>
      <div className="host-command">
        <code id="host-command">{openCommand}</code>
        <button type="button" className="button secondary" id="host-command-copy" onClick={() => {
          navigator.clipboard.writeText(openCommand).then(() => notify('Command copied.'), () => notify('Could not copy. Select the command instead.', true));
        }}>Copy</button>
      </div>
    </section>
  );
}

function Export() {
  const {state, readTransfer, notify} = useSession();
  const [chosen, setChosen] = useState<readonly TransferGroup[]>([]);
  const [scope, setScope] = useState<TransferScope>('global');
  const [bundle, setBundle] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const output = useRef<HTMLTextAreaElement>(null);
  const hasWorktree = Boolean(state.context.worktree);

  async function create() {
    setBusy(true);
    setError('');
    try {
      const result = await readTransfer<{bundle: string}>({action: 'export', groups: [...chosen], scope});
      setBundle(result.bundle);
      requestAnimationFrame(() => output.current?.focus());
    } catch (reason) {
      setBundle('');
      setError((reason as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section aria-labelledby="hosts-export-title" className="host-section">
      <h3 id="hosts-export-title" className="setting-subheading">Export from this host</h3>
      <p className="setting-detail">
        Choose what to put in the file. Tokens, alert destinations, accounts, integrations and state are never
        included; set those up on each host.
      </p>
      <fieldset className="host-choices">
        <legend className="sr-only">Settings to export</legend>
        {groups.map(([key, label, detail]) => (
          <label key={key} className="host-choice">
            <input type="checkbox" id={`export-${key}`} checked={chosen.includes(key)} onChange={event => {
              setBundle('');
              setChosen(event.currentTarget.checked ? [...chosen, key] : chosen.filter(group => group !== key));
            }}/>
            <span><span className="setting-label">{label}</span><span className="setting-detail">{detail}</span></span>
          </label>
        ))}
      </fieldset>
      <div className="setting">
        <div className="setting-info"><span className="setting-label" id="export-scope-label">From</span></div>
        <fieldset className="choice-strip" aria-labelledby="export-scope-label">
          {scopes.map(([key, label]) => (
            <label key={key}>
              <input type="radio" name="export-scope" id={`export-scope-${key}`} checked={scope === key}
                disabled={key !== 'global' && !hasWorktree} onChange={() => { setBundle(''); setScope(key); }}/>
              <span>{label}</span>
            </label>
          ))}
        </fieldset>
      </div>
      <div className="button-row">
        <button type="button" className="button primary" id="export-create" disabled={!chosen.length || busy}
          onClick={() => { void create(); }}>Create bundle</button>
        <button type="button" className="button secondary" id="export-copy" hidden={!bundle} onClick={() => {
          navigator.clipboard.writeText(bundle).then(() => notify('Bundle copied. Paste it into Import on the other host.'),
            () => notify('Could not copy. Select the text instead.', true));
        }}>Copy bundle</button>
      </div>
      <p className="host-error" id="export-error" role="alert">{error}</p>
      <textarea ref={output} id="export-bundle" className="host-bundle" readOnly hidden={!bundle} value={bundle} rows={8}
        aria-label="Exported settings bundle" spellCheck={false}/>
    </section>
  );
}

function Preview({preview, worktree, here, mapHere}: {
  preview: TransferPreview;
  worktree: string | null;
  here: string | null;
  mapHere: (project: string | null) => void;
}) {
  const worktreeName = worktree?.split('/').filter(Boolean).at(-1);
  return (
    <div id="import-preview" className="host-preview" role="group" aria-label="What this import changes">
      {preview.sections.map((section, index) => (
        <div key={index} className="host-preview-section" data-kind={section.kind}>
          <h4>
            {section.project === null ? 'Global defaults' : `Project ${section.project}`}
            {section.kind === 'worktree' ? <span className="setting-detail" title={section.path ?? undefined}>
              → {section.path?.split('/').filter(Boolean).at(-1)} ({section.why})</span> : null}
          </h4>
          {section.kind === 'unmapped' ? (
            <p className="setting-detail">
              Not applied: no worktree here starts from the same commit.
              {worktree && section.project !== null ? (
                <button type="button" className="button text-button" data-map={section.project}
                  onClick={() => mapHere(section.project)}>Apply to {worktreeName}</button>
              ) : ' Open Settings from the matching worktree to choose it.'}
            </p>
          ) : (
            <>
              {section.rows.length ? (
                <table>
                  <thead><tr><th scope="col">Setting</th><th scope="col">Now</th><th scope="col">After import</th></tr></thead>
                  <tbody>
                    {section.rows.map(row => (
                      <tr key={row.label}><th scope="row">{row.label}</th><td>{row.from}</td><td>{row.to}</td></tr>
                    ))}
                  </tbody>
                </table>
              ) : null}
              {section.unchanged ? <p className="setting-detail">
                {section.unchanged === 1 ? '1 setting already matches.' : `${section.unchanged} settings already match.`}</p> : null}
              {section.kind === 'worktree' && section.project !== null && here === section.project ? (
                <p className="setting-detail">Chosen by you.
                  <button type="button" className="button text-button" onClick={() => mapHere(null)}>Don’t apply</button></p>
              ) : null}
            </>
          )}
          {section.skipped.map(item => <p key={item} className="setting-detail">Left out: {item}</p>)}
        </div>
      ))}
    </div>
  );
}

function Import() {
  const {state, readTransfer, writeTransfer, hasUnsavedChanges, running} = useSession();
  const [text, setText] = useState('');
  const [here, setHere] = useState<string | null>(null);
  const [preview, setPreview] = useState<TransferPreview | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);

  async function look(bundle: string, project: string | null) {
    setBusy(true);
    setError('');
    try {
      setPreview(await readTransfer<TransferPreview>({action: 'preview', bundle, here: project}));
      setHere(project);
    } catch (reason) {
      setPreview(null);
      setError((reason as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function apply() {
    if (!preview) return;
    setBusy(true);
    const failure = await writeTransfer({action: 'apply', bundle: text, here, basis: preview.basis});
    setBusy(false);
    setError(failure);
    setPreview(null);
    if (!failure) {
      setText('');
      setHere(null);
    }
  }

  return (
    <section aria-labelledby="hosts-import-title" className="host-section">
      <h3 id="hosts-import-title" className="setting-subheading">Import to this host</h3>
      <label className="setting-detail" htmlFor="import-bundle">
        Paste a bundle exported on another host. Preview shows every change before anything is saved.
      </label>
      <textarea id="import-bundle" className="host-bundle" rows={6} value={text} spellCheck={false}
        placeholder="milo-settings 1" onChange={event => {
          setText(event.currentTarget.value);
          setPreview(null);
          setHere(null);
          setError('');
        }}/>
      <div className="button-row">
        <button type="button" className="button secondary" id="import-preview-button" disabled={!text.trim() || busy}
          onClick={() => { void look(text, null); }}>Preview</button>
        <button type="button" className="button primary" id="import-apply" hidden={!preview}
          disabled={busy || running || hasUnsavedChanges || !preview?.changes} onClick={() => { void apply(); }}>
          {preview?.changes === 1 ? 'Import 1 setting' : `Import ${preview?.changes ?? 0} settings`}
        </button>
      </div>
      {preview && hasUnsavedChanges ? <p className="setting-detail">Save or discard your unsaved changes before importing.</p> : null}
      {preview && !preview.changes ? <p className="setting-detail" id="import-nothing">Nothing to change: this host already has these settings.</p> : null}
      <p className="host-error" id="import-error" role="alert">{error}</p>
      {preview ? <Preview preview={preview} worktree={state.context.worktree} here={here}
        mapHere={project => { void look(text, project); }}/> : null}
    </section>
  );
}

function Undo() {
  const {state, writeTransfer, confirm, notify, hasUnsavedChanges, running} = useSession();
  if (!state.transfer.undo) return null;
  return (
    <div className="setting" id="import-undo-row">
      <div className="setting-info">
        <span className="setting-label">Last import</span>
        <span className="setting-detail" id="import-undo-detail">
          Puts back what the import replaced, unless a setting it changed has been changed again.
        </span>
      </div>
      <button type="button" className="button secondary" id="import-undo" aria-describedby="import-undo-detail"
        disabled={running || hasUnsavedChanges} onClick={() => {
          confirm('Take back the last import?', 'The settings it replaced on this host come back. Other hosts are not changed.',
            'Undo import', () => { void writeTransfer({action: 'undo'}).then(failure => { if (failure) notify(failure, true); }); });
        }}>Undo import</button>
    </div>
  );
}

export default function Hosts() {
  const {state} = useSession();
  return (
    <div className="hosts">
      <header className="integration-heading"><h2 id="hosts-title" tabIndex={-1}>Hosts</h2></header>
      {state.host ? (
        <section aria-labelledby="hosts-this-title" className="host-section">
          <h3 id="hosts-this-title" className="setting-subheading">This host</h3>
          <HostSummary host={state.host} headingId="hosts-this-title"/>
        </section>
      ) : null}
      <OtherHost/>
      <section aria-labelledby="hosts-copy-title" className="host-section">
        <h3 id="hosts-copy-title" className="setting-subheading">Copy settings between hosts</h3>
        <p className="setting-detail">
          Each host keeps its own settings. To make another host match, export a bundle here and import it there,
          or the other way round.
        </p>
        <Undo/>
      </section>
      <Export/>
      <Import/>
    </div>
  );
}
