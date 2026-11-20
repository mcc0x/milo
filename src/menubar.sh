# Invoked by milo with an archive embedded at build time, never a downloaded script.
set -eu
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
apps=$2
name='Milo.app'
dest=$apps/$name
helper=Contents/MacOS/Milo
profile=$apps/.milo-menu-profile.plist
launch() { env -i /usr/bin/open "$dest"; }
fail() { echo "$*" >&2; exit 1; }
verify() {
    [ ! -L "$1" ] && [ -d "$1" ] && codesign --verify --deep --strict -R '=identifier "net.usemilo.usage.local"' "$1" || fail "Not a verified Milo bundle: $1"
}
verify_helper() {
    [ "$(shasum -a 256 < "$1/$helper" | cut -d' ' -f1)" = "$2" ]
}
[ ! -L "$apps" ] && [ -d "$apps" ] && [ "$(stat -f %u "$apps")" = "$(id -u)" ] ||
    fail "Not a user-owned Applications directory: $apps"
case $(stat -f %Lp "$apps") in
    *[2367]?|*[2367]) fail "Applications directory is writable by other users: $apps" ;;
esac
if [ -e "$profile" ] || [ -L "$profile" ]; then
    [ ! -L "$profile" ] && [ -f "$profile" ] && [ "$(stat -f '%u:%Lp' "$profile")" = "$(id -u):600" ] ||
        fail "Not a private Milo menu profile: $profile"
fi
if [ -e "$dest" ] || [ -L "$dest" ]; then verify "$dest"; fi
previous=$dest
legacy="$apps/Milo Usage.app"
if [ -e "$legacy" ] || [ -L "$legacy" ]; then
    verify "$legacy"
    [ ! -e "$dest" ] || fail 'Both Milo and Milo Usage exist. Resolve the duplicate in Applications first; neither was changed.'
    [ "$1" = install ] || fail 'Run milo menubar install to migrate Milo Usage before removing it.'
    previous=$legacy
fi
if [ "$1" = uninstall ]; then
    if [ ! -e "$dest" ]; then echo 'Milo is not installed.'; exit 0; fi
    # An ad-hoc identifier alone is not proof that an installed executable is ours.
    verify_helper "$dest" "$3" || fail 'Run milo menubar install first to upgrade this preview app.'
    "$dest/$helper" --remove-installed "$dest" || fail 'Could not stop Milo or unregister launch at login. The app was kept.'
    rm -rf "$dest"
    rm -f "$profile"
    echo 'Removed Milo. Milo settings and CLI were kept.'
    exit 0
fi
stage=$(mktemp -d "$apps/.milo-menu.XXXXXXXX")
new=$stage/$name
committed=false
replace_bundle=false
replace_profile=false
cleanup() {
    if [ "$committed" = false ]; then
        retained=false
        if [ "$replace_profile" = true ] && [ ! -e "$stage/profile" ]; then
            if [ -e "$stage/previous-profile" ]; then
                mv -fh "$stage/previous-profile" "$profile" || retained=true
            else
                rm -f "$profile" || retained=true
            fi
        fi
        if [ "$replace_bundle" = true ] && [ ! -e "$new" ]; then
            mv "$dest" "$new" || retained=true
        fi
        if [ -d "$stage/previous" ]; then
            if [ ! -e "$previous" ] && [ ! -L "$previous" ]; then
                mv "$stage/previous" "$previous" || retained=true
            else
                retained=true
            fi
        fi
        if [ "$retained" = true ]; then
            echo "Milo could not roll back the menu install. Backup files were retained in $stage" >&2
            return
        fi
    fi
    rm -rf "$stage"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
tar -xzf - -C "$stage"
verify "$new"
verify_helper "$new" "$3" || fail 'The bundled Milo menu helper does not match this installer.'
(umask 077; "$new/$helper" --install-profile "$4" >"$stage/profile") || fail 'Could not prepare the Milo menu profile.'
current=false
if [ "$previous" = "$dest" ] && cmp -s "$new/Contents/_CodeSignature/CodeResources" "$dest/Contents/_CodeSignature/CodeResources" && cmp -s "$new/$helper" "$dest/$helper"; then
    current=true
    if cmp -s "$stage/profile" "$profile"; then
        launch || fail 'macOS could not open Milo.'
        echo 'Milo is already current. Opened.'
        exit 0
    fi
fi
if [ -e "$profile" ]; then cp -pP "$profile" "$stage/previous-profile"; fi
if [ -e "$previous" ]; then
    "$new/$helper" --stop-installed "$previous" || fail 'Could not quit the running Milo app. The installed copy was kept.'
fi
if [ "$current" = false ]; then
    if [ -e "$previous" ]; then mv "$previous" "$stage/previous"; fi
    replace_bundle=true
    mv "$new" "$dest"
fi
replace_profile=true
mv -fh "$stage/profile" "$profile"
launch || fail 'macOS could not open Milo.'
committed=true
printf 'Installed and opened %s\n' "$dest"
