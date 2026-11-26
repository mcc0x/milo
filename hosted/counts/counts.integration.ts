import { afterAll, beforeAll, beforeEach, describe, expect, spyOn, test } from "bun:test";
import { SQL } from "bun";
import { openDb, type Db } from "./db";
import { Bucket, createApp, parseCredentials } from "./server";

const adminUrl = Bun.env.MILO_COUNTS_TEST_DATABASE_URL;
if (!adminUrl) throw new Error("Set MILO_COUNTS_TEST_DATABASE_URL and run bun test ./counts.integration.ts; see hosted/counts/README.md (scripts/pg-ephemeral.sh).");

const user = "synthetic-admin-user-0001";
const password = "synthetic-admin-password-0000000001";
const auth = (u = user, p = password) => ({ authorization: `Basic ${btoa(`${u}:${p}`)}` });
const NOW = Date.parse("2026-11-26T12:00:00Z");
const databaseName = `milo_counts_test_${crypto.randomUUID().replaceAll("-", "")}`;
const admin = new SQL(adminUrl, { max: 1 });
let sql: SQL;
let db: Db;
let clock = NOW;
const app = () => createApp({ db, adminUser: user, adminPassword: password, now: () => clock });

const post = (body: unknown, headers: Record<string, string> = { "content-type": "application/json" }, a = app()) =>
  a.fetch(new Request("http://counts.test/v1/counts", { method: "POST", headers, body: typeof body === "string" ? body : JSON.stringify(body) }));
const valid = { day: "2026-11-26", new_install: false, monthly_active: false };
const rows = () => sql`SELECT to_char(day,'YYYY-MM-DD') AS day, daily_active::int AS a, new_installs::int AS n FROM milo_counts_daily ORDER BY day`;
const months = () => sql`SELECT to_char(month,'YYYY-MM-DD') AS month, monthly_active::int AS m FROM milo_counts_monthly ORDER BY month`;

beforeAll(async () => {
  await admin.unsafe(`CREATE DATABASE ${databaseName}`);
  const url = new URL(adminUrl);
  url.pathname = `/${databaseName}`;
  sql = new SQL(url.toString(), { max: 2 });
  db = openDb(url.toString());
  await db.migrate();
});
afterAll(async () => {
  await db.close();
  await sql.close();
  await admin.unsafe(`DROP DATABASE ${databaseName} WITH (FORCE)`);
  await admin.close();
});
beforeEach(async () => {
  clock = NOW;
  await sql`TRUNCATE milo_counts_daily, milo_counts_monthly`;
});

