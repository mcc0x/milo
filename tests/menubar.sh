#!/bin/sh
# macOS, after sh macos/build-menu.sh. No Launch Services or login-item calls.
set -eu
repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
cli=${1:-"$repo/target/local-menu/milo"}
app=${2:-"$repo/target/local-menu/Milo.app"}
helper=Contents/MacOS/Milo
requirement='=identifier "net.usemilo.usage.local"'
codesign --verify --deep --strict -R "$requirement" "$app"
codesign --verify --strict "$app/Contents/Resources/Tools/milo"
root=$(mktemp -d); trap 'rm -rf "$root"' EXIT
export HOME="$root/home"
apps=$HOME/Applications
dest="$apps/Milo.app"
"$cli" menubar uninstall
test ! -e "$apps"
mkdir -p "$apps" "$root/other"
touch "$root/other/keep"
ln -s "$root/other" "$dest"
if "$cli" menubar uninstall; then exit 1; fi
test -L "$dest"
test -f "$dest/keep"
rm "$dest"

# Real code signing and verification, with a harmless helper executable. Override
# only the OS launch command in the test shell, not in the shipped installer.
cat >"$root/helper.c" <<'C'
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    const char *target = getenv("MILO_FIXTURE_STOP_DEST");
    if (!target) target = getenv("MILO_FIXTURE_DEST");
    const char *log_path = getenv("MILO_FIXTURE_LOG");
    if (argc != 3 || !target || strcmp(argv[2], target) || !log_path)
        return 2;
    FILE *log = fopen(log_path, "a");
    if (!log)
        return 2;
    fprintf(log, "%s\n", argv[1]);
    if (fclose(log))
        return 2;
    const char *failure = getenv("MILO_FIXTURE_FAILURE");
    return failure && !strcmp(failure, argv[1]);
}
C
cc -o "$root/helper" "$root/helper.c"
make_bundle() {
    mkdir -p "$1/Contents/MacOS" "$1/Contents/Resources"
    cp "$repo/macos/Info.plist" "$1/Contents/Info.plist"
    cp "$root/helper" "$1/$helper"
    printf '%s\n' "$2" >"$1/Contents/Resources/revision"
    codesign --force --sign - "$1"
}
previous="$root/previous/Milo.app"
current="$root/current/Milo.app"
make_bundle "$previous" previous
make_bundle "$current" current
archive=$root/menu.tar.gz
COPYFILE_DISABLE=1 /usr/bin/tar -czf "$archive" -C "$root/current" 'Milo.app'
sha=$(shasum -a 256 <"$current/$helper" | cut -d' ' -f1)
export MILO_FIXTURE_DEST="$dest" MILO_FIXTURE_LOG="$root/lifecycle" MILO_FIXTURE_CLI="$cli"

operation() {
    MILO_FIXTURE_FAILURE=${2-} /bin/sh -c '
        open() {
            [ "$#" = 4 ] && [ "$1" = "$MILO_FIXTURE_DEST" ] &&
                [ "$2" = --args ] && [ "$3" = --installer-path ] &&
                [ "$4" = "$MILO_FIXTURE_CLI" ] || return 2
            printf "open\n" >>"$MILO_FIXTURE_LOG"
            [ "${MILO_FIXTURE_FAILURE-}" != open ]
        }
        installer=$1; shift
        . "$installer"
    ' fixture "$repo/src/menubar.sh" "$1" "$apps" "$sha" "$cli" <"$archive"
}

if operation install open; then exit 1; fi
test ! -e "$dest"
operation install
codesign --verify --deep --strict -R "$requirement" "$dest"
identity=$(stat -f %i "$dest")
operation install
test "$identity" = "$(stat -f %i "$dest")"
printf 'tampered\n' >"$dest/Contents/Resources/revision"
if operation uninstall; then exit 1; fi
cp "$current/Contents/Resources/revision" "$dest/Contents/Resources/revision"
if operation uninstall --remove-installed; then exit 1; fi
test "$identity" = "$(stat -f %i "$dest")"
operation uninstall
test ! -e "$dest"
operation uninstall

cp -R "$previous" "$dest"
if operation uninstall; then exit 1; fi
if operation install --stop-installed; then exit 1; fi
cmp "$previous/Contents/Resources/revision" "$dest/Contents/Resources/revision"
if operation install open; then exit 1; fi
cmp "$previous/Contents/Resources/revision" "$dest/Contents/Resources/revision"
operation install
cmp "$current/Contents/Resources/revision" "$dest/Contents/Resources/revision"
operation uninstall
test ! -e "$dest"
test -z "$(find "$apps" -mindepth 1 -maxdepth 1 -name '.milo-menu.*' -print)"
test -f "$root/other/keep"
diff -u - "$MILO_FIXTURE_LOG" <<'LOG'
open
open
open
--remove-installed
--remove-installed
--stop-installed
--stop-installed
open
--stop-installed
open
--remove-installed
LOG

# Roll back to the original bundle name, then migrate without leaving two apps.
legacy="$apps/Milo Usage.app"
cp -R "$previous" "$legacy"
export MILO_FIXTURE_STOP_DEST="$legacy"
if operation install open; then exit 1; fi
test ! -e "$dest"
cmp "$previous/Contents/Resources/revision" "$legacy/Contents/Resources/revision"
operation install
test ! -e "$legacy"
cmp "$current/Contents/Resources/revision" "$dest/Contents/Resources/revision"
