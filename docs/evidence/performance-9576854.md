# History expansion, Outline search, image uploads and local usage sampling

Base: `9576854548` (main with #75 and #76). Product changes: `e40d6c18` (Claude), `18547ab8` (OMP), `969cd7fb` (images), `3d62d778` (History) and `7054c95e` (Outline). No #69 or #78. The comparisons below used the **integrated** release build, not five separately tuned binaries. `536342f` is used only to recheck [#75’s unindexed tail](quality-536342f.md), not as the baseline for a new win.

One Apple M5 Pro, macOS arm64, Rust 1.100.0, Apple Git 2.54.0, SQLite CLI 3.54.0. Fresh processes, warm filesystem caches, generated data and disposable homes. No installed Milo, live Rex session, provider, or hosted service. The PTY drivers use a 160×50 terminal, alternate build order, and compare synchronized output bytes for the sampled actions. Times are p50 / nearest-rank p95; samples within a pane or batch are **not** independent process launches. These are workload comparisons, not release thresholds.

## History: expand a commit, not its memory footprint

| Workload | Samples/build | Base p50 / p95 | Final p50 / p95 |
|---|---:|---:|---:|
| Show all 200,000 files after the eight-file preview, ms | 24 | 0.413 / 0.455 | 0.097 / 0.131 |
| Milo RSS increase on expansion, KiB | 24 | 4,720 / 4,736 | 16 / 16 |
| RSS still above pre-expansion after collapse, KiB | 24 | 4,720 / 4,736 | 16 / 16 |
| First file-list load for the selected commit, after History settled, ms | 24 | 284.92 / 290.64 | 284.18 / 297.76 |
| Page Up/Down in the expanded commit, ms | 720 | 0.023 / 0.036 | 0.024 / 0.039 |
| Read the selected patch, ms | 24 | 3.86 / 5.45 | 4.95 / 5.39 |
| Show all in the 6,000-file control, ms | 24 | 0.097 / 0.119 | 0.098 / 0.151 |

One commit can be open. Its file rows now occupy one index range between the ordinary timeline rows. The file list itself is still retained. This removes per-file **row** storage; it does not accelerate Git, reduce patch I/O or improve ordinary paging. The fixture commits many paths to the same tiny blob, not many large or renamed blobs. RSS is a process snapshot, not peak allocation. Both count probes ran one listing `diff-tree` and one whole-file `diff-tree` per scenario, with none added for paging or cached reopen. Sampled navigation, expansion, reopening and patch frames matched. A Rust fixture also checks selection across refresh and manual reload.

## Outline: narrow an existing search

| Key to observed frame, 50,004 tracked paths | Samples/build | Base p50 / p95, ms | Final p50 / p95, ms |
|---|---:|---:|---:|
| First letter `q` | 24 | 3.522 / 3.811 | 3.544 / 3.816 |
| Extend `q` one letter at a time to `quartz` (two paths) | 120 | 3.414 / 3.553 | 0.016 / 0.026 |
| Append `x`, no matches | 24 | 3.346 / 3.413 | 0.019 / 0.027 |
| Backspace to widen | 24 | 3.375 / 3.442 | 3.379 / 3.513 |
| Extend a broad query, `d` to `data` | 72 | 0.374 / 0.423 | 0.366 / 0.406 |
| Fresh pasted query | 24 | 5.470 / 5.692 | 5.455 / 5.661 |

Appending to a substring query cannot introduce a new path. Filter the existing indices in place; rescan on widening or replacing the file listing. No path cache, extra filesystem reads or change to match order. The first letter, a broad query and backspace still inspect most or all paths. The mixed-case/Unicode fixture also drives result navigation and clearing. This measures interactive search, **not** #75's refresh adoption or initial Git discovery.

## Reader: CPU to send a dense PNG

| 1600×1200 dense PNG, 24 opens/build/mode | Base p50 / p95 | Final p50 / p95 |
|---|---:|---:|
| Kitty: cumulative Milo CPU at first upload bytes, ms | 12.45 / 17.04 | 10.63 / 13.40 |
| Kitty: cumulative Milo CPU at completed frame, ms | 54.72 / 89.82 | 52.88 / 81.25 |
| Kitty: spawn to complete picture update, ms | 181.16 / 317.44 | 176.92 / 263.22 |
| Kitty: first-frame Milo RSS, KiB | 22,248 / 22,320 | 22,192 / 22,240 |
| Cells: spawn to complete picture update, ms | 85.73 / 117.74 | 86.28 / 117.23 |
| Small compressed PNG, Kitty first frame, ms | 84.03 / 85.55 | 83.68 / 85.65 |

Encode complete triples into four output bytes at once; only the tail needs padding. The CPU sample includes startup and some PTY writing, not just encoding. Wall-time tails vary with decoder and PTY scheduling; do not attribute their entire difference to the encoder. The dense source is 5,763,018 bytes; both builds emitted exactly 7,701,036 bytes for its Kitty update (146,745 for cells). Both had 5,763,072 disk-write bytes charged to Milo at the median; this is not total filesystem traffic including decoder children. The small control is a 29,069-byte PNG of the same dimensions. Both modes’ styled updates matched; decoded Kitty packets also matched the original PNG. A PTY receiving the protocol is not a measurement of a terminal emulator displaying it.

## Claude: merge transcript IDs after an append

| Refresh, milliseconds | Base p50 / p95 | Final p50 / p95 |
|---|---:|---:|
| 32 active files, known append | 3.311 / 3.718 | 1.556 / 1.980 |
| Same, simulated filesystem notification before timing | 3.663 / 3.908 | 1.889 / 2.128 |
| Same, idle | 0.550 / 0.924 | 0.555 / 1.861 |
| Four active files, known append | 0.251 / 0.394 | 0.230 / 0.358 |

Eight matched runs/build, alternating order; each value is the **median of eight batch quantiles**, not a pooled percentile. Each batch contains 32 looks (lower middle observation for its p50), with 128 older rollouts; the busy case starts with 63,752 IDs after deduplication across files. Reserve for the prior distinct count, bounded by IDs in the remaining files, rather than repeatedly growing the temporary table. Cold starts can still grow it. The idle path skips the changed code; its worse p95 is reported, not claimed as a win. In each append phase both builds wrote 13,216 fixture bytes and read 13,248 transcript bytes; idle read zero. Token totals and complete/partial state were checked after every look; Codex percentage and time after every append. The fixture calls `filesystem_changed()` directly: this is neither OS notification latency nor menu repaint time.

After session churn, the prior distinct count can exceed the current distinct count: surviving files can share IDs. The sum of their stored IDs caps the hint, but a temporary table can still be oversized. The append workload does not measure peak memory in that case.

## OMP: check the historical tail, then compare the new plan

| Real sampler: writer connected, refresh to result | Pre-#75 p50 / p95, ms | Base p50 / p95, ms | Final p50 / p95, ms |
|---|---:|---:|---:|
| 200,000 unindexed history rows, no cache | 217.70 / 239.81 | 214.31 / 222.37 | 122.53 / 125.99 |
| Same, fresh cached report | 36.12 / 37.24 | 36.28 / 37.16 | 36.25 / 37.06 |

[144 matched observations per column and case](omp-tail-9576854.tsv). Twelve independently seeded batches per revision; all six revision orders twice. Each batch excludes a warm-up, then commits an observation every simulated 20 seconds for 12 reads. Expected displayed percentage alternates 25/75; exactly one completed job per look. This times the release sampler, CLI launch, read, parsing and result adoption, **not** menu rendering. The ignored fixture uses a longer deadline than the production one second. No sample exceeded one second. There is **no evidence here of a persistent unindexed tail regression in #75**; it is not evidence about production timeouts.

| Exact production SQL in a fresh read-only CLI | Pairs | Base p50 / p95, ms | Final p50 / p95, ms |
|---|---:|---:|---:|
| Unindexed, uncached | 120 | 213.01 / 231.16 | 121.99 / 125.91 |
| Unindexed, fresh cache | 120 | 36.03 / 36.80 | 35.97 / 36.81 |
| Indexed, dense | 24 | 136.86 / 149.26 | 115.11 / 127.44 |
| Indexed, sparse | 24 | 3.73 / 4.14 | 3.64 / 3.95 |

Both readers saw the **same WAL snapshot** per pair, alternating AB/BA; one warm-up pair per case. JSON bytes matched between revisions and the decoded rows matched expected values every time. The earlier pre-#75 → base SQL-only recheck gave uncached p95 226.95 → 231.34 ms; the order-stratified paired bootstrap interval for the difference was −12.71 to +22.94 ms. The corresponding base → final interval was −118.70 to −84.69 ms. These descriptive intervals resample pairs on one machine; they do not account for other hardware or changes in the planner.

The final SQL fixes the loop order of the timestamp tie-break join, not the winner selection rule. On the final uncached fixture, SQLite's autoindex insert counter fell from 200,120 to 0; peak SQLite-tracked memory fell from 58,129,056 to 30,872,768 bytes. **Not less disk spill:** both reported 10,406,300 temp bytes spilled and zero kernel block operations in the warm diagnostic run. VM steps rose from 13.41M to 14.41M. The gain is avoiding a large temporary index; this is still a history scan, not constant-time sampling. A single selected account/limit is a favorable dense case; distributions with many distinct limits, different SQLite versions, cold storage and production power use were not measured.

## Reproduction

From this branch, export the three revisions to disposable directories. Install the Rust toolchain and cache the locked dependency before running offline. Commands below use Bash, Git, Cargo, Python 3, `jq` and the system SQLite CLI; image counters and `sips` require macOS. The historical SQL comparison used the system Python's SQLite 3.54.0 writer; the base-to-final comparison used Homebrew Python's 3.54.1 writer. Both read with `/usr/bin/sqlite3` 3.54.0. Each pair uses one writer.

```bash
set -euo pipefail
lab=$(mktemp -d)
mkdir -p "$lab/home" "$lab/tmp"
for name in pre base final; do
  case $name in pre) rev=536342f;; base) rev=9576854;; final) rev=1d9f42c;; esac
  d="$lab/$name"; mkdir "$d"; git archive "$rev" | tar -x -C "$d"
  # Identical test-only fixtures; never copy production SQL or a production reader.
  cp src/usage/tests.rs "$d/src/usage/tests.rs"
  cp src/usage/tests/profile.rs "$d/src/usage/tests/profile.rs"
  (cd "$d"; cargo build --release --locked --offline
   cargo test --release --locked --offline --bin milo --no-run --message-format=json |
     jq -r 'select(.reason=="compiler-artifact" and .profile.test and .target.name=="milo") | .executable'
  ) > "$lab/$name.testpath"
done
isolated() {
  env -i HOME="$lab/home" TMPDIR="$lab/tmp" XDG_CONFIG_HOME="$lab/home/config" \
    XDG_STATE_HOME="$lab/home/state" XDG_DATA_HOME="$lab/home/data" \
    XDG_CACHE_HOME="$lab/home/cache" XDG_RUNTIME_DIR="$lab/tmp" \
    GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null PATH="$PATH" LC_ALL=C "$@"
}
b="$lab/base/target/release/milo"; f="$lab/final/target/release/milo"
isolated python3 -B tests/history_paging.py "$b" "$f" --runs 24
isolated python3 -B tests/outline_search.py "$b" "$f" --files 50000 --runs 24
isolated python3 -B tests/picture_upload.py "$b" "$f" --runs 24
isolated /usr/bin/python3 -B tests/usage_omp_tail.py 536342f 9576854 --pairs 120 --raw
isolated /opt/homebrew/bin/python3 -B tests/usage_omp_tail.py 9576854 18547ab --pairs 120 --indexed-pairs 24 --raw
for ((i=0; i<8; ++i)); do
  order='base final'; if ((i%2)); then order='final base'; fi
  for name in $order; do
    isolated "$(cat "$lab/$name.testpath")" usage_profile_transcripts \
      --ignored --nocapture --test-threads=1 > "$lab/transcripts-$i-$name"
  done
done
for repeat in 1 2; do
  for order in 'pre base final' 'pre final base' 'base pre final' \
               'base final pre' 'final pre base' 'final base pre'; do
    for name in $order; do
      isolated env MILO_BENCH_OMP_TAIL=1 "$(cat "$lab/$name.testpath")" \
        usage_profile_active_sqlite --ignored --nocapture --test-threads=1 \
        >> "$lab/sampler-$name"
    done
  done
done
```

For `sample` rows in the **Rust sampler** logs the last field is completion time in µs. Group by revision and workload, take the median and the 137th ordered value of 144. The SQL-only driver's `--raw` rows instead end with A and B elapsed times in ms. For transcripts take the median of the eight reported batch p50s and of the eight batch p95s for each workload. CLI diagnostic launches are separate from timed queries. History's timed pages move near the beginning of the expanded list; the end-of-list check is not timed as a page. PTY parity covers sampled updates, not every asynchronous frame or every possible cursor position. No cross-platform speedup is inferred from this machine.
