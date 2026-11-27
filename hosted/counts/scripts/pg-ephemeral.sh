#!/bin/sh
# Run a command against a throwaway, unprivileged PostgreSQL cluster.
#   scripts/pg-ephemeral.sh COMMAND [ARGS...]
# The cluster lives in a private temporary directory, listens on 127.0.0.1 and in that directory
# only, and requires a random password that exists only in the environment of COMMAND
# (DATABASE_URL and MILO_COUNTS_TEST_DATABASE_URL). It is stopped and deleted when COMMAND exits or
# the script is interrupted, and the exit status is COMMAND's.
# PG_BIN is the directory holding initdb, pg_ctl, postgres and createdb (default: found on PATH).
set -eu
umask 077
[ "$#" -gt 0 ] || { echo "usage: pg-ephemeral.sh COMMAND [ARGS...]" >&2; exit 2; }
bin() { if [ -n "${PG_BIN:-}" ]; then echo "$PG_BIN/$1"; else command -v "$1"; fi; }

dir=$(mktemp -d "${TMPDIR:-/tmp}/milo-counts-pg.XXXXXX")
cleanup() {
    "$(bin pg_ctl)" -D "$dir/data" -m immediate stop >/dev/null 2>&1 || true
    rm -rf "$dir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

password=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')
printf '%s' "$password" >"$dir/pw"
port=$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')
"$(bin initdb)" -D "$dir/data" -U milo --pwfile="$dir/pw" --auth=scram-sha-256 -E UTF8 --locale=C >"$dir/initdb.log" 2>&1 \
    || { cat "$dir/initdb.log" >&2; exit 1; }
rm -f "$dir/pw"
"$(bin pg_ctl)" -D "$dir/data" -l "$dir/postgres.log" -w \
    -o "-c listen_addresses=127.0.0.1 -p $port -c unix_socket_directories=$dir -c fsync=off" start >/dev/null \
    || { cat "$dir/postgres.log" >&2; exit 1; }
PGPASSWORD=$password "$(bin createdb)" -h 127.0.0.1 -p "$port" -U milo milo_counts

DATABASE_URL="postgres://milo:$password@127.0.0.1:$port/milo_counts"
MILO_COUNTS_TEST_DATABASE_URL=$DATABASE_URL
MILO_COUNTS_TEST_POSTGRES_LOG="$dir/postgres.log"
export DATABASE_URL MILO_COUNTS_TEST_DATABASE_URL MILO_COUNTS_TEST_POSTGRES_LOG
status=0
"$@" || status=$?
exit "$status"
