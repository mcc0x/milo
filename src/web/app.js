'use strict';

const byId = id => document.getElementById(id);
const token = location.hash.slice(1);
const roles = ['outline', 'history', 'usage'];
const roleNames = {outline: 'Outline', history: 'History', usage: 'Usage'};
const originLabels = {default: 'Built-in defaults', global: 'Global default', worktree: 'Worktree override', account: 'Account settings'};
const groupLabels = {layout: 'layout', preferences: 'pane preferences', account: 'Claude Code choices'};
const navButtons = [...document.querySelectorAll('[data-page]')];
const queue = [];
let page = 'layout';
let state;
let running = false;
let paused = false;
let closing = false;
let ended = false;
let connected = false;
let localError = '';
let confirmAction;
let warned = false;

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

function percent(fraction) {
  return `${Math.round(fraction * 1000) / 10}%`;
}

function lookup(data, field) {
  return field.split('.').reduce((object, key) => object[key], data);
}

function edit(field, value) {
  const previous = queue.findLast(action => action.field === field);
  const current = previous ? previous.value : lookup(state, field);
  if (Object.is(current, value) || (Array.isArray(value) && Array.isArray(current) &&
    current.length === value.length && current.every((item, i) => item === value[i]))) return;
  enqueue({action: 'set', field, value});
}

function command(action, payload = {}) {
  enqueue({action, ...payload});
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
  if (!paused) localError = '';
  updateChrome();
  void drain();
}

async function api(path, method = 'GET', payload) {
  if (!token) throw new Error('The session key is missing. Open the URL printed by milo web, including everything after #.');
  const headers = {Authorization: `Bearer ${token}`};
  if (payload !== undefined) headers['Content-Type'] = 'application/json';
  let response;
  try {
    response = await fetch(path, {method, headers, body: JSON.stringify(payload), cache: 'no-store', credentials: 'omit', redirect: 'error'});
  } catch {
    connected = false;
    throw new Error('The local editor did not answer. Reconnect, or run milo web again if the session has ended.');
  }
  if (!response.ok) {
    const data = await response.json().catch(() => null);
    if (response.status === 401 || response.status === 403) connected = false;
    throw new Error(data?.error || `The local editor returned HTTP ${response.status}.`);
  }
  connected = true;
  return response.status === 204 ? null : response.json();
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
    const {pending, ...action} = queue[0];
    try {
      state = await api('/api/action', 'POST', {...action, version: state.version});
      if (state.message) notify(state.message.text, state.message.error);
      queue.splice(0, 1 + (pending ?? 0));
    } catch (error) {
      localError = error.message;
      notify(localError, true);
      if (action.action !== 'set') queue.shift();
      // Do not run a queued save or capture after an earlier edit failed.
      for (let i = queue.length - 1; i >= 0; i--) {
        if (queue[i].action !== 'set') queue.splice(i, 1);
      }
      paused = queue.length > 0;
      break;
    }
    if (state.message?.error) {
      paused = queue.length > 0;
      break;
    }
  }
  running = false;
  render();
  if (!paused) void drain();
}

async function connect(retryEdits = false) {
  if (running || closing || ended) return;
  running = true;
  localError = '';
  if (state) updateChrome();
  byId('open-retry').hidden = true;
  byId('opening-message').textContent = 'Opening your local settings…';
  try {
    const latest = await api('/api/state');
    state = latest;
    if (!paused || retryEdits) {
      paused = false;
    } else {
      localError = 'Reconnected. An edit is still waiting to be applied.';
    }
    byId('editor').hidden = byId('actionbar').hidden = false;
    byId('opening').hidden = true;
    if (localError) notify(localError, true);
    else if (latest.message) notify(latest.message.text, latest.message.error);
  } catch (error) {
    localError = error.message;
    notify(localError, true);
    byId('opening-message').textContent = error.message;
    byId('open-retry').hidden = !token;
  } finally {
    running = false;
    if (state) render();
    else renderConnection();
  }
  if (!paused) void drain();
}

function confirm(title, note, label, action) {
  const dialog = byId('confirm');
  byId('confirm-title').textContent = title;
  byId('confirm-note').textContent = note;
  byId('confirm-do').textContent = label;
  confirmAction = action;
  dialog.returnValue = '';
  dialog.showModal();
}

