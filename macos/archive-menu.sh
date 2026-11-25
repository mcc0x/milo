#!/bin/sh
set -eu
export COPYFILE_DISABLE=1
exec /usr/bin/tar -czf "$2" --format ustar --uid 0 --gid 0 --numeric-owner \
    --no-mac-metadata --no-xattrs --exclude '._*' --exclude .DS_Store -C "$1" Milo.app
