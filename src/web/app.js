'use strict';

const byId = id => document.getElementById(id);
const token = location.hash.slice(1);
const roleNames = {outline: 'Outline', history: 'History', usage: 'Usage'};
const originLabels = {default: 'Built-in defaults', global: 'Global default', worktree: 'Worktree override', account: 'Account settings'};
const groupLabels = {layout: 'layout', preferences: 'pane preferences', account: 'Claude Code choices'};
const navButtons = [...document.querySelectorAll('[data-page]')];
const queue = [];
const numericDrafts = new Map();
const heldDrafts = new Set();
let pointerPress = false;
let saveRequested = false;
let page = 'layout';
let state;
let running = false;
let paused = false;
let closing = false;
let ended = false;
let connected = false;
let localError = '';
let operationWarning = '';
let dismissedReopen;
let confirmAction;
let warned = false;
const KEEPALIVE_INTERVAL_MS = 5 * 60 * 1000;
let keepaliveTimer;

function node(tag, className, text) {
  const el = document.createElement(tag);
  if (className) el.className = className;
  if (text != null) el.textContent = text;
  return el;
}

function button(text, style, id, click, disabled = false) {
  const el = node('button', `button ${style}`, text);
  el.type = 'button';
  el.id = id;
  el.disabled = disabled;
  el.addEventListener('click', click);
  return el;
}

function lookup(data, field) {
  return field.split('.').reduce((object, key) => object[key], data);
}

function draftValue(field) {
  const previous = queue.findLast(action => action.field === field);
  return previous ? previous.value : lookup(state, field);
}

function edit(field, value) {
  const current = draftValue(field);
  if (Object.is(current, value) || (Array.isArray(value) && Array.isArray(current) &&
    current.length === value.length && current.every((item, i) => item === value[i]))) return false;
  enqueue({action: 'set', field, value});
  return true;
}

function applyNumericDraft(field) {
  const draft = numericDrafts.get(field);
  if (!draft?.control.validity.valid) return;
  const {control, scale} = draft;
  numericDrafts.delete(field);
  control.dataset.initial = control.value;
  if (!edit(field, control.valueAsNumber / scale)) updateChrome();
}

function applyNumericDrafts() {
  for (const {control} of numericDrafts.values()) {
    if (!control.reportValidity()) return false;
  }
  for (const field of numericDrafts.keys()) applyNumericDraft(field);
  return true;
}

function command(action, payload = {}) {
  if ((action === 'try' || (action === 'save' && payload.group === 'layout')) && !applyNumericDrafts()) return;
  const replacesLayout = action === 'capture' || (action === 'inherit' && payload.group === 'layout') ||
    (action === 'discard' && (payload.group === 'layout' || payload.group === 'all'));
  const replacedNumbers = replacesLayout && numericDrafts.size ? new Map(numericDrafts) : undefined;
  enqueue({action, ...payload, replacedNumbers});
}

function enqueue(action) {
  if (!state || closing || ended) return;
  if (paused && action.action === 'discard' && action.group === 'all') {
    action.pending = queue.length;
    queue.unshift(action);
    paused = false;
  } else {
    if (action.action !== 'set' && (paused || queue.some(item => item.action !== 'set'))) return;
    queue.push(action);
  }
  if (action.action !== 'set') saveRequested = false;
  operationWarning = '';
  byId('operation-status').textContent = '';
  if (!paused) localError = '';
  const checking = action.action === 'doctor';
  if (checking) byId('health-status').textContent = 'Checking health…';
  void drain();
  if (checking) render();
  else updateChrome();
}

async function api(path, method = 'GET', payload) {
  if (!token) throw new Error('The session key is missing. Open the URL printed by milo web, including everything after #.');
  const headers = {Authorization: `Bearer ${token}`};
  if (payload !== undefined) headers['Content-Type'] = 'application/json';
  let response;
  let data = null;
  try {
    response = await fetch(path, {method, headers, body: JSON.stringify(payload), cache: 'no-store', credentials: 'omit', redirect: 'error'});
    if (response.status !== 204) {
      data = await response.json().catch(error => {
        if (response.ok) throw error;
        return null;
      });
    }
  } catch {
    connected = false;
    throw new Error('The local editor did not answer. Reconnect, or run milo web again if the session has ended.');
  }
  if (!response.ok) {
    if (response.status === 401 || response.status === 403) connected = false;
    throw new Error(data?.error || `The local editor returned HTTP ${response.status}.`);
  }
  connected = true;
  return data;
}

let toastTimer;
function notify(text, error = false) {
  if (!text) return;
  clearTimeout(toastTimer);
  const toast = byId(error ? 'alert-toast' : 'message');
  byId(error ? 'message' : 'alert-toast').textContent = '';
  toast.textContent = text;
  toastTimer = setTimeout(() => { toast.textContent = ''; }, 3000);
}

async function drain() {
  if (running || paused || !queue.length || closing || ended) return;
  running = true;
  while (queue.length) {
    const {pending, replacedNumbers, ...action} = queue[0];
    try {
      state = await api('/api/action', 'POST', {...action, version: state.version});
      if (state.message?.error) {
        operationWarning = state.message.text;
        byId('operation-status').textContent = operationWarning;
      } else if (state.message) notify(state.message.text);
      queue.splice(0, 1 + (pending ?? 0));
      if (replacedNumbers) {
        for (const [field, draft] of replacedNumbers) {
          if (numericDrafts.get(field) === draft) numericDrafts.delete(field);
        }
      }
      if (action.action === 'doctor') {
        const {level, note} = state.doctor;
        byId('health-status').textContent = `Health check: ${healthLabels[level]}.${note ? ` ${note}` : ''}`;
      } else if (!state.doctor && !queue.some(item => item.action === 'doctor')) {
        byId('health-status').textContent = '';
      }
    } catch (error) {
      saveRequested = false;
      localError = error.message;
      if (action.action === 'doctor') {
        state.doctor = null;
        byId('health-status').textContent = '';
        localError = `Health check could not finish. ${localError}`;
      } else if (queue.some(item => item.action === 'doctor')) {
        byId('health-status').textContent = 'Health check did not run because an edit was not applied.';
      }
      notify(localError, true);
      if (action.action !== 'set') queue.shift();
      // Do not run a queued save or capture after an earlier edit failed.
      for (let i = queue.length - 1; i >= 0; i--) {
        if (queue[i].action !== 'set') queue.splice(i, 1);
      }
      paused = queue.length > 0;
      // A change can fail after part of it took effect, such as mirroring saved as on but not started.
      if (!paused && action.action === 'alerts' && action.field !== 'refresh') {
        queue.push({action: 'alerts', field: 'refresh', value: null});
      }
      break;
    }
  }
  const openSave = saveRequested;
  saveRequested = false;
  running = false;
  render();
  if (openSave) showSaveOptions();
  if (!paused) void drain();
}

