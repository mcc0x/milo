#!/bin/sh
set -eu
umask 077

if [ "$#" -ne 2 ]; then
    echo 'Usage: sh .github/ci/stage-macos.sh TESTED_CLI NEW_OUTPUT_DIRECTORY' >&2
    exit 2
fi
repo=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
template=$repo/site/public/install.sh
version=$(MILO_TELEMETRY=0 "$1" --version)
if [ -z "$version" ] || [ "$version" != "$(printf '%s\n' "$version" | grep -m 1 -Ex 'milo [0-9]+\.[0-9]+\.[0-9]+')" ]; then
    echo 'The tested CLI does not report a release version.' >&2
    exit 1
fi
version=v${version#milo }
version_line='^version=v[0-9]+\.[0-9]+\.[0-9]+$'
hash_line='^expected=[0-9a-f]{64}$'
if [ "$(grep -Ec "$version_line" "$template")" != 1 ] ||
    [ "$(grep -Ec "$hash_line" "$template")" != 1 ]; then
    echo 'The installer must have one version pin and one SHA-256 pin.' >&2
    exit 1
fi

mkdir -- "$2"
asset=milo-aarch64-apple-darwin
install -m 755 "$1" "$2/$asset"
cd "$2"
digest=$(shasum -a 256 < "$asset")
sed -E "s/$version_line/version=$version/; s/$hash_line/expected=${digest%% *}/" \
    "$template" > install.sh
sh -n install.sh
shasum -a 256 "$asset" install.sh > SHA256SUMS
