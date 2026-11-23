#!/bin/sh
# Check the screen tmux actually rendered, not the escape sequences Milo wrote.
set -eu
binary=$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")
server=$(command -v tmux)
root=$(mktemp -d /tmp/milo-tmux.XXXXXX)
socket=$root/tmux.sock
mux() {
    env -i HOME="$root" PATH=/usr/bin:/bin SHELL=/bin/sh TERM=xterm-256color \
        MILO="$binary" "$server" -f /dev/null -S "$socket" "$@"
}
cleanup() {
    mux kill-server 2>/dev/null || :
    rm -rf "$root"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
mkdir "$root/project"
printf 'first line of the fixture\n' > "$root/project/notes.txt"
: > "$root/project/second.txt"
printf 'saved: first line of the fixture\n' > "$root/expected"
# A detached server with one test-owned pane. No user tmux server or login shell.
mux new-session -d -s lab -x 80 -y 24 -c "$root/project" \
    'if command -v rex >/dev/null; then exit 1; fi; "$MILO" outline && : > "$HOME/exited"'
see() {
    n=0
    while [ "$n" -lt 200 ]; do
        screen=$(mux capture-pane -t lab -p) || return 1
        if [ "$#" -eq 2 ]; then
            screen=$(printf '%s\n' "$screen" | sed -n "${2}p")
        fi
        case "$screen" in *"$1"*) return 0 ;; esac
        n=$((n + 1)); sleep 0.05
    done
    printf 'Missing %s on tmux screen:\n%s\n' "$1" "$screen" >&2
    return 1
}
see notes.txt
mux send-keys -t lab Enter
see 'first line of the fixture'
mux resize-window -t lab -x 36 -y 12
see 'e edit' 12
mux send-keys -t lab e
see '^S save'
mux send-keys -t lab -l 'saved: '
mux send-keys -t lab C-s
n=0
while ! cmp -s "$root/expected" "$root/project/notes.txt"; do
    n=$((n + 1)); [ "$n" -lt 200 ] || { echo 'Reader did not save the edit' >&2; exit 1; }; sleep 0.05
done
mux resize-window -t lab -x 80 -y 24
see '^S save' 24
mux send-keys -t lab Escape
see 'e edit'
mux send-keys -t lab q
see second.txt
mux send-keys -t lab q
n=0
while [ ! -f "$root/exited" ]; do
    n=$((n + 1)); [ "$n" -lt 200 ] || { echo 'Outline did not exit successfully' >&2; exit 1; }; sleep 0.05
done
printf 'Standalone outline → reader → save → outline passed (%s)\n' "$("$server" -V)"
