#!/usr/bin/env python3
"""Profile release Milo's tab inbox and handed review with synthetic files.

Run: python3 tests/backend_reader.py target/release/milo [--base PATH] [--runs 5]
Use --runs 0 for Git operation counts and the controlled responsiveness probe only.
With --base the two builds take turns on the same generated files, one at a time.
All samples use a real PTY. RSS checkpoints are Milo-only `ps` readings (KiB);
peak RSS is wait4's per-process high-water mark. Git counting and the gate use
an isolated wrapper; measured latencies/RSS use the unwrapped Git executable.
No Rex process or installed state is used: the synthetic block only enables the
reader's inbox, which gets the same NUL-delimited records as a floating reader.
"""

import argparse
from collections import Counter, defaultdict
from contextlib import contextmanager
import errno
import fcntl
import json
import math
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

FRAME = b"\x1b[?2026l"
FRAME_START = b"\x1b[?2026h"
ASK = b"\x1b[c"
ANSWER = b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[6;16;8t\x1b[?62;22c"
ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
ROWS, COLS = 50, 160
WAIT = 20
TABS = 4
TAB_ROWS = 55_000
PATCH_FILES = 24
PATCH_ROWS = 22_000
PATCH_STRIDE = 200
WRAPPER = r'''#!/bin/sh
kind=$1
full=
for arg do
    case "$arg" in
        config|rev-parse|rev-list|show|log|status|diff|diff-tree|ls-files|cat-file|check-ignore)
            kind=$arg;;
        --unified=8388608) full=whole;;
    esac
done
printf '%s\n' "$kind${full:+:$full}" >> "$PROFILE_LOG"
if [ -n "$full" ] && [ -e "$PROFILE_ARM" ]; then
    rm -f "$PROFILE_ARM"
    printf '%s\n' "$$" > "$PROFILE_ENTERED"
    while [ ! -e "$PROFILE_RELEASE" ]; do sleep 0.01; done
    printf 'gate-released\n' >> "$PROFILE_LOG"
fi
if [ "$1" = --exec-path ]; then
    printf '%s\n' "$PROFILE_WRAPPER"
    exit 0
fi
exec "$PROFILE_GIT" "$@"
'''


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def marker(kind, index, version):
    return (f"{kind}{index:02d}{version}row00000").encode()


def make_text(kind, index, rows, width, version, stride):
    return b"".join((f"{kind}{index:02d}{version if row % stride == 0 else 'stable'}row{row:05d}").encode()
                    .ljust(width - 1, b".") + b"\n" for row in range(rows))


class Project:
    def __init__(self, root):
        self.root = root
        self.repo = root / "repo"
        self.repo.mkdir()
        (root / "home").mkdir()
        self.state = root / "state"
        self.env = {"HOME": str(root / "home"), "XDG_STATE_HOME": str(self.state),
                    "XDG_CONFIG_HOME": str(root / "config"), "PATH": os.defpath,
                    "TERM": "xterm-256color", "TERM_PROGRAM": "rex", "LC_ALL": "C",
                    "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
                    "REX_SESSION": "synthetic-reader-profile", "REX_BLOCK": "synthetic-reader-profile"}
        self.git_bin = shutil.which("git", path=self.env["PATH"])
        require(self.git_bin, "git is required on os.defpath")
        self.log = root / "git.log"
        self.arm, self.entered, self.release = [root / name for name in ("arm", "entered", "release")]
        exe = Path(os.fsdecode(self.git("--exec-path").strip())) / "git"
        self.wrapper = root / "wrapper"
        self.wrapper.mkdir()
        script = self.wrapper / "git"
        script.write_text(WRAPPER)
        script.chmod(0o700)
        self.count_env = dict(self.env, PATH=f"{self.wrapper}:{self.env['PATH']}",
                              PROFILE_LOG=str(self.log), PROFILE_ARM=str(self.arm),
                              PROFILE_ENTERED=str(self.entered), PROFILE_RELEASE=str(self.release),
                              PROFILE_WRAPPER=str(self.wrapper),
                              PROFILE_GIT=str(exe if exe.is_file() else self.git_bin))
        self.git("init", "-q", "--template=", "-b", "main")
        self.write("control.txt", b"readerprofilecontrol\n")
        for i in range(TABS):
            self.write(self.tab(i), make_text("tab", i, TAB_ROWS, 104, "before", 20))
        for i in range(PATCH_FILES):
            self.write(self.patch_name(i), make_text("patch", i, PATCH_ROWS, 81, "before", PATCH_STRIDE))
        self.commit("base")
        for i in range(TABS):
            self.write(self.tab(i), make_text("tab", i, TAB_ROWS, 104, "commit", 20))
        self.commit("committed tab changes")
        self.head = os.fsdecode(self.git("rev-parse", "HEAD").strip())
        for i in range(TABS):
            self.write(self.tab(i), make_text("tab", i, TAB_ROWS, 104, "working", 20))
        for i in range(PATCH_FILES):
            self.write(self.patch_name(i), make_text("patch", i, PATCH_ROWS, 81, "handed", PATCH_STRIDE))
        self.full, self.short = (root / "full.patch", root / "short.patch")
        for path, files in [(self.full, PATCH_FILES), (self.short, 2)]:
            data = self.git("diff", "--unified=2", "HEAD", "--", *(self.patch_name(i) for i in range(files)))
            require(0 < len(data) < 2 << 20, f"{path.name} exceeds the handed patch limit")
            path.write_bytes(data)
        self.working_diff = self.git("diff", "--unified=8388608", "HEAD", "--", self.tab(0))
        self.commit_diff = self.git("diff", "--unified=8388608", "HEAD^", "HEAD", "--", self.tab(0))
        for diff in (self.working_diff, self.commit_diff):
            require(0 < len(diff) < 8 << 20, "tab diff exceeds the reader limit")

    @staticmethod
    def tab(i):
        return f"tab-{i}.txt"

    @staticmethod
    def patch_name(i):
        return f"patch-{i:02d}.txt"

    def write(self, name, data):
        (self.repo / name).write_bytes(data)

    def git(self, *args):
        return subprocess.check_output([self.git_bin, *args], env=self.env, cwd=self.repo, timeout=WAIT)

    def commit(self, message):
        self.git("add", "-A")
        self.git("-c", "user.name=Milo Profile", "-c", "user.email=profile@example.invalid", "commit", "-qm", message)

    def counts(self):
        return Counter(self.log.read_text().splitlines()) if self.log.exists() else Counter()

    def inbox(self):
        folder = self.state / "milo" / "readers"
        endpoints = list(folder.iterdir()) if folder.is_dir() else []
        require(len(endpoints) == 1 and endpoints[0].is_fifo(), f"expected one reader inbox, saw {endpoints}")
        return endpoints[0]

    def request(self, *requests):
        payload = b"".join(os.fsencode(file) + b"\0" + os.fsencode(commit) + b"\0" for file, commit in requests)
        with open(self.inbox(), "wb", buffering=0, opener=self.nonblocking) as pipe:
            require(pipe.write(payload) == len(payload), "incomplete reader inbox write")

    @staticmethod
    def nonblocking(path, flags):
        return os.open(path, flags | os.O_NONBLOCK)


class Terminal:
    def __init__(self, binary, project, args, counted):
        self.master, slave = pty.openpty()
        self.buffer = bytearray()
        self.grid = [[" "] * COLS for _ in range(ROWS)]
        self.tail = b""
        self.answered = False
        self.usage = None
        self.closed = False
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        self.started = time.monotonic_ns()
        try:
            self.child = subprocess.Popen([binary, *args], cwd=project.repo,
                                          env=project.count_env if counted else project.env,
                                          stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        except Exception:
            os.close(self.master)
            raise
        finally:
            os.close(slave)

    def send(self, text):
        os.write(self.master, text)

    def read(self, wait):
        if self.closed or not select.select([self.master], [], [], wait)[0]:
            return False
        try:
            chunk = os.read(self.master, 65536)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            chunk = b""
        if not chunk:
            self.closed = True
            return False
        self.buffer.extend(chunk)
        self.tail = (self.tail + chunk)[-1500:]
        if not self.answered and ASK in self.buffer:
            self.send(ANSWER)
            self.answered = True
        return True

    def paint(self, frame):
        # Milo writes only changed cells, so a partial redraw is not a fresh
        # screen. Its text-only frames use absolute cursor moves and SGR.
        start = frame.rfind(FRAME_START)
        if start < 0:
            return False
        frame = frame[start + len(FRAME_START):]
        row = col = offset = 0

        def text(end):
            nonlocal col
            if row >= ROWS:
                return
            for char in frame[offset:end].decode(errors="replace"):
                if col >= COLS:
                    break
                if not unicodedata.combining(char):
                    self.grid[row][col] = char
                    col += 1
                    if unicodedata.east_asian_width(char) in ("W", "F") and col < COLS:
                        self.grid[row][col] = " "
                        col += 1

        for sequence in ANSI.finditer(frame):
            text(sequence.start())
            token = sequence.group()
            if token.endswith(b"H") and b";" in token:
                row, col = (int(n) - 1 for n in token[2:-1].split(b";"))
            elif token == b"\x1b[2J":
                self.grid = [[" "] * COLS for _ in range(ROWS)]
            offset = sequence.end()
        text(len(frame))
        return True

    def take_frames(self):
        while (at := self.buffer.find(FRAME)) >= 0:
            frame = bytes(self.buffer[:at])
            del self.buffer[:at + len(FRAME)]
            start = time.monotonic_ns()
            if self.paint(frame):
                yield start

    def frame(self, wanted):
        end = time.monotonic() + WAIT
        label = wanted.decode()
        while time.monotonic() < end:
            for started in self.take_frames():
                if any(label in "".join(row) for row in self.grid):
                    return started
            if not self.read(max(0, end - time.monotonic())):
                break
        screen = "\n".join("".join(row).rstrip() for row in self.grid[:9])
        raise RuntimeError(f"no frame showing {label!r}; screen:\n{screen}\nPTY tail: {self.tail[-300:]!r}")

    def settle(self):
        # Let initial terminal responses redraw before counting an isolated action.
        end = time.monotonic() + .2
        while time.monotonic() < end and self.read(.035):
            pass
        for _ in self.take_frames():
            pass

    def rss(self):
        raw = subprocess.check_output(["/bin/ps", "-p", str(self.child.pid), "-o", "rss="], timeout=2)
        return int(raw)

    def reap(self):
        if self.usage is None:
            pid, status, usage = os.wait4(self.child.pid, os.WNOHANG)
            if pid:
                self.child.returncode = os.waitstatus_to_exitcode(status)
                self.usage = usage
        return self.usage is not None

    def await_exit(self, timeout):
        until = time.monotonic() + timeout
        while not self.reap() and time.monotonic() < until:
            self.read(.01)
            if self.closed:
                time.sleep(.002)
        return self.reap()

    def __enter__(self):
        return self

    def __exit__(self, exc_type, *_):
        try:
            if not self.reap():
                try:
                    self.send(b"\x03")
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                if not self.await_exit(1):
                    os.killpg(self.child.pid, signal.SIGKILL)
                    require(self.await_exit(2), "failed to reap interrupted Milo")
            if exc_type is None:
                require(self.child.returncode == 0, f"Milo exit {self.child.returncode}: {self.tail!r}")
        finally:
            os.close(self.master)

    def peak(self):
        return self.usage.ru_maxrss / (1024 if sys.platform == "darwin" else 1)


class Samples:
    def __init__(self):
        self.data = defaultdict(list)
        self.operations = defaultdict(list)

    def add(self, key, name, value):
        self.data[f"{key}.{name}"].append(round(value, 3))

    def result(self):
        data = {name: {"n": len(values), "min": min(values), "median": statistics.median(values),
                       "p95": sorted(values)[math.ceil(len(values) * .95) - 1], "max": max(values)}
                for name, values in sorted(self.data.items())}
        return {"measurements": data, "git_operations_per_action": self.operations}

    @contextmanager
    def action(self, key, terminal, project, counted, wanted, checkpoint=False, initial=False):
        before = project.counts() if counted and not initial else Counter()
        began = terminal.started if initial else time.monotonic_ns()
        yield
        drawn = terminal.frame(wanted)
        if counted:
            terminal.settle()
            self.operations[key].append(dict(sorted((project.counts() - before).items())))
        else:
            self.add(key, "ms", (drawn - began) / 1e6)
            if checkpoint:
                self.add(key, "rss_kib", terminal.rss())


def tabs(samples, binary, project, committed, counted):
    key = "commit_tabs" if committed else "working_tabs"
    request_commit = project.head if committed else ""
    version = "commit" if committed else "working"
    project.log.unlink(missing_ok=True)
    with Terminal(binary, project, ["view", "control.txt", "--commit", project.head], counted) as terminal:
        def act(name, wanted, **kwargs):
            return samples.action(f"{key}.{name}", terminal, project, counted, wanted, **kwargs)

        def request(i):
            return project.tab(i), request_commit

        with act("control", b"readerprofilecontrol", checkpoint=True, initial=True):
            pass
        terminal.settle()
        for i in range(TABS):
            with act(f"open_{i + 1}", marker("tab", i, version), checkpoint=True):
                project.request(request(i))
        for _ in range(3):
            for i, move in [(2, b"\x10"), (3, b"\x0e")]:
                with act("key_switch", marker("tab", i, version)):
                    terminal.send(move)
            for i in (2, 3):
                with act("inbox_select_existing", marker("tab", i, version), checkpoint=True):
                    project.request(request(i))
            # Both requests arrive atomically in the same FIFO read: one redundant
            # request for the already-selected tab, then an observable control tab.
            for name, requests in [("active_repeat_and_control", [request(3)]), ("control_only", [])]:
                with act(name, b"readerprofilecontrol", checkpoint=True):
                    project.request(*requests, ("control.txt", project.head))
                with act("key_switch", marker("tab", 3, version)):
                    terminal.send(b"\x10")
        for i in reversed(range(TABS)):
            wanted = marker("tab", i - 1, version) if i else b"readerprofilecontrol"
            with act(f"close_to_{i}", wanted, checkpoint=True):
                terminal.send(b"\x17")
    if not counted:
        samples.add(key, "peak_rss_kib", terminal.peak())


def handed(samples, binary, project, path, counted):
    key = f"handed_{path.stem}"
    project.log.unlink(missing_ok=True)
    with Terminal(binary, project, ["review", str(path)], counted) as terminal:
        def act(name, i, **kwargs):
            return samples.action(f"{key}.{name}", terminal, project, counted, marker("patch", i, "handed"), **kwargs)

        with act("open", 0, checkpoint=True, initial=True):
            pass
        terminal.settle()
        for j in range(6):
            for move, i in [(b"]", 1), (b"[", 0)]:
                with act("switch", i, checkpoint=j == 5):
                    terminal.send(move)
            # Unchanged reload may emit no frame; rewrite an unmodified context
            # line in the selected file so the next frame proves it was reread.
            landed = f"reread{j}proof".encode()
            original = make_text("patch", 0, PATCH_ROWS, 81, "handed", PATCH_STRIDE)
            # Line 3 is context in the generated patch. Line 4 is plain work and
            # displayed immediately under the first edit, despite code folding.
            changed = original.replace(b"patch00stablerow00003", landed, 1)
            project.write(project.patch_name(0), changed)
            try:
                with samples.action(f"{key}.reload", terminal, project, counted, landed):
                    terminal.send(b"r")
            finally:
                project.write(project.patch_name(0), original)
    if not counted:
        samples.add(key, "peak_rss_kib", terminal.peak())


def gated(binary, project, probe):
    project.log.unlink(missing_ok=True)
    for path in (project.arm, project.entered, project.release):
        path.unlink(missing_ok=True)
    with Terminal(binary, project, ["view", "control.txt", "--commit", project.head], True) as terminal:
        terminal.frame(b"readerprofilecontrol")
        terminal.settle()
        project.arm.touch()
        try:
            project.request((project.tab(0), project.head))
            end = time.monotonic() + 5
            while not project.entered.exists() and time.monotonic() < end:
                terminal.read(.01)
            require(project.entered.exists(), "Git never entered the controlled diff gate")
            # Terminal input and inbox input are both readable during the gate.
            # Select back to control with a key, or quit the whole reader.
            terminal.send(b"\x10" if probe == "switch" else b"\x03")
            exited_while_held = terminal.await_exit(.25)
            emitted_while_held = terminal.buffer.count(FRAME)
            held_counts = dict(project.counts())
        finally:
            project.release.touch()
        started = time.monotonic_ns()
        if probe == "switch" and not exited_while_held:
            terminal.frame(b"readerprofilecontrol")
        if probe == "quit":
            require(terminal.await_exit(5), "Milo did not quit after releasing the controlled diff")
        return {"exited_while_held": exited_while_held, "completed_frames_while_held": emitted_while_held,
                "git_operations_while_held": held_counts, "git_operations_after_release": dict(project.counts()),
                "release_to_response_ms": round((time.monotonic_ns() - started) / 1e6, 3)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--base", type=Path, help="compare this release build on the same fixture")
    parser.add_argument("--runs", type=int, default=5)
    args = parser.parse_args()
    require(0 <= args.runs <= 20, "--runs must be between 0 and 20")
    builds = {"candidate": args.binary.resolve(strict=True)}
    if args.base:
        builds["base"] = args.base.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="milo-backend-reader-") as temp:
        project = Project(Path(temp).resolve())
        samples = {label: Samples() for label in builds}
        for run in range(args.runs + 1):
            counted = run == 0
            for label in (builds if run % 2 else reversed(builds)):
                for committed in (False, True):
                    tabs(samples[label], builds[label], project, committed, counted)
                for path in (project.short, project.full):
                    handed(samples[label], builds[label], project, path, counted)
        results = {label: {**samples[label].result(),
                           "controlled_gate": {probe: gated(binary, project, probe) for probe in ("switch", "quit")}}
                   for label, binary in builds.items()}
        fixture = {"tab_count": TABS, "tab_file_bytes": (project.repo / project.tab(0)).stat().st_size,
                   "tab_working_diff_bytes": len(project.working_diff), "tab_commit_diff_bytes": len(project.commit_diff),
                   "patch_current_file_bytes": (project.repo / project.patch_name(0)).stat().st_size,
                   "patch_current_file_lines": PATCH_ROWS,
                   "handed_patches": {path.stem: {"bytes": path.stat().st_size,
                                                 "hunks": path.read_bytes().count(b"\n@@ ")}
                                      for path in (project.short, project.full)}}
        print(json.dumps({"fixture": fixture, **results}, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
