import { createHash } from 'node:crypto';
import { lstatSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const bundle = join(root, 'bundle');
const format = 'milo-dashboard-bundle-v1';
const publicImages = ['favicon.png', 'milo.webp', 'rex.png'];
const sourceInputs = ['app', 'components', 'public', 'bun.lock', 'next.config.ts', 'package.json', 'scripts/bundle.mjs', 'tsconfig.json'];

function files(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap(entry => {
    if (entry.isFile()) return [entry.name];
    if (entry.isDirectory()) return files(join(dir, entry.name)).map(name => `${entry.name}/${name}`);
    throw new Error(`Unexpected file type: ${join(dir, entry.name)}`);
  });
}

function sha256(bytes, encoding = 'hex') {
  return createHash('sha256').update(bytes).digest(encoding);
}

function sourceHash() {
  const names = sourceInputs.flatMap(name => lstatSync(join(root, name)).isDirectory()
    ? files(join(root, name)).map(child => `${name}/${child}`)
    : [name]);
  const entries = names.sort().map(name => [name, sha256(readFileSync(join(root, name)))]);
  return sha256(JSON.stringify(entries));
}

function nextAsset(name) {
  return name.startsWith('_next/static/') && name.split('/').every(part => /^[a-zA-Z0-9_][a-zA-Z0-9_.-]*$/.test(part))
    && /\.(?:css|js|woff2?|png|webp|jpe?g|gif|svg|ico)$/.test(name);
}

function manifest(dir, names) {
  const html = readFileSync(join(dir, 'index.html'), 'utf8').replace(/\r\n?/g, '\n');
  // The static document carries Next's hydration data in ordinary inline script tags.
  // Hash precisely the text the browser sees, not the tag or its attributes.
  const scripts = new Set();
  for (const [, attrs = '', script] of html.matchAll(/<script(\s[^>]*?)?>([\s\S]*?)<\/script>/gi)) {
    if (/(?:^|\s)src\s*=/i.test(attrs)) continue;
    scripts.add(sha256(script, 'base64'));
  }
  if (!scripts.size) throw new Error('No inline hydration scripts found in the Next export');

  const paths = new Set(names);
  for (const name of ['index.html', ...publicImages]) {
    if (!paths.has(name)) throw new Error(`Missing Next export asset: ${name}`);
  }
  if (!names.some(name => nextAsset(name) && name.endsWith('.js'))) throw new Error('No Next JavaScript chunks found');
  for (const name of names) {
    if (name !== 'index.html' && !publicImages.includes(name) && !nextAsset(name)) {
      throw new Error(`Unexpected dashboard bundle asset: ${name}`);
    }
  }

  return [format, `source ${sourceHash()}`, ...[...scripts].sort().map(hash => `script ${hash}`),
    ...[...paths].sort().map(name => `asset ${name} ${sha256(readFileSync(join(dir, name)))}`), ''].join('\n');
}

const args = process.argv.slice(2);
if (args.length === 1 && args[0] === '--check') {
  const names = files(bundle).filter(name => name !== 'manifest');
  if (readFileSync(join(bundle, 'manifest'), 'utf8') !== manifest(bundle, names)) {
    throw new Error('Dashboard bundle is stale; run bun run build in dashboard/ and commit dashboard/bundle/');
  }
} else if (args.length === 0) {
  const out = join(root, 'out');
  const names = ['index.html', ...publicImages, ...files(join(out, '_next/static'))
    .map(name => `_next/static/${name}`).filter(nextAsset)];
  const index = manifest(out, names);
  rmSync(bundle, { recursive: true, force: true });
  for (const name of names) {
    const dest = join(bundle, name);
    mkdirSync(dirname(dest), { recursive: true });
    writeFileSync(dest, readFileSync(join(out, name)));
  }
  writeFileSync(join(bundle, 'manifest'), index);
} else {
  throw new Error('Usage: node dashboard/scripts/bundle.mjs [--check]');
}
