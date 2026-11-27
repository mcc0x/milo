#!/bin/bash
set -euo pipefail
umask 077
[[ "${GITHUB_ACTIONS:-}" == true && "${RUNNER_ENVIRONMENT:-}" == github-hosted && "${RUNNER_OS:-}" == macOS && "$(/usr/bin/uname -m)" == arm64 ]] ||
    { echo 'Refusing non-GitHub-hosted or non-arm64 macOS runner' >&2; exit 1; }
[[ $# == 3 && "$1" == /* && -x "$1" && ! -L "$1" && "$2" =~ ^[a-f0-9]{64}$ && "$3" =~ ^([a-z0-9-]+\.)+[a-z0-9-]+$ ]]
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
binary=$1
cli_pin=$2
host=$3
for command in sudo sandbox-exec security curl python3 openssl dscacheutil shasum lipo codesign git; do
    [[ -x "/usr/bin/$command" ]] || { printf 'Required Apple path unavailable: /usr/bin/%s\n' "$command" >&2; exit 1; }
done
/usr/bin/python3 -I -c 'import pty, ssl, sys; assert sys.version_info >= (3,9); assert sys.flags.optimize == 0'
[[ "$(/usr/bin/lipo -archs "$binary")" == arm64 ]]
/usr/bin/codesign --verify --strict "$binary"
signature="$(/usr/bin/codesign -dv "$binary" 2>&1)"
[[ "$signature" == *Signature=adhoc* ]]
printf 'Scope: exact verified CLI bytes; no installer claim in this lane.\n'
[[ "$(/usr/bin/shasum -a 256 < "$binary" | /usr/bin/cut -d ' ' -f 1)" == "$cli_pin" ]]
script_dir="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}")" && pwd -P)"
py=/usr/bin/python3
sb=/usr/bin/sandbox-exec
root="$(/usr/bin/mktemp -d "${RUNNER_TEMP:?}/milo-exact-tls.XXXXXXXX")"
hosts_dirty=0
trust_dirty=0
listener=
fingerprint=
keychain=/Library/Keychains/System.keychain
flush() {
    /usr/bin/sudo -n /usr/bin/dscacheutil -flushcache
}
cleanup() {
    rc=$?
    trap - EXIT
    if [[ -s "$root/listener.pid" ]]; then
        /usr/bin/sudo -n /bin/kill -TERM "$(/usr/bin/sudo -n /bin/cat "$root/listener.pid")" 2>/dev/null || :
    fi
    if [[ -n "$listener" ]]; then wait "$listener" 2>/dev/null || :; fi
    if (( hosts_dirty )); then
        /usr/bin/sudo -n /bin/cp -p "$root/hosts.before" /private/etc/hosts || rc=1
        /usr/bin/cmp -s "$root/hosts.before" /private/etc/hosts || rc=1
        flush || rc=1
    fi
    if (( trust_dirty )); then
        "$py" -I - "$fingerprint" "$keychain" <<'PY' || rc=1
import subprocess
import sys

subprocess.run(
    ["/usr/bin/sudo", "-n", "/usr/bin/security", "delete-certificate", "-Z", *sys.argv[1:]],
    check=True, timeout=10,
)
PY
    fi
    /usr/bin/sudo -n /bin/rm -rf -- "$root" || rc=1
    exit "$rc"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
/usr/bin/sudo -n /usr/bin/true
mkdir "$root/home" "$root/tmp"
export MILO_TELEMETRY=0
cat > "$root/client.sb" <<'SB'
(version 1)
(allow default)
(deny network*)
(allow network-outbound (remote ip "localhost:443"))
(deny process-exec (literal "/usr/bin/open") (literal "/usr/bin/security") (literal "/usr/bin/sudo"))
SB
cat > "$root/server.sb" <<'SB'
(version 1)
(allow default)
(deny network*)
(allow network-bind (local ip "localhost:443"))
(allow network-inbound (local ip "localhost:443"))
SB
client() {
    /usr/bin/env -i HOME="$root/home" TMPDIR="$root/tmp" PATH=/usr/bin:/bin MILO_TELEMETRY=0 "$sb" -f "$root/client.sb" "$@"
}

cat > "$root/ca.cnf" <<'CONF'
[req]
distinguished_name=dn
x509_extensions=ca
prompt=no
[dn]
CN=Milo ephemeral qualification root
[ca]
basicConstraints=critical,CA:TRUE,pathlen:0
keyUsage=critical,keyCertSign,cRLSign
subjectKeyIdentifier=hash
CONF
/usr/bin/openssl req -new -x509 -newkey rsa:2048 -nodes -sha256 -days 1 -config "$root/ca.cnf" -keyout "$root/ca.key" -out "$root/ca.pem" 2>"$root/openssl.log"
/usr/bin/openssl req -new -newkey rsa:2048 -nodes -subj "/CN=$host" -keyout "$root/leaf.key" -out "$root/leaf.csr" 2>>"$root/openssl.log"
printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:%s\n' "$host" > "$root/leaf.ext"
/usr/bin/openssl x509 -req -in "$root/leaf.csr" -CA "$root/ca.pem" -CAkey "$root/ca.key" -set_serial 1 -days 1 -sha256 -extfile "$root/leaf.ext" -out "$root/leaf.pem" 2>>"$root/openssl.log"
/bin/rm -f "$root/ca.key"
fingerprint="$(/usr/bin/openssl x509 -in "$root/ca.pem" -noout -fingerprint -sha1 | /usr/bin/sed 's/.*=//;s/://g')"
[[ "$fingerprint" =~ ^[a-fA-F0-9]{40}$ ]]
trust_dirty=1
/usr/bin/sudo -n /usr/bin/security add-trusted-cert -d -r trustRoot -k "$keychain" "$root/ca.pem"

cp -p /private/etc/hosts "$root/hosts.before"
/usr/bin/awk -v hostname="$host" '{sub(/#.*/, ""); for(i=2;i<=NF;i++) if(tolower($i)==hostname) exit 9}' "$root/hosts.before"
hosts_dirty=1
printf '\n127.0.0.1 %s\n' "$host" | /usr/bin/sudo -n /usr/bin/tee -a /private/etc/hosts >/dev/null
flush
client "$py" - "$host" <<'PY'
import errno,socket,sys
host=sys.argv[1]
assert {a[4][0] for a in socket.getaddrinfo(host,443,type=socket.SOCK_STREAM)}=={"127.0.0.1"}
for fam, address in [(socket.AF_INET,("192.0.2.1",443)),(socket.AF_INET6,("2001:db8::1",443,0,0)),
                     (socket.AF_INET,("127.0.0.1",444)),(socket.AF_INET6,("::1",444,0,0))]:
    with socket.socket(fam) as s:
        s.settimeout(1)
        assert s.connect_ex(address) in (errno.EPERM,errno.EACCES)
with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as s:
    try: s.sendto(b"deny check",("192.0.2.1",53))
    except PermissionError: pass
    else: raise AssertionError("external UDP was not immediately denied")
print("PASS exact name resolves only to v4 loopback; off-host and nonfixture-port dials denied")
PY
/usr/bin/sudo -n /usr/bin/env -i HOME="$root/home" TMPDIR="$root/tmp" PATH=/usr/bin:/bin MILO_TELEMETRY=0 /bin/sh -c '
  umask 077
  printf "%s\n" "$$" > "$1/listener.pid"
  exec "$2" -f "$1/server.sb" "$3" -B "$4" "$5" "$1/leaf.pem" "$1/leaf.key" "$1/ledger.jsonl"
' server "$root" "$sb" "$py" "$script_dir/capture.py" "$host" >"$root/server.out" 2>"$root/server.err" &
listener=$!
for ((i=0;i<100;i++)); do
    /usr/bin/grep -qx ready "$root/server.out" && break
    /bin/sleep .1
done
/usr/bin/grep -qx ready "$root/server.out"
/usr/bin/sudo -n /bin/chmod 0644 "$root/ledger.jsonl"
day="$(/bin/date -u +%F)"
[[ "$(client /usr/bin/curl -q --proto '=https' --noproxy '*' --globoff --http1.1 --retry 0 --max-time 3 --max-filesize 1024 --user-agent '' --header 'Content-Type: application/json' --data-binary "{\"day\":\"$day\",\"new_install\":false,\"monthly_active\":false}" --write-out '%{http_code}' --url "https://$host/v1/counts")" == 204 ]]
echo 'PASS private TLS control via exact name, default system trust and no override'
client "$py" -B "$script_dir/runner-cases.py" "$root" "$binary" "$day"
[[ "$(/usr/bin/shasum -a 256 < "$binary" | /usr/bin/cut -d ' ' -f 1)" == "$cli_pin" ]]
echo 'PASS staged CLI digest unchanged after private TLS tests'
