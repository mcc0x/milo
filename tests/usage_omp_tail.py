#!/usr/bin/env python3
"""SQL-only OMP WAL benchmark: python3 tests/usage_omp_tail.py [pre-#75] [current]."""

import argparse
import json
import math
from pathlib import Path
import random
import re
import resource
import sqlite3
import statistics
import subprocess
import tempfile
import time


SQLITE = "/usr/bin/sqlite3"
ROOT = Path(__file__).resolve().parents[1]
ROWS = 200_000
NOW = 1_760_000_000
RESET = (NOW + 86_400) * 1000
ACCOUNT = "oauth|account:bench"
LIMIT = "anthropic:5h"
SCHEMA = """
CREATE TABLE cache(key TEXT PRIMARY KEY, value TEXT, expires_at INTEGER);
CREATE TABLE usage_history(id INTEGER PRIMARY KEY, recorded_at INTEGER, provider TEXT,
    account_key TEXT NOT NULL, email TEXT, account_id TEXT, limit_id TEXT, label TEXT,
    window_label TEXT, used_fraction REAL, status TEXT, resets_at INTEGER);
"""
INDEXES = """
CREATE INDEX idx_usage_history_series ON usage_history(provider,account_key,limit_id,recorded_at);
CREATE INDEX idx_usage_history_recorded ON usage_history(recorded_at);
"""
ADD_HISTORY = """
INSERT INTO usage_history(provider, account_key, recorded_at, limit_id, used_fraction, resets_at, window_label)
"""
READER = [SQLITE, "-batch", "-safe", "-bail", "-nofollow", "-readonly", "-json", "-init", "/dev/null"]


def template(revision, env):
    source = (ROOT / "src/usage/omp.rs").read_text(encoding="utf-8") if revision == "-" else subprocess.check_output(
        ["/usr/bin/git", "show", f"{revision}:src/usage/omp.rs"], cwd=ROOT, env=env, text=True, timeout=5
    )
    match = re.search(r'fn sql\(now: i64\) -> String \{.*?format!\(\s+r"(.*?)"\s*\)\s*}', source, re.S)
    if not match or "{latest}" not in match[1] or re.search(r"[{}]", match[1].replace("{latest}", "")):
        raise ValueError(f"cannot extract the production OMP SQL template at {revision}")
    return match[1]


def run_query(path, source, now, env, diagnostics=False):
    # Match the production URI, CLI flags and CLOCK_SKEW substitution.
    sql = source.replace("{latest}", str((now + 300) * 1000 + 999))
    flags = ["-cmd", ".eqp on", "-cmd", ".stats on"] if diagnostics else []
    command = READER + flags + [path.as_uri() + "?mode=ro&readonly_shm=1", sql]
    start = time.perf_counter_ns()
    result = subprocess.run(command, stdin=subprocess.DEVNULL, capture_output=True, env=env, check=True, timeout=5)
    elapsed = (time.perf_counter_ns() - start) / 1_000_000
    if result.stderr:
        raise RuntimeError(result.stderr.decode())
    return result.stdout, elapsed


def seed(path, name):
    db = sqlite3.connect(path)
    db.executescript(SCHEMA)
    with db:
        db.execute(
            f"""WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i+1<{ROWS})
            {ADD_HISTORY}
            SELECT 'anthropic', ?, ?-i*1000, ?, 0.25, ?, '5 Hours' FROM n""",
            (ACCOUNT, (NOW - 60) * 1000, LIMIT, RESET),
        )
        if name == "indexed-sparse":
            db.execute("UPDATE usage_history SET provider='other' WHERE id>1")
    indexed = name.startswith("indexed-")
    if indexed:
        db.executescript(INDEXES)
    if db.execute("PRAGMA journal_mode=WAL").fetchone() != ("wal",):
        raise RuntimeError("fixture did not enter WAL mode")
    assert len(db.execute("PRAGMA index_list(usage_history)").fetchall()) == (2 if indexed else 0)
    return db


def commit(db, cached, now, fraction):
    observed = (now - 1) * 1000
    with db:
        if cached:
            report = {"value": {"provider": "anthropic", "fetchedAt": (now - 4) * 1000, "limits": [{
                "id": LIMIT, "amount": {"usedFraction": 0.1}, "scope": {"tier": None},
                "window": {"durationMs": 18_000_000, "resetsAt": (now + 3600) * 1000},
            }]}}
            db.execute("INSERT OR REPLACE INTO cache VALUES (?, ?, 0)", (
                f"usage_cache:report:anthropic:default:{ACCOUNT}", json.dumps(report),
            ))
        db.execute(
            ADD_HISTORY + "VALUES ('anthropic', ?, ?, ?, ?, ?, '5 Hours')",
            (ACCOUNT, observed, LIMIT, fraction, RESET),
        )
    expected = []
    if cached:
        # The cache report remains, but its only limit is superseded by history.
        expected.append(dict(provider="anthropic", observed_ms=(now - 4) * 1000, limit_id=None, used_fraction=None,
                             reset_ms=None, duration_ms=None, tier=None, window_label=None, history_row=0))
    expected.append(dict(provider="anthropic", observed_ms=observed, limit_id=LIMIT, used_fraction=fraction,
                         reset_ms=RESET, duration_ms=None, tier=None, window_label="5 Hours", history_row=1))
    return expected


