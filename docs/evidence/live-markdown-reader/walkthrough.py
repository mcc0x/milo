#!/usr/bin/env python3
"""Exercise a release Reader through a real standalone PTY and capture xterm screenshots."""

import argparse
import base64
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time


WIDE, NARROW = (96, 26), (48, 16)
ANCHOR = "Cairn checkpoint"
BEFORE = "Surface report: dry shale exposed."
NORMAL = "Surface report: rain on the ridge."
REPLACEMENT = "Surface report: fresh snow at the ridge."
TAIL = "Last recorded light: 17:20."
LIVE_TAIL = "Last recorded light: 18:10."
NARROW_TAIL = "Last recorded light: 18:35."
PAUSED_TAIL = "Last recorded light: 19:00."
DRAFT = "Draft-only note"
DISK = "Disk-only note"
FRAME_START = b"\x1b[?2026h"
FRAME_END = b"\x1b[?2026l"


def require(condition, message, rows=()):
    if not condition:
        raise AssertionError(message + ("\n" + "\n".join(rows) if rows else ""))


def contains(text, rows):
    return any(text in row for row in rows)


def position(text, rows):
    return next((i for i, row in enumerate(rows) if text in row), None)


def marked(text, rows):
    return any(text in row and row.endswith("!") for row in rows)


def body_text(row):
    return row.rstrip(" !▐").lstrip()


def notes(label, count):
    return "".join(f"### {label} {i:02d}\n\nRoute notes and bearings logged.\n\n" for i in range(1, count + 1))


INITIAL = (
    "# Ridge survey field log\n\nWorking notes from the north traverse.\n\n"
    + notes("Trail station", 12)
    + f"## {ANCHOR}\n\n{BEFORE}\n\n"
    + "> Keep the morning readings beside the field sketch.\n\n"
      "- **Radio check** at the next cairn.\n- Trace the `north spur` on the map.\n\n"
      "### West face\n\nCloud line is above the lookout.\n\n"
    + notes("Return station", 10)
    + f"## End of survey\n\n{TAIL}\n"
)


def lab_environment(root):
    (root / "home").mkdir()
    return dict(HOME=str(root / "home"), XDG_CONFIG_HOME=str(root / "config"),
                XDG_STATE_HOME=str(root / "state"), XDG_CACHE_HOME=str(root / "cache"),
                XDG_DATA_HOME=str(root / "data"), PATH="/usr/bin:/bin", TERM="xterm-256color",
                LC_ALL="C", GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)


