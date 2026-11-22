'use client';

import {createContext, useContext, useEffect, useMemo, useState, useSyncExternalStore, type ReactNode} from 'react';
import {canSaveDraft, canWriteGroup, groupLabels, healthLabels, type AlertDestination, type GroupKey, type Scope, type SessionState, type TransferGroup, type TransferScope} from './session-state';

export type {AlertDestination, AlertKind, AlertsState, GroupKey, GroupState, PaneRole, PresenceDetails, PresenceState, Scope, SessionState, UsageChoice} from './session-state';

const openingText = 'Opening your local settings…';
const keepaliveInterval = 5 * 60 * 1000;
const groupKeys = ['layout', 'preferences', 'account'] as const;

type Payload = {field?: string; value?: unknown; group?: GroupKey | 'all'; scope?: Scope};
type QueuedAction = Payload & {action: string};
type NumericDraft = {control: HTMLInputElement; scale: number};
type PendingAction = {request: QueuedAction; pending?: number; replacedNumbers?: Map<string, NumericDraft>; onApplied?: () => void};
type FocusOwner = {element: HTMLElement; id: string; headingId?: string; heading: boolean};
type Confirmation = {title: string; note: string; label: string};
type Toast = {text: string; error: boolean};
export type TelegramChat = {id: string; title: string};
type HostedConnectRequest =
  | {action: 'start'; provider: AlertDestination}
  | {action: 'preview'; provider: AlertDestination; deviceKey: string; finishCode?: string};
type HostedSaveRequest = {provider: AlertDestination; deviceKey: string; finishCode?: string; label: string};
type HostedSaveResult = {status: number; cleanupConfirmed?: boolean};
type TransferRead =
  | {action: 'export'; groups: TransferGroup[]; scope: TransferScope}
  | {action: 'preview'; bundle: string; here: string | null};
type TransferWrite = {action: 'apply'; bundle: string; here: string | null; basis: string} | {action: 'undo'};

type Snapshot = {
  state: SessionState | undefined;
  queue: readonly QueuedAction[];
  numericDraftCount: number;
  connected: boolean;
  running: boolean;
  paused: boolean;
  closing: boolean;
  ended: boolean;
  localError: string;
  operationWarning: string;
  healthStatus: string;
  hasUnsavedChanges: boolean;
  draftStatus: string;
  canDiscard: boolean;
  canSave: boolean;
  reopenNote: string;
  openingMessage: string;
  canOpenRetry: boolean;
  toast: Toast | null;
  saveOptions: readonly GroupKey[];
  confirmation: Confirmation | null;
  focusOwner: FocusOwner | null;
};

interface SessionActions {
  draftValue<T>(field: string): T;
  edit(field: string, value: unknown): boolean;
  command(action: string, payload?: Payload, onApplied?: () => void): void;
  refreshMenuIndicator(signal: AbortSignal): Promise<void>;
  findTelegramChats(token: string, signal: AbortSignal): Promise<{chats: TelegramChat[]}>;
  hostedConnect(request: HostedConnectRequest, signal: AbortSignal): Promise<{status: number; data: unknown}>;
  saveHostedConnection(request: HostedSaveRequest): Promise<HostedSaveResult>;
  /** Asks for a bundle or a preview. Nothing is saved. Rejects with the editor's own words. */
  readTransfer<T>(request: TransferRead): Promise<T>;
  /** Applies an import or takes the last one back. Resolves to what went wrong, or to nothing. */
  writeTransfer(request: TransferWrite): Promise<string>;
  confirm(title: string, note: string, label: string, action: () => void): void;
  discard(group: GroupKey | 'all'): void;
  notify(text: string, error?: boolean): void;
  trackNumericDraft(field: string, control: HTMLInputElement, scale: number): void;
  commitNumericDraft(field: string): void;
  applyNumericDraft(field: string): void;
  applyNumericDrafts(): boolean;
  hasNumericDraft(field: string): boolean;
  connect(retryEdits?: boolean): Promise<void>;
  dropEdit(): void;
  done(): void;
  requestSave(): void;
  finishSave(saved: boolean): void;
  finishConfirmation(confirmed: boolean): void;
}

