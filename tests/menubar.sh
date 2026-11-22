#!/bin/sh
# macOS, after sh macos/build-menu.sh. No Launch Services or login-item calls.
set -eu
repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
cli=${1:-"$repo/target/local-menu/milo"}
app=${2:-"$repo/target/local-menu/Milo.app"}
case "$app" in /*) ;; *) app=$PWD/$app ;; esac
helper=Contents/MacOS/Milo
requirement='=identifier "net.usemilo.usage.local"'
codesign --verify --deep --strict -R "$requirement" "$app"
codesign --verify --strict "$app/Contents/Resources/Tools/milo"
test ! -e "$app/Contents/Resources/ProfilePaths.plist"
root=$(mktemp -d); trap 'rm -rf "$root"' EXIT
swiftc -swift-version 5 -parse-as-library "$repo/macos/MenuBar/BrowserHandoff.swift" "$repo/tests/menu-handoff.swift" -o "$root/check-handoff"
"$root/check-handoff"
# Build the same menu model and SwiftUI surface as the app, without its @main entry.
set --
for source in "$repo"/macos/MenuBar/*.swift; do
    case "$source" in */App.swift) ;; *) set -- "$@" "$source" ;; esac
done
swiftc -swift-version 5 -parse-as-library -target "$(uname -m)-apple-macos13.0" "$@" "$repo/tests/menu-badge.swift" -o "$root/check-badge"
"$root/check-badge"
swiftc -swift-version 5 -parse-as-library -target "$(uname -m)-apple-macos13.0" "$@" "$repo/tests/menu-alerts.swift" -o "$root/check-alerts"
"$root/check-alerts"
swiftc -swift-version 5 -parse-as-library -target "$(uname -m)-apple-macos13.0" "$repo/macos/MenuBar/SystemResources.swift" "$repo/tests/menu-resources.swift" -o "$root/check-resources"
"$root/check-resources"
export HOME="$root/home"
apps=$HOME/Applications
dest="$apps/Milo.app"
profile=$apps/.milo-menu-profile.plist
"$cli" menubar uninstall
test ! -e "$apps"
mkdir -p "$apps" "$root/other"
touch "$root/other/keep"
ln -s "$root/other" "$dest"
if "$cli" menubar uninstall; then exit 1; fi
test -L "$dest"
test -f "$dest/keep"
rm "$dest"

# Real signing and profile encoding with a harmless stand-in for quit, removal
# and Launch Services. No fixture touches the real user's home or login items.
cat >"$root/helper.c" <<'C'
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    const char *writer = getenv("MILO_FIXTURE_PROFILE_WRITER");
    if (argc == 3 && writer && !strcmp(argv[1], "--install-profile")) {
        execl(writer, writer, argv[1], argv[2], (char *)NULL);
        return 2;
    }
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
fixture_cli="$root/Milo CLI"
cp "$cli" "$fixture_cli"
mkdir "$root/install & project"
cwd=$(CDPATH= cd -- "$root/install & project" && pwd -P)
export MILO_FIXTURE_DEST="$dest" MILO_FIXTURE_LOG="$root/lifecycle" MILO_FIXTURE_PROFILE="$profile"
export MILO_FIXTURE_PROFILE_WRITER="$app/$helper"
export CODEX_HOME='alternate & Codex'
export CLAUDE_CONFIG_DIR="$root/Claude profile"
export PI_CODING_AGENT_DIR=''
export OPENCODE_DB='alternate.db'
export XDG_CONFIG_HOME=config XDG_STATE_HOME="$root/state" XDG_DATA_HOME=data
export OPENAI_API_KEY='not-a-real-openai-token' ANTHROPIC_API_KEY='not-a-real-anthropic-token'

operation() (
    cd "$cwd"
    MILO_FIXTURE_FAILURE=${2-} /bin/sh -c '
        env() {
            [ "$#" = 3 ] && [ "$1" = -i ] && [ "$2" = /usr/bin/open ] &&
                [ "$3" = "$MILO_FIXTURE_DEST" ] && [ -f "$MILO_FIXTURE_PROFILE" ] || return 2
            printf "open\n" >>"$MILO_FIXTURE_LOG"
            [ "${MILO_FIXTURE_FAILURE-}" != open ]
        }
        installer=$1; shift
        . "$installer"
    ' fixture "$repo/src/menubar.sh" "$1" "$apps" "$sha" "$fixture_cli" <"$archive"
)

