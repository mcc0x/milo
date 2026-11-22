"""Count real Git operations in Outline and History (no timing benchmark).

    python3 tests/backend_refresh.py target/release/milo

Fixtures: sparse = 12 tracked/2 dirty; large = 6,000 tracked/1,000 dirty.
Use --fixture sparse-checkout for a 6,000-path repo with a sparse index
and just 12 materialized tracked paths. Use --comparison ahead or
--comparison stand-in for synthetic local remote refs; --traces DIR keeps
the Trace2 JSONL files. Discovery covers root, nested, linked, newline
and plain directories. Use --skip-discovery to run just refresh phases.

Run on a Unix PTY with recent Git (2.34+ for sparse-index). The baseline
uses macOS FSEvents; --idle-seconds 31.5 observes its 30 s backup poll.
Linux uses Milo's 2 s fallback, so watcher counts are not comparable.
The harness uses isolated HOME/Git config and no Rex or network.
"""

import argparse
from collections import Counter
from contextlib import contextmanager
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import tempfile
import termios
import time
import unicodedata


SIZE = (24, 100)
FRAME = b"\x1b[?2026h"
FRAME_END = b"\x1b[?2026l"
ASK = b"\x1b[c"
ANSWERS = b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[6;16;8t\x1b[?62;22c"
CSI = re.compile(r"\x1b\[([0-?]*)([ -/]*[@-~])")
TIMEOUT = 20
# Include native FSEvents delivery, the 120 ms prompt floor, and normal
# follow-up checks. This is a measured observation window, not a substitute
# for waiting for stage-specific terminal content and Git's exit events.
TAIL = 0.75
BATCH_INTERVAL = 0.025


def until(predicate, pump, label):
    deadline = time.monotonic() + TIMEOUT
    while not predicate():
        if time.monotonic() >= deadline:
            raise RuntimeError(f"timed out waiting for {label}")
        pump(min(0.01, max(0, deadline - time.monotonic())))


def serve_for(terminal, seconds):
    deadline = time.monotonic() + seconds
    while (left := deadline - time.monotonic()) > 0:
        terminal.pump(left)


class Terminal:
    def __init__(self, binary, pane, cwd, env):
        self.master, slave = pty.openpty()
        try:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", *SIZE, 0, 0))
            self.child = subprocess.Popen([binary, pane], cwd=cwd, env=env,
                                          stdin=slave, stdout=slave, stderr=slave)
        finally:
            os.close(slave)
        self.pending = b""
        self.answered = False
        self.raw = bytearray()
        self.clear()

    def clear(self):
        self.screen = [[" "] * SIZE[1] for _ in range(SIZE[0])]

    def pump(self, wait):
        if not select.select([self.master], [], [], wait)[0]:
            return
        try:
            data = os.read(self.master, 65536)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            data = b""
        if not data:
            raise RuntimeError(f"pane closed early: {bytes(self.raw[-1000:])!r}")
        self.raw.extend(data)
        self.pending += data
        if not self.answered and ASK in self.pending:
            os.write(self.master, ANSWERS)
            self.answered = True
        while (end := self.pending.find(FRAME_END)) >= 0:
            frame, self.pending = self.pending[:end], self.pending[end + len(FRAME_END):]
            start = frame.rfind(FRAME)
            if start >= 0:
                self.paint(frame[start + len(FRAME):].decode())

    def paint(self, frame):
        # The synchronized screen sends cursor-positioned cell deltas, not
        # full lines: changing 1000 to 1001 may send only the final digit.
        row = col = 0
        offset = 0
        for token in [*CSI.finditer(frame), None]:
            end = token.start() if token else len(frame)
            for char in frame[offset:end]:
                if unicodedata.combining(char):
                    continue
                if 0 <= row < SIZE[0] and 0 <= col < SIZE[1]:
                    self.screen[row][col] = char
                col += 2 if unicodedata.east_asian_width(char) in "FW" else 1
            if token:
                offset = token.end()
                params, op = token.groups()
                if op == "H":
                    row, col = (int(n) - 1 for n in params.split(";"))
                elif op == "J" and params == "2":
                    self.clear()
                elif op != "m":
                    raise RuntimeError(f"unexpected screen control {token.group()!r}")

    def showing(self, text):
        return any(text.search("".join(line)) for line in self.screen)

    def see(self, pattern):
        text = re.compile(pattern)
        try:
            until(lambda: self.showing(text), self.pump, pattern)
        except RuntimeError as error:
            screen = "\n".join("".join(line).rstrip() for line in self.screen)
            raise RuntimeError(f"{error}\n{screen}") from error

    def close(self):
        try:
            if self.child.poll() is None:
                os.write(self.master, b"q")
            if code := self.child.wait(timeout=TIMEOUT):
                raise RuntimeError(f"pane exited with {code}")
        finally:
            if self.child.poll() is None:
                self.child.kill()
                self.child.wait()
            os.close(self.master)


def command(args):
    while args and args[0] in {"--no-pager", "--no-optional-locks", "-C", "-c"}:
        args = args[2:] if args[0] in {"-C", "-c"} else args[1:]
    name, *opts = args
    if name == "rev-parse":
        if "--show-toplevel" in opts:
            return "discover-joint" if "--absolute-git-dir" in opts else "discover-root"
        if "--absolute-git-dir" in opts:
            return "discover-git-dir"
        if "--git-common-dir" in opts:
            return "discover-common-dir"
        if "--verify" in opts:
            return "resolve-upstream" if "@{upstream}" in opts[-1] else "resolve-ref"
    if name == "diff" and "--numstat" in opts:
        specs = opts[opts.index("--") + 1:] if "--" in opts else []
        paths = len(specs)
        if (paths == 2 and specs[0].startswith(":(literal)")
                and specs[1] == ":(glob)" + specs[0].removeprefix(":(literal)") + "/**"):
            paths = 1  # the exact file and its former descendants
        if "--no-renames" in opts:
            return f"diff-recount[{paths}]"
        if "-M" in opts:
            if re.fullmatch("[0-9a-f]{40,64}", opts[opts.index("-M") - 2]):
                return "diff-unpushed"
            return f"diff-tracked[{paths or 'all'}]"
    return name


class Trace:
    def __init__(self, path):
        self.path = path
        self.offset = 0
        self.processes = {}

    def read(self):
        with self.path.open("rb") as input_file:
            input_file.seek(self.offset)
            while (line := input_file.readline()).endswith(b"\n"):
                self.offset += len(line)
                event = json.loads(line)
                kind, sid = event["event"], event["sid"]
                if kind == "start":
                    self.processes[sid] = {"command": command(event["argv"][1:]), "exit": None, "data": {}}
                if sid not in self.processes:
                    continue
                if kind == "exit":
                    self.processes[sid]["exit"] = event["code"]
                elif kind == "data" and event["category"] in {"index", "status"}:
                    category, key = event["category"], event["key"]
                    if key in {"read/cache_nr", "refresh/sum_lstat", "count/changed", "count/untracked"}:
                        self.processes[sid]["data"][f"{category}/{key}"] = int(event["value"])
        return list(self.processes.values())

    def done(self):
        return all(item["exit"] is not None for item in self.read())

    def report(self, label, start=0):
        work = self.read()[start:]
        ops = Counter(item["command"] for item in work)
        status = [item["data"] for item in work if item["command"] == "status"]
        data = {key: [item.get(key) for item in status] for key in sorted(set().union(*status))}
        failed = Counter(f'{item["command"]}:{item["exit"]}' for item in work if item["exit"] not in {0, None})
        print(json.dumps({"phase": label, "processes": len(work), "commands": dict(sorted(ops.items())),
                          **({"status_data": data} if data else {}), **({"nonzero": failed} if failed else {})}), flush=True)
        return start + len(work)


class Profile:
    def __init__(self, root, binary, traces):
        self.root, self.binary, self.traces = root, binary, traces
        home = root / "home"
        self.env = {"HOME": str(home), "PATH": os.environ.get("PATH", os.defpath), "TERM": "xterm-256color",
                    "LC_ALL": "C", "XDG_CONFIG_HOME": str(home / "config"), "XDG_STATE_HOME": str(home / "state"),
                    "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1", "GIT_ALLOW_PROTOCOL": "",
                    "GIT_AUTHOR_NAME": "Milo Probe", "GIT_AUTHOR_EMAIL": "probe@example.test",
                    "GIT_COMMITTER_NAME": "Milo Probe", "GIT_COMMITTER_EMAIL": "probe@example.test",
                    "MILO_ICONS": "plain"}
        preferences = home / "config" / "milo" / "preferences.conf"
        preferences.parent.mkdir(parents=True)
        preferences.write_text("static_marks = true\nfollow_activity = false\n")

    def git(self, cwd, *args):
        return subprocess.check_output(["git", "-c", "core.hooksPath=/dev/null", *args], cwd=cwd,
                                       env=self.env, stdin=subprocess.DEVNULL, text=True).strip()

    def init(self, repo):
        repo.mkdir(parents=True)
        self.git(repo, "init", "-q", "--template=", "-b", "main")

    @contextmanager
    def session(self, label, pane, cwd):
        logs = self.traces / f"{label}.jsonl"
        logs.write_text("")
        config = self.root / "trace-config"
        # Milo strips GIT_TRACE2_* env vars. Git reads Trace2 from system
        # config only; override that path with our entire synthetic config.
        self.git(self.root, "config", "--file", str(config), "trace2.eventTarget", str(logs))
        env = {**self.env, "GIT_CONFIG_NOSYSTEM": "0", "GIT_CONFIG_SYSTEM": str(config)}
        terminal, trace = Terminal(self.binary, pane, cwd, env), Trace(logs)
        try:
            yield terminal, trace
        finally:
            terminal.close()

    def discovery(self):
        regular = self.root / "discovery" / "regular"
        linked = regular.parent / "linked"
        newline = regular.parent / "line\nbreak" / "regular"
        plain = regular.parent / "plain"
        for repo in [regular, newline]:
            self.init(repo)
            (repo / "probe.txt").write_text("discovery probe\n")
            self.git(repo, "add", "probe.txt")
            self.git(repo, "commit", "-qm", "probe discovery commit")
            (repo / "nested").mkdir()
        self.git(regular, "worktree", "add", "-qb", "linked", str(linked))
        plain.mkdir()
        (plain / "probe.txt").write_text("plain folder probe\n")
        for name, cwd in [("root", regular), ("nested", regular / "nested"), ("linked", linked),
                          ("newline", newline / "nested"), ("plain", plain)]:
            label = f"discovery/{name}"
            with self.session(label.replace("/", "-"), "outline", cwd) as (terminal, trace):
                terminal.see(r"probe\.txt")
                if not trace.read() or not trace.done():
                    raise RuntimeError("first paint lacks complete Git Trace2 startup events")
                trace.report(label)

    def fixture(self, label, files, dirty, comparison, sparse_checkout):
        repo = self.root / label
        self.init(repo)
        (repo / "src").mkdir()
        if sparse_checkout:
            (repo / "hidden").mkdir()
        for i in range(1, files):
            folder = "hidden" if sparse_checkout and i >= 12 else "src"
            (repo / folder / f"tracked-{i:05}.txt").write_text(f"tracked {i}\n")
        (repo / "00-probe.txt").write_text("one\n")
        self.git(repo, "add", "-A")
        self.git(repo, "commit", "-qm", "probe base commit")
        if comparison != "local":
            self.git(repo, "remote", "add", "origin", "./never-contacted")
            self.git(repo, "update-ref", "refs/remotes/origin/main", "HEAD")
            if comparison == "ahead":
                self.git(repo, "branch", "--set-upstream-to=origin/main", "main")
                (repo / "ahead.txt").write_text("unpushed file\n")
                self.git(repo, "add", "ahead.txt")
                self.git(repo, "commit", "-qm", "probe ahead commit")
            else:
                self.git(repo, "symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main")
        if sparse_checkout:
            self.git(repo, "sparse-checkout", "set", "--cone", "--sparse-index", "src")
        for i in range(1, dirty):
            (repo / "src" / f"tracked-{i:05}.txt").write_text("modified\n")
        (repo / "00-probe.txt").write_text("one\ntwo\n")
        return repo


def watch(terminal, trace, seconds):
    deadline = time.monotonic() + seconds
    while (left := deadline - time.monotonic()) > 0:
        terminal.pump(min(left, 0.05))
        trace.read()
    until(trace.done, terminal.pump, "started Git commands to exit")


def profile_refresh(profile, fixture, pane, comparison, idle_seconds):
    files, dirty = {"sparse": (12, 2), "large": (6000, 1000), "sparse-checkout": (6000, 2)}[fixture]
    name = f"{fixture}-{pane}-{comparison}"
    repo = profile.fixture(name, files, dirty, comparison, fixture == "sparse-checkout")
    marker = lambda n: rf"(?<!\d){n}{' files' if pane == 'history' else ''} to commit"
    with profile.session(name, pane, repo) as (terminal, trace):
        terminal.see(marker(dirty))
        if not trace.read() or not trace.done():
            raise RuntimeError("first paint lacks complete Git Trace2 startup events")
        cursor = trace.report(f"{name}/first-paint")
        if pane == "outline" and not terminal.showing(re.compile(r"00-probe\.txt")):
            # Move to the first changed file's folder and close it; keep root
            # probes visible without changing their on-disk shape.
            os.write(terminal.master, b"hh")
            terminal.see(r"00-probe\.txt")
        watch(terminal, trace, TAIL)
        cursor = trace.report(f"{name}/opening-tail", cursor)
        watch(terminal, trace, idle_seconds)
        cursor = trace.report(f"{name}/quiet-{idle_seconds:g}s", cursor)

        def complete(phase, *wanted):
            for pattern in wanted:
                terminal.see(pattern)
            watch(terminal, trace, TAIL)
            return trace.report(f"{name}/{phase}", cursor)

        untracked = repo / "01-untracked.txt"
        untracked.write_text("new file\n")
        cursor = complete("untracked-only", marker(dirty + 1))
        if pane == "outline":
            terminal.see(r"01-untracked\.txt.*\+1(?!\d)")
            untracked.write_text("new file\nanother line\n")
            cursor = complete("same-porcelain-untracked-save", r"01-untracked\.txt.*\+2(?!\d)")

        probe = repo / "00-probe.txt"
        probe.write_text("one\ntwo\nthree\n")
        (repo / "saved.txt").write_text("marks this save in both panes\n")
        recount = [r"00-probe\.txt.*\+2(?!\d)"] if pane == "outline" else []
        cursor = complete("one-dirty-save", marker(dirty + 2), *recount)

        # A fixed offered load: 24 already-dirty edits and 24 new paths over
        # about 0.6 s. Serviced visits can vary with watcher delivery and rest.
        for i in range(24):
            probe.write_text("one\n" + "edited\n" * (i + 1))
            (repo / f"burst-{i:02}.txt").write_text("burst\n")
            serve_for(terminal, BATCH_INTERVAL)
        recount = [r"00-probe\.txt.*\+24(?!\d)"] if pane == "outline" else []
        cursor = complete("burst-24", marker(dirty + 26), *recount)

        profile.git(repo, "add", probe.name)
        (repo / "stage-marker.txt").write_text("marks the index operation\n")
        cursor = complete("stage-index", marker(dirty + 27))

        profile.git(repo, "commit", "-qm", "probe next commit")
        subject = [r"probe next commit"] if pane == "history" else []
        complete("commit", marker(dirty + 26), *subject)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--fixture", choices=["sparse", "large", "sparse-checkout"], action="append")
    parser.add_argument("--pane", choices=["outline", "history"], action="append")
    parser.add_argument("--comparison", choices=["local", "ahead", "stand-in"], default="local")
    parser.add_argument("--idle-seconds", type=float, default=1)
    parser.add_argument("--traces", type=Path)
    parser.add_argument("--discovery-only", action="store_true")
    parser.add_argument("--skip-discovery", action="store_true")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="milo-backend-refresh-") as temp:
        root = Path(temp).resolve()
        logs = args.traces.resolve() if args.traces else root / "traces"
        logs.mkdir(parents=True, exist_ok=True)
        profile = Profile(root, args.binary.resolve(strict=True), logs)
        print(json.dumps({"git": profile.git(root, "--version"), "binary": str(profile.binary),
                          "tail_seconds": TAIL, "comparison": args.comparison}), flush=True)
        if not args.skip_discovery:
            profile.discovery()
        if not args.discovery_only:
            for fixture in args.fixture or ["sparse", "large"]:
                for pane in args.pane or ["outline", "history"]:
                    profile_refresh(profile, fixture, pane, args.comparison, args.idle_seconds)


if __name__ == "__main__":
    main()
