#!/bin/sh
# Exercise the staged bytes, never target/debug/milo or a binary from PATH.
set -eu
tests=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
assets=$(CDPATH= cd -- "${1:?Usage: sh tests/linux-candidate.sh ARTIFACT_DIRECTORY}" && pwd -P)
umask 077
lab=$(mktemp -d)
trap 'rm -rf -- "$lab"' EXIT
trap 'exit 1' HUP INT TERM
export HOME="$lab/home" PATH=/usr/bin:/bin MILO_TELEMETRY=0
unset MILO_BIN_DIR
mkdir -p "$HOME"
cd "$lab"
asset=milo-x86_64-unknown-linux-gnu
(cd "$assets" && sha256sum -c SHA256SUMS)
sh "$assets/install-linux.sh"
installed=$HOME/.local/bin/milo
cmp "$assets/$asset" "$installed"
"$installed" --version
python3 "$tests/linux-standalone.py" "$installed"
sh "$assets/install-linux.sh" --uninstall
test ! -e "$installed"
test ! -e "$HOME/.local/bin/.milo-install-receipt"