read_profile() { /usr/bin/plutil -extract "$1" raw -o - "$profile"; }

# An unsafe profile location is neither followed nor replaced.
ln -s "$root/other/keep" "$profile"
if operation install; then exit 1; fi
test -L "$profile"
test ! -s "$root/other/keep"
rm "$profile"
touch "$profile"
chmod 644 "$profile"
if operation install; then exit 1; fi
test "$(stat -f %Lp "$profile")" = 644
rm "$profile"

if operation install open; then exit 1; fi
test ! -e "$dest"
test ! -e "$profile"
operation install
codesign --verify --deep --strict -R "$requirement" "$dest"
test "$(stat -f %Lp "$profile")" = 600
test "$(read_profile paths.CODEX_HOME)" = "$cwd/$CODEX_HOME"
test "$(read_profile paths.CLAUDE_CONFIG_DIR)" = "$CLAUDE_CONFIG_DIR"
test "$(read_profile paths.OPENCODE_DB)" = "$OPENCODE_DB"
# The CLI ignores a relative XDG directory, so the menu's reader must be handed
# it as set: anchoring it here would point the menu at settings the CLI never reads.
test "$(read_profile paths.XDG_CONFIG_HOME)" = config
test "$(read_profile paths.XDG_STATE_HOME)" = "$root/state"
test "$(read_profile paths.XDG_DATA_HOME)" = data
test "$(read_profile installer.path)" = "$fixture_cli"
test "$(read_profile installer.digest)" = "$(openssl dgst -sha256 -binary "$fixture_cli" | base64)"
if read_profile paths.PI_CODING_AGENT_DIR >/dev/null 2>&1; then exit 1; fi
if grep -Eq 'not-a-real|API_KEY|<key>PATH</key>' "$profile"; then exit 1; fi
identity=$(stat -f %i "$dest")
profile_identity=$(stat -f %i "$profile")
operation install
test "$identity" = "$(stat -f %i "$dest")"
test "$profile_identity" = "$(stat -f %i "$profile")"

# A changed environment updates even the current release. A launch failure
# restores the previous settings and does not replace the unchanged app.
unset CODEX_HOME
export OPENCODE_DB=':memory:'
export PI_CODING_AGENT_DIR='alternate Pi'
cp "$profile" "$root/original-profile"
if operation install open; then exit 1; fi
cmp "$root/original-profile" "$profile"
test "$identity" = "$(stat -f %i "$dest")"
operation install
test "$identity" = "$(stat -f %i "$dest")"
test "$(read_profile paths.OPENCODE_DB)" = ':memory:'
test "$(read_profile paths.PI_CODING_AGENT_DIR)" = "$cwd/$PI_CODING_AGENT_DIR"
if read_profile paths.CODEX_HOME >/dev/null 2>&1; then exit 1; fi
printf 'tampered\n' >"$dest/Contents/Resources/revision"
if operation uninstall; then exit 1; fi
cp "$current/Contents/Resources/revision" "$dest/Contents/Resources/revision"
if operation uninstall --remove-installed; then exit 1; fi
test "$identity" = "$(stat -f %i "$dest")"
test -f "$profile"
operation uninstall
test ! -e "$dest"
test ! -e "$profile"
operation uninstall

cp -R "$previous" "$dest"
if operation uninstall; then exit 1; fi
if operation install --stop-installed; then exit 1; fi
cmp "$previous/Contents/Resources/revision" "$dest/Contents/Resources/revision"
if operation install open; then exit 1; fi
cmp "$previous/Contents/Resources/revision" "$dest/Contents/Resources/revision"
test ! -e "$profile"
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
--stop-installed
open
--stop-installed
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
test ! -e "$profile"
cmp "$previous/Contents/Resources/revision" "$legacy/Contents/Resources/revision"
operation install
test ! -e "$legacy"
cmp "$current/Contents/Resources/revision" "$dest/Contents/Resources/revision"
