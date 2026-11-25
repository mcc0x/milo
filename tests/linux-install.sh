#!/bin/sh
# Exercise the two-file release, without a Git checkout or network at install time.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
umask 077
lab=$(mktemp -d)
trap 'rm -rf -- "$lab"' EXIT
trap 'exit 1' HUP INT TERM
export HOME="$lab/home"
unset MILO_BIN_DIR CDPATH
asset=milo-x86_64-unknown-linux-gnu
destination="$HOME/.local/bin"
binary=$destination/milo
receipt=$destination/.milo-install-receipt
mkdir -p "$lab/first" "$lab/second" "$lab/caller" "$HOME/.config/milo"
cd "$lab/caller"
for release in first second; do
    (cd "$lab/$release" && printf '%s\n' "$release" > "$asset" && sha256sum "$asset" > SHA256SUMS)
    sh "$root/.github/ci/stage-linux.sh" "$lab/$release"
    (cd "$lab/$release" && sha256sum -c SHA256SUMS)
done

refuses() {
    message=$1
    shift
    if output=$("$@" 2>&1); then echo "Expected refusal: $message" >&2; exit 1; fi
    printf '%s\n' "$output" | grep -Fq -- "$message" || { echo "$output" >&2; exit 1; }
}

first=$lab/first/install-linux.sh
second=$lab/second/install-linux.sh
sh "$first"
cmp "$binary" "$lab/first/$asset"
[ "$(stat -c %a "$binary")" = 755 ]
[ "$(stat -c %a "$receipt")" = 600 ]
printf 'kept\n' > "$HOME/.config/milo/fixture"
printf 'neighbor\n' > "$destination/neighbor"
sh "$second"
cmp "$binary" "$lab/second/$asset"
cp "$receipt" "$lab/good-receipt"
printf 'damaged\n' > "$lab/first/$asset"
refuses 'SHA-256 mismatch' sh "$first"
rm "$lab/first/$asset"
refuses 'beside this installer' sh "$first"
cmp "$binary" "$lab/second/$asset"
cmp "$lab/good-receipt" "$receipt"
[ -z "$(find "$destination" -name '.milo.*' -print)" ]

printf 'unmanaged\n' > "$binary"
refuses 'no matching installer receipt' sh "$second"
refuses 'no matching installer receipt' sh "$second" --uninstall
[ "$(cat "$binary")" = unmanaged ]
rm "$binary"
ln -s "$destination/neighbor" "$binary"
refuses 'not a regular file' sh "$second"
refuses 'not a regular file' sh "$second" --uninstall
rm "$binary"
cp "$lab/second/$asset" "$binary"
mv "$receipt" "$lab/real-receipt"
ln -s "$lab/real-receipt" "$receipt"
refuses 'not a regular file' sh "$second"
refuses 'not a regular file' sh "$second" --uninstall
rm "$receipt"
mv "$lab/real-receipt" "$receipt"
sh "$second" --uninstall
[ ! -e "$binary" ]
[ ! -e "$receipt" ]
[ "$(cat "$HOME/.config/milo/fixture")" = kept ]
[ "$(cat "$destination/neighbor")" = neighbor ]

# Only these command-scoped probes see the stand-ins. No user PATH or files change.
mkdir "$lab/probes"
printf '#!/bin/sh\ncase "$1" in -s) echo "$OS";; -m) echo "$ARCH";; esac\n' > "$lab/probes/uname"
printf '#!/bin/sh\ncase "$1" in LONG_BIT) echo "$BITS";; GNU_LIBC_VERSION) echo "$LIBC";; esac\n' > "$lab/probes/getconf"
chmod +x "$lab/probes/"*
probe() {
    env PATH="$lab/probes:/usr/bin:/bin" OS="$1" ARCH="$2" BITS="$3" LIBC="$4" sh "$second"
}
refuses '64-bit x86 Linux' probe Darwin x86_64 64 'glibc 2.35'
refuses '64-bit x86 Linux' probe Linux aarch64 64 'glibc 2.35'
refuses '64-bit x86 Linux' probe Linux x86_64 32 'glibc 2.35'
refuses 'glibc 2.35' probe Linux x86_64 64 'glibc 2.34'
refuses 'glibc 2.35' probe Linux x86_64 64 'musl 1.2'
refuses 'glibc 2.35' probe Linux x86_64 64 ''
[ ! -e "$binary" ]
[ ! -e "$receipt" ]
echo 'Linux install, upgrade, refusal and receipt-only removal passed.'
