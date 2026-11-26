# Milo counts receiver

A small service that receives anonymous daily usage counts from Milo and shows them on a private dashboard. It stores only totals. It never stores an installation ID, an IP address, a request record or a timestamp of a request.

Stack: Bun 1.4.5 (as for the [pairing relay](../README.md)), its built-in HTTP server and Postgres client, and PostgreSQL 14 or later. There are no npm dependencies.

## Contract

`POST /v1/counts` with `content-type: application/json` and exactly this body, at most 128 bytes:

```json
{"day":"YYYY-MM-DD","new_install":false,"monthly_active":false}
```

- Every accepted request counts one daily active for `day`. `new_install` adds one to that day's new installs. `monthly_active` adds one to the calendar month of `day`.
- `day` is a UTC date, from two days before today to one day after (the extra day allows for a fast client clock). Older, later and impossible dates get `400`. Unknown, missing or mistyped fields get `400`; a wrong content type gets `415`; a larger body gets `413`.
- Success is `204` with no body. `429` (with `Retry-After`) means the global limiter is spent, and `503` means the database is unavailable. A client should drop a day it cannot report rather than retry in a loop.
- There are no redirects, cookies or CORS headers. Reports and dashboard responses are `no-store`; authenticated browsers can cache only the static artwork.

Other routes: `GET /healthz` (process is up), `GET /readyz` (database answers), and `GET /` (the dashboard, with `?days=7`, `30` or `90`). Anything else is `404`.

## Data

`schema.sql` creates two tables and is applied at start. `milo_counts_daily` holds one row per day (daily active, new installs). `milo_counts_monthly` holds one row per month (monthly active), so no per-day "first seen this month" history exists. A report updates both in one transaction with `INSERT … ON CONFLICT DO UPDATE SET n = n + …`, so concurrent reports never lose a count and a failed write changes neither table.

## Limits and privacy

- One fixed global token bucket limits `POST /v1/counts` (200 burst, 50 per second). The service keeps no per-client state and never reads the client address.
- The dashboard checks HTTP Basic credentials, compared in constant time, before reading the database. There is no attempt limiter, so its protection is the length of the two random credentials; do not shorten them. Because it keeps no per-client state, it cannot rate-limit guessing by source.
- The application logs startup or a generic startup failure. Even a failed write produces no per-request log: a series of timestamped errors could otherwise reveal activity during an outage. Use `/readyz` to check the database connection.
- **Public-edge blocker.** Railway's managed HTTP logs record `srcIp`, `requestId` and a timestamp for every request to a public domain, with no per-service way to disable or redact them, and keep them for 90 days ([Railway support](https://station.railway.com/questions/can-http-request-logging-be-disabled-or-30c1f581), [logs](https://docs.railway.com/observability/logs)). The application cannot change that. Under the requirement that no IP, request record or timestamp is collected, a Railway public domain for `POST /v1/counts` must not receive client traffic. Any other ingress, proxy or CDN in front of it needs its own review for the same fields, including copies and backups, before a client build reports here.
- The dashboard is served with `Cache-Control: no-store, private`, a CSP with no scripts, `X-Frame-Options: DENY`, `noindex` and no referrer. HTTP Basic credentials are only as private as the connection, so use it only over TLS, which Railway's ingress terminates.

## Configuration

| Variable | Meaning |
| --- | --- |
| `DATABASE_URL` | Postgres connection URL. |
| `MILO_COUNTS_ADMIN_USER` | Dashboard username: 16 to 128 letters, digits, `-` or `_`. Generate it randomly. |
| `MILO_COUNTS_ADMIN_PASSWORD` | Dashboard password: 32 to 256 printable ASCII characters. Generate it randomly. |
| `PORT` | Listen port. Railway supplies it; the default is `6130`. |
| `MILO_COUNTS_HOST` | Listen address. Default `127.0.0.1`; the image sets `0.0.0.0`. |

The service refuses to start unless both credentials are set and well formed. There is no default. Never put these values in the repository, the image, a bundle or a log. Credential creation and rotation: see [railway-template.md](railway-template.md).

## Deploy on Railway

Follow [railway-template.md](railway-template.md). It is the only deployment procedure; do not provision by hand. Requirements that hold whatever the procedure:

- The Postgres service is private: no public TCP proxy and no public domain. The receiver reaches it over Railway's private network.
- **No public domain for end-user counts.** Railway's edge keeps request logs this service cannot disable (see the blocker above), so no Milo client may report to a Railway public domain. A domain may be attached only to inspect the dashboard with synthetic data in a staging environment, and removed afterwards.
- Applying a template or changing production needs the maintainer's explicit approval of a reviewed plan (see the [Railway skill](../../skills/milo-railway/SKILL.md)).

## Abuse limitation

The receiver cannot tell one client from another, so it cannot stop a determined sender. Anyone who can reach it can add to today's totals: the global limiter only caps the rate (50 reports per second sustained) and a flood fills the limiter for real clients too. A valid payload carries no secret, so the counts are an approximate, unauthenticated signal, not evidence. Only four distinct days (and at most two months) are accepted at any time, so a flood cannot grow storage. If abuse matters, handle it at a layer that is itself reviewed for what it logs, and never by adding identifiers here.

## Before any client traffic: staging log preflight

Run these in a staging environment with synthetic data first and record the result in the pull request.

1. Map the real HTTPS path: DNS, TLS terminator, CDN/WAF, load balancer, reverse proxy and service. Send only synthetic success, rejection and outage requests in staging. Inspect access/error logs, traces, metrics labels and drains at each hop; confirm none stores source IP (including forwarded headers), request ID, exact request time, body, authorization header or query string. Confirm no hop retries POST on timeout or upstream failure. A quiet application console alone proves none of this.
2. Review Postgres statement, duration, error and bind-parameter logging, auditing, query tracing and replication/backup exports. Postgres needs WAL to recover transactions; aggregate tables do **not** guarantee that infrastructure has no transaction-level history. Record who can access each copy (including WAL archives and snapshots), its retention and deletion policy. Verify the provider's actual retention in writing, not just the time range visible in its console.
3. Confirm the Postgres service has no public TCP proxy or public domain, and that the receiver is the only service holding its connection URL.
4. Confirm an unauthenticated request to the dashboard gets `401` and shows nothing, and that the admin variables are set on this service only.
5. Stop if any hop records client data. The client must not ship a build that reports here until each one is cleared.

## Test

The tests need a Postgres. `scripts/pg-ephemeral.sh COMMAND [ARGS...]` starts a throwaway, unprivileged cluster in a private temporary directory, runs the command, then stops the cluster and deletes the directory, whatever the command's outcome. The cluster listens on `127.0.0.1` and in that directory only, and needs a random password that exists only in the command's environment as `DATABASE_URL` and `MILO_COUNTS_TEST_DATABASE_URL`. Nothing is printed. `PG_BIN` is the directory holding `initdb`, `pg_ctl`, `postgres` and `createdb`; leave it unset when they are on `PATH`.

```sh
cd hosted/counts
PG_BIN=/usr/lib/postgresql/18/bin scripts/pg-ephemeral.sh bun test ./counts.integration.ts
```

For the full native path, run `hosted/counts/scripts/pg-ephemeral.sh python3 tests/anonymous-counts.py` from the repository root. It compiles a disposable Milo with a loopback-only endpoint, opens real terminal panes, verifies silence until a later eligible launch, and reads the resulting total from the authenticated dashboard after restarting the receiver.

The suite creates and drops its own database. It is named `counts.integration.ts` so the relay's `bun test` in `hosted/` does not pick it up; run it by that path. It uses synthetic dates and credentials only, calls the handler directly, and opens one loopback socket to check that nothing redirects and that oversize bodies are refused.

Without root, fetch `postgresql-18`, `libpq5` and `liburing2` with `apt download`, unpack each with `dpkg -x` into one directory, and run with `LD_LIBRARY_PATH` set to its `usr/lib/x86_64-linux-gnu`.

### Run it by hand

```sh
export DATABASE_URL=… MILO_COUNTS_ADMIN_USER=… MILO_COUNTS_ADMIN_PASSWORD=… PORT=6130
bun run --no-env-file server.ts
curl -i -X POST http://127.0.0.1:6130/v1/counts -H 'content-type: application/json' \
  -d "{\"day\":\"$(date -u +%F)\",\"new_install\":true,\"monthly_active\":false}"
```

Open `http://127.0.0.1:6130/` in a browser and sign in with the two variables. For a native client end to end test, point a development build at that origin with a throwaway database, then read the totals with `psql`. Use synthetic data only, never a live service.