function discard(group) {
  const all = group === 'all';
  confirm(all ? 'Discard all unsaved changes?' : `Discard unsaved ${groupLabels[group]}?`,
    all ? 'This restores saved settings and undoes any active layout trial.' : 'This restores the last saved settings for this group.',
    'Discard changes', () => command('discard', {group}));
}

async function closeSession() {
  if (running || closing || ended) return;
  closing = true;
  updateChrome();
  try {
    await api('/api/close', 'POST');
    ended = true;
    state = undefined;
    queue.length = 0;
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
  if (running || closing) return;
  if (state.dirty || state.trial || queue.length) {
    confirm('Close without saving?', 'Unsaved changes and any active trial will be discarded. Settings you already saved will stay.',
      'Discard and close', closeSession);
  } else {
    void closeSession();
  }
}

function warnOnUnload(event) {
  event.preventDefault();
  event.returnValue = '';
}

function renderConnection() {
  const connection = byId('connection');
  const status = ended ? 'closed' : connected ? 'ready' : localError ? 'offline' : 'opening';
  connection.dataset.state = status;
  connection.textContent = {closed: 'Closed', ready: 'Local session', offline: 'Not connected', opening: 'Connecting'}[status];
}

function updateChrome() {
  renderConnection();
  const needsWarning = Boolean(state && (state.dirty || state.trial || queue.length));
  if (needsWarning !== warned) {
    if (needsWarning) window.addEventListener('beforeunload', warnOnUnload);
    else window.removeEventListener('beforeunload', warnOnUnload);
    warned = needsWarning;
  }
  if (!state) return;
  const error = localError || (state.message?.error ? state.message.text : '');
  byId('error-box').hidden = !error || (!paused && !localError);
  if (byId('error').textContent !== error) byId('error').textContent = error;
  byId('error-actions').hidden = !paused && !localError;
  byId('retry').hidden = byId('drop-edit').hidden = !paused;
  byId('reconnect').hidden = !localError || paused;
  byId('reconnect').textContent = connected ? 'Refresh editor state' : 'Reconnect';
  byId('retry').disabled = byId('drop-edit').disabled = byId('reconnect').disabled = running || closing;
  const dirty = Object.values(state.groups).filter(group => group.dirty).length;
  const status = byId('draft-status');
  status.dataset.dirty = String(needsWarning);
  const summary = closing ? 'Closing settings…' : paused ? 'Edit not applied' : running || queue.length ? 'Updating draft…' :
    dirty ? `Unsaved changes in ${dirty === 1 ? '1 group' : `${dirty} groups`}` : state.trial ? 'Layout trial active' : 'No unsaved changes';
  if (status.textContent !== summary) status.textContent = summary;
  byId('discard-all').disabled = !needsWarning || running || closing || !connected;
  byId('save-draft').disabled = running || closing || paused || !connected || queue.length > 0 ||
    !Object.entries(state.groups).some(([key, group]) => canSaveDraft(key, group));
  const saving = !byId('save-draft').disabled;
  byId('save-draft').classList.toggle('primary', saving);
  byId('save-draft').classList.toggle('secondary', !saving);
  byId('done').classList.toggle('primary', !needsWarning);
  byId('done').disabled = running || closing;
}

function render() {
  const focus = document.activeElement;
  const focusId = focus?.id;
  const unfinished = focus?.type === 'number' && focus.value !== focus.dataset.initial ? focus.value : null;
  byId('version').textContent = `Milo v${state.app_version}`;
  byId('version').hidden = false;
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
  for (const tab of navButtons) {
    const name = tab.dataset.page;
    if (name === page) tab.setAttribute('aria-current', 'page');
    else tab.removeAttribute('aria-current');
    const dirty = name === 'layout' ? state.groups.layout.dirty :
      name === 'appearance' ? state.groups.preferences.dirty :
      name === 'workflow' && (state.groups.preferences.dirty || state.groups.account.dirty);
    const dot = tab.querySelector('.nav-draft');
    dot.replaceChildren(node('span', 'sr-only', ' (unsaved changes)'));
    dot.hidden = !dirty;
  }
  byId('preview').inert = page === 'health';
  if (page !== 'health') renderPreview();
  const views = {layout: layoutView, appearance: appearanceView, workflow: workflowView, health: healthView};
  const panel = byId('settings-panel');
  panel.replaceChildren(...views[page]());
  panel.setAttribute('aria-labelledby', panel.querySelector('h2').id);
  updateChrome();
  if (focusId && !focus.isConnected) {
    const replacement = byId(focusId);
    if (replacement && !replacement.disabled && !replacement.hidden) {
      if (unfinished !== null) replacement.value = unfinished;
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
    const value = lookup(state, field);
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
      radio.addEventListener('change', () => edit(field, typeof value === 'boolean' ? key === 'true' : key));
      choice.append(radio, node('span', '', text));
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
  const control = node('input');
  const id = `field-${field}`;
  const value = lookup(state, field);
  control.id = label.htmlFor = id;
  explanation.id = `${id}-detail`;
  if (detail) control.setAttribute('aria-describedby', explanation.id);
  control.disabled = Boolean(options.disabled);
  if (type === 'switch') {
    control.type = 'checkbox';
    control.className = 'switch';
    control.setAttribute('role', 'switch');
    control.checked = value;
  } else {
    control.type = 'number';
    control.min = options.min;
    control.max = options.max;
    control.step = options.step;
    control.required = true;
    control.value = options.unit ? String(Number((value * 100).toFixed(6))) : String(value);
    control.dataset.initial = control.value;
    control.addEventListener('keydown', event => {
      if (event.key === 'Enter' && !event.isComposing) {
        event.preventDefault();
        apply();
      }
    });
  }
  function apply() {
    if (!control.reportValidity()) return;
    const next = type === 'switch' ? control.checked : control.valueAsNumber / (options.unit ? 100 : 1);
    if (type === 'number') control.dataset.initial = control.value;
    edit(field, next);
  }
  control.addEventListener('change', apply);
  info.append(label);
  if (detail) info.append(explanation);
  wrap.append(control);
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

function canSaveDraft(key, group) {
  return (group.dirty || (key === 'layout' && state.trial)) &&
    (group.save_global || group.save_worktree || group.save_account);
}

function saveChoices(key, group, select, disabled = false) {
  const choices = node('div', 'button-row');
  for (const [scope, label] of [['worktree', 'Save for this worktree'], ['global', 'Save as global default'], ['account', 'Save Claude Code choices']]) {
    if (group[`save_${scope}`]) choices.append(button(label, choices.childElementCount ? 'secondary' : 'primary',
      `${select}-${key}-${scope}`, () => {
        if (select === 'dock') byId('save-options').close();
        command('save', {group: key, scope});
      }, disabled));
  }
  return choices;
}

function showSaveOptions() {
  const groups = byId('save-groups');
  groups.replaceChildren();
  for (const [key, group] of Object.entries(state.groups)) {
    if (!canSaveDraft(key, group)) continue;
    const title = groupLabels[key];
    groups.append(node('h3', '', title[0].toUpperCase() + title.slice(1)), saveChoices(key, group, 'dock'));
  }
  if (groups.childElementCount) byId('save-options').showModal();
}

function disclosure(label, accessibleName, items) {
  const details = node('details', 'action-menu');
  details.name = 'editor-actions';
  const trigger = node('summary', '', label);
  trigger.setAttribute('aria-label', accessibleName);
  const panel = node('div', 'action-menu-items');
  panel.append(...items);
  panel.addEventListener('click', () => { details.open = false; });
  details.append(trigger, panel);
  return details;
}

document.addEventListener('pointerdown', event => {
  const menu = document.querySelector('.action-menu[open]');
  if (menu && !menu.contains(event.target)) menu.open = false;
});
document.addEventListener('keydown', event => {
  const menu = document.querySelector('.action-menu[open]');
  if (event.key === 'Escape' && menu) {
    menu.open = false;
    menu.firstElementChild.focus();
  }
});

function scopeActions(key, group) {
  const row = node('div', 'scope-actions');
  const label = originLabels[group.scope];
  row.append(group.inherit ? disclosure(`${label} ▾`, `${label} options`, [
    button('Remove override…', 'text-button', `inherit-${key}`, () => {
      confirm('Remove worktree override?', `This worktree will use the global ${groupLabels[key]}${group.dirty ? ' and lose unsaved changes in this group' : ''}.`,
        'Remove override', () => command('inherit', {group: key}));
    }, paused),
  ]) : node('span', 'group-origin', label));
  if (key === 'layout' && state.context.inside_rex && !state.context.installed && (group.save_global || group.save_worktree)) {
    row.append(disclosure('Set up in Rex…', 'Set up in Rex', [saveChoices(key, group, 'install', paused)]));
  }
  return row;
}

function panePicker(editable) {
  const picker = node('fieldset', 'pane-picker');
  picker.disabled = !editable;
  const legend = node('legend', 'sr-only', 'Panes in rail order');
  const list = node('ol', 'pane-list');
  state.layout.order.forEach((role, index, order) => {
    const line = node('li', 'pane-line');
    const label = node('label');
    const input = node('input');
    input.type = 'checkbox';
    input.id = `enable-${role}`;
    input.checked = state.layout.panes[role];
    input.addEventListener('change', () => edit(`layout.panes.${role}`, input.checked));
    label.append(input, document.createTextNode(roleNames[role]));
    const positions = node('div', 'button-row');
    for (const [delta, arrow, direction] of [[-1, '←', 'up'], [1, '→', 'down']]) {
      const to = index + delta;
      const move = button(arrow, 'secondary', `move-${role}-${direction}`, () => {
        const next = [...order];
        [next[index], next[to]] = [next[to], next[index]];
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

function appearanceView() {
  const disabled = !state.groups.preferences.editable;
  return [groupCard('preferences', 'Pane appearance', 'Appearance and workflow share a save. A worktree inherits any choices it does not override.', [
    setting('appearance.icons', 'Icons', 'Choose Nerd Font symbols, plain symbols, or no icons.', 'select',
      {disabled, choices: [['nerd', 'Nerd Font'], ['plain', 'Plain'], ['off', 'Off']]}),
    setting('appearance.static_marks', 'Activity marks', 'A static mark stays still when a file changes.', 'select',
      {disabled, choices: [['false', 'Pulse'], ['true', 'Static']]}),
    setting('appearance.text_git_marks', 'Git status marks', 'Text adds letters and signs beside changed files and commits.', 'select',
      {disabled, choices: [['false', 'Style only'], ['true', 'Text']]}),
    setting('appearance.usage_large_figures', 'Usage display', 'Auto uses text when there isn’t room for large figures.', 'select',
      {disabled, choices: [['false', 'Auto compact'], ['true', 'Large figures']]}),
  ])];
}

function workflowView() {
  const disabled = !state.groups.preferences.editable;
  const account = state.groups.account;
  return [groupCard('preferences', 'Pane workflow', 'Workflow and appearance share a save. A worktree inherits any choices it does not override.', [
    setting('workflow.project_mode', 'Project tracking', 'Follow the shell that opened the Milo panes, or keep the starting project.', 'select',
      {disabled, choices: [['follow', 'Follow shell'], ['pin', 'Pin project']]}),
    setting('workflow.follow_activity', 'Follow file edits', 'Reveal active files without saving the temporary reveals.', 'switch', {disabled}),
    setting('workflow.changes_only', 'Outline starts with', 'Set the initial view. You can switch views later from Outline.', 'select',
      {disabled, choices: [['false', 'All files'], ['true', 'Changes only']]}),
    setting('workflow.history_follow_new', 'Follow new commits', 'Keep History at the newest commit while you’re at the top.', 'switch', {disabled}),
  ]), groupCard('account', 'Claude Code', 'Milo changes the Claude Code status line only if it is unused or already managed by Milo.', [
    setting('account.claude_limits', 'Show Claude Code limits', state.account.claude_status, 'switch', {disabled: !account.editable}),
  ])];
}

function healthView() {
  const card = node('section', 'settings-group');
  const content = node('div', 'health-content');
  const heading = node('h2', 'setting-label', 'Local diagnostics');
  heading.id = 'diagnostics-title';
  card.setAttribute('aria-labelledby', heading.id);
  const tools = node('div', 'button-row');
  tools.append(button('Read diagnostics', 'primary', 'doctor', () => command('doctor'), paused));
  content.append(heading, node('p', '', 'Check preference scope, Rex integration, automatic arrangement and usage sources. This does not change settings.'), tools);
  content.append(node('p', '', 'For integration repairs, run milo setup --repair from your shell. Use Done to stop the editor, or press Ctrl+C in the shell if you’ve already closed the tab.'));
  if (state.doctor) {
    const report = node('pre', 'report', state.doctor);
    report.setAttribute('aria-label', 'Diagnostics report');
    content.append(report);
  }
  card.append(content);
  return [card];
}

function terminalLine(text, className) {
  return node('li', `terminal-line${className ? ` ${className}` : ''}`, text);
}

function scenePane(role) {
  const {appearance, workflow} = state;
  const pane = node('section', 'terminal-pane');
  pane.id = `preview-${role}`;
  const heading = node('h3', '', roleNames[role]);
  if (role === 'usage') {
    pane.classList.add('terminal-usage');
    const reading = node('p', `usage-reading terminal-ok${appearance.usage_large_figures ? ' large-figure' : ''}`, '83%');
    reading.append(node('span', 'sr-only', ' remaining'));
    pane.append(heading, reading);
    return pane;
  }
  const lines = node('ul', 'terminal-lines');
  const dot = appearance.icons === 'off' ? '' : appearance.icons === 'plain' ? '› ' : '▸ ';
  if (role === 'outline') {
    lines.append(terminalLine(`${dot}src`, 'terminal-ok'));
    const active = terminalLine();
    active.classList.toggle('terminal-file', workflow.follow_activity);
    const mark = node('span', `activity-mark${appearance.static_marks ? '' : ' pulse'}`, '● ');
    mark.setAttribute('aria-hidden', 'true');
    active.append(node('span', 'sr-only', 'Recent edit: '), mark, document.createTextNode(`tides.ts${appearance.text_git_marks ? ' M' : ''}`));
    lines.append(active, terminalLine(`${dot}weather.ts${appearance.text_git_marks ? ' +' : ''}`, 'terminal-ok'));
    if (!workflow.changes_only) lines.append(terminalLine(`${dot}README.md`));
  } else {
    if (workflow.history_follow_new) lines.append(terminalLine(`${appearance.text_git_marks ? '+ ' : ''}ea17b Tide charts`, 'terminal-ok'));
    lines.append(terminalLine('13a8f Weekend forecast'), terminalLine('04c4f Start shore guide'));
  }
  pane.append(heading, lines);
  return pane;
}

function sceneShell() {
  const shell = node('section', 'shell');
  shell.append(node('h3', '', 'Shell'), node('p', 'shell-line', '$ git status --short'),
    node('p', 'shell-line terminal-file', ' M src/tides.ts'),
    node('p', 'shell-line terminal-ok', ' A src/weather.ts'),
    node('p', 'shell-line', '$ cd charts'),
    node('p', 'shell-line', state.workflow.project_mode === 'pin' ? 'Project pinned to tidebook' : 'Following tidebook/charts'));
  return shell;
}

function grip(orientation, editable, name, controls, axis, read, write, commit) {
  const el = node('div', `grip ${orientation}`);
  el.id = `split-${controls}`;
  if (!editable) {
    el.setAttribute('aria-hidden', 'true');
    return {el, update() {}};
  }
  el.tabIndex = 0;
  el.setAttribute('role', 'separator');
  el.setAttribute('aria-label', name);
  el.setAttribute('aria-controls', controls);
  el.setAttribute('aria-orientation', orientation);
  el.setAttribute('aria-describedby', 'split-help');
  let drag;

  function update() {
    const {value, min, max, label} = read();
    const ariaNumber = n => String(Math.round(n * 10000) / 100);
    el.setAttribute('aria-valuenow', ariaNumber(value));
    el.setAttribute('aria-valuemin', ariaNumber(min));
    el.setAttribute('aria-valuemax', ariaNumber(max));
    el.setAttribute('aria-valuetext', label);
  }

  function set(value) {
    const {min, max} = read();
    write(Math.min(max, Math.max(min, value)));
  }

  el.addEventListener('pointerdown', event => {
    if (!event.isPrimary || event.button !== 0 || drag) return;
    event.preventDefault();
    el.focus({preventScroll: true});
    drag = {id: event.pointerId, at: axis.position(event), from: read().value};
    el.setPointerCapture(event.pointerId);
    el.dataset.dragging = '';
  });
  el.addEventListener('pointermove', event => {
    if (drag?.id === event.pointerId) set(drag.from + axis.delta(axis.position(event) - drag.at));
  });
  function finish(cancel) {
    if (!drag) return;
    const {id, from} = drag;
    drag = null;
    delete el.dataset.dragging;
    if (el.hasPointerCapture(id)) el.releasePointerCapture(id);
    if (cancel) write(from);
    else if (Math.abs(read().value - from) > .00005) commit();
  }
  for (const [event, cancel] of [['pointerup', false], ['pointercancel', true], ['lostpointercapture', true]]) {
    el.addEventListener(event, e => { if (e.pointerId === drag?.id) finish(cancel); });
  }
  el.addEventListener('keydown', event => {
    if (event.key === 'Escape' && drag) {
      event.preventDefault();
      finish(true);
      return;
    }
    if (drag) return;
    const {value, min, max} = read();
    const step = event.shiftKey ? .05 : .01;
    const next = event.key === 'Home' ? min : event.key === 'End' ? max : event.key === axis.less ?
      value - step : event.key === axis.more ? value + step : undefined;
    if (next === undefined) return;
    event.preventDefault();
    set(next);
    if (read().value !== value) commit();
  });
  return {el, update};
}

function renderPreview() {
  const {layout, preview, context, capabilities, trial} = state;
  const showLayout = page === 'layout';
  const editable = showLayout && state.groups.layout.editable;
  const stage = byId('split-stage');
  const shell = sceneShell();
  const rail = node('div', 'rail');
  const panes = showLayout ? layout.order.filter(role => layout.panes[role]) : roles;
  const side = showLayout ? layout.side : 'left';
  const shares = showLayout ? [...preview.shares] : [.41, .34, .25];
  let width = showLayout ? preview.width : .44;

  byId('preview-title').textContent = showLayout ? 'Tab preview' : 'Pane preview';
  byId('terminal-size').textContent = showLayout && preview.cols && preview.rows ? `${preview.cols} × ${preview.rows}` : 'Sample tab';
  byId('split-help').hidden = !editable || !panes.length;
  if (!panes.length) {
    stage.replaceChildren(shell);
  } else {
    const indexes = panes.map(role => roles.indexOf(role));
    const sections = panes.map(scenePane);
    rail.id = 'preview-rail';
    rail.append(...sections);
    const left = side === 'left';
    const widthGrip = grip('vertical', editable, 'Resize Milo rail', rail.id, {
      position: event => event.clientX,
      delta: pixels => pixels / Math.max(1, stage.clientWidth) * (left ? 1 : -1),
      less: left ? 'ArrowLeft' : 'ArrowRight',
      more: left ? 'ArrowRight' : 'ArrowLeft',
    }, () => ({value: width, min: Math.min(.08, preview.width), max: Math.max(.92, preview.width), label: `${percent(width)} of the tab`}),
    value => { width = value; paint(); }, () => edit('layout.width', Number(width.toPrecision(10))));
    const heightGrips = indexes.slice(0, -1).map((upper, index) => {
      const lower = indexes[index + 1];
      const initialTop = shares[upper];
      const initialBottom = shares[lower];
      return grip('horizontal', editable, `Resize ${roleNames[roles[upper]]} and ${roleNames[roles[lower]]}`, sections[index].id, {
        position: event => event.clientY,
        delta: pixels => pixels / Math.max(1, stage.clientHeight),
        less: 'ArrowUp',
        more: 'ArrowDown',
      }, () => {
        const pair = shares[upper] + shares[lower];
        const minimum = Math.min(.07, pair / 4);
        return {value: shares[upper], min: Math.min(minimum, initialTop, shares[upper]),
          max: Math.max(shares[upper], pair - Math.min(minimum, initialBottom)),
          label: `${roleNames[roles[upper]]} ${percent(shares[upper])}, ${roleNames[roles[lower]]} ${percent(shares[lower])}`};
      },
      value => {
        const total = shares[upper] + shares[lower];
        shares[upper] = value;
        shares[lower] = total - value;
        paint();
      }, () => {
        const ratios = shares.map(share => Math.round(share * 1e6));
        const largest = shares.indexOf(Math.max(...shares));
        ratios[largest] += 1e6 - ratios.reduce((sum, ratio) => sum + ratio, 0);
        edit('layout.ratios', ratios);
      });
    });

    function paint() {
      stage.style.setProperty('--rail-width', `${width * 100}%`);
      stage.style.setProperty('--rail-split', `${(left ? width : 1 - width) * 100}%`);
      widthGrip.update();
      sections.forEach((section, index) => section.style.setProperty('--pane-size', shares[indexes[index]]));
      let through = 0;
      heightGrips.forEach((grip, index) => {
        through += shares[indexes[index]];
        grip.el.style.setProperty('--pane-split', `${through * 100}%`);
        grip.update();
      });
    }

    rail.append(...heightGrips.map(grip => grip.el));
    stage.replaceChildren(...(left ? [rail, widthGrip.el, shell] : [shell, widthGrip.el, rail]));
    paint();
  }

  const secondary = [];
  if (editable && layout.ratios) {
    secondary.push(button('Use automatic heights', 'text-button', 'reset-heights', () => edit('layout.ratios', null), paused));
  }
  if (showLayout && trial && capabilities.undo) {
    secondary.push(button('Undo trial', 'text-button', 'undo-layout', () => command('undo'), paused));
  } else if (showLayout && capabilities.try) {
    secondary.push(button('Try in Rex', 'text-button', 'try-layout', () => command('try'), paused));
  }
  if (showLayout && capabilities.capture) {
    secondary.push(button('Copy layout from Rex tab…', 'text-button', 'capture', () => {
      if (state.groups.layout.dirty || queue.some(item => item.field?.startsWith('layout.'))) {
        confirm('Replace the layout draft?', 'Capture copies the side, order and pane sizes from your Rex tab. It does not move panes or save settings.',
          'Copy layout', () => command('capture'));
      } else command('capture');
    }, paused));
  }
  if (showLayout && capabilities.refresh) secondary.push(button('Refresh preview', 'text-button', 'refresh-tab', () => command('refresh'), paused));
  const more = byId('preview-more');
  more.replaceChildren();
  if (secondary.length) more.append(disclosure('•••', 'Preview actions', secondary));
  more.hidden = !secondary.length;
}

for (const tab of navButtons) {
  tab.addEventListener('click', () => {
    page = tab.dataset.page;
    render();
    byId('settings-panel').parentElement.scrollTop = 0;
    byId('settings-panel').focus({preventScroll: true});
  });
}
byId('skip-settings').addEventListener('click', () => byId('settings-panel').focus());
byId('done').addEventListener('click', done);
byId('save-draft').addEventListener('click', showSaveOptions);
byId('discard-all').addEventListener('click', () => discard('all'));
byId('confirm-cancel').addEventListener('click', () => byId('confirm').close('cancel'));
byId('confirm-do').addEventListener('click', () => byId('confirm').close('continue'));
byId('confirm').addEventListener('close', event => {
  const action = confirmAction;
  confirmAction = undefined;
  if (event.target.returnValue === 'continue') action?.();
});
byId('retry').addEventListener('click', () => { void connect(true); });
byId('drop-edit').addEventListener('click', () => {
  if (running || closing) return;
  queue.shift();
  paused = false;
  localError = '';
  render();
  void drain();
});
for (const id of ['open-retry', 'reconnect']) byId(id).addEventListener('click', () => { void connect(); });
// The dev client may refresh the page once browser-only input has reached the draft.
window.addEventListener('milo:dev-reload', event => {
  const input = document.activeElement;
  if (running || paused || closing || ended || queue.length || byId('confirm').open ||
      document.querySelector('[data-dragging]') || (input?.type === 'number' && input.value !== input.dataset.initial)) {
    event.preventDefault();
  } else {
    window.removeEventListener('beforeunload', warnOnUnload);
  }
});
void connect();