async function connect(retryEdits = false) {
  if (running || closing || ended) return;
  const openingMessage = byId('opening-message');
  const focusId = document.activeElement?.id;
  if (focusId === 'retry' || focusId === 'reconnect') {
    restoreSettingsFocus();
  } else if (focusId === 'open-retry') {
    openingMessage.tabIndex = -1;
    openingMessage.focus();
  }
  let focusOpenedSettings = false;
  running = true;
  localError = '';
  if (state) updateChrome();
  byId('open-retry').hidden = true;
  openingMessage.textContent = 'Opening your local settings…';
  try {
    const latest = await api('/api/state');
    state = latest;
    if (!queue.some(item => item.action === 'doctor')) byId('health-status').textContent = '';
    operationWarning = latest.message?.error ? latest.message.text : '';
    if (!paused || retryEdits) {
      paused = false;
    } else {
      localError = 'Reconnected. An edit is still waiting to be applied.';
    }
    byId('editor').hidden = byId('actionbar').hidden = false;
    byId('operation-status').textContent = operationWarning;
    focusOpenedSettings = document.activeElement === openingMessage;
    byId('opening').hidden = true;
    if (localError) notify(localError, true);
    else if (latest.message && !operationWarning) notify(latest.message.text);
  } catch (error) {
    localError = error.message;
    notify(localError, true);
    openingMessage.textContent = error.message;
    byId('open-retry').hidden = !token;
  } finally {
    running = false;
    if (state) {
      render();
      if (focusOpenedSettings) restoreSettingsFocus();
    } else renderConnection();
  }
  if (!paused) void drain();
}

function confirm(title, note, label, action) {
  const dialog = byId('confirm');
  byId('confirm-title').textContent = title;
  byId('confirm-note').textContent = note;
  byId('confirm-do').textContent = label;
  const opener = document.activeElement;
  const focusId = opener?.id;
  const headingId = opener?.closest('.settings-group')?.getAttribute('aria-labelledby');
  confirmAction = confirmed => {
    restoreSettingsFocus(focusId && byId(focusId), headingId);
    if (confirmed) action();
  };
  dialog.returnValue = '';
  dialog.showModal();
  // Keep numbers blurred by opening the dialog local until the user decides.
  heldDrafts.clear();
}

function discard(group) {
  const all = group === 'all';
  confirm(all ? 'Discard all unsaved changes?' : `Discard unsaved ${groupLabels[group]}?`,
    all ? 'This restores saved settings and undoes any active layout trial.' : 'This restores the last saved settings for this group.',
    'Discard changes', () => command('discard', {group}));
}

async function closeSession(version) {
  if (running || closing || ended) return;
  closing = true;
  updateChrome();
  try {
    await api('/api/close', 'POST', {version});
    ended = true;
    state = undefined;
    queue.length = 0;
    numericDrafts.clear();
    byId('editor').hidden = byId('actionbar').hidden = true;
    byId('closed').hidden = false;
    byId('closed').querySelector('h1').tabIndex = -1;
    byId('closed').querySelector('h1').focus();
  } catch (error) {
    localError = error.message;
    notify(localError, true);
  } finally {
    closing = false;
    updateChrome();
  }
}

function done() {
  if (!state || running || closing) return;
  const version = state.version;
  const unsaved = hasUnsavedChanges();
  confirm('Close the dashboard?', unsaved
    ? 'Closing discards unsaved changes and ends any layout trial. Run milo web in your project to reopen the dashboard.'
    : 'This stops the local editor. Your saved settings stay as they are. Run milo web in your project to reopen the dashboard.',
    unsaved ? 'Discard and close' : 'Close dashboard', () => closeSession(version));
}

function warnOnUnload(event) {
  event.preventDefault();
  event.returnValue = '';
}

async function keepDraftAlive() {
  if (running || closing || ended) return;
  try {
    // Renew the idle deadline without replacing the draft or changing connection status.
    const response = await fetch('/api/state', {
      headers: {Authorization: `Bearer ${token}`},
      cache: 'no-store',
      credentials: 'omit',
      redirect: 'error',
    });
    await response.body?.cancel();
  } catch { /* Only foreground requests report connection errors. */ }
}

function renderConnection() {
  const connection = byId('connection');
  const status = ended ? 'closed' : connected ? 'ready' : localError ? 'offline' : 'opening';
  connection.dataset.state = status;
  connection.textContent = {closed: 'Closed', ready: 'Local session', offline: 'Not connected', opening: 'Connecting'}[status];
}

function affectsDraft({action}) {
  return action !== 'doctor' && action !== 'menubar' && action !== 'alerts' && action !== 'presence';
}

function hasUnsavedChanges() {
  return state.dirty || state.trial || numericDrafts.size > 0 || queue.some(affectsDraft);
}