describe("POST /v1/counts", () => {
  test("every request counts one daily active and flags add to their own totals", async () => {
    expect((await post(valid)).status).toBe(204);
    expect((await post({ ...valid, new_install: true })).status).toBe(204);
    expect((await post({ ...valid, new_install: true, monthly_active: true })).status).toBe(204);
    expect(await rows()).toEqual([{ day: "2026-11-26", a: 3, n: 2 }]);
    expect(await months()).toEqual([{ month: "2026-11-01", m: 1 }]);
  });

  test("concurrent reports never lose an increment", async () => {
    const results = await Promise.all(Array.from({ length: 60 }, () => post({ ...valid, new_install: true, monthly_active: true })));
    expect(results.every((r) => r.status === 204)).toBe(true);
    expect(await rows()).toEqual([{ day: "2026-11-26", a: 60, n: 60 }]);
    expect(await months()).toEqual([{ month: "2026-11-01", m: 60 }]);
  });

  test("monthly activity is stored by calendar month, not by day", async () => {
    clock = Date.parse("2026-12-01T00:30:00Z");
    await post({ day: "2026-11-30", new_install: false, monthly_active: true });
    await post({ day: "2026-12-01", new_install: false, monthly_active: true });
    expect(await months()).toEqual([{ month: "2026-11-01", m: 1 }, { month: "2026-12-01", m: 1 }]);
    const columns = await sql`SELECT column_name FROM information_schema.columns WHERE table_name = 'milo_counts_daily'`;
    expect(columns.map((c: { column_name: string }) => c.column_name).sort()).toEqual(["daily_active", "day", "new_installs"]);
  });

  test("a failed monthly write rolls the daily write back", async () => {
    await sql.unsafe(`CREATE FUNCTION milo_test_fail() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''no''; END';
      CREATE TRIGGER milo_test_fail BEFORE INSERT OR UPDATE ON milo_counts_monthly FOR EACH ROW EXECUTE FUNCTION milo_test_fail()`);
    try {
      const quiet = spyOn(console, "error").mockImplementation(() => {});
      expect((await post({ ...valid, monthly_active: true })).status).toBe(503);
      quiet.mockRestore();
      expect(await rows()).toEqual([]);
    } finally {
      await sql.unsafe("DROP TRIGGER milo_test_fail ON milo_counts_monthly; DROP FUNCTION milo_test_fail()");
    }
  });

  test("accepts only today, two days back and one day ahead (UTC)", async () => {
    for (const day of ["2026-11-24", "2026-11-25", "2026-11-26", "2026-11-27"]) expect((await post({ ...valid, day })).status).toBe(204);
    for (const day of ["2026-11-23", "2026-11-28", "2025-11-26", "9999-12-31", "0000-01-01"]) expect((await post({ ...valid, day })).status).toBe(400);
    expect((await rows()).length).toBe(4);
  });

  test.each([
    ["extra key", { ...valid, id: "x" }],
    ["missing key", { day: valid.day, new_install: true }],
    ["string boolean", { ...valid, new_install: "true" }],
    ["number boolean", { ...valid, monthly_active: 1 }],
    ["null", { ...valid, monthly_active: null }],
    ["impossible date", { ...valid, day: "2026-02-30" }],
    ["timestamp", { ...valid, day: "2026-11-26T00:00:00Z" }],
    ["short date", { ...valid, day: "2026-1-26" }],
    ["number day", { ...valid, day: 20261126 }],
    ["array", [valid]],
    ["scalar", 7],
    ["not json", "{day"],
    ["empty", ""],
  ])("rejects %s without writing", async (_name, body) => {
    expect((await post(body as never)).status).toBe(400);
    expect(await rows()).toEqual([]);
  });

  test("rejects wrong content types, oversize bodies and other methods", async () => {
    expect((await post(valid, { "content-type": "text/plain" })).status).toBe(415);
    expect((await post(valid, {})).status).toBe(415);
    expect((await post(" ".repeat(200) + JSON.stringify(valid))).status).toBe(413);
    for (const method of ["GET", "PUT", "DELETE", "PATCH"]) {
      const res = await app().fetch(new Request("http://counts.test/v1/counts", { method }));
      expect(res.status).toBe(405);
      expect(res.headers.get("allow")).toBe("POST");
    }
    expect(await rows()).toEqual([]);
  });

  test("a fixed global limiter answers 429 with Retry-After and refills", async () => {
    const limited = createApp({ db, adminUser: user, adminPassword: password, now: () => clock, countsBurst: 3, countsPerSecond: 1 });
    const statuses = [];
    for (let i = 0; i < 5; i++) statuses.push((await post(valid, undefined, limited)).status);
    expect(statuses).toEqual([204, 204, 204, 429, 429]);
    const limitedResponse = await post(valid, undefined, limited);
    expect(limitedResponse.headers.get("retry-after")).toBe("1");
    clock += 2000;
    expect((await post(valid, undefined, limited)).status).toBe(204);
    expect((await rows())[0]!.a).toBe(4);
  });

  test("logs carry neither request data nor database messages", async () => {
    const failing = { ...db, add: async () => { throw Object.assign(new Error("secret-detail 2026-11-26"), { code: "XX000" }); } } as Db;
    const spy = spyOn(console, "error").mockImplementation(() => {});
    const res = await post(valid, undefined, createApp({ db: failing, adminUser: user, adminPassword: password, now: () => clock }));
    expect(res.status).toBe(503);
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
  });
});

