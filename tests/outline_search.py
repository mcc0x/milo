#!/usr/bin/env python3
"""Measure file-search input to visible frame in a clean, synthetic Git repo.

python3 -B tests/outline_search.py BASE_BINARY [CANDIDATE_BINARY] [--files 50000] [--runs 15]
Builds alternate in a 160×50 PTY. Comparison checks the synchronized updates
for each action, not just the result text. No Rex or installed state is used.
"""
import argparse
import hashlib
import math
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import time
from types import SimpleNamespace

from backend_reader import FRAME_START, Terminal


class Outline(Terminal):
    def paint(self, frame):
        if not super().paint(frame):
            return False
        self.frame_hash = hashlib.sha256(frame[frame.rfind(FRAME_START):]).digest()
        return True

    def action(self, keys, header, result):
        start = time.monotonic_ns()
        self.send(keys)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            for painted in self.take_frames():
                if header in ''.join(self.grid[0]) and result in ''.join(self.grid[1]):
                    return (painted - start) / 1e6, self.frame_hash
            if not self.read(max(0, deadline - time.monotonic())):
                break
        screen = '\n'.join(''.join(row).rstrip() for row in self.grid[:5])
        raise RuntimeError(f'expected {header!r} and {result!r} after {keys!r}; screen:\n{screen}')


def fixture(root, files):
    home, repo, scratch = (root / name for name in ('home', 'repo', 'tmp'))
    for path in (home, repo, scratch):
        path.mkdir()
    env = dict(HOME=str(home), XDG_CONFIG_HOME=str(home / 'config'), XDG_STATE_HOME=str(home / 'state'),
               TMPDIR=str(scratch), PATH=os.defpath, TERM='xterm-256color', LC_ALL='C',
               GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull, GIT_ALLOW_PROTOCOL='',
               MILO_ICONS='plain')
    prefs = home / 'config' / 'milo' / 'preferences.conf'
    prefs.parent.mkdir(parents=True)
    prefs.write_text('static_marks = true\nfollow_activity = false\n')

    def git(*args):
        subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', *args], cwd=repo, env=env,
                       check=True, stdin=subprocess.DEVNULL, timeout=120)

    git('init', '-q', '--template=', '-b', 'main')
    for start in range(0, files, 250):
        folder = repo / 'data' / f'module-{start // 250:04d}' / 'components' / 'shared' / 'sources'
        folder.mkdir(parents=True)
        for i in range(start, min(start + 250, files)):
            stem = 'Münchner' if i % 4 == 3 else 'Generated'
            (folder / f'{stem}DataTransferModel{i:06d}.md').write_text('synthetic file\n')
    for name in ('00-probe.txt', 'QUARTZ-entry.txt', 'quartz-report.txt', 'Ångström-entry.txt'):
        (repo / name).write_text('synthetic marker\n')
    git('add', '-A')
    git('-c', 'user.name=Milo Profile', '-c', 'user.email=profile@example.invalid', 'commit', '-qm', 'fixture')
    return SimpleNamespace(repo=repo, env=env)


ACTIONS = [
    ('open', b'/', 'Find files', '00-probe.txt'),
    ('first', b'q', '/ q ', 'QUARTZ-entry.txt'),
    ('extend', b'u', '/ qu ', 'QUARTZ-entry.txt'),
    ('extend', b'a', '/ qua ', 'QUARTZ-entry.txt'),
    ('extend', b'r', '/ quar ', 'QUARTZ-entry.txt'),
    ('extend', b't', '/ quart ', 'QUARTZ-entry.txt'),
    ('extend', b'z', '/ quartz ', 'QUARTZ-entry.txt'),
    ('navigate', b'\x1b[B', '/ quartz ', 'QUARTZ-entry.txt'),
    ('miss', b'x', '/ quartzx ', 'No matches'),
    ('widen', b'\x7f', '/ quartz ', 'QUARTZ-entry.txt'),
    ('clear', b'\x15', 'Find files', '00-probe.txt'),
    ('unicode', 'å'.encode(), '/ å ', 'Ångström-entry.txt'),
    ('clear', b'\x15', 'Find files', '00-probe.txt'),
    ('wide_start', b'd', '/ d ', 'GeneratedDataTransferModel000000.md'),
    ('wide_extend', b'a', '/ da ', 'GeneratedDataTransferModel000000.md'),
    ('wide_extend', b't', '/ dat ', 'GeneratedDataTransferModel000000.md'),
    ('wide_extend', b'a', '/ data ', 'GeneratedDataTransferModel000000.md'),
    ('clear', b'\x15', 'Find files', '00-probe.txt'),
    ('paste', b'\x1b[200~datatransfermodel000000\x1b[201~', '/ datatransfermodel000000 ',
     'GeneratedDataTransferModel000000.md'),
]


def sample(binary, project):
    with Outline(binary, project, ['outline'], counted=False) as tty:
        tty.frame(b'00-probe.txt')
        tty.settle()
        values = [tty.action(keys, header, result) for _, keys, header, result in ACTIONS]
        tty.send(b'\x1b')
        tty.frame(b'00-probe.txt')
        tty.send(b'q')
        if not tty.await_exit(5):
            raise RuntimeError('Outline did not close')
        return zip(*values)


def compare(root, binaries, files, runs):
    project = fixture(root, files)
    groups = tuple(dict.fromkeys(name for name, *_ in ACTIONS))
    timings = [{name: [] for name in groups} for _ in binaries]
    reference = None
    for run in range(runs):
        order = range(len(binaries))
        for i in reversed(order) if run % 2 else order:
            elapsed, frames = sample(binaries[i], project)
            if reference is not None and frames != reference:
                different = [name for (name, *_), a, b in zip(ACTIONS, frames, reference) if a != b]
                raise RuntimeError(f'rendered updates differ: {different}')
            reference = frames
            for (name, *_), ms in zip(ACTIONS, elapsed):
                timings[i][name].append(ms)
    print(f'{files + 4} files, {runs} sessions per build; synchronized updates match')
    print('build\taction\tsamples\tp50_ms\tp95_ms')
    for build, series in zip(('base', 'candidate'), timings):
        for name, values in series.items():
            p95 = sorted(values)[math.ceil(.95 * len(values)) - 1]
            print(f'{build}\t{name}\t{len(values)}\t{statistics.median(values):.3f}\t{p95:.3f}')


if __name__ == '__main__':
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument('binary', nargs='+', type=Path)
    cli.add_argument('--files', type=int, default=50_000)
    cli.add_argument('--runs', type=int, default=15)
    args = cli.parse_args()
    if not 1 <= len(args.binary) <= 2 or min(args.files, args.runs) < 1:
        cli.error('pass one or two binaries and positive file/run counts')
    with tempfile.TemporaryDirectory(prefix='milo-outline-search-') as directory:
        compare(Path(directory), [b.resolve(strict=True) for b in args.binary], args.files, args.runs)
