#!/usr/bin/env python3
"""Compare History paging in release binaries with real Git and a 160x50 PTY.

Usage: python3 tests/history_paging.py BASE [CANDIDATE] [--runs 15]

The two builds alternate on the same synthetic history. Only the separate Git
count probe uses a wrapper. Timings start at a key and end at its rendered
frame; synchronized frame hashes check text, cursor and styles across builds.
RSS deltas compare the same process at preview, show-all and collapsed views.
"""
import argparse
from collections import Counter
import hashlib
import math
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import tempfile
import time
from types import SimpleNamespace

from backend_reader import FRAME_START, Terminal, WRAPPER

SCENARIOS = (("control", 6_000, 3), ("sweep", 200_000, 1))
UP, DOWN = b"\x1b[5~", b"\x1b[6~"


def project(root):
    repo = root / "repo"
    repo.mkdir()
    home = root / "home"
    home.mkdir()
    env = dict(HOME=str(home), XDG_CONFIG_HOME=str(root / "config"),
               XDG_STATE_HOME=str(root / "state"), XDG_CACHE_HOME=str(root / "cache"),
               TMPDIR=str(root), PATH=os.defpath, TERM="xterm-256color", LC_ALL="C",
               GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    git = shutil.which("git", path=env["PATH"])
    subprocess.run([git, "init", "-q", "--template=", "-b", "main"], cwd=repo, env=env, check=True)
    with subprocess.Popen([git, "fast-import", "--quiet"], cwd=repo, env=env, stdin=subprocess.PIPE) as importer:
        def commit(label):
            message = label.encode()
            importer.stdin.write(b"commit refs/heads/main\n"
                                 b"committer Milo Profile <profile@example.invalid> 981173106 +0000\n"
                                 + f"data {len(message)}\n".encode() + message + b"\n")

        importer.stdin.write(b"blob\nmark :1\ndata 23\nsynthetic-history-file\n")
        for i in range(300):
            commit(f"earlier-{i:03d}")
            importer.stdin.write(b"\n")
        for name, count, _ in SCENARIOS:
            commit(f"add-{name}")
            for i in range(count):
                importer.stdin.write(f"M 100644 :1 src/{i // 1000:03d}/{name}{i:06d}.txt\n".encode())
            importer.stdin.write(b"\n")
            commit(f"clear-{name}")
            importer.stdin.write(b"deleteall\n\n")
        importer.stdin.close()
        if importer.wait() != 0:
            raise RuntimeError("git fast-import failed")
    subprocess.run([git, "reset", "-q", "--hard", "HEAD"], cwd=repo, env=env, check=True)
    wrapper = root / "wrapper"
    wrapper.mkdir()
    script = wrapper / "git"
    script.write_text(WRAPPER)
    script.chmod(0o700)
    exec_path = Path(os.fsdecode(subprocess.check_output([git, "--exec-path"], cwd=repo, env=env).strip())) / "git"
    log = root / "git.log"
    count_env = dict(env, PATH=f"{wrapper}:{os.defpath}", PROFILE_WRAPPER=str(wrapper),
                     PROFILE_GIT=str(exec_path if exec_path.is_file() else git), PROFILE_LOG=str(log))
    return SimpleNamespace(repo=repo, env=env, count_env=count_env, log=log)


class History(Terminal):
    def paint(self, frame):
        if not super().paint(frame):
            return False
        self.digest = hashlib.sha256(frame[frame.rfind(FRAME_START):]).digest()
        return True

    def step(self, key, marker=None):
        start = time.monotonic_ns()
        self.send(key)
        if marker is not None:
            end = self.frame(marker.encode())
            return (end - start) / 1e6, self.digest
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            for end in self.take_frames():
                return (end - start) / 1e6, self.digest
            if not self.read(max(0, deadline - time.monotonic())):
                break
        raise RuntimeError(f"no frame after {key!r}: {self.tail!r}")


def sample(binary, fixture, scenario, counted=False):
    name, count, below_tip = scenario
    with History(binary, fixture, ["history"], counted) as tty:
        tty.frame(b"clear-sweep")
        tty.settle()
        frames = []
        for _ in range(below_tip):
            _, frame = tty.step(b"j")
            frames.append(frame)
        listed, _ = tty.step(b"\r", f"{count} files")
        tty.settle()
        for _ in range(8):
            _, frame = tty.step(b"j")
            frames.append(frame)
        _, frame = tty.step(b"j", "Enter show all")
        frames.append(frame)
        before_rss = tty.rss() if not counted else 0
        expanded, frame = tty.step(b"\r", f"{name}000008.txt")
        frames.append(frame)
        expanded_rss = tty.rss() - before_rss if not counted else 0
        pages = []
        for _ in range(15):
            for key in (DOWN, UP):
                elapsed, frame = tty.step(key)
                pages.append(elapsed)
                frames.append(frame)
        _, frame = tty.step(b"G", "Older commits not shown.")
        frames.append(frame)
        _, frame = tty.step(b"g", "clear-sweep")
        frames.append(frame)
        for _ in range(below_tip):
            tty.step(b"j")
        _, frame = tty.step(b"\r", "Enter show files")
        frames.append(frame)
        closed_rss = tty.rss() - before_rss if not counted else 0
        reopened, frame = tty.step(b"\r", f"{count} files")
        frames.append(frame)
        _, frame = tty.step(b"j", "Enter read")
        frames.append(frame)
        patch, frame = tty.step(b"\r", "synthetic-history-file")
        frames.append(frame)
        tty.step(b"q", "Enter read")
        tty.send(b"q")
        if not tty.await_exit(5):
            raise RuntimeError("History did not close")
        return frames, (listed, expanded, reopened, patch), pages, (expanded_rss, closed_rss)


def compare(root, binaries, runs):
    fixture = project(root)
    print("scenario\tbuild\tmeasure\tsamples\tp50\tp95\tunit", flush=True)
    for scenario in SCENARIOS:
        name = scenario[0]
        samples = [[[] for _ in range(7)] for _ in binaries]
        reference = None
        for run in range(runs):
            for i in (reversed(range(len(binaries))) if run % 2 else range(len(binaries))):
                frames, timings, pages, rss = sample(binaries[i], fixture, scenario)
                if reference is not None and frames != reference:
                    different = [j for j, (one, other) in enumerate(zip(frames, reference)) if one != other]
                    raise RuntimeError(f"{name}: sampled frames differ at {different}")
                reference = frames
                for column, value in zip(samples[i], timings + rss):
                    column.append(value)
                samples[i][-1].extend(pages)
        for build, binary, columns in zip(("base", "candidate"), binaries, samples):
            measures = (("list-files", "ms"), ("show-all", "ms"), ("reopen-cached", "ms"), ("read-patch", "ms"),
                        ("show-all-rss-delta", "KiB"), ("closed-rss-delta", "KiB"), ("page", "ms"))
            for (measure, unit), values in zip(measures, columns):
                p95 = sorted(values)[math.ceil(.95 * len(values)) - 1]
                print(f"{name}\t{build}\t{measure}\t{len(values)}\t"
                      f"{statistics.median(values):.3f}\t{p95:.3f}\t{unit}", flush=True)
            fixture.log.write_text("")
            sample(binary, fixture, scenario, counted=True)
            counts = Counter(fixture.log.read_text().splitlines())
            if counts["diff-tree"] != 1 or counts["diff-tree:whole"] != 1:
                raise RuntimeError(f"unexpected listing/patch Git operations: {counts}")
            print(f"# {name} {build} Git operations: {dict(counts)}", flush=True)


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("base", type=Path)
    cli.add_argument("candidate", type=Path, nargs="?")
    cli.add_argument("--runs", type=int, default=15)
    args = cli.parse_args()
    if args.runs < 1:
        cli.error("--runs must be positive")
    with tempfile.TemporaryDirectory(prefix="milo-history-paging-") as directory:
        binaries = [p.resolve(strict=True) for p in (args.base, args.candidate) if p is not None]
        compare(Path(directory), binaries, args.runs)
