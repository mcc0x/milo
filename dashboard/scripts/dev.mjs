// Developer tooling only. The release binary serves the static export itself.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import next from 'next';

const hostname = '127.0.0.1';
const port = Number(process.env.PORT ?? 6110);
if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('Invalid PORT');
const origin = `http://${hostname}:${port}`;
const root = fileURLToPath(new URL('../', import.meta.url));
const app = next({ dev: true, dir: root, hostname, port });
const handle = app.getRequestHandler();
let upstream;
let child;

const server = createServer(async (req, res) => {
  // Never turn Next into a way around Milo's Host, Origin or bearer checks.
  if (req.headers.host !== `${hostname}:${port}` || (req.headers.origin && req.headers.origin !== origin)) {
    res.writeHead(403).end(); return;
  }
  if (!upstream) { res.writeHead(503).end(); return; }
  if (!req.url?.startsWith('/api/')) { await handle(req, res); return; }
  const read = req.url === '/api/state' && req.method === 'GET';
  const write = ['/api/action', '/api/close'].includes(req.url) && req.method === 'POST';
  if (!read && !write) { res.writeHead(404).end(); return; }
  if (write && req.headers.origin !== origin) { res.writeHead(403).end(); return; }
  const headers = { authorization: req.headers.authorization ?? '', origin: upstream.origin };
  let body;
  try {
    if (write) {
      if (req.headers['content-type'] !== 'application/json') { res.writeHead(415).end(); return; }
      headers['content-type'] = 'application/json';
      const parts = []; let bytes = 0;
      for await (const chunk of req) {
        bytes += chunk.length;
        if (bytes > 16384) { res.writeHead(413).end(); return; }
        parts.push(chunk);
      }
      body = Buffer.concat(parts, bytes);
    }
    const result = await fetch(new URL(req.url, upstream.origin), { method: req.method, headers, body, redirect: 'error' });
    res.writeHead(result.status, { 'Cache-Control': 'no-store', 'Content-Type': 'application/json' });
    res.end(Buffer.from(await result.arrayBuffer()));
  } catch { if (!res.headersSent) res.writeHead(502); res.end(); }
});
// Bind before generating a session key: never point the user at a port another process owns.
await new Promise((resolve, reject) => server.once('error', reject).listen(port, hostname, resolve));
try {
  await app.prepare();
  child = spawn(process.env.MILO_BIN || 'milo', ['web', '--no-open', '--port', '0'], {
    cwd: process.env.MILO_PROJECT || process.cwd(), stdio: ['ignore', 'pipe', 'inherit'],
  });
  process.once('exit', () => child.kill());
  for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => process.exit(0));
  child.once('error', error => { console.error(error.message); process.exit(1); });
  child.once('exit', code => {
    if (!upstream || code) process.exit(code || 1);
    upstream = undefined;
    // The port belongs to Milo only for its lifetime. Flush the final Done response, then exit.
    server.close(() => process.exit(0));
    setTimeout(() => process.exit(0), 1500).unref();
  });
  const lines = createInterface({ input: child.stdout });
  lines.once('line', line => {
    if (!/^http:\/\/127\.0\.0\.1:\d+\/#\w+$/.test(line)) { console.error('Unexpected Milo session address'); process.exit(1); }
    upstream = new URL(line);
    console.log(`${origin}/${upstream.hash}`);
    lines.close();
  });
} catch (error) { console.error(error.message); process.exit(1); }