function updateChrome() {
  renderConnection();
  const needsWarning = Boolean(state && hasUnsavedChanges());
  if (needsWarning !== warned) {
    if (needsWarning) {
      window.addEventListener('beforeunload', warnOnUnload);
      keepaliveTimer = setInterval(keepDraftAlive, KEEPALIVE_INTERVAL_MS);
    } else {
      window.removeEventListener('beforeunload', warnOnUnload);
      clearInterval(keepaliveTimer);
    }
    warned = needsWarning;
  }
  if (!state) return;
  const reopen = state.reopen_panes;
  if (reopen && (numericDrafts.size || queue.some(item => item.action === 'set'))) dismissedReopen = reopen.version;
  const reopenNote = byId('reopen-panes');
  const instruction = reopen && reopen.version !== dismissedReopen ? reopen.text : '';
  reopenNote.hidden = !instruction;
  if (reopenNote.textContent !== instruction) reopenNote.textContent = instruction;
  const error = localError || operationWarning;
  byId('error-box').hidden = !error;
  if (byId('error').textContent !== error) byId('error').textContent = error;
  byId('error-actions').hidden = !paused && !localError;
  byId('retry').hidden = byId('drop-edit').hidden = !paused;
  byId('reconnect').hidden = !localError || paused;
  byId('reconnect').textContent = connected ? 'Refresh editor state' : 'Reconnect';
  byId('retry').disabled = byId('drop-edit').disabled = byId('reconnect').disabled = running || closing;
  const dirty = Object.entries(state.groups).filter(([key, group]) =>
    group.dirty || (key === 'layout' && numericDrafts.size)).length;
  const status = byId('draft-status');
  status.dataset.dirty = String(needsWarning);
  const updatingDraft = queue.some(affectsDraft);
  const summary = closing ? 'Closing settings…' : paused ? 'Edit not applied' : updatingDraft ? 'Updating draft…' :
    dirty ? `Unsaved changes in ${dirty === 1 ? '1 group' : `${dirty} groups`}` : state.trial ? 'Layout trial active' : 'No unsaved changes';
  if (status.textContent !== summary) status.textContent = summary;
  const discard = byId('discard-all');
  const save = byId('save-draft');
  const canDiscard = needsWarning && !running && !closing && connected;
  const canSave = canRequestSave();
  if ((!canDiscard && document.activeElement === discard) || (!canSave && document.activeElement === save)) {
    // The current section stays focusable while a draft action is pending or no longer needed.
    navButtons.find(tab => tab.dataset.page === page)?.focus({preventScroll: true});
  }
  discard.disabled = !canDiscard;
  save.disabled = !canSave;
  save.classList.toggle('primary', canSave);
  save.classList.toggle('secondary', !canSave);
  byId('done').disabled = running || closing;
  const menuAction = byId('menubar-action');
  if (menuAction) {
    const operation = queue.find(item => item.action === 'menubar');
    const pending = !operation ? '' : operation.value === 'uninstall' ? 'Removing menu app…' :
      state.menubar.installed ? 'Opening or updating menu app…' : 'Installing menu app…';
    const live = byId('menubar-status');
    if (live.textContent !== pending) live.textContent = pending;
    const unavailable = !connected || paused || closing || (running && !queue.length) ||
      queue.some(item => item.action !== 'set');
    // Keep the focused action focusable while the installer runs.
    for (const control of [menuAction, byId('menubar-remove')]) {
      if (unavailable) control?.setAttribute('aria-disabled', 'true');
      else control?.removeAttribute('aria-disabled');
    }
  }
}

function restoreSettingsFocus(target, headingId) {
  if (target) {
    if (target.matches('h2')) target.tabIndex = -1;
    target.focus();
    if (document.activeElement === target) return;
  }
  const heading = (headingId && byId(headingId)) || byId('settings-panel').querySelector('h2');
  if (heading) {
    heading.tabIndex = -1;
    heading.focus();
  }
}

function render() {
  const focus = document.activeElement;
  const trigger = focus?.closest('.action-menu')?.firstElementChild;
  const focusId = trigger?.id || focus?.id;
  const headingId = trigger?.closest('.settings-group')?.getAttribute('aria-labelledby');
  const locationLabel = node('strong', '', state.context.inside_rex ? 'In Rex' : 'Outside Rex');
  const path = state.context.worktree || state.context.directory;
  const name = path?.split('/').filter(Boolean).at(-1) || path || 'Directory unavailable';
  const worktree = node('span', 'context-worktree', name);
  const context = byId('context');
  context.replaceChildren(locationLabel, worktree);
  if (path) {
    worktree.title = path;
    worktree.setAttribute('aria-hidden', 'true');
    context.append(node('span', 'sr-only', `${state.context.worktree ? 'Git worktree' : 'Directory'}: ${path}`));
  }
  const hasMenuBar = state.menubar.installed !== null;
  if ((page === 'menubar' && !hasMenuBar) || (page === 'alerts' && !state.alerts)) page = 'layout';
  for (const tab of navButtons) {
    const name = tab.dataset.page;
    if (name === 'menubar') tab.hidden = !hasMenuBar;
    if (name === 'alerts') tab.hidden = !state.alerts;
    if (name === page) tab.setAttribute('aria-current', 'page');
    else tab.removeAttribute('aria-current');
    const dirty = name === 'layout' ? state.groups.layout.dirty :
      name === 'appearance' ? state.groups.preferences.dirty :
      name === 'workflow' && (state.groups.preferences.dirty || state.groups.account.dirty);
    const dot = tab.querySelector('.nav-draft');
    dot.replaceChildren(node('span', 'sr-only', ' (unsaved changes)'));
    dot.hidden = !dirty;
  }
  const views = {layout: layoutView, appearance: appearanceView, workflow: workflowView, alerts: alertsView,
    menubar: menuBarView, health: healthView};
  const panel = byId('settings-panel');
  const openMenu = panel.querySelector('.action-menu[open] > summary')?.id;
  panel.replaceChildren(...views[page]());
  if (openMenu && byId(openMenu)) byId(openMenu).parentElement.open = true;
  panel.setAttribute('aria-labelledby', panel.querySelector('h2').id);
  updateChrome();
  if (focusId && (!focus.isConnected || (focus.type === 'number' && focus !== document.activeElement))) {
    const replacement = byId(focusId) || (focusId === 'menubar-remove' && byId('menubar-action'));
    if (trigger || focus.matches('h2[tabindex="-1"]')) {
      restoreSettingsFocus(replacement, headingId);
    } else if (replacement && !replacement.matches(':disabled, [hidden]')) {
      replacement.focus({preventScroll: true});
    } else {
      panel.focus({preventScroll: true});
    }
  }
}

