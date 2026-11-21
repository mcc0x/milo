#!/bin/sh
set -eu
umask 077

if [ "${MILO_RELAY_DATA_DIR:-}" != /data ] ||
   [ "${RAILWAY_VOLUME_MOUNT_PATH-/data}" != /data ] ||
   [ ! -d /data ] || [ -L /data ] || ! mountpoint -q /data; then
    echo 'A persistent volume must be mounted at /data to start the relay image.' >&2
    exit 1
fi

# Railway starts the mounted volume as root. Limit setup to this mount root.
# Never change permissions or ownership recursively or run the service as root.
if [ "$(id -u)" = 0 ]; then
    chmod 0700 /data
    chown --no-dereference bun:bun /data
    export HOME=/home/bun
    exec setpriv --reuid=bun --regid=bun --clear-groups --no-new-privs -- "$@"
fi
exec "$@"
