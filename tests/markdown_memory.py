#!/usr/bin/env python3
"""Compare two release Readers' RSS and output on dense and plain Markdown.

python3 tests/markdown_memory.py BASE_BINARY CANDIDATE_BINARY [--runs 25]
Uses a 160×50 PTY, a temporary Git repository and no Rex or provider.
"""
import argparse
import hashlib
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import time
from types import SimpleNamespace

from backend_reader import FRAME_START, Terminal


class Reader(Terminal):
    def paint(self, frame):
        if not super().paint(frame):
            return False
        self.frame_hash = hashlib.sha256(frame[frame.rfind(FRAME_START):]).digest()
        return True

    def at_row(self, marker, row=1):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            for when in self.take_frames():
                if marker in ''.join(self.grid[row]):
                    return when, self.frame_hash
            if not self.read(max(0, deadline - time.monotonic())):
                break
        raise RuntimeError(f'expected {marker!r} on row {row}')


def sample(binary, project, path):
    with Reader(binary, project, ['view', str(path)], counted=False) as tty:
        when, first = tty.at_row('GuideRow00000')
        opened = (when - tty.started) / 1e6
        first_rss = tty.rss()
        frames = [first]
        tty.settle()
        for page in range(1, 10):
            tty.send(b' ')
            _, frame = tty.at_row(f'GuideRow{47 * page:05d}')
            frames.append(frame)
        tty.send(b'G')
        _, frame = tty.at_row('GuideRow03929', row=48)
        frames.append(frame)
        bottom_rss = tty.rss()
        tty.send(b'q')
        if not tty.await_exit(5):
            raise RuntimeError('reader did not close')
        return frames, (opened, first_rss, bottom_rss)


def compare(root, binaries, runs):
    home, repo = root / 'home', root / 'repo'
    home.mkdir()
    repo.mkdir()
    env = dict(HOME=str(home), PATH=os.defpath, TERM='xterm-256color', LC_ALL='C',
               GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull)
    subprocess.run(['git', 'init', '-q', '--template=', str(repo)], env=env, check=True)
    project = SimpleNamespace(repo=repo, env=env)
    path = repo / 'guide.md'
    print('fixture\tbuild\tfirst_ms\tfirst_rss_KiB\tbottom_rss_KiB')
    for fixture, filler in [('dense', '`a`a' * 29), ('plain', 'aa a' * 29)]:
        path.write_text(''.join(f'- GuideRow{i:05d} {filler}\n' for i in range(3930)))
        reference = None
        samples = [[], []]
        for run in range(runs):
            for i in ((1, 0) if run % 2 else (0, 1)):
                frames, result = sample(binaries[i], project, path)
                if reference is not None and frames != reference:
                    raise RuntimeError(f'{fixture}: rendered frames differ')
                reference = frames
                samples[i].append(result)
        for build, values in zip(('base', 'candidate'), samples):
            print(fixture, build, *(round(statistics.median(col), 3) for col in zip(*values)), sep='\t', flush=True)


if __name__ == '__main__':
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument('binary', nargs=2, type=Path)
    cli.add_argument('--runs', type=int, default=25)
    args = cli.parse_args()
    if args.runs < 1:
        cli.error('--runs must be positive')
    with tempfile.TemporaryDirectory(prefix='milo-markdown-memory-') as directory:
        compare(Path(directory), [b.resolve(strict=True) for b in args.binary], args.runs)
