// Run with Node 20+, playwright-core, and either its installed Chromium or CHROME_BIN.
// Usage: node tests/web-smoke.cjs path/to/packaged/milo
// Add --menubar with a menu-capable binary in a macOS GUI session to exercise the installer.
const assert = require('node:assert/strict');
const {execFileSync, spawn} = require('node:child_process');
const {once} = require('node:events');
const {mkdir, mkdtemp, readFile, rm, stat} = require('node:fs/promises');
const {tmpdir} = require('node:os');
const {join, resolve} = require('node:path');
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

async function saveNumbers(page) {
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
  assert.equal(await readFile(preferences, 'utf8'), 'usage_claude = always\nusage_opencode = hide\n');
  assert(await usageChoice(page, 'Codex', 'Auto').isChecked(), 'Codex was left on Auto');
  // What the panes read is what the controls saved, and a hidden tool is not looked for.
  const doctor = execFileSync(binary, ['doctor'], {cwd, env, encoding: 'utf8'});
  assert.match(doctor, /^ {2}Usage tools: Claude Code always shown; OpenCode hidden; others automatic$/m);
  assert.match(doctor, /^Claude Code limits: no local report; /m);
  assert.doesNotMatch(doctor, /^(Codex limits|OpenCode activity|Usage tools: none)/m);
  await section(page, 'Layout').click();
  console.log('Usage tools: three choices saved globally and read back by doctor');
}

async function manageMenuApp(page, url, binary, cwd, env) {
  await section(page, 'Workflow').click();
  const menu = page.getByRole('region', {name: 'Menu bar app', exact: true});
  const response = await page.request.get(new URL('/api/state', url).href, {
    headers: {Authorization: `Bearer ${new URL(url).hash.slice(1)}`},
  });
  assert(response.ok(), 'the editor reports menu app availability');
  const {menubar} = await response.json();
  const menuTab = section(page, 'Menu Bar');
  assert.equal(await menu.count(), 0, 'Workflow does not contain menu app settings');
  assert.equal(await menuTab.isVisible(), menubar.installed !== null, 'Menu Bar navigation follows server availability');
  if (process.argv.includes('--menubar')) {
    assert.equal(menubar.installed, false, 'the menu-capable binary starts without an app in the fixture home');
    const bundle = join(env.HOME, 'Applications', 'Milo Usage.app');
    const preferences = join(env.XDG_CONFIG_HOME, 'milo', 'preferences.conf');
    const previous = await readFile(preferences, 'utf8');
    const pin = page.getByRole('radio', {name: 'Pin project', exact: true});
    await pin.click();
    await status(page, dirty).waitFor({state: 'attached'});
    await menuTab.click();

    try {
      await button(menu, 'Install Milo Usage').click();
      const remove = button(menu, 'Remove Milo Usage…');
      await remove.waitFor();
      assert((await stat(bundle)).isDirectory(), 'the button installed the app');
      await status(page, dirty).waitFor({state: 'attached'});

      const confirmation = dialog(page, 'Remove Milo Usage?');
      await remove.click();
      await button(confirmation, 'Keep editing').click();
      await confirmation.waitFor({state: 'hidden'});
      assert((await stat(bundle)).isDirectory(), 'cancel left the app installed');

      await remove.click();
      await button(confirmation, 'Remove Milo Usage').click();
      await button(menu, 'Install Milo Usage').waitFor();
      await assert.rejects(stat(bundle), {code: 'ENOENT'}, 'confirm removed the app');
      await status(page, dirty).waitFor({state: 'attached'});
      await section(page, 'Workflow').click();
      assert(await pin.isChecked(), 'removal retained the draft');
      assert.equal(await readFile(preferences, 'utf8'), previous, 'menu app changes did not save the draft');
    } finally {
      if (await stat(bundle).then(() => true, e => { if (e.code !== 'ENOENT') throw e; return false; })) {
        execFileSync(binary, ['menubar', 'uninstall'], {cwd, env});
      }
    }
    await button(page, 'Discard').click();
    await button(dialog(page, 'Discard all unsaved changes?'), 'Discard changes').click();
    await status(page, clean).waitFor({state: 'attached'});
    console.log('Menu app: installed, kept on cancel, and removed without saving or losing the draft');
  }
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

async function rejectStaleDone(context, current, url) {
  const confirmation = dialog(current, 'Close the dashboard?');
  await button(current, 'Done').click();
  await button(confirmation, 'Close dashboard').waitFor();
  const second = await openEditor(context, url);

  const right = second.getByRole('radio', {name: 'Right', exact: true});
  await right.click();
  await status(second, dirty).waitFor({state: 'attached'});
  await current.bringToFront();

  const [response] = await Promise.all([
    current.waitForResponse(response => new URL(response.url()).pathname === '/api/close'),
    button(confirmation, 'Close dashboard').click(),
  ]);
  assert.equal(response.status(), 409, 'stale Done must not close the session');
  await current.getByText(/The draft changed in another request/).first().waitFor();
  await button(current, 'Refresh editor state').click();
  await status(current, dirty).waitFor({state: 'attached'});
  assert(await current.getByRole('radio', {name: 'Right', exact: true}).isChecked(), 'the newer draft was retained');
  console.log('Stale Done: the second tab’s draft survived and was visible after refresh');
}

async function confirmFromMobileFooter(browser, url, exited) {
  const viewport = {width: 390, height: 700};
  const context = await browser.newContext({viewport, isMobile: true, hasTouch: true});
  context.setDefaultTimeout(timeout);
  const page = await openEditor(context, url);
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
    PATH: process.env.PATH,
    HOME: home,
    XDG_CONFIG_HOME: join(home, '.config'),
    XDG_STATE_HOME: join(home, '.local', 'state'),
    GIT_CONFIG_NOSYSTEM: '1',
  };
  let browser;
  try {
    await Promise.all([mkdir(cwd), mkdir(home)]);
    execFileSync('git', ['init', '-q', '--template='], {cwd, env});
    browser = await chromium.launch({executablePath: process.env.CHROME_BIN, env});
    const desktop = await browser.newContext();
    desktop.setDefaultTimeout(timeout);
    await withMilo(binary, cwd, env, async url => {
      const page = await openEditor(desktop, url);
      const dev = await page.request.get(new URL('/dev/version', url).href);
      assert.equal(dev.status(), 404, 'the smoke test must run without web-dev');
      await saveNumbers(page);
      await chooseUsageTools(page, binary, cwd, env);
      await manageMenuApp(page, url, binary, cwd, env);
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
      await rm(fixture, {recursive: true, force: true});
    }
  }
}

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
