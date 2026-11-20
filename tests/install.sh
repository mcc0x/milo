#!/bin/sh
# Exercise both installers without network requests or a real Cargo build.
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
[ "${GIT_DIR+x}${GIT_WORK_TREE+x}${GIT_INDEX_FILE+x}${GIT_COMMON_DIR+x}" = '' ]
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
    -vV)
        case "${FIXTURE_CARGO_VERSION:-ok}" in
            missing-host) printf 'cargo (fixture)\n' ;;
            *)
                printf 'host: fixture-host-%s\n' "$kind"
                [ "${FIXTURE_CARGO_VERSION:-ok}" != fail ]
                ;;
        esac
        ;;
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
    rm "$MILO_BIN_DIR/milo" "$MILO_BIN_DIR/.milo-install-receipt"
    [ "$(find "$TMPDIR" -print)" = "$TMPDIR" ]
}

expect_failure() (
    diagnostic=$1
    shift
    if output=$("$@" 2>&1); then
        printf 'Unexpected success: %s\n%s\n' "$*" "$output" >&2
        exit 1
    fi
    if ! printf '%s\n' "$output" | grep -Fq "$diagnostic"; then
        printf 'Expected %s: %s\n%s\n' "$diagnostic" "$*" "$output" >&2
        exit 1
    fi
)

cd "$checkout"
expect_install checkout sh install.sh
cd "$unrelated"
expect_install checkout sh '../milo checkout/install.sh'
# Git hooks and plumbing can export overrides for the caller's checkout.
expect_install checkout env GIT_DIR="$unrelated/.git" GIT_WORK_TREE="$unrelated" \
    GIT_INDEX_FILE="$unrelated/.git/index" sh '../milo checkout/install.sh'
expect_install checkout sh linked-install.sh
# A case-insensitive volume reaches the checkout through another spelling of its path.
respelled="$fixture/MILO checkout"
if [ "$respelled" -ef "$checkout" ]; then
    expect_install checkout sh "$respelled/install.sh"
fi
expect_install fetched sh install.sh
cat "$checkout/install.sh" | expect_install fetched sh

# A failed `cargo -vV` can print a host; neither failure should reach the build.
expect_failure 'Could not determine the Cargo host target' \
    env FIXTURE_CARGO_VERSION=fail sh install.sh
expect_failure 'Cargo did not report a valid host target' \
    env FIXTURE_CARGO_VERSION=missing-host sh install.sh
[ "$(find "$MILO_BIN_DIR" -print)" = "$MILO_BIN_DIR" ]
[ "$(find "$TMPDIR" -print)" = "$TMPDIR" ]

# Pin the fixture payload instead of the release binary; use the real SHA-256 tool.
RELEASE_FIXTURE=$fixture/release-binary
export RELEASE_FIXTURE
printf 'release\n' > "$RELEASE_FIXTURE"
if command -v shasum >/dev/null 2>&1; then
    sum=$(shasum -a 256 < "$RELEASE_FIXTURE")
else
    sum=$(sha256sum < "$RELEASE_FIXTURE")
fi
release_installer=$fixture/release-install.sh
awk -v digest="${sum%% *}" '/^expected=/ { print "expected=" digest; next } { print }' \
    "$root/site/public/install.sh" > "$release_installer"
cat > "$fixture/tools/uname" <<'EOF'
#!/bin/sh
case "$*" in
    -s) printf 'Darwin\n' ;;
    -m) printf 'arm64\n' ;;
    *) exit 1 ;;
esac
EOF
cat > "$fixture/tools/curl" <<'EOF'
#!/bin/sh
set -eu
[ "$#:$1:$2:$3:$4:$5:$6:$7" = '9:--disable:--proto:=https:--proto-redir:=https:-fsSL:-o' ]
case "$9" in
    https://github.com/mcc0x/milo/releases/download/*/milo-aarch64-apple-darwin) ;;
    *) exit 1 ;;
