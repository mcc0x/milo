#!/bin/sh
# Stage the offline installer only after verifying the build's recorded checksum.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
cd -- "${1:?Usage: sh stage-linux.sh ARTIFACT_DIRECTORY}"
asset=milo-x86_64-unknown-linux-gnu
sum=$(sha256sum "$asset")
# Accept only the build's exact one-entry manifest, not other paths from a file.
printf '%s\n' "$sum" | cmp -s - SHA256SUMS || { echo 'Build checksum does not match.' >&2; exit 1; }
sed "s/^expected=@SHA256@$/expected=${sum%% *}/" "$root/linux/install.sh.in" > install-linux.sh
sha256sum "$asset" install-linux.sh > SHA256SUMS
