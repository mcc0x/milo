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

resolve_destination() {
    if [ -d "$bin_dir" ]; then
        binary=$(cd -P "$bin_dir" && printf '%s/milo' "${PWD%/}")
        bin_dir=${binary%/*}
        bin_dir=${bin_dir:-/}
    else
        binary=${bin_dir%/}/milo
    fi
    receipt=${binary%/*}/.milo-install-receipt
}

check_destination() {
    if [ -L "$binary" ] || { [ -e "$binary" ] && [ ! -f "$binary" ]; }; then
        fail "Not changing $binary: it is not a regular file."
    fi
}

sha256() {
    if command -v shasum >/dev/null 2>&1; then
        sum=$(shasum -a 256 < "$1") || return 1
    else
        sum=$(sha256sum < "$1") || return 1
    fi
    printf '%s\n' "${sum%% *}"
}

resolve_destination
check_destination

if [ "${1-}" = --uninstall ]; then
    if [ -f "$binary" ]; then
        if [ -L "$receipt" ] || [ ! -f "$receipt" ] || ! digest=$(sha256 "$binary") ||
            ! printf '%s\n%s\n' "$digest" "$binary" | cmp -s - "$receipt"; then
            fail "Not removing $binary: no matching installer receipt. For older installs, verify the binary before removing it manually."
        fi
        rm -f "$binary"
        rm -f "$receipt"
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
# Ventura is Darwin 22. Check before downloading or replacing an older binary.
kernel=$(uname -r) || fail 'Could not determine the macOS version.'
major=${kernel%%.*}
case "$major" in ''|*[!0-9]*) fail 'Could not determine the macOS version.' ;; esac
[ "$major" -ge 22 ] || fail 'This release requires macOS 13 or later.'

version=v0.1.9
asset=milo-aarch64-apple-darwin
expected=7f2f0349d33fa25765c9bce92d0e6fdf1b1d01ffa8e8dbc74fcfa4f7daaf2e2d
release=https://github.com/mcc0x/milo/releases/download/$version

# Stage on the same filesystem so a verified download can replace milo by rename.
if [ ! -d "$bin_dir" ]; then
    mkdir -p "$bin_dir"
    resolve_destination
fi
scratch=$(mktemp -d "${bin_dir%/}/.milo.XXXXXXXX")
trap 'rm -rf "$scratch"' 0
trap 'exit 1' HUP INT TERM
staged=$scratch/milo

curl --disable --proto '=https' --proto-redir '=https' -fsSL \
    -o "$staged" "$release/$asset" ||
    fail "Could not download $asset for Milo $version."
actual=$(sha256 "$staged")
[ "$actual" = "$expected" ] || fail "SHA-256 checksum mismatch for $asset. Milo was not installed."
chmod 755 "$staged"

staged_receipt=$scratch/${receipt##*/}
if [ -L "$receipt" ] || { [ -e "$receipt" ] && [ ! -f "$receipt" ]; }; then
    fail "Not changing $receipt: it is not a regular file."
fi
(umask 077; printf '%s\n%s\n' "$actual" "$binary" > "$staged_receipt")

check_destination
# Always publish the staged inode: matching bytes say nothing about permissions or links.
# The directory form replaces the entry without following a final symlink.
/bin/mv -f "$staged" "${bin_dir%/}/"
# Publish after the binary so an interruption cannot authorize different bytes.
/bin/mv -f "$staged_receipt" "${bin_dir%/}/"
printf 'Installed milo %s at %s.\n' "$version" "$binary"

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
exit 0