function setting(field, title, detail, type, options = {}) {
  const row = node('div', 'setting');
  const info = node('div', 'setting-info');
  const label = node('label', 'setting-label', title);
  const explanation = node('span', 'setting-detail', detail);
  const wrap = node('div', 'setting-input');
  if (type === 'select') {
    const value = draftValue(field);
    const choices = node('fieldset', 'choice-strip');
    choices.disabled = Boolean(options.disabled);
    choices.append(node('legend', 'sr-only', title));
    for (const [key, text] of options.choices) {
      const choice = node('label');
      const radio = node('input');
      radio.type = 'radio';
      radio.name = field;
      radio.id = `field-${field}-${key}`;
      radio.checked = key === String(value);
      radio.addEventListener('change', () =>
        (options.change || edit)(field, typeof value === 'boolean' ? key === 'true' : key));
      const caption = node('span', '', text);
      if (options.examples) {
        const sample = node('code', 'git-example', options.examples[key]);
        sample.setAttribute('aria-hidden', 'true');
        caption.append(sample);
      }
      choice.append(radio, caption);
      choices.append(choice);
    }
    info.append(node('span', 'setting-label', title));
    if (detail) {
      explanation.id = `${field}-detail`;
      choices.setAttribute('aria-describedby', explanation.id);
      info.append(explanation);
    }
    row.append(info, choices);
    return row;
  }
  const draft = type === 'number' && numericDrafts.get(field);
  const control = draft ? draft.control : node('input');
  const id = `field-${field}`;
  control.id = label.htmlFor = id;
  explanation.id = `${id}-detail`;
  info.append(label);
  if (detail) info.append(explanation);
  control.disabled = Boolean(options.disabled);
  if (type === 'switch') {
    if (detail) control.setAttribute('aria-describedby', explanation.id);
    control.type = 'checkbox';
    control.className = 'switch';
    control.setAttribute('role', 'switch');
    control.checked = draftValue(field);
    control.addEventListener('change', () => (options.change || edit)(field, control.checked));
  } else {
    const error = draft?.error || node('span');
    info.append(error);
    const value = draftValue(field);
    const initial = options.unit ? String(Number((value * 100).toFixed(6))) : String(value);
    control.dataset.initial = initial;
    if (draft) {
      if (control.valueAsNumber === Number(initial)) numericDrafts.delete(field);
    } else {
      control.type = 'number';
      control.min = options.min;
      control.max = options.max;
      control.step = options.step;
      control.required = true;
      error.id = `${id}-error`;
      error.setAttribute('aria-live', 'polite');
      control.setAttribute('aria-describedby', detail ? `${explanation.id} ${error.id}` : error.id);
      control.value = initial;
      const scale = options.unit ? 100 : 1;
      const showValidation = () => {
        const message = control.validationMessage;
        if (error.textContent === message) return;
        error.textContent = message;
        error.classList.toggle('setting-detail', Boolean(message));
        if (message) control.setAttribute('aria-invalid', 'true');
        else control.removeAttribute('aria-invalid');
      };
      control.addEventListener('blur', showValidation);
      control.addEventListener('invalid', showValidation);
      control.addEventListener('input', () => {
        if (control.validity.valid) showValidation();
        if (control.valueAsNumber !== Number(control.dataset.initial)) {
          // Keep the actual input: a number field may display incomplete text that .value cannot represent.
          numericDrafts.set(field, {control, scale, error});
        } else {
          numericDrafts.delete(field);
        }
        saveRequested = false;
        updateChrome();
      });
      control.addEventListener('change', () => {
        // Let the pressed control act before submitting a draft blurred by this press.
        if (pointerPress) heldDrafts.add(field);
        else applyNumericDraft(field);
      });
      control.addEventListener('keydown', event => {
        if (event.key === 'Enter' && !event.isComposing) {
          event.preventDefault();
          if (control.reportValidity()) applyNumericDraft(field);
        }
      });
    }
  }
  wrap.append(control);
  if (type === 'switch') {
    const track = node('span', 'switch-track');
    track.setAttribute('aria-hidden', 'true');
    wrap.append(track);
  }
  if (options.unit) wrap.append(node('span', 'unit', '%'));
  row.append(info, wrap);
  return row;
}

function groupCard(key, title, description, controls) {
  const group = state.groups[key];
  const card = node('section', 'settings-group');
  const header = node('header', 'group-header');
  const titleline = node('div', 'group-titleline');
  const heading = node('h2', '', title);
  heading.id = `group-${key}`;
  card.setAttribute('aria-labelledby', heading.id);
  titleline.append(heading, scopeActions(key, group));
  header.append(titleline);
  if (description) header.append(node('p', '', description));
  const list = node('div', 'group-controls');
  list.append(...controls);
  card.append(header, list);
  if (!group.editable) card.append(node('p', 'setting-note', 'Read-only in this session.'));
  return card;
}

function canWriteGroup(group) {
  return group.save_global || group.save_worktree || group.save_account;
}

function canSaveDraft(key, group) {
  return (group.dirty || (key === 'layout' && state.trial)) && canWriteGroup(group);
}

function canRequestSave() {
  if (!connected || closing || paused || saveRequested || (running && !queue.length) ||
      queue.some(item => item.action !== 'set')) return false;
  for (const {control} of numericDrafts.values()) {
    if (!control.validity.valid) return false;
  }
  return (numericDrafts.size > 0 && canWriteGroup(state.groups.layout)) ||
    Object.entries(state.groups).some(([key, group]) => canSaveDraft(key, group)) ||
    queue.some(({field}) => {
      const section = field.split('.')[0];
      const group = section === 'layout' || section === 'account' ? section : 'preferences';
      return canWriteGroup(state.groups[group]);
    });
}

function requestSave() {
  if (!canRequestSave() || !applyNumericDrafts()) return;
  if (queue.length) {
    saveRequested = true;
    updateChrome();
  } else showSaveOptions();
}

function saveChoices(key, group, select, disabled = false) {
  const choices = node('div', 'button-row');
  for (const [scope, label] of [['worktree', 'Save for this worktree'], ['global', 'Save as global default'], ['account', 'Save Claude Code choices']]) {
    if (group[`save_${scope}`]) choices.append(button(label, choices.childElementCount ? 'secondary' : 'primary',
      `${select}-${key}-${scope}`, () => {
        if (select === 'dock') byId('save-options').close('save');
        command('save', {group: key, scope});
      }, disabled));
  }
  return choices;
}

function showSaveOptions() {
  if (byId('confirm').open) return;
  const groups = byId('save-groups');
  groups.replaceChildren();
  for (const [key, group] of Object.entries(state.groups)) {
    if (!canSaveDraft(key, group)) continue;
    const title = key === 'preferences' ? 'Appearance and workflow' : groupLabels[key];
    const section = node('div');
    const heading = node('h3', '', title[0].toUpperCase() + title.slice(1));
    heading.id = `save-group-${key}`;
    section.setAttribute('role', 'group');
    section.setAttribute('aria-labelledby', heading.id);
    section.append(heading, saveChoices(key, group, 'dock'));
    groups.append(section);
  }
  if (groups.childElementCount) {
    const dialog = byId('save-options');
    dialog.returnValue = '';
    dialog.showModal();
  }
}

