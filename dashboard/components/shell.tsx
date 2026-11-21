'use client';

import {useEffect, useLayoutEffect, useRef, useState} from 'react';
import Alerts from './alerts';
import Appearance from './appearance';
import {SaveChoices} from './controls';
import Layout from './layout-settings';
import MenuBar from './menu-bar';
import {restoreSettingsFocus, SessionProvider, useEditor, useSession} from './session';
import {groupLabels, healthLabels, type SessionState} from './session-state';
import Workflow from './workflow';

const tabs = [
  {id: 'layout', label: 'Layout', heading: 'group-layout', icon: <><rect x="3" y="4" width="18" height="16" rx="3"/><path d="M3 8h18m-7 0v12m0-6h7"/></>},
  {id: 'appearance', label: 'Appearance', heading: 'group-preferences', icon: <><circle cx="12" cy="12" r="8"/><path fill="currentColor" stroke="none" d="M12 4a8 8 0 0 1 0 16Z"/></>},
  {id: 'workflow', label: 'Workflow', heading: 'group-preferences', icon: <><path d="M6 17V7m0 8c8 0 12-2 12-8"/><circle cx="6" cy="5" r="2"/><circle cx="6" cy="19" r="2"/><circle cx="18" cy="5" r="2"/></>},
  {id: 'alerts', label: 'Alerts', heading: 'group-alerts', icon: <path d="M6 16V11a6 6 0 0 1 12 0v5l2 2H4l2-2Zm4 5h4"/>},
  {id: 'menubar', label: 'Menu Bar', heading: 'group-menubar', icon: <><rect x="3" y="4" width="18" height="16" rx="3"/><path d="M3 10h18m-5-3h2"/></>},
  {id: 'health', label: 'Health', heading: 'diagnostics-title', icon: <><path d="M5 5h14a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2Z"/><path d="M6 12h3l2-4 3 8 2-4h2"/></>},
] as const;

type Page = (typeof tabs)[number]['id'];
const views = {layout: Layout, appearance: Appearance, workflow: Workflow, alerts: Alerts, menubar: MenuBar, health: Health};
const connectionLabels = {closed: 'Closed', ready: 'Local session', offline: 'Not connected', opening: 'Connecting'};

function isAvailable(page: Page, state: SessionState | undefined) {
  if (page === 'alerts') return Boolean(state?.alerts);
  if (page === 'menubar') return state != null && state.menubar.installed !== null;
  return true;
}

function isDirty(page: Page, groups: SessionState['groups'] | undefined) {
  if (!groups) return false;
  if (page === 'layout') return groups.layout.dirty;
  if (page === 'appearance') return groups.preferences.dirty;
  return page === 'workflow' && (groups.preferences.dirty || groups.account.dirty);
}

function Navigation({page, navigate}: {page: Page; navigate: (page: Page) => void}) {
  const {state, discard, requestSave, canDiscard, canSave} = useEditor();
  return (
    <div className="control-dock">
      <nav className="navigation" aria-label="Settings sections">
        {tabs.map(tab => (
          <button key={tab.id} type="button" data-page={tab.id} aria-current={tab.id === page ? 'page' : undefined}
            aria-controls="settings-panel" hidden={!isAvailable(tab.id, state)} onClick={() => navigate(tab.id)}>
            <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">{tab.icon}</svg>
            <span>{tab.label}<span className="nav-draft" hidden={!isDirty(tab.id, state?.groups)}><span className="sr-only"> (unsaved changes)</span></span></span>
          </button>
        ))}
      </nav>
      <div className="dock-actions" aria-label="Draft actions" role="group">
        <button id="discard-all" className="button text-button draft-action" type="button" aria-label="Discard" title="Discard" disabled={!canDiscard} onClick={() => discard('all')}>
          <svg viewBox="0 0 20 20" aria-hidden="true"><path d="m5 5 10 10M15 5 5 15"/></svg>
        </button>
        <button id="save-draft" className={`button ${canSave ? 'primary' : 'secondary'} draft-action`} type="button" aria-label="Save" title="Save" disabled={!canSave} onClick={requestSave}>
          <svg viewBox="0 0 20 20" aria-hidden="true"><path d="m3.5 10 4.2 4.2L16.5 5.5"/></svg>
        </button>
      </div>
    </div>
  );
}

