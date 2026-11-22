// Run with Node 20+, playwright-core, and either its installed Chromium or CHROME_BIN.
// Usage: node tests/web-smoke.cjs path/to/packaged/milo
const assert = require('node:assert/strict');
const {execFileSync, spawn} = require('node:child_process');
const {once} = require('node:events');
const {chmod, mkdir, mkdtemp, readFile, rm, writeFile} = require('node:fs/promises');
const {createServer} = require('node:net');
const {tmpdir} = require('node:os');
const {basename, join, resolve} = require('node:path');
const {createInterface} = require('node:readline');
const {setTimeout: delay} = require('node:timers/promises');
const {chromium} = require('playwright-core');

const timeout = 10_000;
const edits = [['Rail width', '37'], ['Usage height', '8']];
const clean = 'No unsaved changes';
const dirty = 'Unsaved changes in 1 group';
const button = (page, name) => page.getByRole('button', {name, exact: true});
const dialog = (page, name) => page.getByRole('dialog', {name, exact: true});
const number = (page, name) => page.getByRole('spinbutton', {name, exact: true});
const status = (page, text) => page.getByRole('status').filter({hasText: text});
const reopenNote = page => page.getByRole('region', {name: 'Project session'}).getByText('Reopen panes to apply.', {exact: true});
const section = (page, name) => page.getByRole('navigation').getByRole('button', {name: new RegExp(`^${name}`)});
const usageTools = ['Codex', 'Claude Code', 'OpenCode'];
const usageChoice = (page, name) => page.getByRole('combobox', {name, exact: true});
const usageAccounts = ['DeepSeek balance', 'OpenRouter spend', 'Devin ACUs', 'Copilot AI credits'];

