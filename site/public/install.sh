#!/bin/sh
set -eu

fail() {
    printf '%s\n' "$*" >&2
    exit 1
}

case "$#:${1-}" in
    0:|1:--uninstall) ;;
    *) fail 'Usage: sh install.sh [--uninstall]' ;;
esac

bin_dir=${MILO_BIN_DIR:-${HOME:?Set HOME or MILO_BIN_DIR first.}/.local/bin}
case "$bin_dir" in
    /*) ;;
    *) bin_dir=$PWD/$bin_dir ;;
esac
binary=${bin_dir%/}/milo

check_destination() {
    if [ -L "$binary" ] || { [ -e "$binary" ] && [ ! -f "$binary" ]; }; then
        fail "Not changing $binary: it is not a regular file."
    fi
}

check_destination

if [ "${1-}" = --uninstall ]; then
    if [ -f "$binary" ]; then
        rm -f "$binary"
        printf 'Removed %s.\n' "$binary"
    else
        printf 'No milo binary at %s. Nothing to do.\n' "$binary"
    fi
    printf '%s\n' 'Rex and Claude Code settings were not changed.' \
        'To remove your Rex layout, run milo setup inside Rex while milo is installed.' \
        'Turn off Outline, History, Usage and Arrange new shell tabs, then press s.' \
        'To remove milo from the Claude Code status line, turn off Show Claude Code limits and press s.'
    exit 0
fi

if [ "$(uname -s)" != Darwin ] ||
    { [ "$(uname -m)" != arm64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" != 1 ]; }; then
    fail 'This release requires an Apple Silicon Mac. See https://github.com/mcc0x/milo for source installation.'
fi

version=v0.1.0
asset=milo-aarch64-apple-darwin
expected=e469ed3d206a8d24d458c2cbbdadf4338ec0277e0b37a18e1906cf14413b7d04
release=https://github.com/mcc0x/milo/releases/download/$version

# Stage on the same filesystem so a verified download can replace milo by rename.
mkdir -p "$bin_dir"
scratch=$(mktemp -d "${bin_dir%/}/.milo.XXXXXXXX")
trap 'rm -rf "$scratch"' 0
trap 'exit 1' HUP INT TERM

curl --disable --proto '=https' --proto-redir '=https' -fsSL \
    -o "$scratch/$asset" "$release/$asset" ||
    fail "Could not download $asset for Milo $version."
actual=$(shasum -a 256 < "$scratch/$asset")
[ "${actual%% *}" = "$expected" ] || fail "SHA-256 checksum mismatch for $asset. Milo was not installed."

check_destination
if [ -f "$binary" ] && [ -x "$binary" ] && cmp -s "$scratch/$asset" "$binary"; then
    printf 'milo is already installed at %s. Nothing to do.\n' "$binary"
else
    chmod 755 "$scratch/$asset"
    /bin/mv -fh "$scratch/$asset" "$binary"
    printf 'Installed milo %s at %s.\n' "$version" "$binary"
fi

active=$(command -v milo || :)
if [ -n "$active" ] && [ "$active" -ef "$binary" ]; then
    printf '%s\n' 'Run milo setup inside Rex to choose your panes, then press s to save.'
else
    if [ -n "$active" ]; then
        printf 'Your PATH selects %s instead of %s.\n' "$active" "$binary"
    else
        printf '%s is not on PATH.\n' "$bin_dir"
    fi
    printf "Run this copy: '"
    printf '%s' "$binary" | sed "s/'/'\\\\''/g"
    printf "' setup\n"
fi
