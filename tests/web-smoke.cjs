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
const reopenNote = page => page.getByRole('contentinfo').getByText('Reopen panes to apply.', {exact: true});
const section = (page, name) => page.getByRole('navigation').getByRole('button', {name: new RegExp(`^${name}`)});
const usageTools = ['Codex', 'Claude Code', 'OpenCode'];
const usageChoice = (page, tool, name) =>
  page.getByRole('group', {name: `${tool} usage`, exact: true}).getByRole('radio', {name, exact: true});

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
  await page.getByRole('heading', {name: 'Panes', exact: true}).waitFor();
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
  await section(page, 'Appearance').click();
  for (const tool of usageTools) {
    assert(await usageChoice(page, tool, 'Auto').isChecked(), `${tool} starts on Auto`);
  }
  await usageChoice(page, 'OpenCode', 'Hide').click();
  await usageChoice(page, 'OpenCode', 'Hide').and(page.locator(':checked')).waitFor({state: 'attached'});
  await page.getByText('Never shown, and never looked for.').waitFor();
  await usageChoice(page, 'Claude Code', 'Always show').click();
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
  assert(await usageChoice(page, 'Codex', 'Auto').isChecked(), 'Codex was left on Auto');
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

  await section(page, 'Workflow').click();
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
  await section(page, 'Workflow').click();
  const menu = page.getByRole('region', {name: 'Menu bar app', exact: true});
  const readState = async () => {
    const response = await page.request.get(new URL('/api/state', url).href, {
      headers: {Authorization: `Bearer ${new URL(url).hash.slice(1)}`},
    });
    assert(response.ok(), 'the editor reports menu app availability');
    return response.json();
  };
  const {menubar} = await readState();
  const menuTab = section(page, 'Menu Bar');
  assert.equal(await menu.count(), 0, 'Workflow does not contain menu app settings');
  assert.equal(await menuTab.isVisible(), menubar.installed !== null, 'Menu Bar navigation follows server availability');
  const preferences = join(env.XDG_CONFIG_HOME, 'milo', 'preferences.conf');
  const previous = await readFile(preferences, 'utf8');

  const actions = [];
  let draft;
  let gate;
  const intercept = async route => {
    const action = route.request().postDataJSON();
    if (action.field === 'workflow.project_mode') {
      const response = await route.fetch();
      assert(response.ok(), 'the real editor kept the preference draft');
      draft = await response.json();
      return route.fulfill({response, json: {...draft, menubar: {installed: false}}});
    }
    if (action.action !== 'menubar') return route.fallback();
    actions.push(action);
    await gate;
    await route.fulfill({json: {...draft, menubar: {installed: action.value === 'install'}}});
  };
  // Hold menu app replies so the dashboard is exercised without opening apps
  // or touching the logged-in user's services, even with a packaged installer.
  await page.route('**/api/action', intercept);
  const install = button(menu, 'Install menu app');
  const update = button(menu, 'Open or update menu app');
  const remove = button(menu, 'Remove menu app…');
  const confirmation = dialog(page, 'Remove menu app?');
  const pin = page.getByRole('radio', {name: 'Pin project', exact: true});
  const perform = async (activate, pending, active, next) => {
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
    assert(await next.evaluate(el => el === document.activeElement), 'completed action restores focus');
  };

  try {
    await pin.click();
    await status(page, dirty).waitFor({state: 'attached'});
    await menuTab.click();
    assert.equal(await remove.count(), 0, 'nothing to remove before installation');
    await perform(() => install.click(), 'Installing menu app…', install, update);
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
  await section(page, 'Workflow').click();
  assert(await pin.isChecked(), 'menu app operations retained the draft');
  assert.equal(await readFile(preferences, 'utf8'), previous, 'menu app operations did not save the draft');
  await button(page, 'Discard').click();
  await button(dialog(page, 'Discard all unsaved changes?'), 'Discard changes').click();
  await status(page, clean).waitFor({state: 'attached'});
  console.log('Menu app: installed, opened or updated twice, kept on cancel, and removed; draft preserved');
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
  const id = '1234567890123456789';
  const file = join(env.XDG_CONFIG_HOME, 'milo', 'discord.conf');
  await section(page, 'Workflow').click();
  const card = page.getByRole('region', {name: 'Discord presence', exact: true});
  const turnOn = button(card, 'Turn on…');
  const question = dialog(page, 'Turn on Discord presence?');
  await card.getByText('Anyone who can see your Discord activity sees it').waitFor();
  await card.getByText('Milo is not connected to Discord and shows nothing there.').waitFor();

  await turnOn.click();
  await page.getByRole('alert').filter({hasText: 'Enter your Discord application ID first'}).waitFor();
  assert.equal(await question.count(), 0, 'nothing is asked before there is an application to show');
  await card.getByRole('textbox', {name: 'Application ID', exact: true}).fill(id);
  await turnOn.click();
  await question.getByText('Milo sends nothing else: no session, tab, project or file names').waitFor();
  await button(question, 'Keep editing').click();
  await question.waitFor({state: 'hidden'});
  await assert.rejects(readFile(file), {code: 'ENOENT'}, 'reading what becomes public saves nothing');
  assert.equal(discord.frames.length, 0, 'Discord is not contacted before presence is turned on');

  await turnOn.click();
  await button(question, 'Turn on').click();
  await status(page, 'Discord presence is on. Discord is showing “In a Rex session”.').waitFor();
  await button(card, 'Turn off').waitFor();
  assert.equal(await readFile(file, 'utf8'), `presence = true\napplication_id = ${id}\n`);
  await until(() => discord.frames.length === 2, 'the helper introduced itself and set the activity');
  const [handshake, activity] = discord.frames;
  assert.deepEqual(handshake, [0, `{"v":1,"client_id":"${id}"}`]);
  assert.equal(activity[0], 1);
  const sent = JSON.parse(activity[1]);
  assert.deepEqual(sent, {
    cmd: 'SET_ACTIVITY', args: {pid: sent.args.pid, activity: {details: 'In a Rex session'}}, nonce: 'milo-1',
  }, 'the activity is one fixed line and nothing about the work');
  assert.equal(execFileSync(binary, ['presence'], {cwd, env, encoding: 'utf8'}),
    'Discord presence: on\nApplication ID: 1234567890123456789\nDiscord is showing “In a Rex session”.\n');

  await button(card, 'Turn off').click();
  await status(page, 'Discord presence is off.').waitFor();
  await turnOn.waitFor();
  await until(() => discord.connections() === 0, 'turning presence off closed the connection to Discord');
  assert.deepEqual(discord.frames.slice(2).map(([opcode, payload]) => [opcode, JSON.parse(payload)]),
    [[1, {cmd: 'SET_ACTIVITY', args: {pid: sent.args.pid}, nonce: 'milo-2'}]], 'the activity was taken down first');
  assert.equal(await readFile(file, 'utf8'), `presence = false\napplication_id = ${id}\n`);
  assert.equal(await status(page, clean).count(), 1, 'presence is saved at once and leaves no draft');
  console.log('Discord presence: asked first, shown to the stand-in for Discord, and taken down when turned off');
  await section(page, 'Layout').click();
}

