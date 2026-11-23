#!/usr/bin/env python3
"""Compare standalone release builds: python3 tests/standalone_bench.py BASE CANDIDATE"""

import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace

from backend_reader import Terminal


def sample(binary, project):
    times = []
    for args, ready in (([], b'q quit'), (['history'], b'fixture commit'), (['outline'], b'file000.txt')):
        with Terminal(binary, project, args, counted=False) as tty:
            times.append(tty.frame(ready) - tty.started)
            if args == ['outline']:
                tty.settle()
                start = time.monotonic_ns()
                tty.send(b'\r')
                times.append(tty.frame(b'reader fixture line') - start)
                tty.send(b'q')
                tty.frame(b'file001.txt')
            tty.send(b'q')
            if not tty.await_exit(5) or tty.child.returncode != 0:
                raise RuntimeError(f'{args} did not exit normally')
    return times


def compare(builds, root):
    repo = root / 'project'
    repo.mkdir()
    home = root / 'home'
    home.mkdir()
    env = dict(HOME=str(home), PATH=os.defpath, TERM='xterm-256color', LC_ALL='C',
               GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1')
    if shutil.which('rex', path=env['PATH']):
        raise RuntimeError('This benchmark needs a system PATH without rex')
    def git(*args):
        subprocess.run(['git', *args], cwd=repo, env=env, check=True)
    git('init', '-q', '--template=')
    for i in range(128):
        (repo / f'file{i:03}.txt').write_text('reader fixture line\n' * 20)
    git('add', '.')
    git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture commit')
    # Compare repeat launches, not the old welcome screen against the new Home.
    subprocess.run([builds[0], 'setup', '--skip-onboarding'], cwd=repo, env=env, check=True)
    project = SimpleNamespace(repo=repo, env=env)
    samples = [[], []]
    for i in range(12):
        for index in (i % 2, 1 - i % 2):
            samples[index].append(sample(builds[index], project))
    print('measure\tbase_median_ms\tcandidate_median_ms')
    medians = [map(statistics.median, zip(*runs)) for runs in samples]
    for name, before, after in zip(('home', 'history', 'outline', 'outline-enter-reader'), *medians):
        print(f'{name}\t{before / 1e6:.2f}\t{after / 1e6:.2f}')


if __name__ == '__main__':
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    with tempfile.TemporaryDirectory(prefix='milo-standalone-') as directory:
        compare([Path(arg).resolve(strict=True) for arg in sys.argv[1:]], Path(directory))
