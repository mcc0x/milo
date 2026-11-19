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

if [ -L "$binary" ] || { [ -e "$binary" ] && [ ! -f "$binary" ]; }; then
    fail "Not changing $binary: it is not a regular file."
fi

if [ "${1-}" = --uninstall ]; then
    if [ -f "$binary" ]; then
        rm -f "$binary"
        printf 'Removed %s.\n' "$binary"
    else
        printf 'No milo binary at %s. Nothing to do.\n' "$binary"
    fi
    printf '%s\n' 'Rex and Claude Code settings were not changed.' \
        'To disconnect a remaining managed integration, run cargo run -- setup --remove in this checkout.'
    exit 0
fi

command -v cargo >/dev/null 2>&1 || fail 'Cargo is required. Get Rust 1.91 or later at https://rustup.rs/ and run this installer again.'
command -v git >/dev/null 2>&1 || fail 'Git is required. Install Git and run this installer again.'

if ! repo=$(git rev-parse --show-toplevel 2>/dev/null) ||
    [ ! -f "$repo/src/main.rs" ] ||
    ! grep -q '^name = "milo"$' "$repo/Cargo.toml" 2>/dev/null; then
    tmp_base=${TMPDIR:-/tmp}
    case "$tmp_base" in
        /*) ;;
        *) tmp_base=$PWD/$tmp_base ;;
    esac
    scratch=$(mktemp -d "${tmp_base%/}/milo.XXXXXXXX")
    trap 'rm -rf "$scratch"' 0
    trap 'exit 1' HUP INT TERM
    repo=$scratch/source
    if ! git clone --depth 1 https://github.com/mcc0x/milo "$repo"; then
        fail 'Could not clone milo from GitHub.' \
            'If you already have a clone, change to its directory and run: sh install.sh'
    fi
fi

(cd "$repo" && cargo build --release --locked --target-dir target)
built=$repo/target/release/milo
if [ -f "$binary" ] && [ -x "$binary" ] && cmp -s "$built" "$binary"; then
    printf 'milo is already installed at %s. Nothing to do.\n' "$binary"
else
    mkdir -p "$bin_dir"
    install -m 755 "$built" "$binary"
    printf 'Installed milo at %s.\n' "$binary"
fi

on_path() {
    remaining=${PATH-}:
    while [ -n "$remaining" ]; do
        entry=${remaining%%:*}
        if [ "${entry:-.}" -ef "$bin_dir" ]; then
            return 0
        fi
        remaining=${remaining#*:}
    done
    return 1
}

if ! on_path; then
    printf '%s is not on PATH. Add it to PATH before running milo.\n' "$bin_dir"
fi
printf '%s\n' 'Run milo to get started, or milo setup --skip-onboarding for an unattended install.'