function SessionBar() {
  const {state, connected, ended, localError, running, closing, reopenNote, done} = useEditor();
  const status = ended ? 'closed' : connected ? 'ready' : localError ? 'offline' : 'opening';
  const context = state?.context;
  const path = context?.worktree || context?.directory;
  const name = path?.split('/').filter(Boolean).at(-1) || path || 'Directory unavailable';
  return (
    <section className="actionbar" aria-label="Project session" id="actionbar" hidden={!state}>
      <img src="/rex.png" width="20" height="20" alt=""/>
      <div className="session-info">
        <div id="context" className="context">
          {context ? <>
            <strong>{context.inside_rex ? 'In Rex' : 'Outside Rex'}</strong>
            <span className="context-worktree" title={path || undefined} aria-hidden={path ? true : undefined}>{name}</span>
            {path ? <span className="sr-only">{context.worktree ? 'Git worktree' : 'Directory'}: {path}</span> : null}
          </> : null}
        </div>
        <span id="connection" className="connection" data-state={status}>{connectionLabels[status]}</span>
        <p id="reopen-panes" className="reopen-panes" hidden={!reopenNote}>{reopenNote}</p>
      </div>
      <button id="done" className="button secondary" type="button" disabled={running || closing} onClick={done}>Done</button>
    </section>
  );
}

function Feedback({page}: {page: Page}) {
  const {
    localError, operationWarning, paused, connected, running, closing, connect, dropEdit,
    hasUnsavedChanges, draftStatus, healthStatus,
  } = useEditor();
  const error = localError || operationWarning;
  const unavailable = running || closing;
  return (
    <>
      <p className="sr-only" id="draft-status" role="status" data-dirty={String(hasUnsavedChanges)}>{draftStatus}</p>
      <p className="sr-only" id="health-status" role="status" aria-atomic="true">{healthStatus}</p>
      <p className="sr-only" id="operation-status" role="status" aria-atomic="true">{operationWarning}</p>
      <main className="editor-main">
        <h1 className="sr-only">Milo settings</h1>
        <div id="error-box" hidden={!error}>
          <p id="error">{error}</p>
          <div className="button-row" id="error-actions" hidden={!paused && !localError}>
            <button className="button secondary" id="retry" type="button" hidden={!paused} disabled={unavailable} onClick={() => { void connect(true); }}>Retry edit</button>
            <button className="button text-button" id="drop-edit" type="button" hidden={!paused} disabled={unavailable} onClick={dropEdit}>Drop this edit</button>
            <button className="button text-button" id="reconnect" type="button" hidden={!localError || paused} disabled={unavailable} onClick={() => { void connect(); }}>{connected ? 'Refresh editor state' : 'Reconnect'}</button>
          </div>
        </div>
        <SettingsPanel page={page}/>
      </main>
    </>
  );
}

