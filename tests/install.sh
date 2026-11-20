#!/bin/sh
# Exercise the source installer without a network request or a real Cargo build.
set -eu
unset CDPATH GIT_CONFIG_COUNT GIT_CONFIG_PARAMETERS GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR
root=$(cd -P "$(dirname "$0")/.." && pwd -P)
tmp_base=$(cd -P "${TMPDIR:-/tmp}" && pwd -P)
fixture=$(mktemp -d "$tmp_base/milo-installer-test.XXXXXXXX")
trap 'rm -rf "$fixture"' 0
trap 'exit 1' HUP INT TERM
umask 022

checkout="$fixture/milo checkout"
unrelated="$fixture/unrelated checkout"
HOME=$fixture/home
TMPDIR=$fixture/tmp
MILO_BIN_DIR=$fixture/bin
GIT_CONFIG_GLOBAL=/dev/null
GIT_CONFIG_NOSYSTEM=1
REAL_GIT=$(command -v git)
export HOME TMPDIR MILO_BIN_DIR GIT_CONFIG_GLOBAL GIT_CONFIG_NOSYSTEM REAL_GIT
mkdir -p "$HOME" "$TMPDIR" "$fixture/tools"
for dir in "$checkout" "$unrelated"; do
    mkdir -p "$dir/src"
    git -C "$dir" init -q --template=
    printf '[package]\nname = "milo"\n' > "$dir/Cargo.toml"
    : > "$dir/src/main.rs"
    cp "$root/install.sh" "$dir/install.sh"
done
git -C "$checkout" add -- install.sh
printf 'checkout\n' > "$checkout/fixture-source"
ln -s '../milo checkout/install.sh' "$unrelated/linked-install.sh"

cat > "$fixture/tools/git" <<'EOF'
#!/bin/sh
set -eu
[ "$1" = clone ] || exec "$REAL_GIT" "$@"
[ "$#:$2:$3:$4" = '5:--depth:1:https://github.com/mcc0x/milo' ]
case "$5" in
    "$TMPDIR"/milo.*/source) ;;
    *) exit 1 ;;
esac
[ -n "$(find "${5%/*}" -prune -perm 700)" ]
mkdir "$5"
printf 'fetched\n' > "$5/fixture-source"
EOF
cat > "$fixture/tools/cargo" <<'EOF'
#!/bin/sh
set -eu
case "$PWD" in
    /) kind=fetched ;;
    *)
        [ "$PWD" -ef "$CHECKOUT" ] || exit 1
        kind=checkout
        ;;
esac
case "$1" in
    -vV) printf 'host: fixture-host-%s\n' "$kind" ;;
    build)
        [ "$#:$2:$3:$4:$6:$8:$9" = "9:--release:--locked:--manifest-path:--target-dir:--target:fixture-host-$kind" ]
        source=${5%/Cargo.toml}
        [ "$5" = "$source/Cargo.toml" ] && [ "$7" = "$source/target" ]
        case "$kind:$source" in
            "fetched:$TMPDIR"/milo.*/source) ;;
            checkout:/*) [ "$source" -ef "$CHECKOUT" ] || exit 1 ;;
            *) exit 1 ;;
        esac
        mkdir -p "$7/$9/release"
        cp "$source/fixture-source" "$7/$9/release/milo"
        ;;
    *) exit 1 ;;
esac
EOF
chmod +x "$fixture/tools/git" "$fixture/tools/cargo"
CHECKOUT=$checkout
export CHECKOUT
PATH=$fixture/tools:$PATH
export PATH

expect_install() {
    expected=$1
    shift
    "$@" >/dev/null
    actual=$(cat "$MILO_BIN_DIR/milo")
    if [ "$actual" != "$expected" ]; then
        printf 'Expected %s source, installed %s source: %s\n' "$expected" "$actual" "$*" >&2
        exit 1
    fi
    rm "$MILO_BIN_DIR/milo"
    [ "$(find "$TMPDIR" -print)" = "$TMPDIR" ]
}

cd "$checkout"
expect_install checkout sh install.sh
cd "$unrelated"
expect_install checkout sh '../milo checkout/install.sh'
expect_install checkout sh linked-install.sh
# A case-insensitive volume reaches the checkout through another spelling of its path.
respelled="$fixture/MILO checkout"
if [ "$respelled" -ef "$checkout" ]; then
    expect_install checkout sh "$respelled/install.sh"
fi
expect_install fetched sh install.sh
cat "$checkout/install.sh" | expect_install fetched sh
