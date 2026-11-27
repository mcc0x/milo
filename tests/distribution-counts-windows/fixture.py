import ctypes
from contextlib import ExitStack
from datetime import date, datetime, timedelta, timezone
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import sys
import time

HOST = "counts-production.up.railway.app"
URL = f"https://{HOST}"
ROOT = Path.cwd()
CURL = Path(os.environ["SystemRoot"]) / "System32/curl.exe"
LEDGER = ROOT / "evidence/ledger.jsonl"
CRL_LEDGER = ROOT / "evidence/crl.log"

def require_local():
    result = {r[4][0] for r in socket.getaddrinfo(HOST, 443, type=socket.SOCK_STREAM)}
    assert result == {"127.0.0.1"}, f"Refusing: production hostname does not resolve to IPv4 loopback only: {result}"

def serve():
    from threading import Thread
    pki = ROOT / "pki"
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.set_alpn_protocols(["http/1.1"])
    ctx.load_cert_chain(pki / "leaf.pem", pki / "leaf.key")
    ctx.sni_callback = lambda _, name, __: None if name == HOST else ssl.ALERT_DESCRIPTION_UNRECOGNIZED_NAME
    crl = (pki / "fixture.crl").read_bytes()

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def reply(self, code, body=b"", media=None):
            self.send_response_only(code)
            if media:
                self.send_header("Content-Type", media)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)
            self.close_connection = True

        def do_GET(self):
            key = self.headers.get("Host")
            if self.server.server_port == 443 and self.path == "/_lab_health" and key == HOST:
                self.reply(204)
            elif self.server.server_port == 18443 and self.path == "/fixture.crl" and key == "127.0.0.1:18443":
                with CRL_LEDGER.open("a", encoding="ascii") as ledger:
                    ledger.write("served\n")
                self.reply(200, crl, "application/pkix-crl")
            else:
                self.reply(404)

        def do_POST(self):
            event = {"event": "rejected"}
            try:
                assert self.server.server_port == 443
                assert self.path == "/v1/counts" and self.request_version == "HTTP/1.1"
                actual = [k.lower() for k in self.headers]
                assert sorted(actual) == ["accept", "content-length", "content-type", "host"]
                assert self.headers["Host"] == HOST and self.headers["Accept"] == "*/*"
                assert self.headers["Content-Type"] == "application/json"
                n = int(self.headers["Content-Length"])
                assert 0 < n <= 128
                self.connection.settimeout(3)
                pairs = json.loads(self.rfile.read(n), object_pairs_hook=tuple)
                assert sorted(k for k, _ in pairs) == ["day", "monthly_active", "new_install"]
                body = dict(pairs)
                assert date.fromisoformat(body["day"]).isoformat() == body["day"]
                assert type(body["new_install"]) is bool and type(body["monthly_active"]) is bool
                status = 503 if (ROOT / "force-503").exists() else 204
                event = {"event": "accepted", "tls": self.connection.version(), "status": status, **body}
            except (ValueError, TypeError, AssertionError, OSError):
                status = 400
            with LEDGER.open("a", encoding="ascii") as ledger:
                ledger.write(json.dumps(event, separators=(",", ":")) + "\n")
            self.reply(status)

    require_local()
    LEDGER.touch(exist_ok=False)
    CRL_LEDGER.touch(exist_ok=False)
    with ThreadingHTTPServer(("127.0.0.1", 18443), Handler) as crl_server, ThreadingHTTPServer(("127.0.0.1", 443), Handler) as tls_server:
        tls_server.socket = ctx.wrap_socket(tls_server.socket, server_side=True, do_handshake_on_connect=False)
        Thread(target=crl_server.serve_forever, daemon=True).start()
        (ROOT / "ready").touch()
        tls_server.serve_forever()

def curl(url):
    return subprocess.run([CURL, "-q", "--noproxy", "*", "--proto", "=https", "--http1.1",
                           "--no-ssl-auto-client-cert", "--max-time", "5", "--fail", "--silent",
                           "--show-error", url], capture_output=True)