async function usageToolsPersisted(page) {
  await section(page, 'Appearance').click();
  const expected = [['Codex', 'Auto'], ['Claude Code', 'Always show'], ['OpenCode', 'Hide']];
  for (const [tool, name] of expected) {
    assert(await usageChoice(page, tool, name).isChecked(), `${tool} persisted as ${name}`);
  }
  await section(page, 'Layout').click();
  await page.getByRole('heading', {name: 'Panes', exact: true}).waitFor();
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
  await button(page, 'Appearance').click();
  assert.equal(await page.getByRole('heading', {name: 'Panes', exact: true}).count(), 1,
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

async function confirmFromMobileFooter(browser, url, exited) {
  const viewport = {width: 390, height: 700};
  const context = await browser.newContext({viewport, isMobile: true, hasTouch: true});
  context.setDefaultTimeout(timeout);
  const page = await openEditor(context, url);
  await page.setViewportSize({width: 260, height: viewport.height});
  const dock = await page.locator('.control-dock').boundingBox();
  assert(dock && dock.x >= 0 && dock.x + dock.width <= 260, 'navigation and draft controls fit at narrow widths');
  await page.setViewportSize(viewport);
  const done = button(page.getByRole('contentinfo'), 'Done');
  const box = await done.boundingBox();
  assert(box && box.x >= 0 && box.x + box.width <= viewport.width &&
    box.y >= 0 && box.y + box.height <= viewport.height, 'mobile footer Done fits in the viewport');

  const confirmation = dialog(page, 'Close the dashboard?');
  await done.tap();
  await button(confirmation, 'Keep editing').tap();
  await confirmation.waitFor({state: 'hidden'});
  await status(page, dirty).waitFor({state: 'attached'});

  await done.tap();
  await button(confirmation, 'Discard and close').tap();
  await page.getByRole('heading', {name: 'Settings closed', exact: true}).waitFor();
  const result = await Promise.race([exited, delay(timeout, null, {ref: false})]);
  assert.deepEqual(result, [0, null], 'confirming from the mobile footer stops Milo cleanly');
  console.log('Mobile Done: cancel retained the draft, confirm closed the editor');
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
    await writeFile(rex, '#!/bin/sh\ncase " $* " in *" -e "*) echo "milo-rex active"; exec sleep 600 ;; esac\n');
    await chmod(rex, 0o700);
    discord = await standInForDiscord(env.XDG_RUNTIME_DIR);
    execFileSync('git', ['init', '-q', '--template='], {cwd, env});
    browser = await chromium.launch({executablePath: process.env.CHROME_BIN, env});
    const desktop = await browser.newContext();
    desktop.setDefaultTimeout(timeout);
    await withMilo(binary, cwd, env, async url => {
      const page = await openEditor(desktop, url);
      const dev = await page.request.get(new URL('/dev/version', url).href);
      assert.equal(dev.status(), 404, 'the smoke test must run without web-dev');
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