function disclosure(id, label, accessibleName, items) {
  const details = node('details', 'action-menu');
  details.name = 'editor-actions';
  const trigger = node('summary');
  trigger.id = id;
  trigger.append(node('span', '', label));
  trigger.setAttribute('aria-label', accessibleName);
  const panel = node('div', 'action-menu-items');
  panel.append(...items);
  panel.addEventListener('click', event => {
    if (!event.target.closest('button:not(:disabled)')) return;
    // Return focus before the action can open a dialog or replace this menu.
    trigger.focus();
    details.open = false;
  }, {capture: true});
  details.append(trigger, panel);
  return details;
}

document.addEventListener('pointerdown', event => {
  const menu = document.querySelector('.action-menu[open]');
  if (menu && !menu.contains(event.target)) menu.open = false;
});
document.addEventListener('keydown', event => {
  if (event.key !== 'Escape' || event.defaultPrevented || document.querySelector('dialog[open]')) return;
  const menu = document.querySelector('.action-menu[open]');
  if (!menu) return;
  if (menu.contains(document.activeElement)) menu.firstElementChild.focus();
  menu.open = false;
  event.preventDefault();
});

function scopeActions(key, group) {
  const row = node('div', 'scope-actions');
  const label = originLabels[group.scope];
  row.append(group.inherit ? disclosure(`scope-${key}`, `${label} ▾`, `${label} options`, [
    button('Remove override…', 'text-button', `inherit-${key}`, () => {
      const dirty = group.dirty || (key === 'layout' && numericDrafts.size > 0);
      confirm('Remove worktree override?', `This worktree will use the global ${groupLabels[key]}${dirty ? ' and lose unsaved changes in this group' : ''}.`,
        'Remove override', () => command('inherit', {group: key}));
    }, paused),
  ]) : node('span', 'group-origin', label));
  if (key === 'layout' && state.context.inside_rex && !state.context.installed && (group.save_global || group.save_worktree)) {
    row.append(disclosure('setup-layout', 'Set up in Rex…', 'Set up in Rex', [saveChoices(key, group, 'install', paused)]));
  }
  if (key === 'layout') {
    const actions = layoutActions();
    if (actions.length) row.append(disclosure('layout-actions', '•••', 'Layout actions', actions));
  }
  return row;
}

function panePicker(editable) {
  const picker = node('fieldset', 'pane-picker');
  picker.disabled = !editable;
  const legend = node('legend', 'sr-only', 'Panes in rail order');
  const list = node('ol', 'pane-list');
  draftValue('layout.order').forEach((role, index, order) => {
    const line = node('li', 'pane-line');
    const label = node('label');
    const input = node('input');
    input.type = 'checkbox';
    input.id = `enable-${role}`;
    input.checked = draftValue(`layout.panes.${role}`);
    input.addEventListener('change', () => edit(`layout.panes.${role}`, input.checked));
    label.append(input, document.createTextNode(roleNames[role]));
    const positions = node('div', 'button-row');
    for (const [delta, arrow, direction] of [[-1, '←', 'up'], [1, '→', 'down']]) {
      const to = index + delta;
      const move = button(arrow, 'secondary', `move-${role}-${direction}`, () => {
        const next = [...draftValue('layout.order')];
        const from = next.indexOf(role);
        const destination = from + delta;
        if (destination < 0 || destination >= next.length) return;
        [next[from], next[destination]] = [next[destination], next[from]];
        edit('layout.order', next);
      }, to < 0 || to >= order.length);
      move.setAttribute('aria-label', `Move ${roleNames[role]} ${delta < 0 ? 'earlier' : 'later'}`);
      positions.append(move);
    }
    line.append(label, positions);
    list.append(line);
  });
  picker.append(legend, list);
  return picker;
}

function layoutView() {
  const {groups, capabilities} = state;
  const disabled = !groups.layout.editable;
  const options = {disabled, min: 0.000001, max: 99.999999, step: 'any', unit: '%'};
  const controls = [panePicker(!disabled),
    setting('layout.side', 'Rail side', '', 'select', {disabled, choices: [['left', 'Left'], ['right', 'Right']]}),
    setting('layout.width', 'Rail width', 'Percentage of the tab.', 'number', options),
    setting('layout.usage_rows', 'Usage height', 'Rows. Changing this restores automatic pane heights.', 'number',
      {disabled, min: 1, max: 4294967295, step: 1}),
  ];
  if (capabilities.auto_arrange) {
    controls.push(setting('layout.auto_arrange', 'Arrange new shell tabs', 'This switch is global. New command tabs and tabs with an existing layout are skipped.', 'switch'));
  }
  return [groupCard('layout', 'Panes', '', controls)];
}

const usageTools = [['codex', 'Codex'], ['claude', 'Claude Code'], ['opencode', 'OpenCode']];
const usageChoices = [['auto', 'Auto'], ['always', 'Always show'], ['hide', 'Hide']];
const usageDetails = {
  auto: 'Shown while it has a figure saved on this Mac.',
  always: 'Shown even when nothing is known about it.',
  hide: 'Never shown, and never looked for.',
};
// Accounts are asked about over the network, so each starts hidden.
const usageAccounts = [['deepseek', 'DeepSeek balance'], ['openrouter', 'OpenRouter spend'],
  ['devin', 'Devin ACUs'], ['copilot', 'Copilot AI credits']];
const accountDetails = {
  auto: 'Asks the provider with the key you set for Milo. Shown once it answers. Not a limit.',
  always: 'Asks the provider, and says why when there is no figure. Not a limit.',
  hide: 'Never shown, and the provider is never contacted.',
};

function appearanceView() {
  const disabled = !state.groups.preferences.editable;
  return [groupCard('preferences', 'Pane appearance', '', [
    setting('appearance.icons', 'Icons', '', 'select',
      {disabled, choices: [['nerd', 'Nerd Font'], ['plain', 'Plain'], ['off', 'Off']]}),
    setting('appearance.static_marks', 'Activity marks', '', 'select',
      {disabled, choices: [['false', 'Pulse'], ['true', 'Static']]}),
    setting('appearance.text_git_marks', 'Git status marks', '', 'select',
      {disabled, choices: [['false', 'Style only'], ['true', 'Text']], examples: {false: 'main.rs', true: 'M main.rs'}}),
    setting('appearance.usage_large_figures', 'Usage display', 'Auto fits the available space.', 'select',
      {disabled, choices: [['false', 'Auto compact'], ['true', 'Large figures']]}),
    ...usageTools.map(([key, name]) => setting(`appearance.usage_${key}`, `${name} usage`,
      usageDetails[draftValue(`appearance.usage_${key}`)], 'select', {disabled, choices: usageChoices})),
    ...usageAccounts.map(([key, name]) => setting(`appearance.usage_${key}`, name,
      accountDetails[draftValue(`appearance.usage_${key}`)], 'select', {disabled, choices: usageChoices})),
  ])];
}