def negative():
    require_local()
    r = curl(URL + "/_lab_health")
    diagnostic = r.stderr.decode(errors="replace")
    assert r.returncode in {35, 60} and any(word in diagnostic for word in ("UNTRUSTED_ROOT", "untrusted", "not trusted", "CERT_E_UNTRUSTEDROOT")), diagnostic
    assert not LEDGER.read_bytes()
    print("Pre-trust Schannel rejected the exact mapped hostname's untrusted leaf.")

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def qualify(exe_pin, installer_pin, manifest_pin):
    from installed import pane, preload, state, until
    assert not ctypes.windll.shell32.IsUserAnAdmin(), "Installer and candidate must run as standard user"
    require_local()
    response = curl(URL + "/_lab_health")
    assert response.returncode == 0, response.stderr.decode(errors="replace")
    assert CRL_LEDGER.read_text().count("served") >= 1, "No Schannel CRL fetch observed"
    assert not LEDGER.read_bytes(), "Health controls must not POST"
    offhost = curl("https://192.0.2.1/")
    assert offhost.returncode in {7, 28}, "Reserved off-host probe did not fail to connect"
    print("Default Schannel verified mapped hostname and guest-local CRL; off-host TEST-NET curl failed.")

    pkg = ROOT / "package"
    script = pkg / "install-windows.ps1"
    asset = pkg / "milo-x86_64-pc-windows-msvc.exe"
    assert (digest(asset), digest(script), digest(pkg / "SHA256SUMS")) == (exe_pin, installer_pin, manifest_pin)
    exe = ROOT / "installed/milo.exe"
    ps = Path(os.environ["SystemRoot"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"
    invocation = [ps, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", script]
    env = dict(os.environ, MILO_BIN_DIR=str(exe.parent), MILO_TELEMETRY="0")
    env["PSMODULEPATH"] = str(ps.parent / "Modules")
    git = Path((ROOT / "git.txt").read_text(encoding="utf-8-sig").strip())
    project = ROOT / "project"
    project.mkdir()
    subprocess.run([git, "init", "--template=", "-b", "main", project], check=True)
    subprocess.run([git, "fast-import", "--quiet", "--done"], cwd=project, check=True, input=b"commit refs/heads/main\ncommitter Milo Fixture <fixture@example.invalid> 1000000000 +0000\ndata <<END\nCounts fixture commit\nEND\ndone\n")
    zone = Path(str(script) + ":Zone.Identifier")
    zone.write_bytes(b"[ZoneTransfer]\r\nZoneId=3\r\n")
    assert b"ZoneId=3" in zone.read_bytes() and digest(script) == installer_pin
    stock = [ps, "-NoLogo", "-NoProfile", "-NonInteractive", "-File", script]
    policy = subprocess.run([ps, "-NoProfile", "-Command", "Get-ExecutionPolicy -List; Get-ExecutionPolicy"],
                            env=env, capture_output=True, check=True).stdout
    print(policy.decode(errors="replace").strip())
    observed = subprocess.run(stock, env=env, capture_output=True)
    if exe.exists():
        assert observed.returncode == 0, observed.stderr
        print("Stock PowerShell accepted the Internet-zone-marked installer.")
    else:
        assert observed.returncode != 0
        diagnostic = observed.stderr.decode(errors="replace")
        assert any(word in diagnostic.lower() for word in ("not digitally signed", "running scripts is disabled", "pssecurityexception")), diagnostic
        print("Stock PowerShell blocked the Internet-zone-marked installer; verified bytes then installed with a single process-scoped Bypass.")
        subprocess.run(invocation, env=env, check=True)
    unchanged = subprocess.run([ps, "-NoProfile", "-Command", "Get-ExecutionPolicy -List; Get-ExecutionPolicy"],
                               env=env, capture_output=True, check=True).stdout
    assert policy == unchanged, "Execution policy changed"
    assert digest(exe) == exe_pin
    assert (exe.parent / ".milo-install-receipt").read_text().splitlines()[0] == exe_pin
    assert digest(exe.parent / "uninstall-milo.ps1") == installer_pin
    print(f"Installed sealed package: {exe_pin}")

    def events():
        return [json.loads(line) for line in LEDGER.read_text(encoding="ascii").splitlines()]

    def quiet(expected):
        time.sleep(3.2)
        assert len(events()) == expected, events()

    now = datetime.now(timezone.utc)
    midnight = (now + timedelta(days=1)).replace(hour=0, minute=0, second=0, microsecond=0)
    assert (midnight - now).total_seconds() > 180, "Rerun away from UTC midnight"
    today = now.date()
    epoch = date(1970, 1, 1)
    day_bytes = f"{(today - epoch).days}\n".encode()
    month_before = today.replace(day=1) - timedelta(days=1)
    main = ROOT / "main"

    def next_report(profile, new, monthly, status=204):
        before = len(events())
        with pane(profile, git):
            until(lambda: len(events()) > before, "Sealed exe did not reach the exact URL TLS stub")
        [event] = events()[before:]
        assert event.pop("tls") in {"TLSv1.2", "TLSv1.3"}
        assert event == dict(event="accepted", status=status, day=today.isoformat(), new_install=new, monthly_active=monthly), event
        assert state(profile).read_bytes() == day_bytes
        print(f"Captured exact three fields over TLS: new_install={new}, monthly_active={monthly}; response {status}.")

    with ExitStack() as stack:
        stack.callback(subprocess.run, [ps, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", exe.parent / "uninstall-milo.ps1"], env=env, check=True)
        with pane(main, git):
            with pane(main, git):
                assert state(main).read_bytes() == b"first-seen\n"
        quiet(0)
        print("First interactive launch and concurrent launch were silent.")

        for label, override in (("QA opt-out", {"MILO_TELEMETRY": "0"}), ("CI", {"CI": "true"}), ("GitHub Actions", {"GITHUB_ACTIONS": "true"})):
            p = ROOT / label
            if label == "QA opt-out":
                saved = p / "Roaming/milo/anonymous-counts.conf"
                saved.parent.mkdir(parents=True)
                saved.write_bytes(b"on\n")
                preload(p, month_before)
            with pane(p, git, **override):
                pass
            if label == "QA opt-out":
                assert saved.read_bytes() == b"on\n"
                assert state(p).read_bytes() == f"{(month_before - epoch).days}\n".encode()
            else:
                assert not state(p).exists(), label
        off = ROOT / "persisted-opt-out"
        prefer = off / "Roaming/milo/anonymous-counts.conf"
        prefer.parent.mkdir(parents=True)
        prefer.write_bytes(b"off\n")
        with pane(off, git):
            pass
        assert not state(off).exists() and prefer.read_bytes() == b"off\n"

        unattended = ROOT / "unattended"
        unattended.mkdir()
        no_tty = {key: os.environ[key] for key in ("SystemRoot", "WINDIR", "COMSPEC", "TEMP", "TMP")}
        no_tty.update(USERPROFILE=str(unattended), APPDATA=str(unattended / "Roaming"),
                      LOCALAPPDATA=str(unattended / "Local"), PATH=str(CURL.parent))
        subprocess.run([exe, "--version"], env=no_tty, capture_output=True, stdin=subprocess.DEVNULL, check=True)
        p = subprocess.run([exe, "history"], cwd=project, env=no_tty, capture_output=True, stdin=subprocess.DEVNULL)
        assert p.returncode != 0 and b"terminal" in p.stderr.lower(), p.stderr
        assert not state(unattended).exists()
        quiet(0)
        print("Persisted opt-out, QA env, CI, Actions, redirected version and no-terminal pane were silent.")

        next_report(main, True, True)
        with pane(main, git):
            pass
        subprocess.run(invocation, env=env, check=True)
        with pane(main, git):
            pass
        quiet(1)
        print("Same day, including after reinstall, did not recount.")

        yesterday = today - timedelta(days=1)
        previous = ROOT / "previous-day"
        preload(previous, yesterday)
        next_report(previous, False, yesterday.month != today.month)
        previous = ROOT / "previous-month"
        preload(previous, month_before)
        next_report(previous, False, True)
        future = ROOT / "future-clock"
        preload(future, today + timedelta(days=1))
        with pane(future, git):
            pass
        quiet(3)

        failed = ROOT / "failed-delivery"
        preload(failed, month_before)
        (ROOT / "force-503").touch()
        try:
            next_report(failed, False, True, 503)
        finally:
            (ROOT / "force-503").unlink()
        with pane(failed, git):
            pass
        quiet(4)
        print("Future high-water and failed-delivery same-day retry were suppressed.")
    assert not exe.exists()
    print("Original kept uninstaller removed the installed sealed executable.")

def diagnose():
    assert not ctypes.windll.shell32.IsUserAnAdmin()
    require_local()
    assert curl("https://192.0.2.1/").returncode in {7, 28}
    health = curl(URL + "/_lab_health")
    assert health.returncode == 0, health.stderr.decode(errors="replace")
    assert CRL_LEDGER.read_text().count("served") >= 1
    assert not LEDGER.read_bytes()
    today = datetime.now(timezone.utc).date().isoformat()
    response = subprocess.run([CURL, "-q", "--proto", "=https", "--noproxy", "*",
              "--globoff", "--http1.1", "--retry", "0", "--max-time", "3", "--max-filesize", "1024",
              "--no-ssl-auto-client-cert", "--user-agent", "", "--header", "Content-Type: application/json",
              "--data-binary", json.dumps({"day": today, "new_install": True, "monthly_active": True}),
              "--write-out", "%{http_code}", "--url", URL + "/v1/counts"], capture_output=True)
    assert response.returncode == 0 and response.stdout == b"204", response.stderr.decode(errors="replace")
    event, = [json.loads(x) for x in LEDGER.read_text().splitlines()]
    assert event.pop("tls") in {"TLSv1.2","TLSv1.3"}
    assert event == dict(event="accepted", status=204, day=today, new_install=True, monthly_active=True)
    print("PASS synthetic system curl: live routing blocked, exact host and CRL verified, exactly one private TLS POST.")

if __name__ == "__main__":
    {"serve": serve, "negative": negative, "qualify": qualify, "diagnose": diagnose}[sys.argv[1]](*sys.argv[2:])
