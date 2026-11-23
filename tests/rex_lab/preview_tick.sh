# This is the process Rex starts in the preview PTY. The production watcher
# verifies its direct caller PID against the terminal's actual child PID.
set -eu
root=$1 script=$2 owner=$3 owner_pid=$4 main=$5 tab=$6 url=$7 native_window_id=$8
if [ -n "${9-}" ]; then
    export MILO_REX_TICK_PERF_FILE="$root/native-perf.jsonl" MILO_REX_TICK_PERF_PAGE="$9"
fi
watch_pipe="$root/native-states"
anchor_pipe="$root/native-input"
ready_pipe="$root/native-ready"
/usr/bin/mkfifo "$watch_pipe" "$anchor_pipe" "$ready_pipe"
watch_args=$(printf '{"mode":"watch","server":"%s","uid":%s,"pid":%s,"session":"%s","block":"%s","main":"%s","window":"%s","owner":"%s","owner_pid":%s,"native_window_id":%s}' \
    "$REX_SERVER" "$UID" "$$" "$REX_SESSION" "$REX_BLOCK" "$main" "$tab" "$owner" "$owner_pid" "$native_window_id")
(
    IFS= read -r -n 1 receipt <"$ready_pipe" && [ "$receipt" = R ] || exit 1
    exec "$root/bin/rex" --timeout 3s do -s "$REX_SESSION" -b "$REX_BLOCK" "$script" --args "$watch_args" --fail-fast
) </dev/null >"$watch_pipe" 2>"$root/native-watch.errors" &
if [ -n "${9-}" ]; then
    printf '%s\n' "$!" >"$root/native-watch.pid"
fi
# Only signal these shell jobs, never PIDs that could have been reused.
reap_workers() { kill %1 %2 2>/dev/null || :; wait || :; }
trap reap_workers EXIT
# tee records the production visibility pipe without changing its records.
# The native child still shares this real Rex process group's controlling PTY.
/usr/bin/tee "$root/native-watch.log" <"$watch_pipe" >"$anchor_pipe" &
status=0
"$root/preview-rex-tick" "$owner_pid" "$url" "$root" "$native_window_id" <"$anchor_pipe" >"$ready_pipe" || status=$?
reap_workers
trap - EXIT
printf '%s\n' "$status" >"$root/native-exit"
exit "$status"
