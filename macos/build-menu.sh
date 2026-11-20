#!/bin/sh
# Build a self-contained, ad-hoc signed macOS app. Does not install or launch it.
set -eu
[ "$(uname -s)" = Darwin ] || { echo 'The menu app requires macOS.' >&2; exit 1; }
repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
arch=$(uname -m)
case "$arch" in
    arm64) rust_target=aarch64-apple-darwin ;;
    x86_64) rust_target=x86_64-apple-darwin ;;
    *) echo "Unsupported macOS architecture: $arch" >&2; exit 1 ;;
esac
export MACOSX_DEPLOYMENT_TARGET=13.0
base=$repo/target/local-menu
mkdir -p "$base"
tmp=$(mktemp -d "$base/build.XXXXXXXX")
trap 'rm -rf "$tmp"' EXIT
app="$tmp/Milo Usage.app"
resources=$app/Contents/Resources
mkdir -p "$app/Contents/MacOS" "$resources/Tools" "$tmp/Milo.iconset"
(cd "$repo" && cargo build --locked --release --target "$rust_target" --target-dir "$repo/target")
install -m 755 "$repo/target/$rust_target/release/milo" "$resources/Tools/milo"
swiftc -O -swift-version 5 -parse-as-library -target "$arch-apple-macos$MACOSX_DEPLOYMENT_TARGET" "$repo"/macos/MenuBar/*.swift -o "$app/Contents/MacOS/MiloUsage"
cp "$repo"/src/usage/marks/claude.png "$repo"/src/usage/marks/codex.png "$repo/site/src/images/milo-lockup.webp" "$resources/"
for size in 16 32 128 256 512; do
    for scale in 1 2; do
        pixels=$((size * scale)); suffix=''; [ "$scale" = 1 ] || suffix=@2x
        sips -z "$pixels" "$pixels" "$repo/docs/img/milo-icon.png" --out "$tmp/Milo.iconset/icon_${size}x${size}${suffix}.png" >/dev/null
    done
done
iconutil -c icns "$tmp/Milo.iconset" -o "$resources/Milo.icns"
cp "$repo/macos/Info.plist" "$app/Contents/Info.plist"
version=$("$resources/Tools/milo" --version)
/usr/bin/plutil -insert CFBundleShortVersionString -string "${version#milo }" "$app/Contents/Info.plist"
# Finder and login items do not inherit a shell's exported provider paths. Keep
# only the supported paths in this local bundle, not tokens or logs. Never ship it.
python3 - "$resources/ProfilePaths.plist" <<'PY'
import os, plistlib, sys
keys = ('CODEX_HOME','CLAUDE_CONFIG_DIR','PI_CODING_AGENT_DIR','OPENCODE_DB','XDG_CONFIG_HOME','XDG_STATE_HOME','XDG_DATA_HOME')
values = {key:os.environ[key] for key in keys if os.environ.get(key)}
for key in ('CODEX_HOME','CLAUDE_CONFIG_DIR','PI_CODING_AGENT_DIR'):
    if key in values:
        values[key] = os.path.abspath(values[key])
with open(sys.argv[1], 'wb') as stream:
    plistlib.dump(values, stream)
PY
codesign --force --sign - "$resources/Tools/milo"
codesign --force --sign - "$app"
codesign --verify --strict "$resources/Tools/milo"
codesign --verify --deep --strict -R '=identifier "net.usemilo.usage.local"' "$app"
# Produce a matching offline installer; the app's own reader stays lean.
COPYFILE_DISABLE=1 /usr/bin/tar -czf "$tmp/menu.tar.gz" -C "$tmp" 'Milo Usage.app'
(cd "$repo" && MILO_MENU_ARCHIVE="$tmp/menu.tar.gz" MILO_MENU_HELPER_SHA="$(shasum -a 256 "$app/Contents/MacOS/MiloUsage" | cut -d' ' -f1)" cargo rustc --locked --release --target "$rust_target" --target-dir "$base/cli-build" --bin milo -- --cfg milo_menu_payload)
install -m 755 "$base/cli-build/$rust_target/release/milo" "$base/milo"
# Replace only our generated bundle inside the ignored target directory.
rm -rf "$base/Milo Usage.app"
mv "$app" "$base/Milo Usage.app"
printf '%s\n' "$base/Milo Usage.app"