function Dialogs() {
  const {state, saveOptions, confirmation, finishSave, finishConfirmation, command} = useEditor();
  const saveDialog = useRef<HTMLDialogElement>(null);
  const confirmDialog = useRef<HTMLDialogElement>(null);

  useLayoutEffect(() => {
    const dialog = saveDialog.current;
    if (saveOptions.length && dialog && !dialog.open) {
      dialog.returnValue = '';
      dialog.showModal();
    }
  }, [saveOptions]);

  useLayoutEffect(() => {
    const dialog = confirmDialog.current;
    if (confirmation && dialog && !dialog.open) {
      dialog.returnValue = '';
      dialog.showModal();
    }
  }, [confirmation]);

  return (
    <>
      <dialog ref={saveDialog} id="save-options" aria-labelledby="save-title"
        onClose={event => finishSave(event.currentTarget.returnValue === 'save')}>
        <h2 id="save-title">Save changes</h2>
        <p>Choose where to save each draft.</p>
        <div id="save-groups">
          {state ? saveOptions.map(key => {
            const title = key === 'preferences' ? 'Appearance and workflow' : groupLabels[key];
            return (
              <div key={key} role="group" aria-labelledby={`save-group-${key}`}>
                <h3 id={`save-group-${key}`}>{title[0].toUpperCase() + title.slice(1)}</h3>
                <SaveChoices group={key} available={state.groups[key]} select="dock" onSave={scope => {
                  saveDialog.current?.close('save');
                  command('save', {group: key, scope});
                }}/>
              </div>
            );
          }) : null}
        </div>
        <form method="dialog" className="dialog-actions button-row"><button className="button secondary">Cancel</button></form>
      </dialog>
      <dialog ref={confirmDialog} id="confirm" aria-labelledby="confirm-title" aria-describedby="confirm-note"
        onClose={event => finishConfirmation(event.currentTarget.returnValue === 'continue')}>
        <h2 id="confirm-title">{confirmation?.title}</h2>
        <p id="confirm-note">{confirmation?.note}</p>
        <div className="button-row dialog-actions">
          <button className="button secondary" id="confirm-cancel" type="button" onClick={() => confirmDialog.current?.close('cancel')}>Keep editing</button>
          <button id="confirm-do" className="button danger" type="button" onClick={() => confirmDialog.current?.close('continue')}>{confirmation?.label}</button>
        </div>
      </dialog>
    </>
  );
}

function Health() {
  const {state, queue, paused, running, command} = useSession();
  const checking = queue.some(item => item.action === 'doctor');
  const result = checking ? null : state.doctor;
  return (
    <section className="health">
      <h2 id="diagnostics-title">Health Check</h2>
      {result ? <div className="health-result" data-level={result.level}>
        <strong>{healthLabels[result.level]}</strong>
        {result.note ? <p>{result.note}</p> : null}
      </div> : null}
      <button className="button secondary" id="doctor" type="button" aria-disabled={checking || undefined}
        disabled={!checking && (paused || running)} onClick={() => { if (!checking) command('doctor'); }}>
        {checking ? 'Checking…' : result ? 'Run again' : 'Run check'}
      </button>
    </section>
  );
}

function SettingsPanel({page}: {page: Page}) {
  const {state} = useEditor();
  const View = views[page];
  return (
    <section id="settings-panel" className="settings-panel" tabIndex={-1}
      aria-labelledby={state ? tabs.find(tab => tab.id === page)?.heading : undefined}>
      {state ? <View/> : null}
    </section>
  );
}