def percentile(values, fraction):
    return sorted(values)[math.ceil(len(values) * fraction) - 1]


def summary(times):
    return (f"p50={statistics.median(times):.2f} p95={percentile(times, .95):.2f} "
            f"max={max(times):.2f} >1s={sum(t > 1000 for t in times)}")


def tail_interval(samples):
    # Resample intact pairs within each AB/BA order, so order remains balanced.
    rng = random.Random(75)
    orders = (samples[::2], samples[1::2])
    differences = []
    for _ in range(4000):
        pairs = [rng.choice(order) for order in orders for _ in order]
        pre, current = zip(*pairs)
        differences.append(percentile(current, .95) - percentile(pre, .95))
    return percentile(differences, .025), percentile(differences, .975)


def measure(root, name, queries, count, env, raw):
    cached = name == "cached"
    path = root / f"{name}.db"
    db = seed(path, name)
    samples = []
    try:
        for i in range(count + 1):
            now = NOW + i * 20
            expected = commit(db, cached, now, 0.75 if i % 2 else 0.25)
            assert path.with_name(path.name + "-wal").is_file()
            assert path.with_name(path.name + "-shm").is_file()
            ab = i % 2 == 1
            order = (0, 1) if ab else (1, 0)
            results = [None, None]
            for side in order:
                results[side] = run_query(path, queries[side], now, env)
            (pre, a), (current, b) = results
            assert pre == current, (name, i, pre, current)
            assert json.loads(pre) == expected, (name, i, pre, expected)
            if i:
                samples.append((a, b))
                if raw:
                    print(f"sample\t{name}\t{i}\t{'AB' if ab else 'BA'}\t{a:.3f}\t{b:.3f}")

        pre, current = zip(*samples)
        low, high = tail_interval(samples)
        print(f"{name}: {count} matched pairs, {count // 2} AB + {count // 2} BA, {2 * count} timed CLI launches; "
              f"{path.stat().st_size} base DB bytes, {path.with_name(path.name + '-wal').stat().st_size} WAL bytes")
        print(f"  A/pre:     {summary(pre)} ms")
        print(f"  B/current: {summary(current)} ms")
        print(f"  B minus A: paired median={statistics.median(b-a for a, b in samples):+.2f} ms; "
              f"p95 difference={percentile(current, .95)-percentile(pre, .95):+.2f} ms "
              f"(paired bootstrap 95% interval {low:+.2f} to {high:+.2f} ms)")
        for order in (0, 1):
            a, b = zip(*samples[order::2])
            print(f"  {'BA' if order else 'AB'} p95: {percentile(a, .95):.2f} -> {percentile(b, .95):.2f} ms")
        for side, query in zip(("A/pre", "B/current"), queries):
            before = resource.getrusage(resource.RUSAGE_CHILDREN)
            output, _ = run_query(path, query, now, env, diagnostics=True)
            after = resource.getrusage(resource.RUSAGE_CHILDREN)
            print(f"\n{name} {side} diagnostics (separate CLI launch; child block operations "
                  f"in={after.ru_inblock-before.ru_inblock} out={after.ru_oublock-before.ru_oublock}):")
            # Print the plan and final SELECT's stats, excluding two PRAGMA stats blocks.
            print(output.decode().split("QUERY PLAN\n", 1)[-1])
    finally:
        db.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pre", default="536342f", nargs="?")
    parser.add_argument("current", default="9576854", nargs="?", help="revision or '-' for worktree SQL")
    parser.add_argument("--pairs", type=int, default=120, help="matched pairs per profile (even)")
    parser.add_argument("--indexed-pairs", type=int, default=0, help="also run this many pairs per indexed control (even)")
    parser.add_argument("--raw", action="store_true", help="print individual times as TSV")
    args = parser.parse_args()
    if args.pairs < 2 or args.pairs % 2:
        parser.error("--pairs must be positive and even")
    if args.indexed_pairs < 0 or args.indexed_pairs % 2:
        parser.error("--indexed-pairs must be nonnegative and even")
    with tempfile.TemporaryDirectory(prefix="milo-omp-tail-") as directory:
        root = Path(directory).resolve()
        env = dict.fromkeys(("HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "TMPDIR"), str(root))
        queries = [template(revision, env) for revision in (args.pre, args.current)]
        version = subprocess.check_output([SQLITE, "-version"], env=env, text=True, timeout=5).strip()
        print(f"SQL only: {args.pre} vs {args.current}, SQLite CLI {version}, writer {sqlite3.sqlite_version}")
        print(f"{ROWS} seeded rows, one committed WAL transaction per pair, one warm-up pair per profile")
        print("Fresh readers, warm OS cache; read-only SQLite CLI spawn through stdout and process exit; no Rust sampler")
        for name in ("uncached", "cached"):
            measure(root, name, queries, args.pairs, env, args.raw)
        if args.indexed_pairs:
            for name in ("indexed-dense", "indexed-sparse"):
                measure(root, name, queries, args.indexed_pairs, env, args.raw)


if __name__ == "__main__":
    main()