function workflowView() {
  const disabled = !state.groups.preferences.editable;
  const account = state.groups.account;
  return [groupCard('preferences', 'Pane workflow', '', [
    setting('workflow.project_mode', 'Project tracking', '', 'select',
      {disabled, choices: [['follow', 'Follow shell'], ['pin', 'Pin project']]}),
    setting('workflow.follow_activity', 'Follow file edits', 'Temporarily reveal active files.', 'switch', {disabled}),
    setting('workflow.changes_only', 'Outline starts with', '', 'select',
      {disabled, choices: [['false', 'All files'], ['true', 'Changes only']]}),
    setting('workflow.history_follow_new', 'Follow new commits', 'Only while History is at the top.', 'switch', {disabled}),
    setting('workflow.tab_titles', 'Title Rex tabs', 'Unnamed tabs only: project, worktree and agent.', 'switch', {disabled}),
  ]), groupCard('account', 'Claude Code', '', [
    setting('account.claude_limits', 'Show Claude Code limits', state.account.claude_status, 'switch', {disabled: !account.editable}),
  ]), ...presenceView()];
}

// What was typed for an application ID, kept while the page redraws around it.
let applicationDraft;
const presenceDetails = [['generic', 'Generic'], ['session', 'Session name'], ['project', 'Project name'], ['both', 'Both names']];
const presenceShows = {
  generic: 'Only the fixed line. No names.',
  session: 'Also the name of the Rex session in front.',
  project: 'Also the folder name of the Git project in the focused pane.',
  both: 'Also the session name and the project’s folder name.',
};
// Whether a choice shows a name that the saved one does not, which is asked about first.
const showsMore = (next, saved) =>
  (['session', 'both'].includes(next) && !['session', 'both'].includes(saved)) ||
  (['project', 'both'].includes(next) && !['project', 'both'].includes(saved));

function presenceView() {
  const presence = state.presence;
  if (!presence) return [];
  const waiting = paused || queue.some(item => item.action !== 'set');
  const card = node('section', 'settings-group');
  const header = node('header', 'group-header');
  const titleline = node('div', 'group-titleline');
  const heading = node('h2', '', 'Discord presence');
  heading.id = 'group-presence';
  card.setAttribute('aria-labelledby', heading.id);
  const current = node('span', 'group-origin', presence.enabled ? 'On' : 'Off');
  current.id = 'presence-state';
  titleline.append(heading, current);
  const disclosure = node('p', '', presence.public[presence.details]);
  disclosure.id = 'presence-public';
  header.append(titleline, disclosure);

  const details = node('div', 'setting');
  const detailsInfo = node('div', 'setting-info');
  const shows = node('span', 'setting-detail', presenceShows[presence.details]);
  shows.id = 'presence-details-detail';
  detailsInfo.append(node('span', 'setting-label', 'Shows'), shows);
  const choices = node('fieldset', 'choice-strip');
  choices.disabled = waiting;
  choices.setAttribute('aria-describedby', shows.id);
  choices.append(node('legend', 'sr-only', 'What Discord presence shows'));
  for (const [key, text] of presenceDetails) {
    const choice = node('label');
    const radio = node('input');
    radio.type = 'radio';
    radio.name = 'presence-details';
    radio.id = `presence-details-${key}`;
    radio.checked = key === presence.details;
    radio.addEventListener('change', () => {
      const choose = () => command('presence', {field: 'details', value: key});
      if (presence.enabled && showsMore(key, presence.details)) {
        confirm('Show these names on Discord?', presence.public[key], 'Show names', choose);
        // Until the answer, the page shows the saved choice.
        render();
      } else choose();
    });
    choice.append(radio, node('span', '', text));
    choices.append(choice);
  }
  details.append(detailsInfo, choices);

  const application = node('div', 'setting');
  const info = node('div', 'setting-info');
  const own = presence.application_id;
  const help = node('span', 'setting-detail', own ? `Your own application, ${own}.` : presence.help);
  help.id = 'presence-application-detail';
  const wrap = node('div', 'setting-input');
  if (own) {
    info.append(node('span', 'setting-label', 'Shown under'), help);
    const reset = button('Use Milo’s application', 'secondary', 'presence-official',
      () => command('presence', {field: 'application', value: 'official'}), waiting);
    reset.setAttribute('aria-describedby', help.id);
    wrap.append(reset);
  } else {
    const label = node('label', 'setting-label', 'Shown under Milo’s application');
    const input = node('input');
    input.id = label.htmlFor = 'presence-application';
    input.type = 'text';
    input.inputMode = 'numeric';
    input.autocomplete = 'off';
    input.spellcheck = false;
    input.placeholder = 'Your Application ID';
    input.value = applicationDraft ?? '';
    input.setAttribute('aria-describedby', help.id);
    input.addEventListener('input', () => { applicationDraft = input.value.trim(); });
    info.append(label, help);
    const use = button('Use my own', 'secondary', 'presence-own', () => {
      const id = (applicationDraft ?? '').trim();
      if (!/^[1-9]\d{16,18}$/.test(id)) {
        notify('Enter the Application ID of your own Discord application: a number of 17 to 19 digits.', true);
        byId('presence-application').focus();
        return;
      }
      applicationDraft = undefined;
      command('presence', {field: 'application', value: id});
    }, waiting);
    wrap.append(input, use);
  }
  application.append(info, wrap);

  const now = node('div', 'setting');
  const status = node('span', 'setting-detail', presence.status);
  status.id = 'presence-status';
  const nowInfo = node('div', 'setting-info');
  nowInfo.append(node('span', 'setting-label', 'Now'), status);
  const change = presence.enabled
    ? button('Turn off', 'secondary', 'presence-change', () => command('presence', {value: 'off'}), waiting)
    : button('Turn on…', 'secondary', 'presence-change', () => {
      confirm('Turn on Discord presence?', presence.public[presence.details], 'Turn on',
        () => command('presence', {value: 'on'}));
    }, waiting);
  change.setAttribute('aria-describedby', `${status.id} ${disclosure.id}`);
  now.append(nowInfo, change);

  const controls = node('div', 'group-controls');
  controls.append(details, application, now);
  card.append(header, controls);
  return [card];
}