function EditorShell({devVersion}: {devVersion?: string}) {
  const {state, ended, canSave, canDiscard, focusOwner, applyNumericDrafts, command, openingMessage, canOpenRetry, connect, toast} = useEditor();
  const [selectedPage, setSelectedPage] = useState<Page>('layout');
  const pendingNavigation = useRef(false);
  const closedHeading = useRef<HTMLHeadingElement>(null);
  const page = isAvailable(selectedPage, state) ? selectedPage : 'layout';
  if (state && page !== selectedPage) setSelectedPage(page);

  useEffect(() => {
    function dismissOnPointer(event: PointerEvent) {
      const menu = document.querySelector<HTMLDetailsElement>('.action-menu[open]');
      if (menu && event.target instanceof Node && !menu.contains(event.target)) menu.open = false;
    }
    function dismissOnEscape(event: KeyboardEvent) {
      if (event.key !== 'Escape' || event.defaultPrevented || document.querySelector('dialog[open]')) return;
      const menu = document.querySelector<HTMLDetailsElement>('.action-menu[open]');
      if (!menu) return;
      if (menu.contains(document.activeElement)) menu.querySelector('summary')?.focus();
      menu.open = false;
      event.preventDefault();
    }
    document.addEventListener('pointerdown', dismissOnPointer);
    document.addEventListener('keydown', dismissOnEscape);
    return () => {
      document.removeEventListener('pointerdown', dismissOnPointer);
      document.removeEventListener('keydown', dismissOnEscape);
    };
  }, []);

  function focusPanel() {
    const panel = document.getElementById('settings-panel');
    if (!panel) return;
    panel.scrollTop = 0;
    panel.focus({preventScroll: true});
  }

  useLayoutEffect(() => {
    if (ended) {
      closedHeading.current?.focus();
      return;
    }
    if (!state) return;
    if (pendingNavigation.current) {
      pendingNavigation.current = false;
      focusPanel();
      return;
    }
    if (!focusOwner) return;
    const {element, id, headingId, heading} = focusOwner;
    const active = document.activeElement;
    // An async response must not steal focus that the user has already moved elsewhere.
    if (active && active !== element && active !== document.body) return;
    if (id === 'opening-message') {
      restoreSettingsFocus();
      return;
    }
    if ((id === 'discard-all' && !canDiscard) || (id === 'save-draft' && !canSave)) {
      document.querySelector<HTMLElement>(`.navigation [data-page="${page}"]`)?.focus({preventScroll: true});
      return;
    }
    if (element.isConnected && !element.matches(':disabled, [hidden]')) return;
    const replacement = document.getElementById(id) || (id === 'menubar-remove' ? document.getElementById('menubar-action') : null);
    if (heading) restoreSettingsFocus(replacement, headingId);
    else if (replacement && !replacement.matches(':disabled, [hidden]')) replacement.focus({preventScroll: true});
    else document.getElementById('settings-panel')?.focus({preventScroll: true});
  }, [state, ended, focusOwner, canSave, canDiscard, page]);

  function navigate(next: Page) {
    if (next !== page && !applyNumericDrafts()) return;
    const openMenu = document.querySelector<HTMLDetailsElement>('.action-menu[open]');
    if (openMenu) openMenu.open = false;
    if (next === page) focusPanel();
    else {
      pendingNavigation.current = true;
      setSelectedPage(next);
    }
    if (next === 'alerts' && state?.alerts) command('alerts', {field: 'refresh', value: null});
  }

  return (
    <>
      <div className="frame surface" id="editor" hidden={!state}>
        <button className="skip-link" id="skip-settings" type="button" onClick={() => document.getElementById('settings-panel')?.focus()}>Skip to settings</button>
        <Navigation page={page} navigate={navigate}/>
        <SessionBar/>
        <Feedback page={page}/>
        <footer className="version-badge"><img src="/favicon.png" width="24" height="24" alt="Milo"/>
          <span>v{state?.milo_version ?? devVersion}</span>
        </footer>
      </div>
      <div className="frame opening" id="opening" hidden={Boolean(state) || ended}>
        <p id="opening-message" role="status" tabIndex={-1}>{openingMessage}</p>
        <button className="button secondary" id="open-retry" type="button" hidden={!canOpenRetry} onClick={() => { void connect(); }}>Connect again</button>
      </div>
      <main id="closed" className="frame opening" hidden={!ended}>
        <h1 ref={closedHeading} tabIndex={-1}>Settings closed</h1>
        <p>Run <code>milo web</code> in your project to reopen the dashboard. You can close this tab.</p>
      </main>
      <Dialogs/>
      <div className="toast-stack">
        <div className="toast" id="message" role="status" aria-atomic="true">{toast && !toast.error ? toast.text : ''}</div>
        <div className="toast toast-error" id="alert-toast" role="alert" aria-atomic="true">{toast?.error ? toast.text : ''}</div>
      </div>
    </>
  );
}

export default function Dashboard({devVersion}: {devVersion?: string}) {
  return <SessionProvider><EditorShell devVersion={devVersion}/></SessionProvider>;
}
