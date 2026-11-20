# Invoked by milo with an archive embedded at build time, never a downloaded script.
set -eu
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
apps=$2
name='Milo Usage.app'
dest=$apps/$name
helper=Contents/MacOS/MiloUsage
fail() { echo "$*" >&2; exit 1; }
verify() {
    [ ! -L "$1" ] && [ -d "$1" ] && codesign --verify --deep --strict -R '=identifier "net.usemilo.usage.local"' "$1" || fail "Not a verified Milo Usage bundle: $1"
}
if [ -e "$dest" ] || [ -L "$dest" ]; then verify "$dest"; fi
if [ "$1" = uninstall ]; then
    if [ ! -e "$dest" ]; then echo 'Milo Usage is not installed.'; exit 0; fi
    # An ad-hoc identifier alone is not proof that an installed executable is ours.
    [ "$(shasum -a 256 < "$dest/$helper" | cut -d' ' -f1)" = "$3" ] || fail 'Run milo menubar install first to upgrade this preview app.'
    "$dest/$helper" --remove-installed "$dest" || fail 'Could not stop Milo Usage or unregister launch at login. The app was kept.'
    rm -rf "$dest"
    echo 'Removed Milo Usage. Milo settings and CLI were kept.'
    exit 0
fi
stage=$(mktemp -d "$apps/.milo-menu.XXXXXXXX")
cleanup() {
    if [ -d "$stage/previous" ]; then
        if [ ! -e "$dest" ]; then mv "$stage/previous" "$dest" || return; else echo "Previous bundle retained in $stage" >&2; return; fi
    fi
    rm -rf "$stage"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
tar -xzf - -C "$stage"
new=$stage/$name
verify "$new"
if [ -e "$dest" ]; then
    if cmp -s "$new/Contents/_CodeSignature/CodeResources" "$dest/Contents/_CodeSignature/CodeResources" && cmp -s "$new/$helper" "$dest/$helper"; then
        open "$dest"; echo 'Milo Usage is already current. Opened.'; exit 0
    fi
    "$new/$helper" --stop-installed "$dest" || fail 'Could not quit the running Milo Usage app. The installed copy was kept.'
    mv "$dest" "$stage/previous"
fi
mv "$new" "$dest"
if ! open "$dest"; then mv "$dest" "$new"; fail 'macOS could not open Milo Usage.'; fi
rm -rf "$stage/previous"
printf 'Installed and opened %s\n' "$dest"
