#!/bin/sh
# Time this checkout's reader beside the reader of another revision.
# Usage: sh tests/reader-compare.sh [REVISION] > report.tsv
#
# The revision is exported and built in release mode in a temporary folder, so
# no branch or worktree changes; this checkout only gets its usual release
# build. Cargo's output goes to stderr and the report to stdout, also when
# MILO_BENCH_MAX_RATIO fails the run. That and MILO_BENCH_RUNS are passed on to
# tests/reader_bench.rs.
set -eu
unset CDPATH GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR
root=$(cd -P "$(dirname "$0")/.." && pwd -P)
revision=$(git -C "$root" rev-parse --verify --quiet "${1:-HEAD}^{commit}") || {
    echo "reader-compare: no revision ${1:-HEAD}" >&2
    exit 2
}
tmp_base=$(cd -P "${TMPDIR:-/tmp}" && pwd -P)
work=$(mktemp -d "$tmp_base/milo-reader-compare.XXXXXXXX")
trap 'rm -rf "$work"' 0
trap 'exit 1' HUP INT TERM

mkdir "$work/source"
git -C "$root" archive "$revision" | tar -xf - -C "$work/source"
cargo build --release --locked --manifest-path "$work/source/Cargo.toml" --target-dir "$work/target" >&2

cd "$root"
status=0
MILO_BENCH_BASE=$work/target/release/milo MILO_BENCH_REPORT=$work/report \
    cargo test --release --locked --test reader_bench -- --ignored >&2 || status=$?
[ -f "$work/report" ] || exit "$status"

processor=$(sysctl -n machdep.cpu.brand_string 2>/dev/null ||
    awk -F': ' '/^model name/ { print $2; exit }' /proc/cpuinfo 2>/dev/null) || processor=
changes=
# Only what the timed build and this test are made from counts.
[ -z "$(git status --porcelain -- src Cargo.toml Cargo.lock tests/reader_bench.rs)" ] ||
    changes=' with uncommitted changes'
printf '# base revision: %s\n' "$revision"
printf '# timed checkout: %s%s\n' "$(git rev-parse HEAD)" "$changes"
printf '# %s; %s; %s\n' "${processor:-unknown processor}" "$(uname -srm)" "$(rustc --version)"
cat "$work/report"
exit "$status"
