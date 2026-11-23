#!/bin/sh
# Run on a Mac with a graphical session; resizes a disposable test window.
# Pass --keys-only to check NSEvent shortcut selection without opening a window.
# Pass --layout-only to check picker chrome in an offscreen window without a fixture server or clipboard.
set -eu
case "${1-}:$#" in
    :0|--keys-only:1|--layout-only:1) ;;
    *) echo 'Usage: sh smoke.sh [--keys-only | --layout-only]' >&2; exit 2 ;;
esac
here=$(CDPATH= cd -- "$(dirname "$0")" && pwd -P)
work=$(mktemp -d "${TMPDIR:-/tmp}/milo-webkit-smoke.XXXXXXXX")
cleanup() {
    if [ -n "${server-}" ]; then
        kill "$server" 2>/dev/null || :
        wait "$server" 2>/dev/null || :
    fi
    rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
xcrun clang -fobjc-arc -mmacosx-version-min=13.0 -framework AppKit -framework WebKit \
    -Wl,-sectcreate,__TEXT,__info_plist,"$here/../Info.plist" \
    "$here/BrowserTests.m" -o "$work/test-browser"
if [ "$#" -eq 1 ]; then
    "$work/test-browser" "$1"
    exit
fi
printf ready >"$work/hmr"
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj /CN=localhost \
    -keyout "$work/key.pem" -out "$work/cert.pem" >/dev/null 2>&1
python3 - "$work" <<'PY' &
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import socket
import ssl
import sys
import threading
import time
from urllib.parse import parse_qs, urlsplit

root = Path(sys.argv[1])

class QuietHandler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def body(self, mime, text, headers=None):
        data = text.encode()
        self.send_response(200)
        self.send_header('Content-Type', mime)
        self.send_header('Content-Length', str(len(data)))
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        try:
            self.wfile.write(data)
        except OSError:
            pass

    def html(self, text, headers=None):
        self.body('text/html; charset=utf-8', text, headers)

class Outside(QuietHandler):
    def do_GET(self):
        if self.path in ('/outside', '/slow-outside'):
            if self.path == '/slow-outside':
                time.sleep(0.6)
            if self.headers.get('Referer'):
                (root / 'escaped.referrer').touch()
            self.html(f'''<!doctype html><title>Outside fixture</title>
<link rel="stylesheet" href="{start_origin}/outside-style">
<style>html,body{{margin:0;background:transparent}}</style>
<a href="{start_origin}/cookie-check" id="return">Visit local site without preview authority</a>
<form action="{start_origin}/outside-form" method="post"><input type="hidden" name="typed" value="x">
<button id="outside-form">Cross-host form</button></form><div id="outside-edge"></div>''')
        else:
            if self.path.startswith('/leak'):
                (root / 'escaped').touch()
            self.send_response(204)
            self.end_headers()

outside = ThreadingHTTPServer(('127.0.0.1', 0), Outside)
other = f'http://localhost:{outside.server_port}/outside'
threading.Thread(target=outside.serve_forever, daemon=True).start()

class Fixture(QuietHandler):
    protocol_version = 'HTTP/1.1'

    def redirect(self, location):
        self.send_response(302)
        self.send_header('Location', location)
        self.send_header('Referrer-Policy', 'unsafe-url')
        self.send_header('Content-Length', '0')
        self.end_headers()

    def do_GET(self):
        path = urlsplit(self.path).path
        if path == '/cookie-check':
            if 'milo_dev_fixture=private' in self.headers.get('Cookie', ''):
                (root / 'escaped.cookie').touch()
            self.html('<!doctype html><title>Unprivileged local fixture</title>')
        elif path == '/hmr' and self.headers.get('Upgrade', '').lower() == 'websocket':
            self.hot_reload()
        elif path == '/redirect':
            (root / 'escaped.redirect').touch()
            self.redirect(other)
        elif path == '/resource-redirect':
            self.redirect(other.replace('/outside', '/leak-redirect'))
        elif path == '/outside-style':
            self.body('text/css', '#outside-edge {position:fixed;inset:0 0 0 auto;width:12px;background:rgb(229,17,39)}')
        elif path == '/failed':
            self.close_connection = True
            self.connection.shutdown(socket.SHUT_RDWR)
        elif path == '/sandbox':
            data = b'<!doctype html><title>Sandbox fixture</title>'
            self.send_response(200)
            self.send_header('Content-Security-Policy', "default-src 'self'; sandbox")
            self.send_header('Content-Type', 'text/html')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        elif path == '/attachment':
            self.send_response(200)
            (root / 'escaped.attachment').touch()
            self.send_header('Content-Disposition', 'attachment; filename="milo-browser-fixture.txt"')
            self.send_header('Content-Type', 'text/plain')
            self.send_header('Content-Length', '0')
            self.end_headers()
        elif path == '/download-attr':
            (root / 'escaped.download').touch()
            self.html('<!doctype html><title>Download must not navigate</title>')
        else:
            self.html(f'''<!doctype html><html><head><title>Milo fixture</title><style>
body {{ margin:0; min-height:120vh; }} #typed, button, private-component, a {{ position:absolute; display:block; }}
::-webkit-scrollbar {{ width:32px; }} ::-webkit-scrollbar-thumb {{ background:#777; }}
#typed {{ inset:10px auto auto 12px; width:105px; height:20px; }}
button {{ inset:10px auto auto 133px; width:75px; height:26px; }}
private-component {{ inset:52px auto auto 12px; width:250px; height:30px; }}
#popup {{ inset:98px auto auto 12px; }} #redirect {{ inset:136px auto auto 12px; }}
#outside {{ inset:205px auto auto 12px; }} #hash {{ inset:239px auto auto 12px; }}
#unsafe {{ inset:271px auto auto 12px; }} #download {{ inset:302px auto auto 12px; }}
#attachment {{ inset:333px auto auto 12px; }}
#hmr {{ position:absolute; inset:175px auto auto 12px; }}
#edge {{ position:absolute; top:52px; right:0; width:8px; height:30px; background:#aaa; }}
@media (max-width: 260px) {{
  #typed, private-component {{ box-sizing:border-box; max-width:calc(100% - 16px); }}
  button {{ inset:33px 4px auto auto; height:18px; }}
  private-component {{ overflow:hidden; }}
}}
</style></head><body><form method="post" action="/form"><input id="typed" name="typed"><button>Submit</button></form>
<private-component data-credential="FIXTURE-SECRET">FIXTURE-SECRET-TEXT</private-component>
<a id="popup" href="{other}" target="_blank">Blocked popup</a>
<a id="redirect" href="/redirect" referrerpolicy="unsafe-url">Follow redirect</a>
<output id="hmr">waiting</output><img src="{other.replace('/outside', '/leak-image')}" hidden><iframe src="{other.replace('/outside', '/leak-frame')}" hidden></iframe>
<div id="edge"></div>
<a id="outside" href="{other}" referrerpolicy="unsafe-url">Go outside</a>
<a id="hash" href="#section">Same document</a>
<a id="unsafe" href="file:///__milo_browser_fixture_missing">Blocked file</a>
<a id="download" href="/download-attr" download>Blocked download</a>
<a id="attachment" href="/attachment">Blocked attachment</a>
<img src="/resource-redirect" hidden>
<script>
  // This fixture doesn't know about the native picker and has no message bridge.
  fetch('{other.replace('/outside', '/leak-fetch')}', {{mode:'no-cors'}}).catch(() => {{}});
  new WebSocket('{other.replace('http:', 'ws:').replace('/outside', '/leak-ws')}').onerror = () => {{}};
  window.returnKeyDowns = 0;
  document.getElementById('typed').addEventListener('keydown', event => {{
    if (event.key !== 'Enter') return;
    ++window.returnKeyDowns;
    event.preventDefault();
  }});
  let marker;
  new WebSocket('ws://' + location.host + '/hmr').onmessage = event => {{
    const target = document.querySelector('private-component');
    if (event.data === 'move' && !marker) {{
      marker = document.createElement('span');
      target.before(marker);
    }}
    if (event.data === 'reset' && marker) {{
      marker.remove();
      marker = null;
    }}
    hmr.textContent = event.data;
  }};
</script></body></html>''', {'Set-Cookie': 'milo_dev_fixture=private; Path=/; SameSite=Lax'} if path == '/start' else None)

    def do_POST(self):
        data = self.rfile.read(int(self.headers.get('Content-Length', '0'))).decode()
        passed = parse_qs(data) == {'typed': ['x']}
        if self.path == '/outside-form':
            title = 'Outside form passed' if passed else 'Outside form failed'
        else:
            title = 'Form passed' if self.path == '/form' and passed else 'Form failed'
        self.html(f'<!doctype html><title>{title}</title><p>{title}</p>')

    def hot_reload(self):
        accept = self.headers['Sec-WebSocket-Key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11'
        encoded = base64.b64encode(hashlib.sha1(accept.encode()).digest()).decode()
        self.send_response(101)
        self.send_header('Upgrade', 'websocket')
        self.send_header('Connection', 'Upgrade')
        self.send_header('Sec-WebSocket-Accept', encoded)
        self.end_headers()
        self.close_connection = True
        last = b''
        # WebKit may keep the original fixture page in its back-forward cache.
        # A bounded watcher is sufficient for the disposable run.
        until = time.monotonic() + 120
        while time.monotonic() < until:
            current = (root / 'hmr').read_bytes()
            if current != last:
                try:
                    self.wfile.write(bytes([0x81, len(current)]) + current)
                    self.wfile.flush()
                except OSError:
                    return
                last = current
            time.sleep(0.05)

fixture = ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
start_origin = f'http://127.0.0.1:{fixture.server_port}'
secure = ThreadingHTTPServer(('127.0.0.1', 0), Outside)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(root / 'cert.pem', root / 'key.pem')
secure.socket = context.wrap_socket(secure.socket, server_side=True)
threading.Thread(target=secure.serve_forever, daemon=True).start()
(root / 'outside').write_text(other)
(root / 'alias').write_text(f'http://localhost.:{outside.server_port}/outside')
(root / 'secure').write_text(f'https://localhost:{secure.server_port}/outside')
(root / 'url').write_text(f'{start_origin}/start')
fixture.serve_forever()
PY
server=$!
for i in $(seq 1 100); do
    if [ -s "$work/url" ]; then break; fi
    kill -0 "$server" 2>/dev/null || { echo 'Fixture server failed to start.' >&2; exit 1; }
    sleep 0.05
done
[ -s "$work/url" ] || { echo 'Fixture server timed out.' >&2; exit 1; }
"$work/test-browser" "$(cat "$work/url")" "$work/hmr" "$work/escaped" \
    "$(cat "$work/outside")" "$(cat "$work/alias")" "$(cat "$work/secure")"