interface Editor {
  actions: SessionActions;
  start(): () => void;
  subscribe(listener: () => void): () => void;
  getSnapshot(): Snapshot;
  getServerSnapshot(): Snapshot;
}

function byId(id: string) {
  return document.getElementById(id);
}

export function restoreSettingsFocus(target?: HTMLElement | null, headingId?: string) {
  if (target) {
    if (target.matches('h2')) target.tabIndex = -1;
    target.focus();
    if (document.activeElement === target) return;
  }
  const heading = (headingId && byId(headingId)) || document.querySelector<HTMLElement>('#settings-panel h2');
  if (heading) {
    heading.tabIndex = -1;
    heading.focus();
  }
}

function captureFocus(): FocusOwner | null {
  const element = document.activeElement;
  if (!(element instanceof HTMLElement)) return null;
  const summary = element.closest('.action-menu')?.querySelector('summary');
  const id = summary?.id || element.id;
  if (!id) return null;
  const headingId = summary?.closest('.settings-group')?.getAttribute('aria-labelledby') ?? undefined;
  return {element, id, headingId, heading: Boolean(summary) || element.matches('h2[tabindex="-1"]')};
}

function affectsDraft({action}: QueuedAction) {
  return action !== 'doctor' && action !== 'menubar' && action !== 'alerts' && action !== 'presence';
}

function publicAction(request: QueuedAction) {
  // A credential in flight is needed only for the authenticated request, never for React.
  if (request.action === 'alerts' && (request.field === 'telegram' || request.field === 'discord')) {
    return {action: request.action, field: request.field};
  }
  return request;
}

function warnOnUnload(event: BeforeUnloadEvent) {
  event.preventDefault();
  event.returnValue = '';
}

