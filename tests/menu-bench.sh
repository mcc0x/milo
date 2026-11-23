#!/bin/sh
# Times the menu app's update path with made-up frames, or checks its panel on a Mac.
# Builds a separate, throwaway app under target/menu-bench and runs it once. It shows its own
# menu bar item and popover for about a minute; the installed app is neither stopped nor
# replaced, no reader is started and no usage is read. --check skips the timing and only
# checks that the panel is redrawn when a reset passes. --check-popover exercises
# production dismissal and briefly puts a disposable test app in front.
set -eu
case "${1-}:$#" in
    :0|--check:1|--check-popover:1) ;;
    *) echo 'Usage: sh tests/menu-bench.sh [--check | --check-popover]' >&2; exit 2 ;;
esac
mode=${1-}
[ "$(uname -s)" = Darwin ] || { echo 'The menu app requires macOS.' >&2; exit 1; }
repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
root=$repo/target/menu-bench
mkdir -p "$root"
base=$(mktemp -d "$root/run.XXXXXXXX")
trap 'rm -rf "$base"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
app=$base/MiloBench.app

export HOME="$base/home" CFFIXED_USER_HOME="$base/home"
export XDG_CONFIG_HOME="$base/config" XDG_DATA_HOME="$base/data" XDG_STATE_HOME="$base/state"
export XDG_CACHE_HOME="$base/cache" XDG_RUNTIME_DIR="$base/runtime"
export XDG_CONFIG_DIRS="$XDG_CONFIG_HOME" XDG_DATA_DIRS="$XDG_DATA_HOME"
export TMPDIR="$base/tmp" TMP="$base/tmp" TEMP="$base/tmp"
mkdir -p "$HOME/Library/Preferences" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" \
    "$XDG_CACHE_HOME" "$XDG_RUNTIME_DIR" "$TMPDIR" "$app/Contents/MacOS" "$app/Contents/Resources"
chmod 700 "$XDG_RUNTIME_DIR"
# The harness needs the app's private model and delegate, so it joins their file.
sed '/^@main$/d' "$repo/macos/MenuBar/App.swift" | cat - "$repo/tests/menu-bench.swift" >"$base/App.swift"
set --
for source in "$repo"/macos/MenuBar/*.swift; do
    case "$source" in */App.swift) ;; *) set -- "$@" "$source" ;; esac
done
swiftc -O -swift-version 5 -parse-as-library -target "$(uname -m)-apple-macos13.0" "$base/App.swift" "$@" -o "$app/Contents/MacOS/MiloBench"
cp "$repo"/src/usage/marks/*.png "$repo/site/src/images/milo-lockup.webp" "$app/Contents/Resources/"
# Unique identities also isolate per-app defaults stored by system services.
id=net.usemilo.usage.bench.${base##*.}
make_plist() {
    sed -e "s/net\.usemilo\.usage\.local/$2/" -e "s/<string>Milo<\/string>/<string>$3<\/string>/" \
        "$repo/macos/Info.plist" >"$1"
    /usr/bin/plutil -insert CFBundleShortVersionString -string 0.0.0 "$1"
}
make_plist "$app/Contents/Info.plist" "$id" MiloBench
if [ "$mode" = --check-popover ]; then
    focus=$base/MiloBenchFocus.app
    mkdir -p "$focus/Contents/MacOS"
    cp "$app/Contents/MacOS/MiloBench" "$focus/Contents/MacOS/MiloBenchFocus"
    make_plist "$focus/Contents/Info.plist" "$id.focus" MiloBenchFocus
    /usr/bin/plutil -replace LSUIElement -bool NO "$focus/Contents/Info.plist"
    codesign --force --sign - "$focus" 2>/dev/null
fi
codesign --force --sign - "$app" 2>/dev/null
if [ "$mode" = --check-popover ]; then
    # Launch Services grants the fixture an actual foreground session. Starting
    # an accessory executable from a background shell doesn't confer activation.
    touch "$base/out" "$base/err"
    /usr/bin/open -n -W -a "$app" --stdout "$base/out" --stderr "$base/err" \
        --env "HOME=$HOME" --env "CFFIXED_USER_HOME=$HOME" --env "TMPDIR=$TMPDIR" \
        --env "XDG_CONFIG_HOME=$XDG_CONFIG_HOME" --env "XDG_STATE_HOME=$XDG_STATE_HOME" \
        --env "XDG_DATA_HOME=$XDG_DATA_HOME" --args "$mode"
    cat "$base/out" "$base/err"
    # open -W reports launch errors, not the app's own exit status.
    test -f "$TMPDIR/popover-passed"
elif [ -n "$mode" ]; then
    "$app/Contents/MacOS/MiloBench" "$mode"
else
    "$app/Contents/MacOS/MiloBench"
fi
