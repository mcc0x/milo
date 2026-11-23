#!/usr/bin/env python3
"""Compare macOS release Readers in a 160×50 PTY (cells and Kitty).

python3 -B tests/picture_upload.py BASE_BINARY CANDIDATE_BINARY [--runs 24]

First-frame time ends at receipt of a complete synchronized update. RSS and
process I/O counters are sampled there, before checking its output. Disk and
logical writes are Darwin counters charged to Milo, not a sum for its children
or a count of every logical read of the fixture. PTY bytes count that update.
Kitty's start CPU is Milo's cumulative CPU as the first upload bytes arrive,
before the bulkiest PTY writes; for cells this metric is not applicable.
"""

import argparse
import base64
import ctypes
import hashlib
import math
import os
from pathlib import Path
import random
import re
import statistics
import struct
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import zlib

import backend_reader as backend

CELL = "▀".encode()
UPLOAD = b"\x1b_Ga=t,f=100,i="
KITTY_OK = b"\x1b_Gi=31;OK\x1b\\"
PACKET = re.compile(rb"\x1b_G([^;]+);([A-Za-z0-9+/=]{0,4096})\x1b\\")
PLACE = re.compile(rb"\x1b_Ga=p,i=(\d+),c=\d+,r=\d+,C=1,q=2\x1b\\")


class ProcessCounters:
    def __init__(self):
        self.lib = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
        self.lib.proc_pid_rusage.argtypes = (ctypes.c_int, ctypes.c_int, ctypes.c_void_p)
        timebase = (ctypes.c_uint32 * 2)()
        if self.lib.mach_timebase_info(ctypes.byref(timebase)) != 0:
            raise RuntimeError("mach_timebase_info failed")
        self.ms_per_tick = timebase[0] / timebase[1] / 1e6

    def at(self, pid):
        # Darwin sys/resource.h: rusage_info_v4, a 16-byte UUID then uint64_t fields.
        counters = (ctypes.c_uint64 * 39)()
        if self.lib.proc_pid_rusage(pid, 4, ctypes.byref(counters)) != 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error))
        return ((counters[2] + counters[3]) * self.ms_per_tick,
                (counters[12] + counters[13]) * self.ms_per_tick,
                counters[8] // 1024, counters[18], counters[19], counters[29])

class PictureTerminal(backend.Terminal):
    def update(self, marker, counters):
        # A large upload spans thousands of PTY reads. Scan only new data and
        # enough preceding bytes to detect a split frame terminator.
        deadline = time.monotonic() + backend.WAIT
        scan = upload_scan = 0
        start_cpu = None
        while self.read(max(0, deadline - time.monotonic())):
            received = time.monotonic_ns()
            if marker == UPLOAD and start_cpu is None:
                if self.buffer.find(UPLOAD, upload_scan) >= 0:
                    start_cpu = counters.at(self.child.pid)[0]
                upload_scan = max(0, len(self.buffer) - len(UPLOAD) + 1)
            while (end := self.buffer.find(backend.FRAME, scan)) >= 0:
                frame = bytes(self.buffer[:end])
                del self.buffer[:end + len(backend.FRAME)]
                scan = upload_scan = 0
                start = frame.rfind(backend.FRAME_START)
                if start >= 0 and frame.find(marker, start) >= 0:
                    return received, frame[start:] + backend.FRAME, start_cpu
            scan = max(0, len(self.buffer) - len(backend.FRAME) + 1)
            if time.monotonic() >= deadline:
                break
        raise RuntimeError(f"no complete picture update; PTY tail: {self.tail[-300:]!r}")


def png_chunk(kind, payload):
    return (struct.pack("!I", len(payload)) + kind + payload
            + struct.pack("!I", zlib.crc32(kind + payload)))


def fixture(noise):
    width, height = 1600, 1200  # Kitty can use these PNGs without resampling.
    rng = random.Random(1110)
    pixels = b"".join(b"\0" + (rng.getrandbits(width * 24).to_bytes(width * 3, "little")
                              if noise else bytes([row % 256, 111, 30]) * width)
                      for row in range(height))
    header = struct.pack("!IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", header)
            + png_chunk(b"IDAT", zlib.compress(pixels, 1)) + png_chunk(b"IEND", b""))