describe("private dashboard", () => {
  const get = (path = "/", headers: Record<string, string> = auth(), a = app()) => a.fetch(new Request(`http://counts.test${path}`, { headers }));

  test("requires both the username and the password", async () => {
    for (const headers of [{}, auth("wrong", password), auth(user, "wrong"), auth("", password), auth(user, ""), { authorization: "Bearer x" }, { authorization: "Basic !!" }, { authorization: `Basic ${btoa(password)}` }]) {
      const res = await get("/", headers);
      expect(res.status).toBe(401);
      expect(res.headers.get("www-authenticate")).toContain("Basic");
      expect(res.headers.get("cache-control")).toContain("no-store");
      expect(await res.text()).not.toContain("Anonymous usage counts");
    }
    expect((await get()).status).toBe(200);
  });

  test("is never cached, framed, indexed or scripted", async () => {
    const res = await get();
    expect(res.headers.get("cache-control")).toBe("no-store, private, max-age=0");
    expect(res.headers.get("content-security-policy")).toContain("default-src 'none'");
    expect(res.headers.get("content-security-policy")).not.toContain("script");
    expect(res.headers.get("x-robots-tag")).toContain("noindex");
    expect(res.headers.get("x-frame-options")).toBe("DENY");
    const html = await res.text();
    expect(html).not.toContain("<script");
    expect(html).not.toContain("http://");
    expect(html).not.toContain("https://");
  });

  test("shows an empty state, then real totals zero-filled by day", async () => {
    expect(await (await get()).text()).toContain("No counts in the last 30 days");
    await post({ ...valid, new_install: true, monthly_active: true });
    await post({ ...valid, day: "2026-11-25" });
    await post({ ...valid, day: "2026-11-25" });
    const html = await (await get("/?days=7")).text();
    expect(html).not.toContain("No counts in the last");
    expect(html).toContain('<a href="/?days=7" aria-current="page">7 days</a>');
    expect(html).toContain("<dt>Active today (UTC)</dt><dd>1</dd>");
    expect(html).toContain("<dt>Active yesterday</dt><dd>2</dd>");
    expect(html).toContain("November 2026");
    expect((html.match(/<time /g) ?? []).length).toBe(7);
    expect((html.match(/<tr>/g) ?? []).length).toBe(1 + 7 + 1 + 6);
  });

  test("allows only the documented ranges and no other parameters", async () => {
    for (const q of ["?days=1", "?days=30&x=1", "?x=1", "?days=", "?days=30abc"]) expect((await get(`/${q}`)).status).toBe(400);
    for (const q of ["?days=7", "?days=30", "?days=90"]) expect((await get(`/${q}`)).status).toBe(200);
  });

  test("brand assets need the same credentials", async () => {
    for (const path of ["/assets/milo.webp", "/assets/meadow-desktop.webp", "/assets/meadow-mobile.webp", "/assets/favicon.png"]) {
      expect((await get(path, {})).status).toBe(401);
      const ok = await get(path);
      expect(ok.status).toBe(200);
      expect((await ok.arrayBuffer()).byteLength).toBeGreaterThan(1000);
    }
    expect((await get("/assets/other.webp")).status).toBe(404);
    expect((await get("/assets/../server.ts", auth())).status).toBe(404);
  });

  test("health and unknown paths", async () => {
    expect((await get("/healthz", {})).status).toBe(200);
    expect((await get("/readyz", {})).status).toBe(200);
    expect((await get("/admin", {})).status).toBe(404);
    expect((await get("/v1/counts/", {})).status).toBe(404);
    expect((await app().fetch(new Request("http://counts.test/", { method: "POST", headers: auth() }))).status).toBe(405);
  });
});

describe("over a real socket", () => {
  test("never redirects and caps bodies before the handler", async () => {
    const server = Bun.serve({ hostname: "127.0.0.1", port: 0, maxRequestBodySize: 1024, fetch: app().fetch });
    try {
      const base = `http://127.0.0.1:${server.port}`;
      for (const path of ["//", "/v1/counts/", "/index.html", "/?days=30", "/healthz/"]) {
        const res = await fetch(base + path, { redirect: "manual" });
        expect([200, 204, 401, 404, 405]).toContain(res.status);
        expect(res.headers.get("location")).toBeNull();
      }
      const post204 = await fetch(`${base}/v1/counts`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(valid), redirect: "manual" });
      expect(post204.status).toBe(204);
      const big = await fetch(`${base}/v1/counts`, { method: "POST", headers: { "content-type": "application/json" }, body: "x".repeat(5000), redirect: "manual" });
      expect(big.status).toBe(413);
      expect(await rows()).toEqual([{ day: "2026-11-26", a: 1, n: 0 }]);
    } finally {
      await server.stop(true);
    }
  });
});

describe("configuration", () => {
  const good = { MILO_COUNTS_ADMIN_USER: user, MILO_COUNTS_ADMIN_PASSWORD: password };
  test("fails closed unless both credentials are set and strong", () => {
    expect(parseCredentials(good)).toEqual({ adminUser: user, adminPassword: password });
    expect(() => parseCredentials({})).toThrow();
    expect(() => parseCredentials({ MILO_COUNTS_ADMIN_USER: user })).toThrow();
    expect(() => parseCredentials({ MILO_COUNTS_ADMIN_PASSWORD: password })).toThrow();
    expect(() => parseCredentials({ ...good, MILO_COUNTS_ADMIN_USER: "admin" })).toThrow();
    expect(() => parseCredentials({ ...good, MILO_COUNTS_ADMIN_PASSWORD: "short" })).toThrow();
    expect(() => parseCredentials({ ...good, MILO_COUNTS_ADMIN_PASSWORD: "has space ".repeat(5) })).toThrow();
  });
  test("the token bucket is global and time-based", () => {
    let t = 0;
    const bucket = new Bucket(2, 1, () => t);
    expect([bucket.take(), bucket.take(), bucket.take()]).toEqual([true, true, false]);
    t = 1000;
    expect(bucket.take()).toBe(true);
  });
});
