# Outline, Markdown, usage and alert measurements

Baseline: `536342ffab5b` (main). Both sides used the same release profile, Rust 1.100.0 and Apple M5 Pro on arm64 macOS. Workloads used generated files, fresh processes and warm filesystem caches. Times below are medians unless noted. These are comparisons on one machine, not release thresholds.

## Outline: getting a completed refresh onto the screen

| 50,000 tracked files, one dirty file, 20 subsequent saves | Baseline | Changed |
|---|---:|---:|
| Take a completed worker result into Outline, p50 / p95 | 3,598 / 4,293 µs | 12 / 32 µs |
| Last Git diff exit to visible counts, PTY, p50 / p95 | 5.23 / 5.52 ms | 1.79 / 1.86 ms |
| Save to visible counts, same PTY, p50 / p95 | 352 / 402 ms | 369 / 403 ms |

The first measurement isolates work on the pane thread. The second includes scheduling, painting and the observing process. The third also includes filesystem notifications, refresh pacing and Git: it did **not** get faster. The fixture edits a path near the start of the tracked listing; it does not time initial discovery or structural changes.

Only the same shared tracked listing and the same ordered pending paths, kinds and unpushed flags qualify. A recount updates the first contributing entry and its ancestors; a changed structure takes the rebuild path. A regression covers nested totals, selection, a failed index read and subsequent recovery.

## Reader: memory held by a Markdown tab

| 160×50 PTY, 25 opens per build and file | Baseline | Changed |
|---|---:|---:|
| Dense inline styles, first-frame RSS | 15,360 KiB | 12,400 KiB |
| Plain control, first-frame RSS | 10,080 KiB | 9,888 KiB |
| Dense, spawn to first rendered frame | 29.07 ms | 28.67 ms |
| Plain, spawn to first rendered frame | 28.23 ms | 28.05 ms |

Both files contain 3,930 list items and 522,690 source bytes. The dense input alternates inline code and plain text; the control changes only the inline pattern. Builds alternate on the same input. The first frame, nine page-downs and the last page produced byte-identical synchronized frames across builds. RSS at the last page matched first-frame RSS at median. This exercises an actual release Reader in a PTY, not a Rex window or an installed app. RSS includes allocator and process effects and is not peak memory.

A separate in-process capacity measurement attributed 4,194,304 → 2,097,152 bytes to the dense fixture's 235,800 style runs. The bounded preview can represent their byte offsets in 32 bits; the text itself is not truncated further. The regression renders styled, multibyte text beyond the first 64 KiB. The change does not target first-frame latency.

## Usage: sampling a local provider database

| 200,000 OMP history rows, WAL writer connected | Baseline | Changed |
|---|---:|---:|
| No cached report, result p50 / p95 | 219.1 / 226.2 ms | 119.9 / 132.2 ms |
| Fresh cached report, result p50 / p95 | 36.2 / 36.8 ms | 36.2 / 37.1 ms |

The release sampler committed one new observation per simulated 20-second interval and checked the displayed percentage after each of 12 reads. One warm-up was excluded. Times include launching the read-only SQLite CLI (3.54.0) and consuming the result. Each build was run separately; this is not menu repaint time, battery usage, or a provider API benchmark.

The query first finds the latest eligible time for each account and limit, resolves tied timestamps by row ID, then fetches the selected payloads. The account, freshness and clock-skew rules still apply. A later backfill cannot outrank a newer observation just because it has a higher ID; an unknown newer limit must not resurrect an older value. The sampler still rechecks an active WAL. Costs depend on SQLite's planner, row distribution and indexes; the query still reads history and is not constant-time.

## Alert queues: retain only the selected content

In a release parser fixture, 64 distinct blocked events with 2,048 synthetic message bytes each produced 64 alerts on both builds. The sum of text lengths at the metadata-only queue boundary fell from **131,072 bytes to zero**. Parsing the batch took 594 → 423 µs (median of three batch medians, 21 iterations each, alternating build order). The fixture retains parser output in memory; it does not measure RSS, Rex, Keychain, or network delivery.

Metadata mode no longer copies status text into the event. Notification text is still read transiently to distinguish two different notifications from one terminal, but does not wait in delivery queues. Opting into text still sends it through the existing formatting and length limits. This is not memory zeroization: the input buffer still contains the event, and clearing an allocation is not a promise about allocator reuse.

## Other candidates

- Git discovery on ordinary, nested and linked throwaway checkouts took five Git processes each. The refresh measurement supported improving pane-thread adoption rather than adding a discovery cache.
- The existing loopback settings benchmark measured a 0.60 ms median preference save and a 0.40 ms median read during a competing save. The slow Rex stand-in dominated first state (191 ms with a 50 ms delay per call). No settings lock, permissions or persistence behavior changed.
- The shipped relay ran in a read-only container with no external network and a disposable volume. A saturated batch made no extra provider calls, an idle 55 s interval had zero block writes, and shutdown drained a send with a 25 ms provider delay in 26.3 ms and aborted the long poll. No relay or retry changes. This is not production traffic or a general security assessment.

## Repeating the workloads

Build both revisions with `cargo build --release --locked`. Run the in-crate fixtures separately in each checkout. The settings and Reader drivers take binary paths; use the candidate checkout's Markdown driver for both binaries.

```sh
cargo test --release --locked --bin milo adopting_edits_in_a_large_worktree -- --ignored --nocapture
python3 tests/markdown_memory.py /path/to/base/milo /path/to/candidate/milo
cargo test --release --locked --bin milo usage_profile_active_sqlite -- --ignored --nocapture --test-threads=1
python3 tests/settings-bench.py /path/to/base/milo
```

The [relay container workload](../../hosted/README.md#benchmark-the-shipped-relay) includes the isolation flags. None of these measurements requires provider credentials.