function createEditor(): Editor {
  let token = '';
  let active = false;
  let started = false;
  let state: SessionState | undefined;
  let connected = false;
  let running = false;
  let paused = false;
  let closing = false;
  let ended = false;
  let localError = '';
  let operationWarning = '';
  let healthStatus = '';
  let dismissedReopen: number | undefined;
  let openingMessage = openingText;
  let canOpenRetry = false;
  let toast: Toast | null = null;
  let toastTimer: number | undefined;
  let keepaliveTimer: number | undefined;
  let pointerTimer: number | undefined;
  let pointerPress = false;
  let saveRequested = false;
  let saveOptions: readonly GroupKey[] = [];
  let confirmation: Confirmation | null = null;
  let onConfirmed: ((confirmed: boolean) => void) | undefined;
  const queue: PendingAction[] = [];
  const numericDrafts = new Map<string, NumericDraft>();
  const heldDrafts = new Set<string>();
  const listeners = new Set<() => void>();

  function hasUnsavedChanges() {
    return Boolean(state && (state.dirty || state.trial || numericDrafts.size || queue.some(({request}) => affectsDraft(request))));
  }

  function canRequestSave() {
    if (!state || !connected || closing || paused || saveRequested || (running && !queue.length) ||
      queue.some(({request}) => request.action !== 'set')) return false;
    for (const {control} of numericDrafts.values()) {
      if (!control.validity.valid) return false;
    }
    const latest = state;
    return (numericDrafts.size > 0 && canWriteGroup(latest.groups.layout)) ||
      groupKeys.some(key => canSaveDraft(latest, key)) || queue.some(({request}) => {
        const section = request.field?.split('.')[0];
        const group = section === 'layout' || section === 'account' ? section : 'preferences';
        return canWriteGroup(latest.groups[group]);
      });
  }

  function readSnapshot(focusOwner: FocusOwner | null): Snapshot {
    const unsaved = hasUnsavedChanges();
    const groups = state?.groups;
    const dirtyCount = groups ? groupKeys.filter(key => groups[key].dirty ||
      (key === 'layout' && numericDrafts.size)).length : 0;
    const draftStatus = closing ? 'Closing settings…' : paused ? 'Edit not applied' :
      queue.some(({request}) => affectsDraft(request)) ? 'Updating draft…' : dirtyCount ?
        `Unsaved changes in ${dirtyCount === 1 ? '1 group' : `${dirtyCount} groups`}` :
        state?.trial ? 'Layout trial active' : 'No unsaved changes';
    const reopen = state?.reopen_panes;
    if (reopen && (numericDrafts.size || queue.some(({request}) => request.action === 'set'))) dismissedReopen = reopen.version;
    return {
      state, queue: queue.map(({request}) => publicAction(request)), numericDraftCount: numericDrafts.size,
      connected, running, paused, closing, ended, localError, operationWarning, healthStatus,
      hasUnsavedChanges: unsaved, draftStatus, canDiscard: unsaved && !running && !closing && connected,
      canSave: canRequestSave(), reopenNote: reopen && reopen.version !== dismissedReopen ? reopen.text : '',
      openingMessage, canOpenRetry, toast, saveOptions, confirmation, focusOwner,
    };
  }

  const initialSnapshot = readSnapshot(null);
  let snapshot = initialSnapshot;

  function syncKeepalive() {
    if (!active) return;
    if (snapshot.hasUnsavedChanges && keepaliveTimer === undefined) {
      window.addEventListener('beforeunload', warnOnUnload);
      keepaliveTimer = window.setInterval(keepDraftAlive, keepaliveInterval);
    } else if (!snapshot.hasUnsavedChanges && keepaliveTimer !== undefined) {
      window.removeEventListener('beforeunload', warnOnUnload);
      window.clearInterval(keepaliveTimer);
      keepaliveTimer = undefined;
    }
  }

  function publish() {
    snapshot = readSnapshot(active ? captureFocus() : null);
    syncKeepalive();
    for (const listener of listeners) listener();
  }

  function draftValue<T>(field: string): T {
    const previous = queue.findLast(({request}) => request.field === field);
    if (previous) return previous.request.value as T;
    let value: unknown = state;
    for (const key of field.split('.')) value = (value as Record<string, unknown>)[key];
    return value as T;
  }

  function edit(field: string, value: unknown) {
    if (!state || closing || ended) return false;
    const current = draftValue(field);
    if (Object.is(current, value) || (Array.isArray(value) && Array.isArray(current) &&
      current.length === value.length && current.every((item, i) => item === value[i]))) return false;
    enqueue({action: 'set', field, value});
    return true;
  }

  function trackNumericDraft(field: string, control: HTMLInputElement, scale: number) {
    if (control.valueAsNumber !== Number(control.dataset.initial)) numericDrafts.set(field, {control, scale});
    else numericDrafts.delete(field);
    saveRequested = false;
    publish();
  }

  function applyNumericDraft(field: string) {
    const draft = numericDrafts.get(field);
    if (!draft?.control.validity.valid) return;
    const {control, scale} = draft;
    numericDrafts.delete(field);
    control.dataset.initial = control.value;
    if (!edit(field, control.valueAsNumber / scale)) publish();
  }

  function commitNumericDraft(field: string) {
    // A pointer press can blur a number before Save, Discard or another control sees its click.
    if (pointerPress) heldDrafts.add(field);
    else applyNumericDraft(field);
  }

  function applyNumericDrafts() {
    for (const {control} of numericDrafts.values()) {
      if (!control.reportValidity()) return false;
    }
    for (const field of numericDrafts.keys()) applyNumericDraft(field);
    return true;
  }

  function syncNumericDrafts() {
    for (const [field, {control, scale}] of numericDrafts) {
      const value = draftValue<number>(field) * scale;
      const initial = scale === 1 ? value : Number(value.toFixed(6));
      if (control.valueAsNumber === initial) numericDrafts.delete(field);
    }
  }

  function command(action: string, payload: Payload = {}, onApplied?: () => void) {
    if ((action === 'try' || (action === 'save' && payload.group === 'layout')) && !applyNumericDrafts()) return;
    const replacesLayout = action === 'capture' || (action === 'inherit' && payload.group === 'layout') ||
      (action === 'discard' && (payload.group === 'layout' || payload.group === 'all'));
    const replaced = replacesLayout && numericDrafts.size ? new Map(numericDrafts) : undefined;
    enqueue({action, ...payload}, replaced, onApplied);
  }

  function enqueue(request: QueuedAction, replacedNumbers?: Map<string, NumericDraft>, onApplied?: () => void) {
    if (!state || closing || ended) return;
    const queued: PendingAction = {request, replacedNumbers, onApplied};
    if (paused && request.action === 'discard' && request.group === 'all') {
      queued.pending = queue.length;
      queue.unshift(queued);
      paused = false;
    } else {
      if (request.action !== 'set' && (paused || queue.some(item => item.request.action !== 'set'))) return;
      queue.push(queued);
    }
    if (request.action !== 'set') saveRequested = false;
    operationWarning = '';
    if (!paused) localError = '';
    if (request.action === 'doctor') healthStatus = 'Checking health…';
    void drain();
    publish();
  }

  function sessionFetch(path: string, method: string, payload?: unknown, signal?: AbortSignal) {
    const headers: Record<string, string> = {Authorization: `Bearer ${token}`};
    if (payload !== undefined) headers['Content-Type'] = 'application/json';
    return fetch(path, {method, headers, body: JSON.stringify(payload), cache: 'no-store', credentials: 'omit', redirect: 'error', signal});
  }

  async function api<T>(path: string, method = 'GET', payload?: unknown, signal?: AbortSignal): Promise<T> {
    if (!token) throw new Error('The session key is missing. Open the URL printed by milo web, including everything after #.');
    let response: Response;
    let data: unknown = null;
    try {
      response = await sessionFetch(path, method, payload, signal);
      if (response.status !== 204) {
        data = await response.json().catch(error => {
          if (response.ok) throw error;
          return null;
        });
      }
    } catch {
      if (!signal?.aborted) connected = false;
      throw new Error('The local editor did not answer. Reconnect, or run milo web again if the session has ended.');
    }
    if (!response.ok) {
      if (response.status === 401 || response.status === 403) connected = false;
      const error = data && typeof data === 'object' && 'error' in data && typeof data.error === 'string' ? data.error : '';
      throw new Error(error || `The local editor returned HTTP ${response.status}.`);
    }
    connected = true;
    return data as T;
  }

  async function refreshMenuIndicator(signal: AbortSignal) {
    const before = state;
    const idle = () => active && connected && !running && !paused && !closing && !ended && !queue.length;
    if (!before || !idle()) return;
    try {
      const response = await sessionFetch('/api/menu-indicator', 'GET', undefined, signal);
      if (!response.ok) return;
      const indicator = await response.json() as SessionState['menubar']['indicator'];
      // Only update the saved choice, never a draft or a later in-flight action.
      if (!signal.aborted && state === before && idle() && indicator !== before.menubar.indicator) {
        state = {...before, menubar: {...before.menubar, indicator}};
        publish();
      }
    } catch { /* A background read never interrupts the draft. */ }
  }

  function findTelegramChats(botToken: string, signal: AbortSignal) {
    return api<{chats: TelegramChat[]}>('/api/telegram-chats', 'POST', {token: botToken}, signal);
  }

  async function hostedConnect(request: HostedConnectRequest, signal: AbortSignal) {
    const before = state;
    const response = await sessionFetch('/api/hosted-connect', 'POST', request, signal);
    if (!response.ok) await response.body?.cancel();
    if (response.status === 409 && request.provider === 'telegram') {
      const refreshed = await api<SessionState>('/api/state', 'GET', undefined, signal).catch(() => null);
      if (refreshed && !signal.aborted && state === before) {
        state = refreshed;
        publish();
      }
    }
    return {status: response.status, data: response.ok ? await response.json() as unknown : null};
  }

  async function saveHostedConnection(request: HostedSaveRequest): Promise<HostedSaveResult> {
    if (!state || !connected || running || paused || closing || ended || queue.length) return {status: 409};
    running = true;
    localError = '';
    operationWarning = '';
    publish();
    try {
      const response = await sessionFetch('/api/hosted-connect', 'POST', {...request, action: 'save', version: state.version});
      const {status} = response;
      if (status === 200) {
        state = await response.json() as SessionState;
        if (state.message?.text) setToast(state.message.text, state.message.error);
        return {status};
      }
      let cleanupConfirmed: boolean | undefined;
      if (status === 410) {
        const data: unknown = await response.json().catch(() => null);
        cleanupConfirmed = !!data && typeof data === 'object' && 'cleanupConfirmed' in data && data.cleanupConfirmed === true;
      } else {
        await response.body?.cancel();
      }
      // Stale or failed writes may leave the editor's publicly visible state changed.
      if (status === 409 || status === 410 || status === 500) {
        try { state = await api<SessionState>('/api/state'); } catch { /* Keep the save status. */ }
      }
      return {status, cleanupConfirmed};
    } catch {
      return {status: 502};
    } finally {
      running = false;
      syncNumericDrafts();
      if (!paused) void drain();
      publish();
    }
  }

  async function readTransfer<T>(request: TransferRead): Promise<T> {
    try {
      return await api<T>('/api/transfer', 'POST', request);
    } finally {
      publish();
    }
  }

  async function writeTransfer(request: TransferWrite): Promise<string> {
    if (!state || !connected || running || paused || closing || ended || queue.length) {
      return 'The editor is busy. Wait for it to finish, then try again.';
    }
    running = true;
    localError = '';
    operationWarning = '';
    publish();
    try {
      state = await api<SessionState>('/api/transfer', 'POST', {...request, version: state.version});
      if (state.message?.error) operationWarning = state.message.text;
      else if (state.message?.text) setToast(state.message.text, false);
      return '';
    } catch (error) {
      // A refused import can follow a change made elsewhere; show what is saved now.
      try { state = await api<SessionState>('/api/state'); } catch { /* Keep the reason below. */ }
      return (error as Error).message;
    } finally {
      running = false;
      syncNumericDrafts();
      if (!paused) void drain();
      publish();
    }
  }

  async function keepDraftAlive() {
    if (running || closing || ended) return;
    try {
      // Renew the server's idle deadline without replacing the draft or changing connection status.
      const response = await fetch('/api/state', {
        headers: {Authorization: `Bearer ${token}`}, cache: 'no-store', credentials: 'omit', redirect: 'error',
      });
      await response.body?.cancel();
    } catch { /* Only foreground requests report connection errors. */ }
  }

  function setToast(text: string, error: boolean) {
    window.clearTimeout(toastTimer);
    toast = {text, error};
    toastTimer = window.setTimeout(() => {
      toast = null;
      publish();
    }, 3000);
  }

  function notify(text: string, error = false) {
    if (!text) return;
    setToast(text, error);
    publish();
  }

  async function drain() {
    if (!state || running || paused || !queue.length || closing || ended) return;
    running = true;
    while (queue.length) {
      const {request, pending, replacedNumbers, onApplied} = queue[0];
      try {
        state = await api<SessionState>('/api/action', 'POST', {...request, version: state.version});
        queue.splice(0, 1 + (pending ?? 0));
        if (replacedNumbers) {
          for (const [field, draft] of replacedNumbers) {
            if (numericDrafts.get(field) === draft) numericDrafts.delete(field);
          }
        }
        if (request.action === 'doctor' && state.doctor) {
          const {level, note} = state.doctor;
          healthStatus = `Health check: ${healthLabels[level]}.${note ? ` ${note}` : ''}`;
        } else if (!state.doctor && !queue.some(item => item.request.action === 'doctor')) {
          healthStatus = '';
        }
        if (state.message?.error) operationWarning = state.message.text;
        else if (state.message?.text) setToast(state.message.text, false);
      } catch (error) {
        saveRequested = false;
        localError = (error as Error).message;
        if (request.action === 'doctor') {
          state = {...state, doctor: null};
          healthStatus = '';
          localError = `Health check could not finish. ${localError}`;
        } else if (queue.some(item => item.request.action === 'doctor')) {
          healthStatus = 'Health check did not run because an edit was not applied.';
        }
        // A queued tool action must not run after an earlier edit failed.
        for (let i = queue.length - 1; i >= 0; i--) {
          if (queue[i].request.action !== 'set') queue.splice(i, 1);
        }
        paused = queue.length > 0;
        // A change can fail after part of it took effect, such as mirroring saved as on but not started.
        if (!paused && request.action === 'alerts' && request.field !== 'refresh') {
          queue.push({request: {action: 'alerts', field: 'refresh', value: null}});
        }
        setToast(localError, true);
        break;
      }
      if (!state.message?.error) onApplied?.();
    }
    const openSave = saveRequested;
    saveRequested = false;
    running = false;
    syncNumericDrafts();
    if (openSave) showSaveOptions();
    if (!paused) void drain();
    publish();
  }

  async function connect(retryEdits = false) {
    if (running || closing || ended) return;
    const focusId = document.activeElement?.id;
    if (focusId === 'retry' || focusId === 'reconnect') restoreSettingsFocus();
    else if (focusId === 'open-retry') byId('opening-message')?.focus();
    running = true;
    localError = '';
    canOpenRetry = false;
    openingMessage = openingText;
    publish();
    try {
      state = await api<SessionState>('/api/state');
      if (!queue.some(item => item.request.action === 'doctor')) healthStatus = '';
      operationWarning = state.message?.error ? state.message.text : '';
      if (!paused || retryEdits) paused = false;
      else localError = 'Reconnected. An edit is still waiting to be applied.';
      syncNumericDrafts();
      if (localError) setToast(localError, true);
      else if (state.message?.text && !operationWarning) setToast(state.message.text, false);
    } catch (error) {
      localError = (error as Error).message;
      openingMessage = localError;
      canOpenRetry = Boolean(token);
      setToast(localError, true);
    } finally {
      running = false;
      if (!paused) void drain();
      publish();
    }
  }

  function dropEdit() {
    if (!paused || running || closing) return;
    if (document.activeElement?.id === 'drop-edit') restoreSettingsFocus();
    queue.shift();
    paused = queue.length > 0;
    void connect();
  }

  function confirm(title: string, note: string, label: string, action: () => void) {
    const opener = document.activeElement;
    const focusId = opener?.id;
    const headingId = opener?.closest('.settings-group')?.getAttribute('aria-labelledby') ?? undefined;
    onConfirmed = confirmed => {
      restoreSettingsFocus(focusId ? byId(focusId) : null, headingId);
      if (confirmed) action();
    };
    // Numbers blurred by opening the dialog must remain local until the user decides.
    heldDrafts.clear();
    confirmation = {title, note, label};
    publish();
  }

  function finishConfirmation(confirmed: boolean) {
    const action = onConfirmed;
    onConfirmed = undefined;
    confirmation = null;
    action?.(confirmed);
    publish();
  }

  function discard(group: GroupKey | 'all') {
    const all = group === 'all';
    confirm(all ? 'Discard all unsaved changes?' : `Discard unsaved ${groupLabels[group]}?`,
      all ? 'This restores saved settings and undoes any active layout trial.' : 'This restores the last saved settings for this group.',
      'Discard changes', () => command('discard', {group}));
  }

  function showSaveOptions() {
    if (confirmation || !state) return;
    const latest = state;
    saveOptions = groupKeys.filter(key => canSaveDraft(latest, key));
  }

  function requestSave() {
    if (!canRequestSave() || !applyNumericDrafts()) return;
    if (queue.length) saveRequested = true;
    else showSaveOptions();
    publish();
  }

  function finishSave(saved: boolean) {
    saveOptions = [];
    if (!saved) restoreSettingsFocus(byId('save-draft'));
    publish();
  }

  async function closeSession(version: number) {
    if (running || closing || ended) return;
    closing = true;
    publish();
    try {
      await api<null>('/api/close', 'POST', {version});
      ended = true;
      state = undefined;
      queue.length = 0;
      numericDrafts.clear();
    } catch (error) {
      localError = (error as Error).message;
      setToast(localError, true);
    } finally {
      closing = false;
      publish();
    }
  }

  function done() {
    if (!state || running || closing) return;
    const {version} = state;
    const unsaved = hasUnsavedChanges();
    confirm('Close the dashboard?', unsaved
      ? 'Closing discards unsaved changes and ends any layout trial. Run milo web in your project to reopen the dashboard.'
      : 'This stops the local editor. Your saved settings stay as they are. Run milo web in your project to reopen the dashboard.',
    unsaved ? 'Discard and close' : 'Close dashboard', () => { void closeSession(version); });
  }

  function press(event: PointerEvent) {
    if (event.button === 0) pointerPress = true;
  }

  function release() {
    if (!pointerPress) return;
    window.clearTimeout(pointerTimer);
    // Run after the click that this release dispatches.
    pointerTimer = window.setTimeout(() => {
      pointerPress = false;
      for (const field of heldDrafts) applyNumericDraft(field);
      heldDrafts.clear();
    });
  }

  function start() {
    active = true;
    document.addEventListener('pointerdown', press, true);
    const releaseEvents = ['pointerup', 'pointercancel', 'contextmenu', 'blur'] as const;
    for (const type of releaseEvents) window.addEventListener(type, release);
    syncKeepalive();
    if (!started) {
      started = true;
      token = window.location.hash.slice(1);
      void connect();
    }
    return () => {
      active = false;
      document.removeEventListener('pointerdown', press, true);
      for (const type of releaseEvents) window.removeEventListener(type, release);
      window.removeEventListener('beforeunload', warnOnUnload);
      window.clearInterval(keepaliveTimer);
      keepaliveTimer = undefined;
      window.clearTimeout(pointerTimer);
      window.clearTimeout(toastTimer);
    };
  }

  function subscribe(listener: () => void) {
    listeners.add(listener);
    return () => { listeners.delete(listener); };
  }

  return {
    start, subscribe, getSnapshot: () => snapshot, getServerSnapshot: () => initialSnapshot,
    actions: {
      draftValue, edit, command, findTelegramChats, hostedConnect, saveHostedConnection, readTransfer, writeTransfer, confirm, discard, notify,
      refreshMenuIndicator, trackNumericDraft, commitNumericDraft,
      applyNumericDraft, applyNumericDrafts, hasNumericDraft: field => numericDrafts.has(field),
      connect, dropEdit, done, requestSave, finishSave, finishConfirmation,
    },
  };
}

type EditorSession = Snapshot & SessionActions;
type OpenSession = EditorSession & {state: SessionState};

const SessionContext = createContext<EditorSession | null>(null);

export function SessionProvider({children}: {children: ReactNode}) {
  const [editor] = useState(createEditor);
  const snapshot = useSyncExternalStore(editor.subscribe, editor.getSnapshot, editor.getServerSnapshot);
  const session = useMemo(() => ({...editor.actions, ...snapshot}), [editor, snapshot]);
  useEffect(editor.start, [editor]);
  return <SessionContext.Provider value={session}>{children}</SessionContext.Provider>;
}

export function useEditor() {
  const session = useContext(SessionContext);
  if (!session) throw new Error('Settings must be inside SessionProvider.');
  return session;
}

export function useSession(): OpenSession {
  const session = useEditor();
  if (!session.state) throw new Error('Settings are not open.');
  return session as OpenSession;
}
