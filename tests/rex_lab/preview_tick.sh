# This is the process Rex starts in the preview PTY. The production watcher
# verifies its direct caller PID against the terminal's actual child PID.
set -eu
root=$1 script=$2 owner=$3 owner_pid=$4 main=$5 tab=$6 url=$7
watch_pipe="$root/native-states"
anchor_pipe="$root/native-input"
/usr/bin/mkfifo "$watch_pipe" "$anchor_pipe"
watch_args=$(printf '{"mode":"watch","server":"%s","uid":%s,"pid":%s,"session":"%s","block":"%s","main":"%s","window":"%s","owner":"%s","owner_pid":%s,"native_window_id":4294967295,"confirm":true}' \
    "$REX_SERVER" "$UID" "$$" "$REX_SESSION" "$REX_BLOCK" "$main" "$tab" "$owner" "$owner_pid")
"$root/bin/rex" --timeout 3s do -s "$REX_SESSION" -b "$REX_BLOCK" "$script" --args "$watch_args" --fail-fast \
    </dev/null >"$watch_pipe" 2>"$root/native-watch.errors" &
# Only signal these shell jobs, never PIDs that could have been reused.
trap 'kill %1 %2 2>/dev/null || :; wait || :' EXIT
# tee records the real visibility pipe, but neither changes its records nor
# handles terminal input. Anchor opens /dev/tty in this same Rex process group.
/usr/bin/tee "$root/native-watch.log" <"$watch_pipe" >"$anchor_pipe" &
status=0
"$root/preview-rex-tick" "$owner_pid" "$url" "$root" <"$anchor_pipe" || status=$?
printf '%s\n' "$status" >"$root/native-exit"
exit "$status"
