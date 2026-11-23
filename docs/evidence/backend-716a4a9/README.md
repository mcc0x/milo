# Local backend measurements against `716a4a9`

Apple M5 Pro, arm64 macOS 27.2.1, Rust 1.100.0, Apple Git 2.54.0, `/usr/bin/sqlite3` 3.54.0. Release builds, generated files and disposable homes. The Git remote in the History fixture is a local ref, not a service. Nothing here measures production traffic or energy use.

## History: cost on the pane thread

| Tracked / dirty paths | Base, ns per clone/drop | Changed, ns per clone/drop |
|---|---:|---:|
| 13 / 2 | 296, 354 | 170, 225 |
| 6,001 / 1,000 | 11,930, 12,158 | 156, 156 |

Two runs per build in base/changed/changed/base order. Each entry is the median of 21 batches of 512 copies of the actual refresh inputs (`Repo` and `Option<HistoryVersion>`), including their destruction. The fixture reads a real dirty checkout, one commit ahead of its upstream, and checks the selected tip and dirty count. This isolates the cost of copying saved status, **not** thread creation, Git I/O, painting, or key latency. Other small fields of `Repo` still get copied.

A separate History PTY exercise observed the dirty count through saves, staging, and a commit, then the new commit subject. The 24-save burst on the large ahead fixture still ran six sets of config/status/upstream checks on each build. Sharing the prior sample does not substitute it for a new Git read.

## Settings: Git processes per CLI operation

| Operation from a checkout subdirectory unless noted | Base | Changed |
|---|---:|---:|
| Global export | 2 | 0 |
| Preview a global import | 2 | 0 |
| Apply the global import | 2 | 0 |
| Undo the import | 2 | 0 |
| Preview a matching project | 5 | 4 |
| Preview from a different checkout, match via home-relative path | 8 | 7 |

Counts include the once-per-process `git --exec-path` lookup. The driver checks that preview leaves a real preferences file unchanged, apply changes only the chosen setting, undo restores the original bytes, and both project previews find the checkout by repository identity. The browser editor's tab anchoring and write transaction are not changed.

## Usage: one OpenCode sampler result with a WAL writer connected

| Database / workload | Physical seed messages | Base p50 / p95, ms | Changed p50 / p95, ms |
|---|---:|---:|---:|
| 1.19.6 archive, 8 seed sessions | 200,000 | 23.79 / 24.20 | 13.47 / 14.64 |
| Migrated 2.3.2 archive, 8 seed sessions | 400,000 | 23.11 / 24.19 | 12.81 / 13.93 |
| Migrated 2.3.2 archive, 1,000 seed sessions | 400,000 | 23.60 / 26.22 | 18.29 / 19.14 |
| Fresh 2.3.2, mostly history (control) | 50,000 | 13.39 / 14.11 | 12.89 / 13.93 |
| Fresh 2.3.2, dense day (control) | 50,000 | 99.38 / 129.41 | 98.36 / 99.73 |

Each entry covers 12 completed reads after one warm-up. A writer commits one more assistant step every simulated 20 seconds. The fixture verifies the resulting step count at each look. The archives start with 1,500 assistant steps from today; the dense control has 30,000. The migrated archive has two copies of its 200,000 logical messages but must count only one. The writer uses one additional session. Timings include the SQLite CLI processes and taking their result into Milo, not setup or rendering. The fixture has a longer deadline than production; none of these reads took one second. Base and candidate ran separately with warm caches, not on reserved cores. With 12 observations the p95 is the slowest sample, not a reliable tail estimate.

The improvement targets repeated archive scans, not the volume of today's JSON to aggregate. A full, binary-collated index starting with session ID and timestamp permits range seeks. Missing, session-only, partial and incompatible indexes retain the original scan. Fresh 2.x has no legacy table to optimize. No new database index, write, permission or provider call. A separate regression checks that a future-dated migrated row does not turn incomplete current totals into a number.

## Alerts: when a completed transaction releases its credential lock

The parallel relay fixtures exposed a spurious “connection change is in progress” after a previous operation returned. A separate local file test reproduced the kernel behavior without a relay: hold a credential lock, duplicate its descriptor, then drop the original. With last-close release a new acquisition still reports `WouldBlock` while the duplicate is open. Explicit unlock at the end of the transaction allows acquisition immediately; a second writer is still refused during the transaction itself.

`cargo test --locked --bin milo alerts::tests::credential_transaction_unlocks_before_its_last_descriptor_closes -- --exact` exercises this boundary via `forget` in a disposable, unconfigured profile. The duplicate models the interval between fork and exec; the test doesn't depend on scheduling a child in that interval. No retry, change of credentials, provider contact or worker restart is needed for the fix.

## Repeating the comparison

Build from the PR checkout. The two test-only files copied below give the base the same workload; no production reader or query is replaced. Use a synthetic home for the test binaries. Both the settings CLI driver and the PTY driver create their own isolated homes.

```sh
set -eu
lab=$(mktemp -d); mkdir -p "$lab/base" "$lab/home" "$lab/tmp"
git archive 716a4a9 | tar -x -C "$lab/base"
cp src/history/refresh.rs "$lab/base/src/history/refresh.rs"
cp src/usage/tests/profile.rs "$lab/base/src/usage/tests/profile.rs"
for src in "$lab/base" "$PWD"; do
  (cd "$src"; cargo build --release --locked
   cargo test --release --locked --bin milo --no-run --message-format=json |
     jq -r 'select(.reason=="compiler-artifact" and .profile.test and .target.name=="milo") | .executable' > target/backend-test-path)
done
run_fixture() {
  env -i HOME="$lab/home" TMPDIR="$lab/tmp" PATH=/usr/bin:/bin LC_ALL=C TZ=UTC \
    GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
    "$(cat "$1/target/backend-test-path")" "$2" --ignored --nocapture --test-threads=1
}
for src in "$lab/base" "$PWD" "$PWD" "$lab/base"; do run_fixture "$src" history_refresh_snapshot_profile; done
for src in "$lab/base" "$PWD"; do run_fixture "$src" usage_profile_active_sqlite; done
python3 -B tests/settings_transfer.py "$lab/base/target/release/milo"
python3 -B tests/settings_transfer.py --check target/release/milo
for b in "$lab/base/target/release/milo" target/release/milo; do
  python3 -B tests/backend_refresh.py "$b" --skip-discovery --pane history --fixture large --comparison ahead
done
```
