#!/bin/sh
set -eu
export LC_ALL=C
umask 077

die() { printf '%s\n' "$*" >&2; exit 1; }
[ "$#" = 1 ] || die "usage: sh .github/ci/build-linux.sh ABS_OUTPUT_DIRECTORY"
case $1 in /*) ;; *) die "output directory must be absolute";; esac
[ "$(id -u)" -ne 0 ] || die "build as an unprivileged user"
out=$1
mkdir -p "$out"
entries=$(find "$out/" -mindepth 1 -maxdepth 1 -printf x -quit)
[ -z "$entries" ] || die "output directory must be empty"
. /etc/os-release
[ "$ID:$VERSION_ID:$(uname -m):$(getconf GNU_LIBC_VERSION)" = "ubuntu:22.04:x86_64:glibc 2.35" ] ||
    die "requires x86_64 Ubuntu 22.04 with glibc 2.35"

target=x86_64-unknown-linux-gnu
name=milo-$target
src=$(pwd -P)
cargo=$(rustup which --toolchain 1.91.0 cargo)
tools=$(dirname "$cargo")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/home" "$tmp/cargo"
cat > "$tmp/remap" <<'SH'
#!/bin/sh
exec "$@" --remap-path-prefix="$MILO_SOURCE=/src/milo" --remap-path-prefix="$CARGO_HOME=/cargo" --remap-path-prefix="$CARGO_TARGET_DIR=/target"
SH
chmod 700 "$tmp/remap"
env -i HOME="$tmp/home" PATH="$tools:/usr/bin:/bin" LC_ALL=C SOURCE_DATE_EPOCH=0 \
    CARGO_HOME="$tmp/cargo" CARGO_TARGET_DIR="$tmp/target" MILO_SOURCE="$src" \
    RUSTC_WRAPPER="$tmp/remap" RUSTFLAGS="-C target-cpu=x86-64" \
    "$cargo" build --release --locked --target "$target" --bin milo

bin=$tmp/target/$target/release/milo
readelf -hW "$bin" > "$tmp/header"
grep -Eq 'Class: +ELF64$' "$tmp/header" &&
    grep -Eq 'Machine: +Advanced Micro Devices X86-64$' "$tmp/header" || die "not an x86-64 ELF"
readelf -lW "$bin" > "$tmp/program"
grep -Fq '[Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]' "$tmp/program" || die "unexpected loader"
readelf -dW "$bin" > "$tmp/dynamic"
if grep -Eq '\((RPATH|RUNPATH)\)' "$tmp/dynamic"; then die "unexpected library search path"; fi
deps=$(sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' "$tmp/dynamic" | sort)
[ "$deps" = "$(printf '%s\n' libc.so.6 libgcc_s.so.1)" ] || die "unexpected shared libraries: $deps"
readelf -VW "$bin" > "$tmp/versions"
glibc=$(sed -n 's/.*Name: GLIBC_\([^ ]*\).*/\1/p' "$tmp/versions" | sort -V | tail -1)
[ -n "$glibc" ] && dpkg --compare-versions "$glibc" le 2.35 || die "unsupported GLIBC_$glibc"
if grep -aFq -e "$src/" -e "$tmp/" -e "$tools/" "$bin"; then die "build path in executable"; fi
install -m 755 "$bin" "$out/$name"
(cd "$out" && sha256sum "$name" > SHA256SUMS)
