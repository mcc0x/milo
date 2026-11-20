#!/bin/sh
# macOS GUI session, after sh macos/build-menu.sh. Only touches a temporary home.
set -eu
cli=${1:-target/local-menu/milo}
root=$(mktemp -d); trap 'rm -rf "$root"' EXIT
export HOME="$root/home"
apps=$HOME/Applications
dest="$apps/Milo Usage.app"
mkdir -p "$apps" "$root/other"
touch "$root/other/keep"
ln -s "$root/other" "$dest"
if "$cli" menubar uninstall; then exit 1; fi
test -f "$dest/keep"; rm "$dest"
"$cli" menubar install
codesign --verify --strict "$dest"
identity=$(stat -f %i "$dest")
"$cli" menubar install
test "$identity" = "$(stat -f %i "$dest")"
"$cli" menubar uninstall
test ! -e "$dest"
"$cli" menubar uninstall
test -f "$root/other/keep"
