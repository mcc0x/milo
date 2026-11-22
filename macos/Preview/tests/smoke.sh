#!/bin/sh
# Run on a Mac with a graphical session; opens a disposable test window.
set -eu
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
    "$here/BrowserTests.m" -o "$work/test-browser"
printf ready >"$work/hmr"
python3 - "$work" <<'PY' &
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import sys
import threading
import time
from urllib.parse import parse_qs

root = Path(sys.argv[1])

class Outside(BaseHTTPRequestHandler):
    def do_GET(self):
        (root / 'escaped').touch()
        self.send_response(204)
        self.end_headers()

outside = ThreadingHTTPServer(('127.0.0.1', 0), Outside)
other = f'http://127.0.0.1:{outside.server_port}/out'
threading.Thread(target=outside.serve_forever, daemon=True).start()

class Fixture(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def html(self, body):
        data = body.encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == '/hmr' and self.headers.get('Upgrade', '').lower() == 'websocket':
            self.hot_reload()
        elif self.path == '/redirect':
            (root / 'escaped.redirect').touch()
            self.send_response(302)
            self.send_header('Location', other)
            self.send_header('Content-Length', '0')
            self.end_headers()
        else:
            self.html(f'''<!doctype html><html><head><title>Milo fixture</title><style>
body {{ margin:0; min-height:120vh; }} #typed, button, private-component, a {{ position:absolute; display:block; }}
::-webkit-scrollbar {{ width:32px; }} ::-webkit-scrollbar-thumb {{ background:#777; }}
#typed {{ inset:10px auto auto 12px; width:105px; height:20px; }}
button {{ inset:10px auto auto 133px; width:75px; height:26px; }}
private-component {{ inset:52px auto auto 12px; width:250px; height:30px; }}
#popup {{ inset:98px auto auto 12px; }} #redirect {{ inset:136px auto auto 12px; }}
#hmr {{ position:absolute; inset:175px auto auto 12px; }}
#edge {{ position:absolute; top:52px; right:0; width:8px; height:30px; background:#aaa; }}
</style></head><body><form method="post" action="/form"><input id="typed" name="typed"><button>Submit</button></form>
<private-component data-credential="FIXTURE-SECRET">FIXTURE-SECRET-TEXT</private-component>
<a id="popup" href="{other}" target="_blank">Unapproved popup</a>
<a id="redirect" href="/redirect">Unapproved redirect</a>
<output id="hmr">waiting</output><img src="{other}" hidden><iframe src="{other}" hidden></iframe>
<div id="edge"></div>
<script>
  // This fixture doesn't know about the native picker and has no message bridge.
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
</script></body></html>''')

    def do_POST(self):
        data = self.rfile.read(int(self.headers['Content-Length'])).decode()
        title = 'Form passed' if self.path == '/form' and parse_qs(data) == {'typed': ['x']} else 'Form failed'
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
(root / 'url').write_text(f'http://127.0.0.1:{fixture.server_port}/start')
fixture.serve_forever()
PY
server=$!
for i in $(seq 1 100); do
    if [ -s "$work/url" ]; then break; fi
    kill -0 "$server" 2>/dev/null || { echo 'Fixture server failed to start.' >&2; exit 1; }
    sleep 0.05
done
[ -s "$work/url" ] || { echo 'Fixture server timed out.' >&2; exit 1; }
"$work/test-browser" "$(cat "$work/url")" "$work/hmr" "$work/escaped"
