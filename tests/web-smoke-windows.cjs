// The packaged settings editor on native Windows, driven in Chromium: open it from the private
// link, save layout numbers for a worktree, start milo web again and find them saved.
// Run with Node 20+, playwright-core, and either its installed Chromium or CHROME_BIN.
// Usage: node tests/web-smoke-windows.cjs path\to\milo.exe
const assert = require('node:assert/strict');
const {execFileSync, spawn} = require('node:child_process');
const {once} = require('node:events');
const {mkdir, mkdtemp, readdir, rm} = require('node:fs/promises');
const {tmpdir} = require('node:os');
const {basename, join, resolve} = require('node:path');
const {createInterface} = require('node:readline');
const {chromium} = require('playwright-core');

const timeout = 10_000;
const edits = [['Rail width', '37'], ['Usage height', '8']];
const button = (page, name) => page.getByRole('button', {name, exact: true});
const dialog = (page, name) => page.getByRole('dialog', {name, exact: true});
const number = (page, name) => page.getByRole('spinbutton', {name, exact: true});
const status = (page, text) => page.getByRole('status').filter({hasText: text});
const section = (page, name) => page.getByRole('navigation').getByRole('button', {name: new RegExp(`^${name}`)});

async function withMilo(binary, cwd, env, run) {
  const server = spawn(binary, ['web', '--no-open', '--port', '0'], {cwd, env, stdio: ['ignore', 'pipe', 'inherit']});
  const exited = once(server, 'close');
  const stdout = createInterface({input: server.stdout});
  try {
    const url = await Promise.race([
      once(stdout, 'line', {signal: AbortSignal.timeout(timeout)}).then(([line]) => line),
      exited.then(([code]) => { throw new Error(`Milo exited before printing its URL (${code})`); }),
    ]);
    assert.match(url, /^http:\/\/127\.0\.0\.1:\d+\/#[0-9a-f]{64}$/);
    stdout.close();
    await run(url);
  } finally {
    stdout.close();
    if (server.exitCode === null) server.kill();
    await exited.catch(() => {});
  }
}

async function openEditor(context, url) {
  const page = await context.newPage();
  await page.goto(url);
  await page.getByRole('heading', {name: 'Pane layout', exact: true}).waitFor();
  return page;
}

async function saveNumbers(page, cwd) {
  for (const [name, value] of edits) {
    const field = number(page, name);
    await field.fill(value);
    await button(page, 'Save').click();
    await button(dialog(page, 'Save changes'), 'Save for this worktree').click();
    await status(page, `Saved layout for “${basename(cwd)}”. Rex integration unchanged.`).waitFor();
    assert.equal(await field.inputValue(), value, name);
    console.log(`${name}: saved`);
  }
}

async function unsupportedIntegrations(page) {
  await section(page, 'Integrations').click();
  for (const name of ['Alert forwarding', 'Discord presence']) {
    assert(await button(page, name).isDisabled(), `${name} is not offered on Windows`);
  }
  await page.getByText('Not available in this session').first().waitFor();
  await section(page, 'Layout').click();
  console.log('Alerts and presence: shown as unavailable');
}

async function main() {
  assert(process.argv[2], 'Pass the path to a packaged Milo binary');
  const binary = resolve(process.argv[2]);
  const fixture = await mkdtemp(join(tmpdir(), 'milo-web-smoke-'));
  const cwd = join(fixture, 'worktree');
  const home = join(fixture, 'home');
  const env = {...process.env, USERPROFILE: home, APPDATA: join(home, 'roaming'), LOCALAPPDATA: join(home, 'local'),
    GIT_CONFIG_NOSYSTEM: '1'};
  for (const name of Object.keys(env)) {
    if (/^(HOME|XDG_.*|REX_.*|GIT_DIR|GIT_WORK_TREE|CLAUDE_CONFIG_DIR)$/i.test(name)) delete env[name];
  }
  let browser;
  try {
    await Promise.all([mkdir(cwd), mkdir(join(home, 'roaming'), {recursive: true}), mkdir(join(home, 'local'), {recursive: true})]);
    execFileSync('git', ['init', '-q', '--template='], {cwd, env});
    browser = await chromium.launch({executablePath: process.env.CHROME_BIN});
    const context = await browser.newContext();
    context.setDefaultTimeout(timeout);
    await withMilo(binary, cwd, env, async url => {
      const page = await openEditor(context, url);
      await saveNumbers(page, cwd);
      await unsupportedIntegrations(page);
      await page.close();
    });
    await withMilo(binary, cwd, env, async url => {
      const page = await openEditor(context, url);
      for (const [name, value] of edits) {
        assert.equal(await number(page, name).inputValue(), value, `${name} persisted across a server restart`);
      }
      console.log('Layout: read back after a server restart');
      await page.close();
    });
    const left = (await readdir(join(home, 'local', 'milo')).catch(() => [])).filter(name => name.startsWith('milo-web-'));
    assert.deepEqual(left, [], 'no browser handoff is left behind');
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