esac
case "${FIXTURE_RELEASE_DOWNLOAD:-ok}" in
    fail) printf 'partial\n' > "$8"; exit 1 ;;
    corrupt) printf 'corrupt\n' > "$8" ;;
    ok) cp "$RELEASE_FIXTURE" "$8" ;;
    *) exit 1 ;;
esac
EOF
chmod +x "$fixture/tools/uname" "$fixture/tools/curl"

expect_uninstall_refusal() {
    expect_failure 'no matching installer receipt' sh "$1" --uninstall
}

exercise_uninstall() (
    installer=$1
    kind=$2
    payload=$3
    MILO_BIN_DIR="$fixture/$kind bin"
    export MILO_BIN_DIR
    binary=$MILO_BIN_DIR/milo
    receipt=$MILO_BIN_DIR/.milo-install-receipt
    old_binary="$fixture/$kind old binary"
    old_receipt="$fixture/$kind old receipt"
    alias_dir="$fixture/$kind alias"
    other="$fixture/$kind other"
    mkdir "$MILO_BIN_DIR" "$other"
    ln -s "$MILO_BIN_DIR" "$alias_dir"
    printf 'unrelated\n' > "$binary"
    ln "$binary" "$old_binary"
    expect_uninstall_refusal "$installer"
    [ "$(cat "$binary")" = unrelated ]

    MILO_BIN_DIR=$alias_dir sh "$installer" >/dev/null
    [ -x "$binary" ]
    cmp "$binary" "$payload"
    [ "$(cat "$old_binary")" = unrelated ]

    if [ "$kind" = release ]; then
        cp "$receipt" "$fixture/verified-receipt"
        expect_failure 'Could not download' env FIXTURE_RELEASE_DOWNLOAD=fail sh "$installer"
        expect_failure 'SHA-256 checksum mismatch' env FIXTURE_RELEASE_DOWNLOAD=corrupt sh "$installer"
        cmp "$binary" "$payload"
        cmp "$receipt" "$fixture/verified-receipt"
        [ -z "$(find "$MILO_BIN_DIR" -name '.milo.*' -print)" ]
    fi

    # A receipt copied together with the exact binary is not valid at another destination.
    cp "$binary" "$other/milo"
    cp "$receipt" "$other/.milo-install-receipt"
    MILO_BIN_DIR=$other expect_uninstall_refusal "$installer"
    cmp "$other/milo" "$payload"

    # Invalid or symlinked receipts cannot authorize removing even the original binary.
    mv "$receipt" "$old_receipt"
    ln -s "$old_receipt" "$receipt"
    expect_uninstall_refusal "$installer"
    cmp "$binary" "$payload"
    rm "$receipt"
    printf 'invalid\n' > "$receipt"
    expect_uninstall_refusal "$installer"
    cmp "$binary" "$payload"
    invalid_receipt="$fixture/$kind invalid receipt"
    ln "$receipt" "$invalid_receipt"
    sh "$installer" >/dev/null
    [ "$(cat "$invalid_receipt")" = invalid ]
    cmp "$receipt" "$old_receipt"

    # Replacing the installed binary must not authorize deletion of its replacement.
    mv "$binary" "$fixture/$kind installed binary"
    cp "$old_binary" "$binary"
    expect_uninstall_refusal "$installer"
    cmp "$binary" "$old_binary"
    [ -f "$receipt" ]
    mv "$fixture/$kind installed binary" "$binary"

    output=$(sh "$installer" --uninstall)
    printf '%s\n' "$output" | grep -q '^Removed '
    [ "$(find "$MILO_BIN_DIR" -print)" = "$MILO_BIN_DIR" ]
    output=$(sh "$installer" --uninstall)
    printf '%s\n' "$output" | grep -q '^No milo binary '
)

exercise_uninstall "$checkout/install.sh" checkout "$checkout/fixture-source"
exercise_uninstall "$release_installer" release "$RELEASE_FIXTURE"
