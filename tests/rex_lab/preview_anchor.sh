# The lab prepends its socket-scoped Rex command and temporary directory.
# Keep this process as the anchor so Rex's CLI has the real parent it claims.
# The native window ID follows the Rex scope, then the initial visibility mode.
[ "$#" -eq 7 ] || exit 1
case "$7" in
    auto|confirm) ;;
    *) exit 1 ;;
esac
printf '%s\n' "$$" "$@" > "$ROOT/preview-arguments"
request="$ROOT/request-$REX_BLOCK"
watcher=
while :; do
    if [ -z "$watcher" ] && [ -f "$request.watch.json" ]; then
        payload=$(cat "$request.watch.json")
        if [ "$7" = confirm ]; then
            payload="${payload%?},\"confirm\":true}"
        fi
        "$REX" --timeout 3s do -s "$REX_SESSION" -b "$REX_BLOCK" "$SCRIPT" --args "$payload" --fail-fast \
            < /dev/null > "$request.watch.log" 2> "$request.watch.errors" &
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