async function withMilo(binary, cwd, env, run) {
  const server = spawn(binary, ['web', '--no-open', '--port', '0'], {
    cwd, env, stdio: ['ignore', 'pipe', 'inherit'],
  });
  const exited = once(server, 'close');
  const stdout = createInterface({input: server.stdout});
  try {
    const url = await Promise.race([
      once(stdout, 'line', {signal: AbortSignal.timeout(timeout)}).then(([line]) => line),
      exited.then(([code, signal]) => { throw new Error(`Milo exited before printing its URL (${signal ?? code})`); }),
    ]);
    assert.match(url, /^http:\/\/127\.0\.0\.1:\d+\/#\w+$/);
    stdout.close();
    await run(url, exited);
  } finally {
    stdout.close();
    if (server.exitCode === null && server.signalCode === null) server.kill('SIGTERM');
    const kill = setTimeout(() => server.kill('SIGKILL'), timeout).unref();
    try {
      await exited.catch(() => {});
    } finally {
      clearTimeout(kill);
    }
  }
}

async function openEditor(context, url) {
  const page = await context.newPage();
  await page.goto(url);
  await page.getByRole('heading', {name: 'Pane layout', exact: true}).waitFor();
  return page;
}

async function saveNumbers(page, cwd) {
  const save = button(page, 'Save');
  const choices = dialog(page, 'Save changes');
  for (const [name, value] of edits) {
    const field = number(page, name);
    await field.fill('');
    assert(await save.isDisabled(), `Save must be disabled with an empty ${name}`);
    await field.pressSequentially(value);
    assert(await field.evaluate(input => input === document.activeElement), `${name} stayed focused before Save`);
    assert(await save.isEnabled(), `Save must be enabled while ${name} is still focused`);
    // One real pointer click, with no intermediate blur/Enter and no locator retry.
    const box = await save.boundingBox();
    assert(box, 'Save is on screen');
    await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
    await button(choices, 'Save for this worktree').click();
    await status(page, clean).waitFor({state: 'attached'});
    await status(page, `Saved layout for “${basename(cwd)}”. Rex integration unchanged.`).waitFor();
    assert.equal(await reopenNote(page).count(), 0, 'layout saves do not ask to reopen panes');
    assert.equal(await field.inputValue(), value, name);
    console.log(`${name}: saved on the first click`);
  }
}

async function chooseUsageTools(page, binary, cwd, env) {
  await section(page, 'Usage').click();
  for (const tool of usageTools) {
    assert.equal(await usageChoice(page, tool).inputValue(), 'auto', `${tool} starts on Auto`);
  }
  // Accounts are asked about online, so each starts hidden, and trying one out saves nothing.
  const accounts = page.locator('summary').filter({hasText: 'Provider accounts'});
  assert.equal(await usageChoice(page, 'DeepSeek balance').count(), 0, 'optional accounts start collapsed');
  await accounts.click();
  for (const account of usageAccounts) {
    assert.equal(await usageChoice(page, account).inputValue(), 'hide', `${account} starts on Hide`);
  }
  await usageChoice(page, 'DeepSeek balance').selectOption('auto');
  await accounts.getByText('1 enabled').waitFor();
  await usageChoice(page, 'DeepSeek balance').selectOption('hide');
  await usageChoice(page, 'OpenCode').selectOption('hide');
  await accounts.getByText('0 enabled').waitFor();
  await usageChoice(page, 'Claude Code').selectOption('always');
  await status(page, dirty).waitFor({state: 'attached'});
  const preferences = join(env.XDG_CONFIG_HOME, 'milo', 'preferences.conf');
  await assert.rejects(readFile(preferences), {code: 'ENOENT'}, 'a draft writes no preferences');

  await button(page, 'Save').click();
  await button(dialog(page, 'Save changes'), 'Save as global default').click();
  await status(page, clean).waitFor({state: 'attached'});
  const saved = status(page, 'Saved global pane preferences. Reopen panes to apply.');
  await saved.waitFor();
  await reopenNote(page).waitFor();
  await page.clock.fastForward(3001);
  await saved.waitFor({state: 'hidden'});
  await reopenNote(page).waitFor();
  assert.equal(await readFile(preferences, 'utf8'), 'usage_claude = always\nusage_opencode = hide\n');
  assert.equal(await usageChoice(page, 'Codex').inputValue(), 'auto', 'Codex was left on Auto');
  // What the panes read is what the controls saved, and a hidden tool is not looked for.
  const doctor = execFileSync(binary, ['doctor'], {cwd, env, encoding: 'utf8'});
  assert.match(doctor, /^ {2}Usage tools: Claude Code always shown; OpenCode hidden; others automatic$/m);
  assert.match(doctor, /^Claude Code limits: no local report; /m);
  assert.doesNotMatch(doctor, /^(Codex limits|OpenCode activity|Usage tools: none)/m);
  await section(page, 'Layout').click();
  await reopenNote(page).waitFor();
  const width = number(page, 'Rail width');
  const before = await width.inputValue();
  await width.fill('');
  await reopenNote(page).waitFor({state: 'hidden'});
  await width.fill(before);
  assert.equal(await reopenNote(page).count(), 0, 'undoing browser-only input does not revive the instruction');

  await section(page, 'Integrations').click();
  await button(page, 'Rex automation').click();
  await page.getByRole('switch', {name: 'Title Rex tabs', exact: true}).check();
  await status(page, dirty).waitFor({state: 'attached'});
  await button(page, 'Save').click();
  await button(dialog(page, 'Save changes'), 'Save as global default').click();
  await status(page, 'Saved global pane preferences. Milo uses the title setting when naming Rex tabs.').waitFor();
  assert.equal(await reopenNote(page).count(), 0, 'title-only saves do not ask to reopen panes');
  await section(page, 'Layout').click();
  console.log('Usage tools: three choices saved globally and read back by doctor');
}

async function manageMenuApp(page, url, env) {
  await section(page, 'Panes').click();
  const menu = page.getByRole('region', {name: 'Menu bar app', exact: true});
  const readState = async () => {
    const response = await page.request.get(new URL('/api/state', url).href, {
      headers: {Authorization: `Bearer ${new URL(url).hash.slice(1)}`},
    });
    assert(response.ok(), 'the editor reports menu app availability');
    return response.json();
  };
  const {menubar} = await readState();
  assert.equal(menubar.indicator, 'quota', 'a fresh profile uses the quota indicator');
  const menuTab = section(page, 'Menu Bar');
  assert.equal(await menu.count(), 0, 'menu app settings have their own section');
  assert.equal(await menuTab.isVisible(), menubar.installed !== null, 'Menu Bar navigation follows server availability');
  const preferences = join(env.XDG_CONFIG_HOME, 'milo', 'preferences.conf');
  const previous = await readFile(preferences, 'utf8');

  const actions = [];
  let draft;
  let gate;
  const intercept = async route => {
    const action = route.request().postDataJSON();
    if (action.field === 'workflow.project_mode' || action.field === 'indicator') {
      if (action.field === 'indicator') await gate;
      const response = await route.fetch();
      assert(response.ok(), `the real editor accepted ${action.field}`);
      draft = await response.json();
      return route.fulfill({response, json: {...draft, menubar: {...draft.menubar, installed: false}}});
    }
    if (action.action !== 'menubar') return route.fallback();
    actions.push(action);
    await gate;
    await route.fulfill({json: {...draft, menubar: {...draft.menubar, installed: action.value === 'install'}}});
  };
  // Hold menu app replies so the dashboard is exercised without opening apps
  // or touching the logged-in user's services, even with a packaged installer.
  await page.route('**/api/action', intercept);
  const install = button(menu, 'Install');
  const update = button(menu, 'Open Milo');
  const remove = button(menu, 'Uninstall');
  const confirmation = dialog(page, 'Remove menu app?');
  const pin = page.getByRole('radio', {name: 'Pin project', exact: true});
  const perform = async (activate, pending, active, next, focus = next) => {
    let complete;
    gate = new Promise(resolve => { complete = resolve; });
    try {
      await activate();
      await status(menu, pending).waitFor();
      assert(await active.isDisabled(), 'menu actions cannot repeat while pending');
      assert(await active.evaluate(el => el === document.activeElement), 'pending control keeps focus');
      await active.press('Enter');
      await status(page, dirty).waitFor({state: 'attached'});
    } finally {
      complete();
    }
    await next.and(page.locator(':not([aria-disabled])')).waitFor();
    assert(await focus.evaluate(el => el === document.activeElement), 'completed action retains keyboard focus');
  };

  try {
    await pin.click();
    await status(page, dirty).waitFor({state: 'attached'});
    await menuTab.click();
    const radio = name => menu.getByRole('radio', {name, exact: true});
    assert(await radio('Agent quota').isChecked());
    // Choosing an indicator is a real save on macOS; the installer below is only simulated.
    if (menubar.installed !== null) {
      for (const [label, value] of [['Memory', 'memory'], ['CPU', 'cpu']]) {
        let complete;
        gate = new Promise(resolve => { complete = resolve; });
        const choice = radio(label);
        try {
          await choice.click();
          await status(menu, 'Saving menu bar indicator…').waitFor();
          assert(await choice.isChecked(), 'the pending indicator selection is visible');
          assert(await choice.evaluate(el => el === document.activeElement), 'the picker keeps keyboard focus');
        } finally {
          complete();
        }
        await status(menu, 'Saving menu bar indicator…').waitFor({state: 'hidden'});
        assert.equal((await readState()).menubar.indicator, value);
        await status(page, dirty).waitFor({state: 'attached'});
      }
    }
    assert.equal(await remove.count(), 0, 'nothing to remove before installation');
    await perform(() => install.click(), 'Installing menu app…', install, update, remove);
    await remove.waitFor();
    await perform(() => update.click(), 'Opening or updating menu app…', update, update);
    assert.equal(await confirmation.count(), 0, 'opening the installed app does not ask for removal');
    await perform(() => update.click(), 'Opening or updating menu app…', update, update);

    await remove.click();
    await button(confirmation, 'Keep editing').click();
    await confirmation.waitFor({state: 'hidden'});
    assert(await update.isVisible(), 'cancel left the app installed');

    await remove.click();
    await perform(() => button(confirmation, 'Remove menu app').click(), 'Removing menu app…', remove, install);
    assert.equal(await remove.count(), 0, 'confirmed removal hides Remove');
    assert.deepEqual(actions, ['install', 'install', 'install', 'uninstall'].map(value =>
      ({action: 'menubar', value, version: draft.version})));
  } finally {
    await page.unroute('**/api/action', intercept);
  }
  await section(page, 'Panes').click();
  assert(await pin.isChecked(), 'menu app operations retained the draft');
  assert.equal(await readFile(preferences, 'utf8'), previous, 'menu bar actions did not save the pane draft');
  await button(page, 'Discard').click();
  await button(dialog(page, 'Discard all unsaved changes?'), 'Discard changes').click();
  await status(page, clean).waitFor({state: 'attached'});
  console.log('Menu app: installed, opened or updated twice, kept on cancel, and removed; draft preserved');
  if (menubar.installed !== null) {
    await Promise.all([page.waitForResponse(response => response.url().endsWith('/api/state')), menuTab.click()]);
    const indicatorFile = join(env.XDG_CONFIG_HOME, 'milo', 'menubar.conf');
    const malformed = 'indicator = invalid\n';
    await writeFile(indicatorFile, malformed);
    await page.evaluate(() => window.dispatchEvent(new Event('focus')));
    await menu.getByText(/Couldn’t read the saved indicator\. The app uses Agent quota/).waitFor();
    assert.equal((await readState()).menubar.indicator, null, 'API cannot verify a malformed choice');
    assert.equal(await menu.getByRole('radio', {checked: true}).count(), 0, 'fallback is not a saved selection');
    assert.equal(await readFile(indicatorFile, 'utf8'), malformed, 'reading does not repair the indicator');
    const quota = menu.getByRole('radio', {name: 'Agent quota', exact: true});
    await quota.click();
    await status(menu, 'Saving menu bar indicator…').waitFor({state: 'hidden'});
    assert(await quota.isChecked(), 'the explicitly saved repair is selected');
    assert.equal((await readState()).menubar.indicator, 'quota');
    assert.equal(await readFile(indicatorFile, 'utf8'), 'indicator = quota\n');
    assert.equal(await readFile(preferences, 'utf8'), previous, 'repair does not write pane preferences');
  }
  await section(page, 'Layout').click();
}

// Stands in for the Discord app: listens where the presence helper looks,
// answers the handshake as Discord does, and keeps every frame it is sent.
async function standInForDiscord(folder) {
  const frames = [];
  let open = 0;
  const server = createServer(socket => {
    open += 1;
    let received = Buffer.alloc(0);
    socket.on('data', data => {
      received = Buffer.concat([received, data]);
      while (received.length >= 8 && received.length >= 8 + received.readUInt32LE(4)) {
        const opcode = received.readUInt32LE(0);
        const end = 8 + received.readUInt32LE(4);
        const payload = received.subarray(8, end).toString();
        received = received.subarray(end);
        frames.push([opcode, payload]);
        if (opcode === 0) {
          const ready = Buffer.from('{"cmd":"DISPATCH","data":{"v":1,"config":{}},"evt":"READY","nonce":null}');
          const header = Buffer.alloc(8);
          header.writeUInt32LE(1, 0);
          header.writeUInt32LE(ready.length, 4);
          socket.write(Buffer.concat([header, ready]));
        }
      }
    });
    socket.on('error', () => {});
    socket.on('close', () => { open -= 1; });
  });
  await mkdir(folder);
  server.listen(join(folder, 'discord-ipc-0'));
  await once(server, 'listening');
  return {frames, connections: () => open, close: () => server.close()};
}

async function until(happened, what) {
  const deadline = Date.now() + timeout;
  while (!happened()) {
    assert(Date.now() < deadline, what);
    await delay(20);
  }
}

async function turnPresenceOnAndOff(page, binary, cwd, env, discord) {
  const milo = '1573574411418599586';
  const own = '1234567890123456789';
  const file = join(env.XDG_CONFIG_HOME, 'milo', 'discord.conf');
  // The last thing each kind of frame said: who connected, and what Discord was told to show.
  const last = opcode => discord.frames.filter(([code, payload]) => code === opcode &&
    (opcode === 0 || payload.includes('"activity"'))).map(([, payload]) => JSON.parse(payload)).at(-1);
  const shown = () => last(1)?.args.activity;
  await section(page, 'Integrations').click();
  await button(page, 'Discord presence').click();
  const card = page.getByRole('region', {name: 'Discord presence', exact: true});
  const turnOn = card.getByRole('switch', {name: 'Discord presence', checked: false});
  const question = dialog(page, 'Turn on Discord presence?');
  const names = dialog(page, 'Show these names on Discord?');
  const choice = name => card.getByRole('radio', {name, exact: true});
  await card.getByText('Activity preview', {exact: true}).waitFor();
  await card.getByText('In a Rex session', {exact: true}).waitFor();
  await card.getByText('Milo is not connected to Discord and shows nothing there.').waitFor();
  assert(await choice('Generic').isChecked(), 'no names are shown unless chosen');

  await turnOn.click();
  await question.getByText('Milo sends nothing else: no session, tab, project or file names').waitFor();
  await button(question, 'Keep editing').click();
  await question.waitFor({state: 'hidden'});
  await assert.rejects(readFile(file), {code: 'ENOENT'}, 'reading what becomes public saves nothing');
  assert.equal(discord.frames.length, 0, 'Discord is not contacted before presence is turned on');

  // No application ID is asked for: Milo's own is used.
  await turnOn.click();
  await button(question, 'Turn on').click();
  await status(page, 'Discord presence is on. Discord is showing “In a Rex session”.').waitFor();
  await card.getByRole('switch', {name: 'Discord presence', checked: true}).waitFor();
  assert.equal(await readFile(file, 'utf8'), 'presence = true\n');
  await until(() => shown(), 'the helper introduced itself and set the activity');
  assert.deepEqual(last(0), {v: 1, client_id: milo});
  // Rex reports a session in front, and none of it is shown while no names are chosen.
  assert.deepEqual(shown(), {details: 'In a Rex session'}, 'the activity is one fixed line and nothing about the work');
  assert.equal(execFileSync(binary, ['presence'], {cwd, env, encoding: 'utf8'}),
    `Discord presence: on\nApplication: Milo's, ${milo}\nDetails: generic, which shows the fixed line and no names\n` +
    'Discord is showing “In a Rex session”.\n');

  // A name is not shown until the page has said what it shows and been told to go ahead.
  await choice('Session name').click();
  await names.getByText('They will also see the name of the Rex session in front on this computer').waitFor();
  await button(names, 'Keep editing').click();
  await names.waitFor({state: 'hidden'});
  assert(await choice('Generic').isChecked(), 'backing out keeps what was shown');
  assert.equal(await readFile(file, 'utf8'), 'presence = true\n');
  await choice('Session name').click();
  await button(names, 'Show names').click();
  await status(page, 'Discord presence shows the name of the session in front.').waitFor();
  await choice('Session name').and(page.locator(':checked')).waitFor({state: 'attached'});
  await button(card, 'What others can see').hover();
  await card.getByRole('tooltip').filter({hasText: 'May add the name of the Rex session in front.'}).waitFor();
  assert.equal(await readFile(file, 'utf8'), 'presence = true\ndetails = session\n');
  await until(() => shown()?.state === 'Session: smoke session', 'the session in front is named, and its folder is not');

  // Showing less asks nothing and takes the name down.
  await choice('Generic').click();
  await status(page, 'Discord presence shows the fixed line and no names.').waitFor();
  await until(() => shown() && !('state' in shown()), 'the name is gone');
  assert.equal(await readFile(file, 'utf8'), 'presence = true\n');

  // An application of your own replaces Milo's until you go back.
  assert.equal(await card.getByRole('textbox', {name: 'Application ID'}).count(), 0);
  const appSettings = button(page, 'Custom App Setup');
  await appSettings.click();
  const app = dialog(page, 'Discord app settings');
  await button(app, 'Use my own').click();
  await app.getByRole('alert').filter({hasText: 'Enter an Application ID of 17 to 19 digits.'}).waitFor();
  await app.getByRole('textbox', {name: 'Application ID', exact: true}).fill(own);
  await button(app, 'Use my own').click();
  await status(page, `Discord presence uses your application ${own}.`).waitFor();
  await until(() => last(0).client_id === own, 'the helper introduced the other application');
  assert.equal(await readFile(file, 'utf8'), `presence = true\napplication_id = ${own}\n`);
  await appSettings.click();
  await button(app, 'Use Milo’s application').click();
  await status(page, 'Discord presence uses Milo\'s application.').waitFor();
  await until(() => last(0).client_id === milo, 'the helper is back under Milo\'s application');
  assert.equal(await readFile(file, 'utf8'), 'presence = true\n');

  await card.getByRole('switch', {name: 'Discord presence', checked: true}).click();
  await status(page, 'Discord presence is off.').waitFor();
  await turnOn.waitFor();
  await until(() => discord.connections() === 0, 'turning presence off closed the connection to Discord');
  const [, final] = discord.frames.at(-1);
  assert(!final.includes('"activity"'), 'the activity was taken down before the connection closed');
  assert.equal(await readFile(file, 'utf8'), 'presence = false\n');
  assert.equal(await status(page, clean).count(), 1, 'presence is saved at once and leaves no draft');
  console.log('Discord presence: asked first, names only once chosen, own application and back, taken down when turned off');
  await section(page, 'Layout').click();
}

async function usageToolsPersisted(page) {
  await section(page, 'Usage').click();
  const expected = [['Codex', 'auto'], ['Claude Code', 'always'], ['OpenCode', 'hide']];
  for (const [tool, name] of expected) {
    assert.equal(await usageChoice(page, tool).inputValue(), name, `${tool} persisted as ${name}`);
  }
  await section(page, 'Layout').click();
  await page.getByRole('heading', {name: 'Pane layout', exact: true}).waitFor();
}

// One host stands for both ends: what it exports is edited by hand and imported again.
async function copySettingsThroughHosts(page, env, cwd) {
  const preferences = join(env.XDG_CONFIG_HOME, 'milo', 'preferences.conf');
  const before = await readFile(preferences, 'utf8');
  await section(page, 'Hosts').click();
  await page.getByRole('heading', {name: 'This host', exact: true}).waitFor();
  await page.getByText('milo web --host LABEL', {exact: true}).waitFor();
  const create = button(page, 'Create bundle');
  assert(await create.isDisabled(), 'nothing is exported until a group is chosen');
  const layout = page.getByRole('checkbox', {name: /^Layout Panes/});
  const exported = page.getByRole('textbox', {name: 'Exported settings bundle', exact: true});
  const copy = button(page, 'Copy bundle');
  await layout.check();
  await page.getByRole('radio', {name: 'This worktree', exact: true}).check();
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  await page.route('**/api/transfer', route => gate.then(() => route.continue()), {times: 1});
  const isExport = request => new URL(request.url()).pathname === '/api/transfer' && request.postDataJSON().action === 'export';
  const received = page.waitForResponse(response => isExport(response.request()));
  try {
    const sent = page.waitForRequest(isExport);
    await create.click();
    assert.deepEqual((await sent).postDataJSON(), {action: 'export', groups: ['layout'], scope: 'worktree'});
    await page.getByRole('radio', {name: 'Global defaults', exact: true}).check();
  } finally {
    release();
  }
  assert.match((await (await received).json()).bundle, /^\[project\]$/m, 'the delayed response contains project settings');
  await page.locator('#export-create:enabled').waitFor();
  assert.equal(await exported.count(), 0, 'the old scope’s bundle stays hidden after the request settles');
  assert.equal(await copy.count(), 0, 'the old scope’s bundle cannot be copied');
  await layout.uncheck();
  await page.getByRole('checkbox', {name: /^Usage display/}).check();
  await create.click();
  // Export focuses the result on the next animation frame; finish that interaction before typing into Import.
  await exported.and(page.locator(':focus')).waitFor();
  const bundle = await exported.inputValue();
  assert.match(bundle, /^milo-settings 1\n/);
  assert.match(bundle, /^usage_claude = always$/m);
  assert.doesNotMatch(bundle, /tab_titles|deepseek|icons/, 'only the chosen group, and no integration or account');
  assert.doesNotMatch(bundle, /^\[project\]$/m, 'the replacement export uses the current scope');

  const incoming = `${bundle.replace('usage_claude = always', 'usage_claude = hide')}tab_titles = false\nusage_deepseek = always\n`
    + '\n[project]\nname = example\nusage_claude = hide\n';
  await page.getByRole('textbox', {name: /^Paste a bundle/}).fill(incoming);
  const importButton = button(page, 'Import 1 setting');
  assert.equal(await importButton.count(), 0, 'nothing can be imported before a preview');
  const previewButton = button(page, 'Preview');
  let finishPreview;
  const previewGate = new Promise(resolve => { finishPreview = resolve; });
  await page.route('**/api/transfer', route => previewGate.then(() => route.continue()), {times: 1});
  try {
    await previewButton.press('Enter');
    await previewButton.and(page.locator('[aria-disabled="true"]')).waitFor();
    assert(await previewButton.evaluate(el => el === document.activeElement), 'Preview retains focus while loading');
  } finally {
    finishPreview();
  }
  await status(page, 'Preview ready. 1 setting would change.').waitFor();
  assert(await previewButton.evaluate(el => el === document.activeElement), 'Preview remains focused after the response');
  await page.keyboard.press('Tab');
  assert(await importButton.evaluate(el => el === document.activeElement), 'one Tab moves from Preview to Import');
  const preview = page.getByRole('group', {name: 'What this import changes', exact: true});
  const row = preview.getByRole('row', {name: /^Claude Code in Usage/});
  await row.waitFor();
  assert.deepEqual(await row.getByRole('cell').allTextContents(), ['always', 'hide']);
  await preview.getByText('Left out: tab_titles (set on each host, never imported)').waitFor();
  await preview.getByText('Left out: usage_deepseek (set on each host, never imported)').waitFor();
  const map = button(preview, `Apply to ${basename(cwd)}`);
  await map.press('Enter');
  await status(page, 'Preview ready. 2 settings would change.').waitFor();
  assert(await previewButton.evaluate(el => el === document.activeElement), 'mapping does not lose keyboard focus');
  await button(preview, 'Don’t apply').press('Enter');
  await status(page, 'Preview ready. 1 setting would change.').waitFor();
  assert(await previewButton.evaluate(el => el === document.activeElement), 'unmapping does not lose keyboard focus');
  assert.equal(await readFile(preferences, 'utf8'), before, 'a preview saves nothing');

  await page.keyboard.press('Tab');
  assert(await importButton.evaluate(el => el === document.activeElement), 'one Tab also reaches Import after unmapping');
  await page.keyboard.press('Enter');
  await status(page, 'Imported 1 setting. Undo is in Hosts if you change your mind.').waitFor();
  const imported = await readFile(preferences, 'utf8');
  assert.match(imported, /^usage_claude = hide$/m);
  assert.match(imported, /^tab_titles = true$/m, 'the integration switch stayed as this host had it');
  assert.doesNotMatch(imported, /deepseek/);

  await button(page, 'Undo import').click();
  await button(dialog(page, 'Take back the last import?'), 'Undo import').click();
  await status(page, 'The last import was taken back.').waitFor();
  assert.equal(await readFile(preferences, 'utf8'), before, 'undo restored the file byte for byte');
  assert.equal(await button(page, 'Undo import').count(), 0);
  await section(page, 'Layout').click();
  await page.getByRole('heading', {name: 'Pane layout', exact: true}).waitFor();
  console.log('Hosts: a chosen group exported, previewed, imported without its integration lines, and undone');
}

async function pressAfterNumberEdit(page) {
  const field = number(page, 'Rail width');
  const right = page.getByRole('radio', {name: 'Right', exact: true});
  await field.fill('');
  await field.pressSequentially('41');
  const box = await right.boundingBox();
  assert(box, 'Rail side is on screen');
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  // Hold long enough for an eager draft update to redraw the panel under the pointer.
  await delay(250);
  await page.mouse.up();
  await page.getByRole('radio', {name: 'Right', exact: true, checked: true}).waitFor({state: 'attached'});
  await status(page, dirty).waitFor({state: 'attached'});
  assert.equal(await field.inputValue(), '41', 'the blurred Rail width reached the draft');
  console.log('Rail side: the first press after a number edit was not lost');

  await field.fill('');
  await button(page, 'Usage').click();
  assert.equal(await page.getByRole('heading', {name: 'Pane layout', exact: true}).count(), 1,
    'an invalid number keeps Layout open');
  assert(await field.evaluate(input => input === document.activeElement && !input.validity.valid),
    'the invalid Rail width is focused');
  console.log('Invalid number: leaving Layout pointed back at the field');
}

async function movePaneDuringPendingEdit(page) {
  const routePattern = '**/api/action';
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  const hold = route => gate.then(() => route.continue());
  await page.route(routePattern, hold, {times: 1});
  try {
    const move = button(page, 'Move Outline later');
    const sent = page.waitForRequest(request => new URL(request.url()).pathname === '/api/action');
    await move.click();
    await sent;
    await move.click();
  } finally {
    release();
  }
  await status(page, dirty).waitFor({state: 'attached'});
  const panes = page.getByRole('group', {name: 'Panes in rail order', exact: true});
  const order = await panes.getByRole('checkbox').evaluateAll(inputs => inputs.map(input => input.labels[0].textContent));
  assert.deepEqual(order, ['History', 'Usage', 'Outline'], 'both moves apply in order');
}

async function discardFocusedNumber(page) {
  const field = number(page, 'Rail width');
  const original = await field.inputValue();
  await field.fill('42');
  const box = await button(page, 'Discard').boundingBox();
  assert(box, 'Discard is on screen');
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  const confirmation = dialog(page, 'Discard all unsaved changes?');
  await button(confirmation, 'Discard changes').click();
  await status(page, clean).waitFor({state: 'attached'});
  assert.equal(await field.inputValue(), original, 'discard did not reapply the number after blur');
}

async function rejectStaleDone(context, current, url) {
  const confirmation = dialog(current, 'Close the dashboard?');
  const field = number(current, 'Rail width');
  await field.fill('42');
  const box = await button(current, 'Done').boundingBox();
  assert(box, 'Done is on screen');
  await current.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  await button(confirmation, 'Discard and close').waitFor();
  const second = await openEditor(context, url);

  const right = second.getByRole('radio', {name: 'Right', exact: true});
  await right.click();
  await status(second, dirty).waitFor({state: 'attached'});
  await current.bringToFront();

  const [response] = await Promise.all([
    current.waitForResponse(response => new URL(response.url()).pathname === '/api/close'),
    button(confirmation, 'Discard and close').click(),
  ]);
  assert.equal(response.status(), 409, 'stale Done must not close the session');
  await current.getByText(/The draft changed in another request/).first().waitFor();
  await button(current, 'Refresh editor state').click();
  await current.getByRole('radio', {name: 'Right', exact: true, checked: true}).waitFor();
  assert.equal(await field.inputValue(), '42', 'refresh kept the local number after Done was rejected');
  console.log('Stale Done: the second tab’s draft survived and was visible after refresh');

  // Another tab commits a newer draft. Dropping the rejected local edit should
  // show that draft immediately, without resubmitting the dropped value.
  const outline = {name: 'Outline', exact: true};
  const changed = second.waitForResponse(response => new URL(response.url()).pathname === '/api/action');
  await second.getByRole('checkbox', outline).uncheck();
  assert((await changed).ok(), 'the other tab made another change');
  await current.bringToFront();
  await current.getByRole('radio', {name: 'Left', exact: true}).click();
  await button(current, 'Drop this edit').click();
  await current.getByRole('checkbox', {...outline, checked: false}).waitFor();
  assert(await current.getByRole('radio', {name: 'Right', exact: true}).isChecked(), 'the stale edit was dropped');
  assert.equal(await field.inputValue(), '42', 'dropping one edit kept the independent local number');
}

// 320 CSS pixels is a 1280-pixel window at 400% zoom. A detail page there must wrap its
// heading, not push its controls out to the side of the panel.
async function integrationDetailsFitNarrowPanel(page) {
  await page.setViewportSize({width: 320, height: 700});
  await section(page, 'Integrations').click();
  for (const name of ['Rex automation', 'Discord presence', 'Alert forwarding']) {
    await button(page, name).click();
    await page.getByRole('heading', {name, exact: true}).waitFor();
    const hidden = await page.locator('#settings-panel').evaluate(panel => panel.scrollWidth - panel.clientWidth);
    assert.equal(hidden, 0, `${name} fits a narrow panel without scrolling sideways`);
    await button(page, 'Back to integrations').click();
  }
  await section(page, 'Layout').click();
}

async function confirmFromMobileFooter(browser, url, exited) {
  const viewport = {width: 390, height: 700};
  const context = await browser.newContext({viewport, isMobile: true, hasTouch: true});
  context.setDefaultTimeout(timeout);
  const page = await openEditor(context, url);
  await page.setViewportSize({width: 260, height: viewport.height});
  const dock = await page.locator('.control-dock').boundingBox();
  assert(dock && dock.x >= 0 && dock.x + dock.width <= 260, 'navigation and draft controls fit at narrow widths');
  await integrationDetailsFitNarrowPanel(page);
  await page.setViewportSize(viewport);
  const done = button(page.getByRole('region', {name: 'Project session'}), 'Done');
  const box = await done.boundingBox();
  assert(box && box.x >= 0 && box.x + box.width <= viewport.width &&
    box.y >= 0 && box.y + box.height <= viewport.height, 'mobile session Done fits in the viewport');

  const confirmation = dialog(page, 'Close the dashboard?');
  await done.tap();
  await button(confirmation, 'Keep editing').tap();
  await confirmation.waitFor({state: 'hidden'});
  await status(page, dirty).waitFor({state: 'attached'});

  await done.tap();
  await button(confirmation, 'Discard and close').tap();
  await page.getByRole('heading', {name: 'Settings closed', exact: true}).waitFor();
  const result = await Promise.race([exited, delay(timeout, null, {ref: false})]);
  assert.deepEqual(result, [0, null], 'confirming from the session bar stops Milo cleanly');
  console.log('Mobile Done: cancel retained the draft, confirm closed the editor');
}

async function checkHostedConnections(page, destination) {
  const editorUrl = new URL(page.url());
  const authorization = `Bearer ${editorUrl.hash.slice(1)}`;
  const initial = await page.request.get(new URL('/api/state', editorUrl).href, {headers: {authorization}});
  assert(initial.ok(), 'the local editor supplied fixture state');
  let state = await initial.json();
  const initialVersion = state.version;
  const telegramUrl = `https://t.me/MiloSmokeBot?start=${'t'.repeat(43)}`;
  const discordPath = `/discord/start/${'d'.repeat(43)}`;
  const finishCode = 'ABCD1234';
  const providers = [
    {name: 'Telegram', provider: 'telegram', manual: 'Use your own bot', input: 'Bot token',
      url: telegramUrl, localUrl: telegramUrl, chat: '1234',
      label: 'Telegram DM · 1234', credential: `milorelay_prod_${'p'.repeat(43)}`},
    {name: 'Discord', provider: 'discord', manual: 'Use your own webhook', input: 'Webhook link',
      url: `https://connect.usemilo.net${discordPath}`, localUrl: `http://127.0.0.1:6120${discordPath}`, chat: '',
      label: 'Discord channel · 5678', credential: 'https://discord.com/api/webhooks/99999/synthetic_SMOKE_fixture-00'},
  ];
  const context = page.context();
  const launches = [];
  const providerUrls = new Set(providers.flatMap(({url, localUrl}) => [url, localUrl]));
  const outsideEditor = url => url.origin !== editorUrl.origin;
  const openProvider = async route => {
    const url = route.request().url();
    if (route.request().isNavigationRequest()) launches.push(url);
    if (!providerUrls.has(url)) return route.abort();
    await route.fulfill({contentType: 'text/html', body: '<title>Provider fixture</title><link rel="icon" href="data:,">'});
  };
  const hostedPath = '**/api/hosted-connect';
  const actionPath = '**/api/action';
  const statePath = '**/api/state';
  const responses = [];
  const saved = [];
  const scoped = [];
  const alertConsents = {telegram: 'a'.repeat(32), discord: 'b'.repeat(32)};
  let hostedRequests = 0;
  const editorState = route => route.fulfill({json: state});
  const hosted = async route => {
    const request = route.request();
    assert.equal(request.method(), 'POST');
    assert.equal(request.headers().authorization, authorization, 'hosted setup uses the local session key');
    hostedRequests++;
    const next = responses.shift();
    assert(next, 'hosted requests require a user action');
    const body = request.postDataJSON();
    assert.deepEqual(body, next.request);
    if (!next.save) return route.fulfill(await next.response);
    if (next.waitForSave) await next.waitForSave;
    const {provider, chat} = next.save;
    saved.push(body);
    const alerts = {...state.alerts};
    alerts.destination ??= provider;
    alerts[provider] = {saved: true, active: false, consent: alertConsents[provider],
      ...(provider === 'telegram' ? {chat, needs_local_forget: false} : {})};
    state = {...state, version: state.version + 1, alerts, message: null};
    await route.fulfill({json: state});
  };
  const alertAction = async route => {
    const action = route.request().postDataJSON();
    if (action.action !== 'alerts') return route.fallback();
    const {field, value} = action;
    if (field === 'refresh') {
      assert.equal(value, null);
      return editorState(route);
    }
    const alerts = {...state.alerts};
    let message = null;
    if (field === 'forwarding') {
      const {destination, enabled, consent} = value;
      assert.equal(consent, enabled ? alertConsents[destination] : undefined);
      scoped.push({field, value});
      assert(alerts[destination].saved, 'only a saved provider can be toggled');
      alerts[destination] = {...alerts[destination], active: enabled};
      alerts.running = alerts.enabled = alerts.telegram.active || alerts.discord.active;
    } else if (field === 'test') {
      scoped.push({field, value});
      const provider = providers.find(({provider}) => provider === value);
      assert(provider && alerts[value].saved, 'the test targets a saved provider');
      message = {text: `${provider.name} accepted the test message.`, error: false};
    } else if (field === 'forget_local') {
      assert.equal(value, 'telegram', 'local forget is scoped to Telegram');
      assert(alerts.telegram.needs_local_forget);
      scoped.push({field, value});
      alerts.telegram = {saved: false, active: false, chat: '', consent: null, needs_local_forget: false};
      alerts.destination = 'discord';
      message = {text: 'Removed the locally saved Telegram connection.', error: false};
    } else {
      assert.fail(`unexpected alert action during hosted setup: ${field}`);
    }
    state = {...state, version: state.version + 1, alerts, message};
    await route.fulfill({json: state});
  };
  await context.route(outsideEditor, openProvider);
  await page.route(hostedPath, hosted);
  await page.route(actionPath, alertAction);
  try {
    for (const [index, {name, provider, manual, input, url, localUrl, chat, label, credential}] of providers.entries()) {
      const wizard = dialog(page, `Destinations / ${name}`);
      const connect = button(wizard, 'Connect with Milo');
      const check = button(wizard, 'Check connection');
      const finish = wizard.getByLabel('Finish code');
      const approve = button(wizard, `Save ${name}`);
      const proposal = wizard.getByRole('heading', {name: 'Proposed destination'});
      const requestsBeforeOpen = hostedRequests;
      await button(destination, index === 0 ? 'Set up destination' : 'Manage destinations').click();
      await button(page, name).click();
      assert(await connect.isVisible(), 'hosted setup is offered in both destination editors');
      assert.equal(await wizard.getByRole('textbox').count(), 0, 'credentials are not requested up front');
      assert.equal(hostedRequests, requestsBeforeOpen, 'opening an editor makes no hosted request');

      const start = {action: 'start', provider};
      const diagnostic = 'private service diagnostic';
      responses.push({request: start, response: {status: 503, json: {error: diagnostic}}});
      await connect.click();
      await status(wizard, 'Connect with Milo is unavailable').waitFor();
      assert.equal(await wizard.getByText(diagnostic).count(), 0, 'service diagnostics are not displayed');
      await button(wizard, manual).click();
      await wizard.getByRole('textbox', {name: input, exact: true}).waitFor();
      await button(wizard, 'Connection options').click();

      const unsafeUrl = index === 0 ? `https://t.me.example.invalid/MiloSmokeBot?start=${'t'.repeat(43)}` : localUrl;
      const launchCount = launches.length;
      responses.push({request: start, response: {json: {deviceKey: 'u'.repeat(43), url: unsafeUrl, mode: 'production'}}});
      await connect.click();
      await status(wizard, 'Connect with Milo is unavailable').waitFor();
      assert.equal(launches.length, launchCount, 'an untrusted or downgraded pairing URL is not opened');

      const startPair = async (deviceKey, mode) => {
        const pairingUrl = mode === 'production' ? url : localUrl;
        responses.push({request: start, response: {json: {deviceKey, url: pairingUrl, mode}}});
        const [popup] = await Promise.all([context.waitForEvent('page'), connect.click()]);
        await popup.waitForURL(pairingUrl);
        assert(await popup.evaluate(() => window.opener === null), 'the provider page cannot access the local editor');
        await popup.close();
        await check.waitFor();
        assert.equal(await finish.count(), mode === 'production' ? 1 : 0, 'only production requires a finish code');
        if (mode === 'production') {
          assert.equal(await finish.getAttribute('autocomplete'), 'one-time-code');
          assert(await finish.evaluate(input => input === document.activeElement), 'the finish field is focused on return');
        }
        assert.equal(responses.length, 0, 'the start request completed without automatically checking or saving');
      };
      const preview = async (deviceKey, response, code, activate = () => check.click()) => {
        responses.push({request: {action: 'preview', provider, deviceKey, ...(code ? {finishCode: code} : {})}, response});
        await activate();
      };
      const paste = text => finish.evaluate((input, text) => {
        const clipboardData = new DataTransfer();
        clipboardData.setData('text/plain', text);
        input.dispatchEvent(new ClipboardEvent('paste', {bubbles: true, cancelable: true, clipboardData}));
      }, text);
      const autofill = text => finish.evaluate((input, {text, inputType}) => {
        input.value = text;
        // Browsers report autofill as an input event, sometimes without an inputType.
        input.dispatchEvent(inputType ? new InputEvent('input', {bubbles: true, inputType}) : new Event('input', {bubbles: true}));
      }, {text, inputType: index === 0 ? 'insertReplacementText' : ''});
      const copiedCode = ` \n${finishCode.toLowerCase()}\r\n`;
      if (index === 0) {
        const deviceKey = 'e'.repeat(43);
        await startPair(deviceKey, 'production');
        await finish.fill(finishCode);
        await preview(deviceKey, {status: 404, json: {error: diagnostic}}, finishCode);
        await status(wizard, 'This connection expired or was already used').waitFor();
        assert.equal(await check.count() + await approve.count(), 0, 'expired pairing must start over');
      }
      for (const attempt of ['local', 'cancel', 'save']) {
        const production = attempt !== 'local';
        const deviceKey = attempt[0].repeat(43);
        await startPair(deviceKey, production ? 'production' : 'local');
        if (!production) {
          if (index === 1) {
            await preview(deviceKey, {status: 502, json: {error: diagnostic}});
            await status(wizard, 'Couldn’t check the connection').waitFor();
            assert.equal(await wizard.getByText(diagnostic).count(), 0);
          }
          await preview(deviceKey, {status: 202, json: {pending: true}});
          await status(wizard, 'Not connected yet').waitFor();
          assert.equal(await approve.count(), 0, 'pending setup cannot be saved');
        } else if (attempt === 'save') {
          const requestsBeforeCode = hostedRequests;
          await check.click();
          await status(wizard, 'Enter the eight-character finish code').waitFor();
          assert.equal(hostedRequests, requestsBeforeCode, 'production sends no preview without an entered code');
          await finish.pressSequentially('deadbeef');
          assert.equal(hostedRequests, requestsBeforeCode, 'typing a code still requires Check or Enter');
          await preview(deviceKey, {status: 400, json: {error: diagnostic}}, 'DEADBEEF', () => finish.press('Enter'));
          await status(wizard, 'The finish code did not match').waitFor();
          assert.equal(await wizard.getByText(diagnostic).count(), 0);
          if (index === 0) {
            await finish.fill(finishCode);
            await preview(deviceKey, {json: {provider, label, credential, chat}}, finishCode);
            await status(wizard, 'Couldn’t check the connection').waitFor();
            assert.equal(await approve.count(), 0, 'review must never accept a credential');
          }
          await finish.focus();
          await preview(deviceKey, {json: {provider, label, chat}}, finishCode, () => autofill(copiedCode));
        } else {
          const requestsBeforePaste = hostedRequests;
          await paste(`${finishCode}A`);
          await status(wizard, 'Enter the eight-character finish code').waitFor();
          assert.equal(await finish.inputValue(), '', 'malformed clipboard content is not retained or truncated to a code');
          assert.equal(hostedRequests, requestsBeforePaste, 'malformed clipboard content sends no preview');

          let complete;
          const pending = new Promise(resolve => { complete = resolve; });
          try {
            await preview(deviceKey, pending, finishCode, () => paste(copiedCode));
            await status(wizard, 'Checking the connection').waitFor();
            await finish.press('Enter');
            await paste(copiedCode);
          } finally {
            complete({json: {provider, label, chat}});
          }
          await proposal.waitFor();
          assert.equal(hostedRequests, requestsBeforePaste + 1, 'pasting and pressing Enter during a preview makes only one request');
        }
        if (!production) await preview(deviceKey, {json: {provider, label, chat}});
        await proposal.waitFor();
        assert(await proposal.evaluate(heading => heading === document.activeElement), 'review focuses the proposed destination');
        await wizard.getByText(label, {exact: true}).waitFor();
        await wizard.getByText('Saving does not turn forwarding on.', {exact: true}).waitFor();
        assert.equal(saved.length, index, 'previewing a destination does not save it');
        const markup = await wizard.innerHTML();
        assert(!markup.includes(credential) && !markup.includes(deviceKey) && !markup.includes(finishCode),
          'the proposal never renders secrets');
        assert.equal(page.url(), editorUrl.href, 'the finish code never enters the local URL');
        assert.equal(await finish.count(), 0, 'review unmounts the finish code input');
        if (attempt !== 'save') {
          const requestsBeforeCancel = hostedRequests;
          if (index === 0) await button(wizard, 'Cancel').click();
          else {
            await button(wizard, 'Back to destinations').click();
            await button(page, name).click();
          }
          await connect.waitFor();
          assert.equal(await proposal.count() + await approve.count(), 0, 'cancelling or going back discards the proposal');
          assert.equal(hostedRequests, requestsBeforeCancel, 'cancel leaves a reviewed pairing unclaimed');
        } else {
          const request = {action: 'save', provider, deviceKey, finishCode, label, version: state.version};
          responses.push({request, response: {status: 502, json: {error: diagnostic}}});
          await approve.click();
          await status(wizard, 'Your proposed destination hasn’t changed').waitFor();
          await proposal.waitFor();
          assert.equal(saved.length, index, 'a transient Save failure retains the reviewed destination');
          if (index === 0) {
            let releaseSave;
            const waitForSave = new Promise(resolve => { releaseSave = resolve; });
            responses.push({request, save: {provider, chat}, waitForSave});
            try {
              await approve.click();
              await status(wizard, 'Saving the destination').waitFor();
              await button(wizard, 'Close destinations').click();
              await page.locator('#alerts-provider-setup').waitFor({state: 'detached'});
            } finally {
              releaseSave();
            }
            await button(destination, 'Manage destinations').click();
            await button(page, name).click();
          } else {
            responses.push({request, save: {provider, chat}});
            await approve.click();
          }
          await wizard.getByText(chat ? `Saved destination · ${chat}` : 'Saved destination', {exact: true}).waitFor();
          assert.equal(await proposal.count(), 0, 'saving clears the proposal');
        }
      }
      await button(wizard, `Forget ${name}…`).click();
      await button(dialog(page, `Forget ${name}?`), 'Keep editing').click();
      await button(wizard, 'Back to destinations').click();
      const manager = dialog(page, 'Destinations');
      await manager.getByRole('switch', {name: `Forward to ${name}`, checked: false}).waitFor();
      await button(manager, `Send test to ${name}`).waitFor();
      await button(manager, 'Close destinations').click();
    }
    assert.equal(responses.length, 0);
    assert.deepEqual(launches, [providers[0].url, ...providers.flatMap(({url, localUrl}) => [localUrl, url, url])],
      'only approved launches reached the provider fixtures');
    assert.deepEqual(saved, providers.map(({provider, label}, index) => ({
      action: 'save', provider, deviceKey: 's'.repeat(43), finishCode, label, version: initialVersion + index,
    })), 'only explicit Save finalizes; the browser never submits provider credentials');

    await button(destination, 'Manage destinations').click();
    const manager = dialog(page, 'Destinations');
    await button(manager, 'Send test to Discord').click();
    await status(manager, 'Discord accepted the test message.').waitFor();
    await button(manager, 'Send test to Telegram').click();
    await status(manager, 'Telegram accepted the test message.').waitFor();
    const confirmation = dialog(page, 'Turn on alert forwarding?');
    for (const [name, target] of [['Telegram', 'Telegram chat 1234'], ['Discord', 'Discord channel']]) {
      await manager.getByRole('switch', {name: `Forward to ${name}`, checked: false}).click();
      await confirmation.getByText(`Destination: ${target}.`, {exact: false}).waitFor();
      await button(confirmation, 'Turn on').click();
      await manager.getByRole('switch', {name: `Forward to ${name}`, checked: true}).waitFor();
    }
    await manager.getByRole('switch', {name: 'Forward to Telegram', checked: true}).click();
    await manager.getByRole('switch', {name: 'Forward to Telegram', checked: false}).waitFor();
    assert(await manager.getByRole('switch', {name: 'Forward to Discord'}).isChecked(),
      'turning Telegram off keeps Discord on');
    assert.deepEqual(scoped, [
      {field: 'test', value: 'discord'},
      {field: 'test', value: 'telegram'},
      {field: 'forwarding', value: {destination: 'telegram', enabled: true, consent: alertConsents.telegram}},
      {field: 'forwarding', value: {destination: 'discord', enabled: true, consent: alertConsents.discord}},
      {field: 'forwarding', value: {destination: 'telegram', enabled: false}},
    ]);

    // Another local editor restored an old credential just before the user tried to reconnect.
    await button(manager, 'Telegram').click();
    const telegram = dialog(page, 'Destinations / Telegram');
    const connect = button(telegram, 'Connect with Milo');
    const launchCount = launches.length;
    state = {...state, alerts: {...state.alerts, telegram: {...state.alerts.telegram, needs_local_forget: true}}};
    await page.route(statePath, editorState);
    responses.push({request: {action: 'start', provider: 'telegram'}, response: {status: 409}});
    await connect.click();
    const forgetLocal = button(telegram, 'Forget on this host');
    await forgetLocal.waitFor();
    assert.equal(await connect.count(), 0, 'legacy Telegram cannot start another pairing');
    assert.equal(launches.length, launchCount, 'the blocked start does not open a provider page');
    await forgetLocal.click();
    const forget = dialog(page, 'Forget Telegram on this host?');
    await forget.getByText(/stops its forwarding on this host/).waitFor();
    await forget.getByText(/does not revoke the connection on the relay/).waitFor();
    await forget.getByText(/send \/stop/).waitFor();
    const actionsBeforeCancel = scoped.length;
    await button(forget, 'Keep editing').click();
    assert.equal(scoped.length, actionsBeforeCancel);
    // The same recovery in a forwarded editor must name the affected host.
    state = {...state, host: {name: 'studio', forwarded: true, rex: null}};
    await page.reload();
    await page.getByText('studio · forwarded', {exact: true}).waitFor();
    await section(page, 'Integrations').click();
    await button(page, 'Alert forwarding').click();
    await button(destination, 'Manage destinations').click();
    await button(manager, 'Telegram').click();
    await telegram.getByText(/forget the connection on “studio”/).waitFor();
    await button(telegram, 'Forget on “studio”').click();
    const forgetForwarded = dialog(page, 'Forget Telegram on “studio”?');
    await forgetForwarded.getByText(/stops its forwarding on “studio”/).waitFor();
    await forgetForwarded.getByText(/does not revoke the connection on the relay/).waitFor();
    assert.doesNotMatch(await forgetForwarded.innerText(), /this Mac|this host/);
    await button(forgetForwarded, 'Forget on “studio”').click();
    await connect.waitFor();
    assert.deepEqual(scoped.at(-1), {field: 'forget_local', value: 'telegram'});
    await button(telegram, 'Back to destinations').click();
    assert(await manager.getByRole('switch', {name: 'Forward to Discord'}).isChecked(),
      'forgetting Telegram locally keeps Discord forwarding');
    await button(manager, 'Discord').click();
    const discord = dialog(page, 'Destinations / Discord');
    assert.equal(await button(discord, 'Forget on “studio”').count(), 0, 'Discord has no local-only removal action');
    await button(discord, 'Forget Discord…').waitFor();
    await button(discord, 'Back to destinations').click();
    await button(manager, 'Close destinations').click();
  } finally {
    await context.unroute(outsideEditor, openProvider);
    await page.unroute(hostedPath, hosted);
    await page.unroute(actionPath, alertAction);
    await page.unroute(statePath, editorState);
  }
}

async function checkTelegramWizard(page, destination) {
  const url = new URL(page.url());
  const authorization = `Bearer ${url.hash.slice(1)}`;
  const initial = await page.request.get(new URL('/api/state', url).href, {headers: {authorization}});
  assert(initial.ok(), 'the real editor supplied the initial fixture state');
  let state = await initial.json();
  const initialVersion = state.version;
  const fakeToken = 'smoke-test-token-not-a-credential';
  const chatId = '-1001234567890';
  const providerError = 'provider diagnostic must not appear in the dashboard';
  const rejectedChat = '@not-a-channel';
  const saveError = 'Enter a valid Telegram chat ID or @channel.';
  const replies = [
    {json: {chats: [{id: '42', title: 'Direct chat'}, {id: chatId, title: 'Build room'}]}},
    {json: {chats: []}},
    {status: 502, json: {error: providerError}},
    {status: 409, json: {error: providerError}},
  ];
  let releaseLookup;
  const lookupGate = new Promise(resolve => { releaseLookup = resolve; });
  let lookups = 0;
  const received = [];
  const lookupPath = '**/api/telegram-chats';
  const actionPath = '**/api/action';
  const discover = async route => {
    const request = route.request();
    assert.equal(request.method(), 'POST');
    assert(request.headers().authorization === authorization, 'chat lookup uses the editor session key');
    assert.deepEqual(request.postDataJSON(), {token: fakeToken});
    const reply = replies[lookups++];
    assert(reply, 'unexpected chat lookup');
    if (lookups === 1) await lookupGate;
    await route.fulfill(reply);
  };
  const saveTelegram = async route => {
    const action = route.request().postDataJSON();
    if (action.action !== 'alerts') return route.fallback();
    if (action.field === 'refresh') {
      state = {...state, version: state.version + 1};
      return route.fulfill({json: state});
    }
    if (action.field !== 'telegram') return route.fallback();
    received.push(action);
    if (action.value.chat === rejectedChat) return route.fulfill({status: 400, json: {error: saveError}});
    state = {...state, version: state.version + 1, alerts: {...state.alerts,
      destination: 'telegram', telegram: {saved: true, active: false, chat: action.value.chat,
        consent: null, needs_local_forget: false}}};
    await route.fulfill({json: state});
  };
  // No discovery or Telegram save reaches Telegram, even with a packaged editor.
  await page.route(lookupPath, discover);
  await page.route(actionPath, saveTelegram);
  try {
    const wizard = dialog(page, 'Destinations / Telegram');
    const token = wizard.getByRole('textbox', {name: 'Bot token', exact: true});
    const chat = wizard.getByRole('combobox', {name: 'Telegram chat', exact: true});
    const manualChat = wizard.getByRole('textbox', {name: 'Chat ID or @channel', exact: true});
    const find = button(wizard, 'Find chats');
    const manual = button(wizard, 'Enter chat manually');
    const save = button(wizard, 'Save Telegram');
    const telegram = button(page, 'Telegram');
    const open = async name => {
      await button(destination, name).click();
      await telegram.click();
      await button(wizard, 'Use your own bot').click();
    };
    const assertReset = async () => {
      assert.equal(await token.inputValue(), '', 'starting over clears the token');
      assert.equal(await chat.count() + await manualChat.count(), 0, 'starting over clears the chat step');
    };

    await open('Set up destination');
    await manual.click();
    assert(await manualChat.evaluate(element => element === document.activeElement), 'manual entry receives keyboard focus');
    await token.fill(fakeToken);
    await manualChat.fill('@not-saved');
    await button(wizard, 'Back to destinations').click();
    assert(await telegram.evaluate(element => element === document.activeElement), 'Back restores focus to Telegram');
    await telegram.click();
    await button(wizard, 'Use your own bot').click();
    await assertReset();

    await token.fill(` ${fakeToken} `);
    await find.click();
    await status(wizard, 'Looking for recent chats').waitFor();
    assert(await find.isDisabled(), 'lookup cannot repeat while pending');
    assert(await manual.isEnabled(), 'manual entry remains available during lookup');
    releaseLookup();
    await chat.waitFor();
    assert.equal(await chat.inputValue(), '', 'discovery does not select a chat for the user');
    await chat.selectOption({label: `Build room · ${chatId}`});
    await save.click();
    await wizard.getByText(`Saved destination · ${chatId}`, {exact: true}).waitFor();
    await assertReset();

    for (const [index, text] of ['No recent chats found', 'Couldn’t find chats', 'Couldn’t find chats'].entries()) {
      await token.fill(fakeToken);
      await find.click();
      await status(wizard, text).waitFor();
      assert.equal(await wizard.getByText(providerError).count(), 0, 'lookup never displays provider errors');
      await manual.click();
      await manualChat.fill(index === 2 ? rejectedChat : '-1009876543210');
      if (index === 2) {
        await save.click();
        await status(wizard, saveError).waitFor();
        assert.equal(await token.inputValue(), fakeToken, 'a refused save retains the bot token');
        assert.equal(await manualChat.inputValue(), rejectedChat, 'a refused save retains the chat to correct');
        await manualChat.fill('@public_channel');
        await save.click();
        await wizard.getByText('Saved destination · @public_channel', {exact: true}).waitFor();
        await assertReset();
      } else {
        if (index === 0) await button(wizard, 'Close destinations').click();
        else await page.keyboard.press('Escape');
        await wizard.waitFor({state: 'hidden'});
        await open('Manage destinations');
        await assertReset();
      }
    }
    assert.equal(lookups, replies.length);
    assert.deepEqual(received, [chatId, rejectedChat, '@public_channel'].map((chat, index) => ({
      action: 'alerts', field: 'telegram', value: {token: fakeToken, chat}, version: initialVersion + index,
    })), 'discovered and manual chat IDs use the same Telegram save action');
    await button(wizard, 'Close destinations').click();
  } finally {
    releaseLookup();
    await page.unroute(lookupPath, discover);
    await page.unroute(actionPath, saveTelegram);
  }
}

async function checkAlerts(page) {
  await section(page, 'Integrations').click();
  await button(page, 'Alert forwarding').click();
  const destination = page.getByRole('region', {name: 'Destinations', exact: true});
  await button(destination, 'Set up destination').click();
  const manager = dialog(page, 'Destinations');
  assert.equal(await manager.getByRole('switch').count(), 0, 'unconfigured providers cannot forward');
  assert.equal(await manager.getByRole('button', {name: /^Send test to /}).count(), 0, 'unconfigured providers cannot send tests');
  await button(manager, 'Close destinations').click();
  await checkHostedConnections(page, destination);
  await page.reload(); // Drop the hosted route fixture before checking manual setup.
  await section(page, 'Integrations').click();
  await button(page, 'Alert forwarding').click();
  await checkTelegramWizard(page, destination);
  await page.reload(); // Drop the route fixture's Telegram destination before testing real settings.
  await section(page, 'Integrations').click();
  await button(page, 'Alert forwarding').click();
  await button(destination, 'Set up destination').click();
  await button(page, 'Discord').click();
  await button(page, 'Use your own webhook').click();
  const webhook = page.getByRole('textbox', {name: 'Webhook link'});
  await webhook.fill('unsaved example');
  await button(page, 'Close destinations').click();
  await button(destination, 'Set up destination').click();
  await button(page, 'Telegram').click();
  await button(page, 'Use your own bot').click();
  assert.equal(await page.getByRole('textbox', {name: 'Bot token'}).inputValue(), '', 'closing the Discord editor leaves Telegram empty');
  await button(page, 'Back to destinations').click();
  await button(page, 'Discord').click();
  await button(page, 'Use your own webhook').click();
  assert.equal(await webhook.inputValue(), '', 'closing destination setup forgets unsaved credentials');
  await button(page, 'Close destinations').click();
  const content = page.getByRole('group', {name: 'Content', exact: true});
  await content.getByRole('radio', {name: 'Also program text'}).click();
  await page.getByText(/up to 300 characters/i).first().waitFor();
  await page.reload();
  await section(page, 'Integrations').click();
  await button(page, 'Alert forwarding').click();
  assert(await content.getByRole('radio', {name: 'Also program text'}).isChecked(), 'the alert disclosure follows the persisted choice');
  await content.getByRole('radio', {name: 'Kind and program name'}).click();
  await page.getByText(/no text from the program/i).first().waitFor();
  await button(page, 'Back to integrations').click();
  await button(page, 'Alert forwarding').waitFor();
  await section(page, 'Layout').click();
}

async function main() {
  assert(process.argv[2], 'Pass the path to a packaged Milo binary');
  const binary = resolve(process.argv[2]);
  const fixture = await mkdtemp(join(tmpdir(), 'milo-web-smoke-'));
  const cwd = join(fixture, 'a-worktree-with-a-name-long-enough-to-overflow-a-phone-footer');
  const home = join(fixture, 'home');
  // Do not inherit REX_*, GIT_* or settings overrides from the developer's shell.
  const env = {
    // A stand-in for rex comes first, so the presence helper hears that an app has a session open.
    PATH: `${join(fixture, 'bin')}:${process.env.PATH}`,
    HOME: home,
    XDG_CONFIG_HOME: join(home, '.config'),
    XDG_STATE_HOME: join(home, '.local', 'state'),
    GIT_CONFIG_NOSYSTEM: '1',
    // The only folder the presence helper looks for Discord in: never the developer's own Discord.
    XDG_RUNTIME_DIR: join(fixture, 'r'),
  };
  let browser;
  let discord;
  try {
    await Promise.all([mkdir(cwd), mkdir(home), mkdir(join(fixture, 'bin'))]);
    const rex = join(fixture, 'bin', 'rex');
    // It reports a session called "smoke session" in front, working in /tmp.
    const front = 'milo-rex active 736d6f6b652073657373696f6e 2f746d70';
    await writeFile(rex, `#!/bin/sh\ncase " $* " in *" -e "*) echo "${front}"; exec sleep 600 ;; esac\n`);
    await chmod(rex, 0o700);
    discord = await standInForDiscord(env.XDG_RUNTIME_DIR);
    execFileSync('git', ['init', '-q', '--template='], {cwd, env});
    browser = await chromium.launch({executablePath: process.env.CHROME_BIN, env});
    const desktop = await browser.newContext();
    desktop.setDefaultTimeout(timeout);
    await withMilo(binary, cwd, env, async url => {
      const page = await openEditor(desktop, url);
      await checkAlerts(page);
      await page.clock.install();
      await saveNumbers(page, cwd);
      await chooseUsageTools(page, binary, cwd, env);
      await manageMenuApp(page, url, env);
      await turnPresenceOnAndOff(page, binary, cwd, env, discord);
      await movePaneDuringPendingEdit(page);
      await discardFocusedNumber(page);
      await pressAfterNumberEdit(page);
      await page.close();
    });
    await withMilo(binary, cwd, env, async (url, exited) => {
      const page = await openEditor(desktop, url);
      for (const [name, value] of edits) {
        assert.equal(await number(page, name).inputValue(), value, `${name} persisted across a server restart`);
      }
      await usageToolsPersisted(page);
      await copySettingsThroughHosts(page, env, cwd);
      await rejectStaleDone(desktop, page, url);
      await confirmFromMobileFooter(browser, url, exited);
    });
  } finally {
    try {
      await browser?.close();
    } finally {
      // A failed run must not leave the presence helper behind.
      try {
        execFileSync(binary, ['presence', 'disable'], {cwd, env, stdio: 'ignore'});
      } catch { /* The cleanup below still runs. */ }
      discord?.close();
      await rm(fixture, {recursive: true, force: true});
    }
  }
}

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
