"""Time a release editor from another process: python3 tests/settings-bench.py target/release/milo.

Only loopback, an empty home and synthetic preferences. Includes JSON decoding and the
one-connection-per-request client. Holds the same directory lock as another Milo save.
"""

from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import fcntl
import http.client
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit


@contextmanager
def editor(binary, home, delay=None):
    env = {"HOME": str(home), "XDG_CONFIG_HOME": str(home / "config"),
           "XDG_STATE_HOME": str(home / "state"), "XDG_RUNTIME_DIR": str(home / "run"),
           "PATH": "/usr/bin:/bin", "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null"}
    if delay is not None:
        env.update(TERM_PROGRAM="rex", REX_SESSION="session:test", REX_BLOCK="block:shell",
                   REX_CONFIG=str(home / "init.lua"), REX_STUB_DELAY=delay, PATH=f"{home}/bin:" + env["PATH"])
    with subprocess.Popen([binary, "web", "--no-open", "--port", "0"], cwd=home, env=env,
                          stdout=subprocess.PIPE, stdin=subprocess.DEVNULL, text=True) as child:
        try:
            link = urlsplit(child.stdout.readline().strip())
            assert link.hostname == "127.0.0.1" and link.fragment

            def request(action=None):
                conn = http.client.HTTPConnection("127.0.0.1", link.port, timeout=10)
                headers = {"Authorization": f"Bearer {link.fragment}"}
                try:
                    if action is None:
                        conn.request("GET", "/api/state", headers=headers)
                    else:
                        headers.update({"Origin": f"http://{link.netloc}", "Content-Type": "application/json"})
                        conn.request("POST", "/api/action", json.dumps(action), headers)
                    reply = conn.getresponse()
                    assert reply.status == 200, reply.status
                    return json.load(reply)
                finally:
                    conn.close()

            yield request
        finally:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                raise


def sample(binary, home, count=41):
    rows = {}

    def measure(name, op, *args):
        start = time.perf_counter_ns()
        result = op(*args)
        rows.setdefault(name, []).append((time.perf_counter_ns() - start) / 1e6)
        return result

    config = home / "config" / "milo"
    config.mkdir(parents=True)
    with editor(binary, home) as request, ThreadPoolExecutor(1) as pool:
        state = request()
        for turn in range(count):
            shown = measure("GET snapshot", request)
            assert state["version"] == shown["version"]
            value = not state["appearance"]["static_marks"]
            draft = measure("change draft", request, {"action": "set", "version": shown["version"],
                                                      "field": "appearance.static_marks", "value": value})
            assert draft["dirty"] and draft["appearance"]["static_marks"] == value
            action = {"action": "save", "version": draft["version"], "group": "preferences", "scope": "global"}
            if turn % 2 == 0:
                state = measure("persist preferences", request, action)
            else:
                lock = os.open(config, os.O_RDONLY)
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX)
                    saving = pool.submit(request, action)
                    time.sleep(0.01)
                    shown = measure("GET during locked save", request)
                    assert not saving.done() and shown == draft
                finally:
                    os.close(lock)
                state = saving.result()
            assert not state["dirty"] and state["appearance"]["static_marks"] == value
    # A successful reply alone does not establish persistence.
    with editor(binary, home) as reopened:
        assert reopened()["appearance"]["static_marks"] == value

    # Unlike an absent Rex, a connected host answers identity, version and viewers
    # as well as the caller's tab context. Only the stub and the editor run here.
    (home / "bin").mkdir()
    rex = home / "bin/rex"
    rex.write_text("""#!/bin/sh
for arg do
    case "$arg" in
    milo_operation=context) cat >/dev/null; reply='window:test\\nok - 120 40'; break ;;
    whoami) reply='{"rex_version":"1","api_revision":19,"host_id":"00000000-0000-0000-0000-000000000001"}'; break ;;
    version) reply='{"rex_version":"1","api_revision":19}'; break ;;
    client) reply='{"clients":[]}'; break ;;
    esac
done
test -n "$reply" || exit 1
test "$REX_STUB_DELAY" = 0 || sleep "$REX_STUB_DELAY"
printf '%b\\n' "$reply"
""")
    rex.chmod(0o700)

    def first_state(delay):
        with editor(binary, home, delay) as request:
            start = time.perf_counter_ns()
            state = request()
            assert state["host"]["rex"]["compatibility"] == "checked"
            assert state["context"]["inside_rex"] and state["capabilities"]["capture"]
            return (time.perf_counter_ns() - start) / 1e6

    for delay in ["0", "0.05"]:
        rows[f"link to first state (Rex {delay}s)"] = [first_state(delay) for _ in range(20)]
    for label, samples in rows.items():
        samples.sort()
        quantile = lambda p: samples[math.ceil(p * len(samples)) - 1]
        print(f"{label}\t{len(samples)}\t{quantile(.5):.3f}\t{quantile(.95):.3f}")


if __name__ == "__main__":
    print("request\tsamples\tp50_ms\tp95_ms")
    with tempfile.TemporaryDirectory(prefix="milo-settings-bench-") as folder:
        sample(Path(sys.argv[1]).resolve(), Path(folder))
