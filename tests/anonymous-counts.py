"""Exercise an endpoint-enabled Milo in a PTY, with a local receiver and disposable Postgres.

From the repository root: hosted/counts/scripts/pg-ephemeral.sh python3 tests/anonymous-counts.py
Requires cargo, Bun, and initdb/pg_ctl/createdb (set PG_BIN to their directory).
"""

import base64
from contextlib import ExitStack
from datetime import date, datetime, timedelta, timezone
import hashlib
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, HTTPServer
import os
from pathlib import Path
import runpy
import secrets
import shutil
import socket
import subprocess
import tempfile
import threading
import time
from types import SimpleNamespace
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[1]
COUNTS = ROOT / 'hosted/counts'
Terminal = runpy.run_path(str(ROOT / 'tests/backend_reader.py'))['Terminal']


def until(predicate):
    end = time.monotonic() + 8
    while not predicate():
        if time.monotonic() > end:
            raise AssertionError("Timed out waiting for local fixture")
        time.sleep(0.04)


def run(tmp, cleanup):
    db = os.environ.get('MILO_COUNTS_TEST_DATABASE_URL', '')
    if urlsplit(db).hostname != '127.0.0.1' or urlsplit(db).path != '/milo_counts':
        raise SystemExit('Run this test with hosted/counts/scripts/pg-ephemeral.sh.')
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    user, password = secrets.token_hex(16), secrets.token_hex(32)
    def start():
        service = subprocess.Popen(['bun', 'run', '--no-env-file', 'server.ts'], cwd=COUNTS, env={
            **os.environ, 'PORT': str(port), 'MILO_COUNTS_HOST': '127.0.0.1', 'DO_NOT_TRACK': '1',
            'DATABASE_URL': db, 'MILO_COUNTS_ADMIN_USER': user, 'MILO_COUNTS_ADMIN_PASSWORD': password,
        })
        cleanup.callback(service.wait, timeout=15)
        cleanup.callback(service.terminate)
        return service

    service = start()

    def request(method, path, **kwargs):
        conn = HTTPConnection('127.0.0.1', port, timeout=3)
        try:
            conn.request(method, path, **kwargs)
            r = conn.getresponse()
            return r.status, r.read()
        finally:
            conn.close()

    def ready():
        if service.poll() is not None:
            raise AssertionError("Local receiver stopped")
        try:
            return request('GET', '/readyz')[0] == 200
        except ConnectionRefusedError:
            return False

    until(ready)
    assert request('GET', '/')[0] == 401
    attempts = 0

    class Meter(BaseHTTPRequestHandler):
        def do_POST(self):
            nonlocal attempts
            attempts += 1
            assert self.path == '/v1/counts'
            n = int(self.headers['content-length'])
            assert 0 < n <= 128
            status, _ = request('POST', self.path, body=self.rfile.read(n),
                                headers={'Content-Type': self.headers['content-type']})
            self.send_response_only(status)
            self.end_headers()

        def log_message(self, *args):
            pass

    meter = cleanup.enter_context(HTTPServer(('127.0.0.1', 0), Meter))
    threading.Thread(target=meter.serve_forever, daemon=True).start()
    cleanup.callback(meter.shutdown)

    target = tmp / 'target'
    subprocess.run(['cargo', 'build', '--release', '--locked', '--target-dir', target], cwd=ROOT,
                   env={**os.environ, 'MILO_COUNTS_ENDPOINT': f'http://127.0.0.1:{meter.server_port}/v1/counts'},
                   check=True)
    package = tmp / 'package'
    package.mkdir()
    asset = package / 'milo-x86_64-unknown-linux-gnu'
    shutil.copyfile(target / 'release/milo', asset)
    digest = hashlib.file_digest(asset.open('rb'), 'sha256').hexdigest()
    (package / 'SHA256SUMS').write_text(f'{digest}  {asset.name}\n')
    subprocess.run(['sh', ROOT / '.github/ci/stage-linux.sh', package], check=True)
    repo = tmp / 'fixture'
    repo.mkdir()
    (repo / 'sample.txt').touch()
    home = tmp / 'home'
    home.mkdir()
    env = dict(HOME=str(home), PATH=os.defpath, TERM='xterm-256color', LC_ALL='C')
    subprocess.run(['sh', package / 'install-linux.sh'], cwd=repo, env=env, check=True)
    binary = home / '.local/bin/milo'
    assert hashlib.file_digest(binary.open('rb'), 'sha256').hexdigest() == digest
    print(f'Installed Linux x86-64 test binary SHA-256: {digest}', flush=True)

    def pane(wrapper=None, **overrides):
        args = [binary, 'outline'] if wrapper else ['outline']
        return Terminal(wrapper or binary, SimpleNamespace(repo=repo, env=env | overrides), args, False)

    def browse(**overrides):
        with pane(**overrides) as p:
            p.frame(b'sample.txt')
            p.send(b'q')
            assert p.await_exit(3)

    lab = tmp / 'managed-home'
    lab.mkdir()
    browse(HOME=str(lab), MILO_TELEMETRY='0')
    assert not list(lab.rglob('anonymous-counts*')), 'Managed first launch created counting state'
    with pane() as first:
        first.frame(b'sample.txt')
        assert not any('Anonymous usage counts' in ''.join(row) for row in first.grid)
        browse()
        first.send(b'q')
        assert first.await_exit(3)
    browse(CI='true')
    browse(MILO_TELEMETRY='0')
    assert (home / '.local/state/milo/anonymous-counts.state').read_bytes() == b'first-seen\n'
    subprocess.run([binary, '--version'], cwd=repo, env=env, capture_output=True, check=True)
    subprocess.run([binary, 'outline'], cwd=repo, env=env, stdin=subprocess.DEVNULL, capture_output=True)
    off = tmp / 'off'
    config = off / '.config/milo'
    config.mkdir(parents=True)
    (config / 'anonymous-counts.conf').write_bytes(b'off\n')
    browse(HOME=str(off))
    assert attempts == 0, "Unexpected request before an eligible later launch"

    auth = base64.b64encode(f'{user}:{password}'.encode()).decode()
    def dashboard():
        status, html = request('GET', '/', headers={'Authorization': f'Basic {auth}'})
        assert status == 200
        return html

    def counted():
        return b'1 since counting began' in dashboard()

    with pane() as later:
        later.frame(b'sample.txt')
        until(counted)
        browse()
        later.send(b'q')
        assert later.await_exit(3)
    browse()
    assert attempts == 1, "A second eligible launch recounted the same day"

    # Simulate an existing installation whose last attempted day was in the previous
    # UTC month. Only disposable local state changes, never the host or receiver clock.
    today = datetime.now(timezone.utc).date()
    previous = today.replace(day=1) - timedelta(days=1)
    other = tmp / 'returning-home'
    state = other / '.local/state/milo/anonymous-counts.state'
    state.parent.mkdir(parents=True)
    state.write_text(f'{(previous - date(1970, 1, 1)).days}\n')
    with pane(HOME=str(other)) as returning:
        returning.frame(b'sample.txt')
        until(lambda: b'<dt>Monthly active this month</dt><dd>2</dd>' in dashboard())
        returning.send(b'q')
        assert returning.await_exit(3)
    assert attempts == 2 and counted()
    assert b'<dt>Active today (UTC)</dt><dd>2</dd>' in dashboard()

    if wrapper := os.environ.get('MILO_TEST_WITHOUT_CURL'):
        missing = tmp / 'no-curl-home'
        absent_state = missing / '.local/state/milo/anonymous-counts.state'
        absent_state.parent.mkdir(parents=True)
        absent_state.write_text('noticed\n')
        with pane(wrapper=wrapper, HOME=str(missing)) as without_curl:
            without_curl.frame(b'sample.txt')
            until(lambda: absent_state.read_text() != 'noticed\n')
            without_curl.send(b'q')
            assert without_curl.await_exit(5)
        assert attempts == 2
        print('Installed pane worked without executable system curl; no report reached the listening meter.')

    # The endpoint refuses a connection. The foreground pane must still open and quit.
    meter.shutdown()
    meter.server_close()
    offline = tmp / 'offline-home'
    offline_state = offline / '.local/state/milo/anonymous-counts.state'
    offline_state.parent.mkdir(parents=True)
    offline_state.write_text('noticed\n')
    browse(HOME=str(offline))
    service.terminate()
    service.wait(timeout=15)
    service = start()
    until(ready)
    assert request('GET', '/')[0] == 401
    assert counted(), "The same credentials and aggregate should survive restarting the service"
    assert b'<dt>Active today (UTC)</dt><dd>2</dd>' in dashboard()
    print('First-launch, opt-out, CI and scripts were silent. Synthetic new and returning fixtures: '
          '2 daily active, 2 monthly active, 1 new; authenticating after a restart retained those totals.')


if __name__ == '__main__':
    with tempfile.TemporaryDirectory(prefix='milo-counts-e2e-') as tmp, ExitStack() as cleanup:
        run(Path(tmp), cleanup)
