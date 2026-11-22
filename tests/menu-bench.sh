#!/bin/sh
# Times the menu app's update path with made-up frames: sh tests/menu-bench.sh [--check]
# Builds a separate, throwaway app under target/menu-bench and runs it once. It shows its own
# menu bar item and popover for about a minute; the installed app is neither stopped nor
# replaced, no reader is started and no usage is read. --check skips the timing and only
# checks that the panel is redrawn when a reset passes.
set -eu
case "${1-}:$#" in
    :0|--check:1) ;;
    *) echo 'Usage: sh tests/menu-bench.sh [--check]' >&2; exit 2 ;;
esac
mode=${1-}
[ "$(uname -s)" = Darwin ] || { echo 'The menu app requires macOS.' >&2; exit 1; }
repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
base=$repo/target/menu-bench
app=$base/MiloBench.app
rm -rf "$base"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
# The harness needs the app's private model and delegate, so it joins their file.
sed '/^@main$/d' "$repo/macos/MenuBar/App.swift" | cat - "$repo/tests/menu-bench.swift" >"$base/App.swift"
set --
for source in "$repo"/macos/MenuBar/*.swift; do
    case "$source" in */App.swift) ;; *) set -- "$@" "$source" ;; esac
done
swiftc -O -swift-version 5 -parse-as-library -target "$(uname -m)-apple-macos13.0" "$base/App.swift" "$@" -o "$app/Contents/MacOS/MiloBench"
cp "$repo"/src/usage/marks/*.png "$repo/site/src/images/milo-lockup.webp" "$app/Contents/Resources/"
sed -e 's/net\.usemilo\.usage\.local/net.usemilo.usage.bench/' -e 's/<string>Milo<\/string>/<string>MiloBench<\/string>/' \
    "$repo/macos/Info.plist" >"$app/Contents/Info.plist"
/usr/bin/plutil -insert CFBundleShortVersionString -string 0.0.0 "$app/Contents/Info.plist"
codesign --force --sign - "$app" 2>/dev/null
"$app/Contents/MacOS/MiloBench" $mode
