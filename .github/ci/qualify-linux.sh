#!/bin/sh
set -eu
umask 077

[ "$#" = 1 ] && [ "$(id -u)" -ne 0 ] || {
    echo "usage (non-root, Linux x86-64, Docker): sh .github/ci/qualify-linux.sh ABS_OUTPUT_DIRECTORY" >&2
    exit 1
}
case $1 in /*) ;; *) echo "output directory must be absolute" >&2; exit 1;; esac
out=$1
mkdir -p "$out"
entries=$(find "$out/" -mindepth 1 -maxdepth 1 -printf x -quit)
[ -z "$entries" ] || { echo "output directory must be empty" >&2; exit 1; }
w=$(mktemp -d)
trap 'rm -rf "$w"' EXIT
builder=buildpack-deps@sha256:b7ada1fce26ea89df6080ac1903bdd2ca434fd46e72a19f48ea42f3c4f7f7dad
runtime=ubuntu@sha256:b000e69a50ff940390233025e7a337fb8469b509e2334fa4d018ac30f3e18cdb
run() {
    docker run --rm --hostname milo-test --platform linux/amd64 --user "$(id -u):$(id -g)" --cap-drop ALL \
        --security-opt no-new-privileges --read-only --tmpfs /tmp:exec "$@"
}
mkdir -p "$w/tools/bin" "$w/tools/rustup"
cp "$(command -v rustup)" "$w/tools/bin/rustup"
env -i HOME="$w/tools" CARGO_HOME="$w/tools" RUSTUP_HOME="$w/tools/rustup" PATH=/usr/bin:/bin \
    "$w/tools/bin/rustup" toolchain install 1.91.0 --profile minimal --no-self-update
git archive --format=tar HEAD > "$w/source.tar"
for tree in first second-build; do
    mkdir -p "$w/$tree/src" "$w/$tree/tmp" "$w/$tree/out"
    tar -xf "$w/source.tar" -C "$w/$tree/src"
    run -v "$w/tools:/tooling:ro" -v "$w/$tree/src:/$tree/src:ro" \
        -v "$w/$tree/tmp:/$tree/tmp" -v "$w/$tree/out:/$tree/out" -w "/$tree/src" \
        -e PATH=/tooling/bin:/usr/bin:/bin -e RUSTUP_HOME=/tooling/rustup -e HOME=/tmp \
        -e MILO_DISTRIBUTION_COUNTS="${MILO_DISTRIBUTION_COUNTS-}" \
        -e TMPDIR="/$tree/tmp" "$builder" sh .github/ci/build-linux.sh "/$tree/out"
done
name=milo-x86_64-unknown-linux-gnu
cmp "$w/first/out/$name" "$w/second-build/out/$name"
sh "$w/first/src/.github/ci/stage-linux.sh" "$w/first/out"
run --network none -v "$w/first/out:/candidate:ro" -w /candidate "$runtime" \
    env -i HOME=/tmp/home PATH=/usr/bin:/bin LC_ALL=C MILO_TELEMETRY=0 sh -ec '
        mkdir -m 700 "$HOME"
        for tool in cc gcc rustc cargo rex; do
            if command -v "$tool"; then exit 1; fi
        done
        sha256sum -c SHA256SUMS
        sh install-linux.sh
        cmp milo-x86_64-unknown-linux-gnu "$HOME/.local/bin/milo"
        "$HOME/.local/bin/milo" --version
        "$HOME/.local/bin/milo" doctor >/dev/null
        sh install-linux.sh --uninstall
    '
cp "$w/first/out/$name" "$w/first/out/install-linux.sh" "$w/first/out/SHA256SUMS" "$out/"
cat "$out/SHA256SUMS"
