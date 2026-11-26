"""Drive the installed candidate on native Windows. See qualify.ps1 for staging."""

import base64
from contextlib import contextmanager, ExitStack
import ctypes
from datetime import date, datetime, timedelta, timezone
import hashlib
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import threading
import time


WORK = Path.cwd()
BIN = WORK / 'installed' / 'milo.exe'
ENDPOINT_PORT = 36129


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def until(condition, reason):
    deadline = time.monotonic() + 15
    while not condition():
        if time.monotonic() >= deadline:
            raise AssertionError(reason)
        time.sleep(0.05)


def http(port, method, path, **kwargs):
    conn = HTTPConnection('127.0.0.1', port, timeout=4)
    try:
        conn.request(method, path, **kwargs)
        response = conn.getresponse()
        return response.status, response.read()
    finally:
        conn.close()


@contextmanager
def cluster(pg):
    password = secrets.token_hex(24)
    pwfile = WORK / 'pg.pw'
    data = WORK / 'data'
    pwfile.write_text(password, encoding='ascii')
    try:
        subprocess.run([pg / 'initdb.exe', '-D', data, '-U', 'milo', '--pwfile', pwfile,
                        '-A', 'scram-sha-256', '-E', 'UTF8', '--locale=C'], check=True)
    finally:
        pwfile.unlink()
    db_port = port()
    ctl = [pg / 'pg_ctl.exe', '-D', data, '-w']
    subprocess.run([*ctl, '-l', WORK / 'pg.log', '-o', f'-h 127.0.0.1 -p {db_port}', 'start'], check=True)
    try:
        subprocess.run([pg / 'createdb.exe', '-U', 'milo', '-h', '127.0.0.1', '-p', str(db_port), 'milo_counts'],
                       env=os.environ | {'PGPASSWORD': password}, check=True)
        yield f'postgres://milo:{password}@127.0.0.1:{db_port}/milo_counts'
    finally:
        subprocess.run([*ctl, '-m', 'immediate', 'stop'], check=True)


@contextmanager
def receiver(backend, db, user, password):
    env = os.environ | {
        'PORT': str(backend), 'MILO_COUNTS_HOST': '127.0.0.1', 'DATABASE_URL': db,
        'MILO_COUNTS_ADMIN_USER': user, 'MILO_COUNTS_ADMIN_PASSWORD': password, 'DO_NOT_TRACK': '1',
    }
    service = subprocess.Popen([WORK / 'bun.exe', 'run', '--no-env-file', 'server.ts'], cwd=WORK / 'counts', env=env)
    try:
        def ready():
            assert service.poll() is None, 'Receiver exited during startup'
            try:
                return http(backend, 'GET', '/readyz')[0] == 200
            except ConnectionRefusedError:
                return False
        until(ready, 'Receiver never became ready')
        yield
    finally:
        service.terminate()
        service.wait(timeout=10)


@contextmanager
def meter(backend):
    attempts = []

    class Meter(BaseHTTPRequestHandler):
        def do_POST(self):
            length = int(self.headers.get('Content-Length', '0'))
            if self.path != '/v1/counts' or not 0 < length <= 128:
                attempts.append((400, None))
                self.send_response_only(400)
                self.end_headers()
                return
            body = self.rfile.read(length)
            try:
                status, _ = http(backend, 'POST', self.path, body=body,
                                 headers={'Content-Type': self.headers['Content-Type']})
            except ConnectionRefusedError:
                status = 503
            attempts.append((status, body))
            self.send_response_only(status)
            self.end_headers()

        def log_message(self, *args):
            pass

    with ThreadingHTTPServer(('127.0.0.1', ENDPOINT_PORT), Meter) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield attempts
        finally:
            server.shutdown()
            thread.join()


@contextmanager
def pane(profile, git, *, choice=None, no_git=False, **overrides):
    native = Path(os.environ['SystemRoot']) / 'System32'
    want = 'Git could not be run.' if no_git else 'Counts fixture commit'
    env = {key: os.environ[key] for key in ('SystemRoot', 'WINDIR', 'COMSPEC', 'TEMP', 'TMP')}
    env.update(PATH=str(native) if no_git else os.pathsep.join([str(git.parent), str(native)]),
               MILO_TEST_PROFILE=str(profile), MILO_TEST_FRAME=want, **overrides)
    if choice:
        env['MILO_TEST_CHOICE'] = choice
    args = [WORK / 'driver.exe', 'installed_counts_pane', '--exact', '--nocapture']
    with subprocess.Popen(args, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True) as driver:
        try:
            for line in driver.stdout:
                if 'installed pane ready' in line:
                    break
            else:
                raise AssertionError('The ConPTY driver exited without the expected pane')
            yield
        finally:
            driver.stdin.close()
            assert driver.wait(timeout=40) == 0, 'The installed pane did not exit cleanly'


def state(profile):
    return profile / 'Local' / 'milo' / 'anonymous-counts.state'


def preload(profile, previous):
    file = state(profile)
    file.parent.mkdir(parents=True)
    file.write_bytes(f'{(previous - date(1970, 1, 1)).days}\n'.encode())