class Terminal:
    def __init__(self, options, repo, env, arguments):
        renderer = Path(__file__).with_name("terminal.cjs")
        self.renderer = subprocess.Popen(
            [shutil.which("node"), renderer, options.tools, options.chrome],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env, text=True,
        )
        self.output = options.output
        self.master = None
        self.pid = None
        self.pending = b""
        self.frames = 0
        try:
            self.exchange(size=WIDE)
            pid, master = pty.fork()
            if pid == 0:
                os.chdir(repo)
                self.set_size(0, WIDE)
                os.execve(options.binary, [options.binary, *arguments], env)
            self.pid, self.master = pid, master
        except BaseException:
            self.close()
            raise

    @staticmethod
    def set_size(fd, size):
        cols, rows = size
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def exchange(self, **request):
        self.renderer.stdin.write(json.dumps(request) + "\n")
        self.renderer.stdin.flush()
        response = self.renderer.stdout.readline()
        require(response, "xterm renderer exited")
        state = json.loads(response)
        self.rows = state["rows"]
        for reply in state["replies"]:
            self.send(reply.encode())
        return self.rows

    def update(self, timeout):
        if select.select([self.master], [], [], timeout)[0]:
            try:
                data = os.read(self.master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                raise AssertionError("Milo unexpectedly closed its PTY") from error
            require(data, "Milo unexpectedly closed its PTY")
            self.pending += data
            start, end = self.pending.find(FRAME_START), self.pending.rfind(FRAME_END)
            if end != -1:
                boundary = end + len(FRAME_END)
                self.frames += self.pending[:boundary].count(FRAME_END)
            elif start != -1:
                boundary = start
            else:
                partial = max((size for size in range(1, len(FRAME_START))
                               if self.pending.endswith(FRAME_START[:size])), default=0)
                boundary = len(self.pending) - partial
            if boundary:
                ready, self.pending = self.pending[:boundary], self.pending[boundary:]
                self.exchange(bytes=base64.b64encode(ready).decode("ascii"))

    def wait(self, description, predicate, timeout=5):
        deadline = time.monotonic() + timeout
        while not predicate(self.rows) and time.monotonic() < deadline:
            self.update(0.05)
        require(predicate(self.rows), description, self.rows)
        return self.rows

    def observe(self, seconds, description, predicate):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.update(0.05)
            require(predicate(self.rows), description, self.rows)

    def send(self, keys):
        os.write(self.master, keys)

    def resize(self, size):
        before = self.frames
        self.exchange(size=size)
        self.set_size(self.master, size)
        self.wait(f"Reader did not repaint after resize to {size}", lambda _: self.frames > before)

    def screenshot(self, name):
        self.exchange(screenshot=str(self.output / name))

    def quit(self):
        self.send(b"q")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                self.pid = None
                require(os.waitstatus_to_exitcode(status) == 0, "Milo's exit status was not zero")
                return
            time.sleep(0.02)
        raise AssertionError("Milo did not quit after q")

    def close(self):
        if self.master is not None:
            os.close(self.master)
        if self.pid is not None:
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        self.renderer.stdin.close()
        self.renderer.wait(timeout=10)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def save(file, text, atomic=False):
    # This driver is separate from and concurrent with Milo. In-place saves
    # truncate the existing inode; atomic saves replace it from the same directory.
    inode = file.stat().st_ino
    if atomic:
        replacement = file.with_suffix(".tmp")
        replacement.write_text(text, encoding="utf-8")
        os.replace(replacement, file)
    else:
        file.write_text(text, encoding="utf-8")
    require((file.stat().st_ino != inode) == atomic, "Unexpected save inode behavior")


def run(options, repo, env):
    file = repo / "field-log.md"
    file.write_text(INITIAL, encoding="utf-8")
    git = ["git", "-C", repo]
    for command in (["init", "-q", "--template="], ["add", "field-log.md"],
                    ["-c", "user.name=Reader Evidence", "-c", "user.email=evidence@example.invalid",
                     "commit", "-qm", "Record field log"]):
        subprocess.run([*git, *command], env=env, check=True)

    with Terminal(options, repo, env, ["view", "field-log.md"]) as tty:
        tty.wait("initial rendered Markdown was not visible",
                 lambda rows: contains("Ridge survey field log", rows) and contains("m source", rows))
        # Reach the same real screen position in both builds without assuming a row count per block.
        for _ in range(100):
            anchor = position(ANCHOR, tty.rows)
            if anchor == 2:
                break
            before = tty.rows.copy()
            tty.send(b"j")
            tty.wait("j did not move the screen", lambda rows: rows != before)
        else:
            raise AssertionError("Couldn't scroll to the cairn checkpoint")
        require(contains(BEFORE, tty.rows), "Original paragraph not on screen", tty.rows)

        normal = INITIAL.replace("Working notes from the north traverse.\n\n",
                                 "Working notes from the north traverse.\n\n"
                                 + notes("Inserted survey station", 3)).replace(BEFORE, NORMAL)
        save(file, normal)
        if options.mode == "before":
            tty.wait("baseline did not show the external in-place save", lambda rows: contains(NORMAL, rows))
            require(position(ANCHOR, tty.rows) != anchor and not marked(NORMAL, tty.rows),
                    "baseline did not reproduce the anchor jump and absent changed mark", tty.rows)
            tty.screenshot("before-normal-save-96x26.png")
        else:
            tty.wait("in-place save did not refresh with a marked paragraph and the same anchor",
                     lambda rows: position(ANCHOR, rows) == anchor and marked(NORMAL, rows))
            tty.screenshot("after-normal-save-96x26.png")
            print("In-place save: marked paragraph at the unchanged reading anchor.")

        replaced = normal.replace(NORMAL, REPLACEMENT)
        save(file, replaced, atomic=True)
        if options.mode == "before":
            tty.wait("baseline did not show the atomic replacement", lambda rows: contains(REPLACEMENT, rows))
            require(position(ANCHOR, tty.rows) != anchor and not marked(REPLACEMENT, tty.rows),
                    "baseline did not reproduce the moved anchor and absent changed mark", tty.rows)
            tty.screenshot("before-atomic-save-96x26.png")
            print(f"Baseline refreshed after both saves, but the anchor moved from row {anchor + 1} "
                  f"to row {position(ANCHOR, tty.rows) + 1} and no ! marked the changed paragraph.")
            tty.quit()
            return

        tty.wait("atomic replacement did not refresh and mark the paragraph at the same anchor",
                 lambda rows: position(ANCHOR, rows) == anchor and marked(REPLACEMENT, rows))
        tty.screenshot("after-atomic-save-96x26.png")
        tty.wait("changed marks did not expire in place while idle",
                 lambda rows: position(ANCHOR, rows) == anchor and contains(REPLACEMENT, rows)
                 and not any(row.endswith("!") for row in rows[1:-1]), timeout=15)
        tty.screenshot("after-mark-expiry-96x26.png")
        print("Atomic replacement: the anchor held again and the changed marks expired while idle.")
        idle_frames = tty.frames
        tty.observe(1, "Reader changed during the post-expiry idle window",
                    lambda rows: position(ANCHOR, rows) == anchor and contains(REPLACEMENT, rows)
                    and not any(row.endswith("!") for row in rows[1:-1]))
        print(f"Post-expiry idle window (1 second): {tty.frames - idle_frames} additional PTY frames.")

        tty.send(b"G")
        tty.wait("G did not reach the end in follow mode",
                 lambda rows: contains(TAIL, rows) and contains("Following end", rows))
        evening = f"\n## Evening update\n\n{LIVE_TAIL}\n"
        with file.open("a", encoding="utf-8") as stream:
            stream.write(evening)
        followed = replaced + evening
        tty.wait("G did not follow the append", lambda rows: marked(LIVE_TAIL, rows))
        tty.screenshot("after-follow-96x26.png")
        tty.resize(NARROW)
        tty.wait("the narrow Reader did not retain the tail", lambda rows: contains(LIVE_TAIL, rows))
        narrowed = followed + f"\n## Night watch\n\n{NARROW_TAIL}\n"
        save(file, narrowed, atomic=True)
        tty.wait("follow did not survive the resize", lambda rows: marked(NARROW_TAIL, rows))
        tty.screenshot("after-follow-48x16.png")
        print("G followed a new append, then followed another after a 96×26 to 48×16 PTY resize.")

        tty.send(b"b")
        tty.wait("manual navigation did not leave follow mode",
                 lambda rows: not contains(NARROW_TAIL, rows) and "Following" not in rows[0])
        old_top = body_text(tty.rows[2])
        # Change a visible non-anchor heading as well as appending beyond the
        # viewport. Seeing its mark proves refresh completed with follow off.
        station = next(text for text in map(body_text, tty.rows[3:-1]) if re.fullmatch(r"Return station \d+", text))
        checked = f"{station} checked"
        paused = (narrowed.replace(f"### {station}\n", f"### {checked}\n") + "\n"
                  + notes("Night station", 8) + f"## Night close\n\n{PAUSED_TAIL}\n")
        save(file, paused)
        tty.wait("manual view did not refresh in place with follow disengaged",
                 lambda rows: marked(checked, rows) and body_text(rows[2]) == old_top and not contains(PAUSED_TAIL, rows))
        tty.screenshot("after-manual-scroll-48x16.png")
        tty.send(b"G")
        tty.wait("G did not return to the current end", lambda rows: contains(PAUSED_TAIL, rows))
        tty.resize(WIDE)
        tty.wait("the wide Reader did not retain the tail", lambda rows: contains(PAUSED_TAIL, rows))
        print("Manual page-up stopped following; G returned to the current end, including after widening.")

        tty.send(b"ge")
        tty.wait("could not enter editing", lambda rows: contains("editing", rows))
        tty.send((DRAFT + ": ").encode())
        tty.wait("typing did not leave an unsaved draft on screen",
                 lambda rows: contains("unsaved", rows) and contains(DRAFT, rows))
        disk = paused.replace("Working notes from the north traverse.", f"{DISK}: compare the new survey route.")
        save(file, disk, atomic=True)
        tty.observe(1, "external save replaced the draft",
                    lambda rows: contains(DRAFT, rows) and not contains(DISK, rows))
        tty.send(b"\x13")
        tty.wait("Ctrl-S did not protect the external file and draft with a conflict prompt",
                 lambda rows: contains("Changed on disk", rows) and contains(DRAFT, rows))
        tty.screenshot("after-draft-conflict-96x26.png")
        require(file.read_text(encoding="utf-8") == disk, "Conflict silently overwrote disk")
        tty.send(b"c")
        tty.wait("compare did not show both versions", lambda rows: contains(DISK, rows) and contains(DRAFT, rows))
        tty.send(b"\x1b")
        tty.wait("Esc did not return from compare", lambda rows: contains("Changed on disk", rows))
        tty.send(b"\x1b")
        tty.wait("Esc did not return to the intact draft", lambda rows: contains("unsaved", rows)
                 and contains(DRAFT, rows) and not contains("Changed on disk", rows))
        tty.send(b"\x1b")
        tty.wait("Esc did not ask before discarding", lambda rows: contains("Unsaved edits", rows))
        tty.send(b"d")
        tty.wait("discard did not restore the external version", lambda rows: contains(DISK, rows)
                 and not contains(DRAFT, rows))
        require(file.read_text(encoding="utf-8") == disk, "Compare or discard changed disk")
        tty.quit()
        print("Dirty draft survived an atomic save; Ctrl-S showed the conflict, compare and Esc preserved both versions, and explicit discard restored the disk view.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("before", "after"))
    parser.add_argument("binary", type=Path)
    parser.add_argument("--tools", required=True, type=Path, help="npm prefix containing xterm and playwright-core")
    parser.add_argument("--chrome", required=True, type=Path, help="Chrome or Chromium executable")
    parser.add_argument("--output", type=Path, default=Path(__file__).resolve().parent)
    options = parser.parse_args()
    options.binary = options.binary.resolve(strict=True)
    options.output = options.output.resolve()
    options.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="milo-live-markdown-") as directory:
        root = Path(directory)
        repo = root / "project"
        repo.mkdir()
        run(options, repo, lab_environment(root))


if __name__ == "__main__":
    main()
