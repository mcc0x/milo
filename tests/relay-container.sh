#!/bin/sh
# Exercise the shipped image. Synthetic Discord configuration; no network or provider access.
set -eu
image=${1:?supply a locally built relay image}
volume=$(docker volume create)
cid=''
cleanup() {
  if [ -n "$cid" ]; then docker rm -f "$cid" >/dev/null; fi
  docker volume rm "$volume" >/dev/null
}
trap cleanup EXIT
run() {
  docker run --network none --read-only -e PORT=8080 -e MILO_DISCORD_CLIENT_ID=123456 \
    -e MILO_DISCORD_CLIENT_SECRET=fixture-only "$@" "$image"
}
if run --rm >/dev/null 2>&1; then
  echo 'Relay started without its persistent volume' >&2
  exit 1
fi
cid=$(run -d -v "$volume:/data")
docker exec "$cid" bun -e '
  const opts = { headers: { Host: "healthcheck.railway.app" } };
  for (let i = 0; i < 50; ++i) {
    const res = await fetch("http://127.0.0.1:8080/readyz", opts).catch(() => null);
    if (res?.ok && (await res.json()).ok === true) break;
    if (i === 49) throw Error("Relay never became ready");
    await Bun.sleep(100);
  }
  const uid = (await Bun.file("/proc/1/status").text()).match(/^Uid:\s+(\d+)/m)?.[1];
  if (!uid || uid === "0") throw Error("Server did not drop root");
  const pair = host => fetch("http://127.0.0.1:8080/v1/pair", {method:"POST",
    headers:{Host:host,"Content-Type":"application/json","X-Real-IP":"192.0.2.1"},body:JSON.stringify({provider:"discord"})});
  if ((await pair("healthcheck.railway.app")).ok) throw Error("Health-check host accepted a pairing");
  const started = await pair("connect.usemilo.net");
  if (!started.ok) throw Error(`Canonical host refused pairing: ${started.status}`);
  const data = await started.json();
  if (!/^https:\/\/connect\.usemilo\.net\/discord\/start\/[A-Za-z0-9_-]{43}$/.test(data.url)) throw Error("Noncanonical launcher URL");'
docker stop -t 12 "$cid" >/dev/null
test "$(docker inspect -f '{{.State.ExitCode}}' "$cid")" = 0