@contextmanager
def without_curl(curl):
    from ctypes import wintypes
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    open_file = kernel.CreateFileW
    open_file.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, wintypes.LPVOID,
                          wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    open_file.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = open_file(str(curl), 0x80000000, 0, None, 3, 0, None)
    if handle == wintypes.HANDLE(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        try:
            subprocess.run([curl, '-q', '--version'], check=True)
        except OSError as error:
            assert error.winerror in (5, 32), 'Unexpected reason for curl launch failure'
        else:
            raise AssertionError('The exclusive read handle did not prevent curl execution')
        yield
    finally:
        assert kernel.CloseHandle(handle)


def qualify(pg, git):
    assert not ctypes.windll.shell32.IsUserAnAdmin(), 'Run the installer and panes as a standard user'
    native_curl = Path(os.environ['SystemRoot']) / 'System32' / 'curl.exe'
    subprocess.run([native_curl, '-q', '--version'], check=True)
    subprocess.run([git, '--version'], check=True)
    project = WORK / 'project'
    project.mkdir()
    subprocess.run([git, 'init', '--template=', '-b', 'main', project], check=True)
    subprocess.run([git, 'fast-import', '--quiet', '--done'], cwd=project, check=True, input=b'''commit refs/heads/main
committer Milo Fixture <fixture@example.invalid> 1000000000 +0000
data <<END
Counts fixture commit
END
done
''')

    script = WORK / 'package' / 'install-windows.ps1'
    powershell = Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0'
    installer = [powershell / 'powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', script]
    env = os.environ | {'MILO_BIN_DIR': str(BIN.parent)}
    # The orchestrator was started from pwsh 7. Give Windows PowerShell its
    # own built-in modules, as it would find in a normal user session.
    env['PSMODULEPATH'] = str(powershell / 'Modules')
    subprocess.run(installer, env=env, check=True)
    with ExitStack() as stack:
        stack.callback(subprocess.run, [*installer, '-Uninstall'], env=env, check=True)
        asset = WORK / 'package' / 'milo-x86_64-pc-windows-msvc.exe'
        digest = hashlib.sha256(asset.read_bytes()).hexdigest()
        assert hashlib.sha256(BIN.read_bytes()).hexdigest() == digest
        assert (BIN.parent / '.milo-install-receipt').read_text().splitlines()[0] == digest
        print(f'Qualifying installed {BIN.name}, SHA-256 {digest}')

        db = stack.enter_context(cluster(pg))
        backend = port()
        attempts = stack.enter_context(meter(backend))
        user, password = secrets.token_hex(16), secrets.token_hex(32)
        auth = 'Basic ' + base64.b64encode(f'{user}:{password}'.encode()).decode()

        def dashboard(active, installs, month):
            assert http(backend, 'GET', '/')[0] == 401
            status, body = http(backend, 'GET', '/', headers={'Authorization': auth})
            assert status == 200
            for label, value in [('Active today (UTC)', active), ('New installs, 30 days', installs),
                                 ('Monthly active this month', month)]:
                assert f'<dt>{label}</dt><dd>{value}</dd>'.encode() in body, f'Unexpected {label} total'
            assert f'{installs} since counting began'.encode() in body

        now = datetime.now(timezone.utc)
        midnight = (now + timedelta(days=1)).replace(hour=0, minute=0, second=0, microsecond=0)
        if (midnight - now).total_seconds() < 180:
            time.sleep((midnight - now).total_seconds() + 1)
        today = datetime.now(timezone.utc).date()
        epoch_day = f'{(today - date(1970, 1, 1)).days}\n'.encode()
        main, off = WORK / 'main-user', WORK / 'off-user'
        main.mkdir()
        off.mkdir()
        credentials = backend, db, user, password

        def next_report(profile, new, monthly, status=204):
            before = len(attempts)
            with pane(profile, git):
                until(lambda: len(attempts) > before, 'No request reached the loopback meter')
            [(result, body)] = attempts[before:]
            assert result == status
            assert json.loads(body) == {'day': today.isoformat(), 'new_install': new, 'monthly_active': monthly}
            assert state(profile).read_bytes() == epoch_day

        with receiver(*credentials):
            with pane(main, git, choice='keep'):
                with pane(main, git):
                    pass
            assert state(main).read_bytes() == b'noticed\n'
            with pane(off, git, choice='off'):
                pass
            with pane(off, git):
                pass
            with pane(main, git, CI='true'):
                pass
            with pane(main, git, MILO_TELEMETRY='0', no_git=True):
                pass
            assert state(main).read_bytes() == b'noticed\n'
            assert not attempts
            dashboard(0, 0, 0)
            next_report(main, True, True)
            subprocess.run(installer, env=env, check=True)
            with pane(main, git):
                pass
            assert len(attempts) == 1
            dashboard(1, 1, 1)
            print('First-start, opt-out, CI and opt-out environment were silent. Reinstall did not recount.')

            yesterday = today - timedelta(days=1)
            previous_month = today.replace(day=1) - timedelta(days=1)
            previous = WORK / 'previous-day-user'
            preload(previous, yesterday)
            month_changed = yesterday.month != today.month
            next_report(previous, False, month_changed)
            previous = WORK / 'previous-month-user'
            preload(previous, previous_month)
            next_report(previous, False, True)
            dashboard(3, 1, 2 + month_changed)
            print('Prior UTC day and prior UTC month added active counts without a new install.')

            blocked = WORK / 'no-transport-user'
            preload(blocked, previous_month)
            with without_curl(native_curl), pane(blocked, git):
                until(lambda: state(blocked).read_bytes() == epoch_day, 'Unavailable curl blocked the day claim')
            with pane(blocked, git):
                pass
            assert len(attempts) == 3
            print('With system curl held open read-only and non-executable, History still worked; no retry.')

        failed = WORK / 'offline-user'
        preload(failed, previous_month)
        next_report(failed, False, True, status=503)
        with receiver(*credentials):
            with pane(failed, git):
                pass
            assert len(attempts) == 4, 'A failed delivery was retried after recovery'
            dashboard(3, 1, 2 + month_changed)
            print('Receiver outage did not block the pane or retry; private totals survived restart.')


if __name__ == '__main__':
    tools = json.loads((WORK / 'tools.json').read_text())
    qualify(Path(tools['pg']), Path(tools['git']))
