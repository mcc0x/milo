#!/usr/bin/env python3
"""Measure the shipped Reader in a 160×50 non-Kitty PTY. Fixture generation uses ffmpeg and cwebp.

python3 tests/rich_reader_bench.py target/release/milo
Reports medians of five opens, from spawning Milo to a complete rendered frame,
not the loading indicator. RSS is Milo's resident set at that frame (not decoder peak).
"""

import os
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile
from types import SimpleNamespace

from backend_reader import Terminal


def measure(binary, root):
    repo = root / "project"
    repo.mkdir()
    home = root / "home"
    home.mkdir()
    env = dict(HOME=str(home), PATH=os.defpath, TERM="xterm-256color", LC_ALL="C",
               GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    subprocess.run(["git", "init", "-q", "--template=", str(repo)], env=env, check=True)
    project = SimpleNamespace(repo=repo, env=env)
    for ext in ("png", "jpg", "mp4"):
        codec = ["-t", "8", "-c:v", "libx264", "-preset", "fast"] if ext == "mp4" else ["-frames:v", "1"]
        subprocess.run(["ffmpeg", "-nostdin", "-loglevel", "error", "-f", "lavfi", "-i",
                        "testsrc2=size=1920x1080:rate=24", *codec, str(repo / f"chart.{ext}")], check=True)
    subprocess.run(["cwebp", "-quiet", str(repo / "chart.png"), "-o", str(repo / "chart.webp")], check=True)
    (repo / "guide.md").write_text("# Field guide\n\n" +
                                  "- **Habitat** and `range`. See [field notes](notes.md).\n" * 2048)
    print("file\tbytes\tmedian_first_frame_ms\tmedian_milo_rss_KiB", flush=True)
    for file in sorted(path for path in repo.iterdir() if path.is_file()):
        elapsed, rss = [], []
        marker = b"Field guide" if file.suffix == ".md" else "▀".encode()
        for _ in range(5):
            with Terminal(binary, project, ["view", str(file)], counted=False) as tty:
                ready = tty.frame(marker)
                elapsed.append((ready - tty.started) / 1_000_000)
                rss.append(tty.rss())
                tty.send(b"q")
                if not tty.await_exit(5):
                    raise RuntimeError(f"{file.name}: reader did not close")
        print(f"{file.name}\t{file.stat().st_size}\t{statistics.median(elapsed):.1f}\t{statistics.median(rss)}", flush=True)


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="milo-rich-bench-") as directory:
        measure(Path(sys.argv[1]).resolve(strict=True), Path(directory))
