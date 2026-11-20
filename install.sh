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

# Use a checkout only when this script belongs to it, not because the caller is in one.
checkout_for_script() {
    case $- in
        *s*) return 1 ;;
    esac
    case "$0" in
        /*) script=$0 ;;
        *) script=$PWD/$0 ;;
    esac
    [ -f "$script" ] || return 1
    while [ -L "$script" ]; do
        link=$(readlink "$script") || return 1
        case "$link" in
            /*) script=$link ;;
            *) script=${script%/*}/$link ;;
        esac
    done
    [ "${script##*/}" = install.sh ] || return 1
    dir=$(cd -P "${script%/*}/" && pwd -P) || return 1
    top=$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null) || return 1
    # Compare the directories, not their spelling: the shell keeps the typed case, Git the stored one.
    [ "$dir" -ef "$top" ] &&
        [ -f "$dir/src/main.rs" ] &&
        grep -q '^name = "milo"$' "$dir/Cargo.toml" 2>/dev/null &&
        git -C "$dir" ls-files --error-unmatch -- install.sh >/dev/null 2>&1 || return 1
    printf '%s\n' "$dir"
}

if ! repo=$(checkout_for_script); then
    tmp_base=${TMPDIR:-/tmp}
    case "$tmp_base" in
        /*) ;;
        *) tmp_base=$PWD/$tmp_base ;;
    esac
    scratch=$(mktemp -d "${tmp_base%/}/milo.XXXXXXXX")
    trap 'rm -rf "$scratch"' 0
    trap 'exit 1' HUP INT TERM
    cargo_dir=/
    repo=$scratch/source
    if ! git clone --depth 1 https://github.com/mcc0x/milo "$repo"; then
        fail 'Could not clone milo from GitHub.' \
            'If you already have a clone, change to its directory and run: sh install.sh'
    fi
else
    cargo_dir=$repo
fi

# Cargo reads configuration from its working directory, not the manifest directory.
# Keep checkout configuration, but never search the downloaded source or TMPDIR.
host=$(cd "$cargo_dir" && cargo -vV | sed -n 's/^host: //p')
(cd "$cargo_dir" && cargo build --release --locked --manifest-path "$repo/Cargo.toml" \
    --target-dir "$repo/target" --target "$host")
built=$repo/target/$host/release/milo
if [ -f "$binary" ] && [ -x "$binary" ] && cmp -s "$built" "$binary"; then
    printf 'milo is already installed at %s. Nothing to do.\n' "$binary"
else
    mkdir -p "$bin_dir"
    install -m 755 "$built" "$binary"
    printf 'Installed milo at %s.\n' "$binary"
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
exit 0