def check_upload(frame, expected):
    place = PLACE.search(frame)
    packets = list(PACKET.finditer(frame, frame.index(UPLOAD)))
    backend.require(place is not None and len(packets) > 1, "missing Kitty placement or chunked upload")
    image_id = place[1]
    previous = packets[0].start()
    for index, packet in enumerate(packets):
        last = index == len(packets) - 1
        more = b"m=0" if last else b"m=1"
        header = b"a=t,f=100,i=" + image_id + b",q=2," + more if index == 0 else more
        backend.require(packet.start() == previous and packet[1] == header,
                        "discontinuous or mislabelled Kitty upload")
        backend.require(len(packet[2]) == 4096 or last, "short non-final Kitty chunk")
        previous = packet.end()
    backend.require(previous <= place.start(), "Kitty placement preceded its upload")
    encoded = b"".join(packet[2] for packet in packets)
    backend.require(base64.b64decode(encoded, validate=True) == expected, "Kitty payload differs from source")


def sample(counters, binary, project, path, marker):
    with PictureTerminal(binary, project, ["view", str(path)], counted=False) as tty:
        when, frame, start_cpu = tty.update(marker, counters)
        metrics = ((when - tty.started) / 1e6, *counters.at(tty.child.pid), len(frame), start_cpu)
        backend.require(marker != UPLOAD or start_cpu is not None, "missed initial Kitty bytes")
        tty.send(b"q")
        backend.require(tty.await_exit(5), "Reader did not close")
    return frame, metrics


def compare(root, binaries, runs):
    home, repo, temp = (root / name for name in ("home", "project", "tmp"))
    xdg = {key: home / name for key, name in (("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                                               ("XDG_CACHE_HOME", "cache"), ("XDG_STATE_HOME", "state"))}
    for directory in (home, repo, temp, *xdg.values()):
        directory.mkdir()
    env = dict(HOME=str(home), TMPDIR=str(temp), PATH="/usr/bin:/bin", TERM="xterm-256color", LC_ALL="C",
               GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, **{k: str(v) for k, v in xdg.items()})
    subprocess.run(["git", "init", "-q", "--template=", str(repo)], env=env, check=True)
    counters = ProcessCounters()
    answer = backend.ANSWER
    print("file\tfile_B\tterminal\tbuild\tn\tquantile\tfirst_ms\tmilo_cpu_ms\tchild_cpu_ms\tfirst_rss_KiB"
          "\tdisk_read_B\tdisk_write_B\tlogical_write_B\tupdate_B\tkitty_start_milo_cpu_ms", flush=True)
    for name, noise in (("small", False), ("dense", True)):
        path = repo / (name + ".png")
        original = fixture(noise)
        path.write_bytes(original)
        for terminal, marker in (("cells", CELL), ("kitty", UPLOAD)):
            kitty = terminal == "kitty"
            project = SimpleNamespace(repo=repo, env=dict(env, MILO_GRAPHICS="kitty" if kitty else "off"))
            backend.ANSWER = (KITTY_OK if kitty else b"") + answer
            values = [[], []]
            reference = None
            for run in range(runs):
                for i in ((1, 0) if run % 2 else (0, 1)):
                    frame, result = sample(counters, binaries[i], project, path, marker)
                    digest = hashlib.sha256(frame).digest()
                    if reference is None:
                        if kitty:
                            check_upload(frame, original)
                        else:
                            backend.require(UPLOAD not in frame, "unexpected Kitty upload in cell mode")
                        reference = digest
                    backend.require(digest == reference, f"{name}/{terminal}: styled updates differ")
                    values[i].append(result)
            for build, samples in zip(("base", "candidate"), values):
                for label, quantile in (("p50", statistics.median), ("p95", lambda xs: sorted(xs)[math.ceil(.95 * len(xs)) - 1])):
                    print(name, len(original), terminal, build, len(samples), label,
                          *(round(quantile(col), 3) if col[0] is not None else "-"
                            for col in zip(*samples)), sep="\t", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs=2, type=Path)
    parser.add_argument("--runs", type=int, default=15)
    args = parser.parse_args()
    if sys.platform != "darwin" or args.runs < 1:
        parser.error("requires macOS and a positive run count")
    with tempfile.TemporaryDirectory(prefix="milo-picture-upload-") as directory:
        compare(Path(directory), [path.resolve(strict=True) for path in args.binary], args.runs)