// Alert choices are saved as they are made, like the menu app, and are no part of the draft.
// Drawing again at once disables the controls until the change has been made.
function changeAlerts(field, value) {
  command('alerts', {field: field.replace(/^alerts\./, ''), value});
  render();
}

// A secret is typed into a masked text field, not a password field, which a
// browser would offer to remember.
function secretForm(id, fields, label, disabled, submit) {
  const form = node('form', 'secret-form');
  form.id = id;
  const inputs = fields.map(([name, placeholder, secret, value]) => {
    const input = node('input', secret ? 'masked' : '');
    input.id = `${id}-${name}`;
    input.type = 'text';
    input.placeholder = placeholder;
    input.setAttribute('aria-label', placeholder);
    input.autocomplete = 'off';
    input.autocapitalize = 'off';
    input.spellcheck = false;
    input.required = true;
    input.disabled = disabled;
    if (value) input.value = value;
    return input;
  });
  const save = node('button', 'button secondary', label);
  save.id = `${id}-save`;
  save.disabled = disabled;
  form.append(...inputs, save);
  form.addEventListener('submit', event => {
    event.preventDefault();
    submit(...inputs.map(input => input.value.trim()));
  });
  return form;
}

function alertsView() {
  const alerts = state.alerts;
  const busy = paused || running;
  const row = (title, detail, control, detailClass = '') => {
    const line = node('div', 'setting');
    const info = node('div', 'setting-info');
    info.append(node('span', 'setting-label', title));
    if (detail) info.append(node('span', `setting-detail ${detailClass}`, detail));
    line.append(info);
    if (control) line.append(control);
    return line;
  };
  const card = (id, title, status, rows) => {
    const section = node('section', 'settings-group');
    const header = node('header', 'group-header group-titleline');
    const heading = node('h2', '', title);
    heading.id = id;
    section.setAttribute('aria-labelledby', id);
    header.append(heading);
    if (status) {
      const live = node('span', 'setting-detail', status);
      live.id = `${id}-status`;
      live.setAttribute('role', 'status');
      header.append(live);
    }
    const controls = node('div', 'group-controls');
    controls.append(...rows);
    section.append(header, controls);
    return section;
  };
  const names = {telegram: 'Telegram', discord: 'Discord'};
  const target = alerts.destination === 'telegram' ? `Telegram chat ${alerts.telegram.chat}` :
    alerts.destination === 'discord' ? 'the Discord channel of the saved webhook' : '';
  const toggle = alerts.enabled ?
    button('Turn off', 'secondary', 'alerts-toggle', () => changeAlerts('enabled', false), busy) :
    button('Turn on…', 'secondary', 'alerts-toggle', () => confirm('Mirror Rex alerts?',
      `Alerts go to ${target}.\n${alerts.disclosure}\nRex’s own notifications are unchanged.`,
      'Turn on', () => changeAlerts('enabled', true)), busy || !alerts.destination);
  const status = !alerts.enabled ? 'Off' : alerts.running ? 'On' : 'On, but not running';
  const main = card('group-alerts', 'Mirror Rex alerts', status, [
    row('Send Rex alerts to a chat or channel you choose',
      `${alerts.enabled ? 'On.' : 'Off until you turn it on.'} Rex’s own notifications stay as they are. ` +
        (alerts.destination ? `Alerts go to ${target}.` : 'Save a destination below first.'), toggle),
    row('What is sent', alerts.disclosure + (alerts.note ? `\n${alerts.note}` : ''), null, 'disclosure-text'),
    setting('alerts.content', 'Content', 'Program text can name files or quote a prompt.', 'select',
      {disabled: busy, change: changeAlerts, choices: [['metadata', 'Kind and program name'], ['text', 'Also program text']]}),
    row('Test message', 'One message written by Milo. No Rex alert is involved.',
      button('Send test', 'secondary', 'alerts-test', () => changeAlerts('test', null), busy || !alerts.destination)),
  ]);
  const kinds = [['blocked', 'Waiting for you', 'A program is blocked on a permission, a question or a sign-in.'],
    ['error', 'Reported an error', 'A program says it failed.'], ['done', 'Finished', 'A program says it finished.'],
    ['notification', 'Desktop notification', 'A program asks for one. A repeat within 30 seconds is not sent.'],
    ['bell', 'Terminal bell', 'At most one per terminal in 30 seconds.']];
  const events = card('group-alert-kinds', 'Alerts to mirror', '', kinds.map(([kind, title, detail]) =>
    setting(`alerts.events.${kind}`, title, detail, 'switch', {disabled: busy, change: changeAlerts})));
  const saved = name => {
    const actions = node('div', 'button-row');
    if (alerts[name].saved && alerts.destination !== name) {
      actions.append(button(`Use ${names[name]}`, 'secondary', `alerts-use-${name}`, () => changeAlerts('destination', name), busy));
    }
    if (alerts[name].saved) {
      actions.append(button('Forget…', 'text-button', `alerts-forget-${name}`, () => confirm(`Forget ${names[name]}?`,
        `This removes the saved ${names[name]} destination and its secret from ${alerts.place}. Mirroring to it ends.`,
        'Forget', () => changeAlerts('forget', name)), busy));
    }
    const label = !alerts[name].saved ? `Not saved. Once saved, its secret is kept in ${alerts.place} and is never shown.` :
      `${alerts.destination === name ? 'Saved and selected.' : 'Saved.'} The secret is kept in ${alerts.place} and is never shown again.`;
    return row(names[name], label, actions);
  };
  const destinations = card('group-alert-destinations', 'Destinations', '', [
    saved('telegram'),
    row('Telegram bot', 'The chat’s number or a public channel’s @name, and the bot token from BotFather.',
      secretForm('alerts-telegram', [['chat', 'Chat', false, alerts.telegram.chat], ['token', 'Bot token', true]],
        'Save Telegram', busy, (chat, token) => changeAlerts('telegram', {token, chat}))),
    saved('discord'),
    row('Discord webhook', 'The webhook link from the channel’s Integrations settings.',
      secretForm('alerts-discord', [['link', 'Webhook link', true]], 'Save Discord', busy,
        link => changeAlerts('discord', link))),
  ]);
  return [main, events, destinations];
}

