// Render actual PTY bytes, and return xterm's screen cells and terminal replies.
const path = require('node:path');
const readline = require('node:readline');
const { createRequire } = require('node:module');
const requireTools = createRequire(path.join(path.resolve(process.argv[2]), 'package.json'));
const { chromium } = requireTools('playwright-core');
const xterm = path.dirname(requireTools.resolve('@xterm/xterm/package.json'));

async function main() {
  const browser = await chromium.launch({ executablePath: process.argv[3] });
  try {
    const page = await browser.newPage({ viewport: { width: 1400, height: 1000 }, deviceScaleFactor: 1 });
    await page.setContent('<style>html{background:#10151e}body{margin:0}#terminal{display:inline-block;padding:18px;background:#151c28}</style><div id="terminal"></div>');
    await page.addStyleTag({ path: path.join(xterm, 'css', 'xterm.css') });
    await page.addScriptTag({ path: path.join(xterm, 'lib', 'xterm.js') });
    for await (const line of readline.createInterface({ input: process.stdin, crlfDelay: Infinity })) {
      const request = JSON.parse(line);
      if (request.size) {
        await page.evaluate(([cols, rows]) => {
          if (window.term) return term.resize(cols, rows);
          window.replies = [];
          window.term = new Terminal({
            cols, rows, scrollback: 0, fontFamily: 'Menlo, monospace', fontSize: 15,
            theme: { background: '#151c28', foreground: '#dbe5f4', cursor: '#b8dcff',
              black: '#151c28', brightBlack: '#8591a6', red: '#ff858b', green: '#9cdb9b',
              yellow: '#f3ce85', blue: '#92b9ff', magenta: '#e3a9ee', cyan: '#85d8ed',
              white: '#dbe5f4' },
          });
          term.onData(data => replies.push(data));
          term.open(document.getElementById('terminal'));
          term.focus();
        }, request.size);
      }
      if (request.bytes) {
        await page.evaluate(data => new Promise(resolve => {
          term.write(Uint8Array.from(atob(data), ch => ch.charCodeAt(0)), resolve);
        }), request.bytes);
      }
      const state = await page.evaluate(() => ({
        rows: Array.from({ length: term.rows }, (_, i) => term.buffer.active.getLine(i).translateToString(true)),
        replies: replies.splice(0),
      }));
      if (request.screenshot) await page.locator('#terminal').screenshot({ path: request.screenshot });
      console.log(JSON.stringify(state));
    }
  } finally {
    await browser.close();
  }
}

main().catch(error => { console.error(error); process.exitCode = 1; });
