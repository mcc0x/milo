# The lab prepends its socket-scoped Rex command and temporary directory.
# Keep this process as the anchor so Rex's CLI has the real parent it claims.
# The startup socket name precedes the Rex scope and native window ID.
[ "$#" -eq 6 ] || exit 1
printf '%s\n' "$$" "$@" > "$ROOT/preview-arguments"
request="$ROOT/request-$REX_BLOCK"
watcher=
while :; do
    if [ -z "$watcher" ] && [ -f "$request.watch.json" ]; then
        payload=$(cat "$request.watch.json")
        "$REX" --timeout 3s do -s "$REX_SESSION" -b "$REX_BLOCK" "$SCRIPT" --args "$payload" --fail-fast \
            < /dev/null > "$request.watch.log" 2> "$request.watch.errors" 3> "$request.watch.ready" &
        watcher=$!
        printf '%s\n' "$watcher" > "$request.watch.pid"
        rm "$request.watch.json"
    fi
    if [ -n "$watcher" ] && ! kill -0 "$watcher" 2>/dev/null; then
        wait "$watcher"
        printf '%s\n' "$?" > "$request.watch.exit"
        watcher=
    fi
    if [ -f "$request" ]; then
        . "$ROOT/preview-request" "$request"
        rm "$request"
    fi
    /bin/sleep 0.03
done
