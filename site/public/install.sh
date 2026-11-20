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
        'To disconnect a remaining managed integration, reinstall Milo and run milo setup --remove.'
    exit 0
fi

if [ "$(uname -s)" != Darwin ] ||
    { [ "$(uname -m)" != arm64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" != 1 ]; }; then
    fail 'This release requires an Apple Silicon Mac. See https://github.com/mcc0x/milo for source installation.'
fi

version=v0.1.3
asset=milo-aarch64-apple-darwin
expected=935cc8d08b5d52a958862a64ffd1249c5688babac2c02d820a2176426e3ba04b
release=https://github.com/mcc0x/milo/releases/download/$version

# Stage on the same filesystem so a verified download can replace milo by rename.
mkdir -p "$bin_dir"
scratch=$(mktemp -d "${bin_dir%/}/.milo.XXXXXXXX")
trap 'rm -rf "$scratch"' 0
trap 'exit 1' HUP INT TERM
staged=$scratch/milo

curl --disable --proto '=https' --proto-redir '=https' -fsSL \
    -o "$staged" "$release/$asset" ||
    fail "Could not download $asset for Milo $version."
actual=$(shasum -a 256 < "$staged")
[ "${actual%% *}" = "$expected" ] || fail "SHA-256 checksum mismatch for $asset. Milo was not installed."

check_destination
if [ -f "$binary" ] && [ -x "$binary" ] && cmp -s "$staged" "$binary"; then
    printf 'milo is already installed at %s. Nothing to do.\n' "$binary"
else
    chmod 755 "$staged"
    # Directory form appends "milo" once without following a final symlink.
    /bin/mv -f "$staged" "${bin_dir%/}/"
    printf 'Installed milo %s at %s.\n' "$version" "$binary"
fi

active=$(command -v milo || :)
if [ -n "$active" ] && [ "$active" -ef "$binary" ]; then
    printf '%s\n' 'Run milo to get started, or milo setup --skip-onboarding for an unattended install.'
else
    if [ -n "$active" ]; then
        printf 'Your PATH selects %s instead of %s.\n' "$active" "$binary"
    else
        printf '%s is not on PATH.\n' "$bin_dir"
    fi
    printf "Run this copy: '"
    printf '%s' "$binary" | sed "s/'/'\\\\''/g"
    printf "'\n"
fi