function menuBarView() {
  const installed = state.menubar.installed;
  const card = node('section', 'settings-group');
  const header = node('header', 'group-header group-titleline');
  const heading = node('h2', '', 'Menu bar app');
  heading.id = 'group-menubar';
  card.setAttribute('aria-labelledby', heading.id);
  const live = node('span', 'setting-detail');
  live.id = 'menubar-status';
  live.setAttribute('role', 'status');
  header.append(heading, live);

  const row = node('div', 'setting');
  const info = node('div', 'setting-info');
  const detail = node('span', 'setting-detail',
    installed ? 'Installed. Opens Milo and applies the bundled update if needed.' : 'Not installed. Install and open Milo in your Mac’s menu bar.');
  detail.id = 'menubar-detail';
  info.append(node('span', 'setting-label', 'Milo'), detail);
  const control = button(installed ? 'Open or update menu app' : 'Install menu app', 'secondary', 'menubar-action', event => {
    if (!event.currentTarget.hasAttribute('aria-disabled')) command('menubar', {value: 'install'});
  });
  control.setAttribute('aria-describedby', detail.id);
  row.append(info, control);
  const controls = node('div', 'group-controls');
  controls.append(row);
  if (installed) {
    const removal = node('div', 'setting');
    const note = node('span', 'setting-detail', 'The Milo CLI and your settings will be kept.');
    note.id = 'menubar-remove-detail';
    const remove = button('Remove menu app…', 'secondary', 'menubar-remove', event => {
      if (event.currentTarget.hasAttribute('aria-disabled')) return;
      confirm('Remove menu app?', 'This removes only the menu bar app. The Milo CLI, saved settings and current draft will be kept.',
        'Remove menu app', () => command('menubar', {value: 'uninstall'}));
    });
    remove.setAttribute('aria-describedby', note.id);
    removal.append(note, remove);
    controls.append(removal);
  }
  card.append(header, controls);
  return [card];
}

const healthLabels = {good: 'All good', warning: 'Warning', issue: 'Issue found'};

function healthView() {
  const section = node('section', 'health');
  const checking = queue.some(item => item.action === 'doctor');
  const result = checking ? null : state.doctor;
  const heading = node('h2', '', 'Health Check');
  heading.id = 'diagnostics-title';
  section.append(heading);
  if (result) {
    const status = node('div', 'health-result');
    status.dataset.level = result.level;
    status.append(node('strong', '', healthLabels[result.level]));
    if (result.note) status.append(node('p', '', result.note));
    section.append(status);
  }
  const run = button(checking ? 'Checking…' : result ? 'Run again' : 'Run check', 'secondary', 'doctor',
    () => { if (!checking) command('doctor'); }, !checking && (paused || running));
  // Keep the pending control focusable so render can preserve keyboard ownership.
  if (checking) run.setAttribute('aria-disabled', 'true');
  section.append(run);
  return [section];
}

function layoutActions() {
  const {layout, capabilities, trial} = state;
  const actions = [];
  if (state.groups.layout.editable && layout.ratios) {
    actions.push(button('Use automatic heights', 'text-button', 'reset-heights', () => edit('layout.ratios', null), paused));
  }
  if (trial && capabilities.undo) {
    actions.push(button('Undo trial', 'text-button', 'undo-layout', () => command('undo'), paused));
  } else if (capabilities.try) {
    actions.push(button('Try in Rex', 'text-button', 'try-layout', () => command('try'), paused));
  }
  if (capabilities.capture) {
    actions.push(button('Copy layout from Rex tab…', 'text-button', 'capture', () => {
      if (state.groups.layout.dirty || numericDrafts.size || queue.some(item => item.field?.startsWith('layout.'))) {
        confirm('Replace the layout draft?', 'Copy the side, order and pane sizes from your Rex tab without saving.',
          'Copy layout', () => command('capture'));
      } else command('capture');
    }, paused));
  }
  return actions;
}

document.addEventListener('pointerdown', event => {
  if (event.button === 0) pointerPress = true;
}, {capture: true});
for (const type of ['pointerup', 'pointercancel', 'contextmenu', 'blur']) {
  window.addEventListener(type, () => {
    if (!pointerPress) return;
    // The timer runs after the click that this release dispatches.
    setTimeout(() => {
      pointerPress = false;
      for (const field of heldDrafts) applyNumericDraft(field);
      heldDrafts.clear();
    });
  });
}
for (const tab of navButtons) {
  tab.addEventListener('click', () => {
    // An invalid number left behind on Layout would disable Save with no visible reason.
    if (tab.dataset.page !== page && !applyNumericDrafts()) return;
    const menu = document.querySelector('.action-menu[open]');
    if (menu) menu.open = false;
    page = tab.dataset.page;
    render();
    // Whether mirroring is running, and what it last noted, may have changed since the page was loaded.
    if (page === 'alerts' && state.alerts) changeAlerts('refresh', null);
    byId('settings-panel').scrollTop = 0;
    byId('settings-panel').focus({preventScroll: true});
  });
}
byId('skip-settings').addEventListener('click', () => byId('settings-panel').focus());
byId('done').addEventListener('click', done);
byId('save-draft').addEventListener('click', requestSave);
byId('save-options').addEventListener('close', event => {
  if (event.target.returnValue !== 'save') restoreSettingsFocus(byId('save-draft'));
});
byId('discard-all').addEventListener('click', () => discard('all'));
byId('confirm-cancel').addEventListener('click', () => byId('confirm').close('cancel'));
byId('confirm-do').addEventListener('click', () => byId('confirm').close('continue'));
byId('confirm').addEventListener('close', event => {
  const action = confirmAction;
  confirmAction = undefined;
  action?.(event.target.returnValue === 'continue');
});
byId('retry').addEventListener('click', () => { void connect(true); });
byId('drop-edit').addEventListener('click', () => {
  if (!paused || running || closing) return;
  if (document.activeElement === byId('drop-edit')) restoreSettingsFocus();
  queue.shift();
  paused = queue.length > 0;
  void connect();
});
for (const id of ['open-retry', 'reconnect']) byId(id).addEventListener('click', () => { void connect(); });
// The dev client may refresh the page once browser-only input has reached the draft.
window.addEventListener('milo:dev-reload', event => {
  if (running || paused || closing || ended || queue.length || document.querySelector('dialog[open]') || numericDrafts.size) {
    event.preventDefault();
  } else {
    window.removeEventListener('beforeunload', warnOnUnload);
  }
});
void connect();
