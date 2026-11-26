import { createHash, timingSafeEqual } from "node:crypto";
import { openDb, type Db } from "./db";
import { RANGES, renderDashboard } from "./page";

export const MAX_BODY = 128;
export const WINDOW_DAYS_BACK = 2;
export const WINDOW_DAYS_FORWARD = 1;
const dayMs = 86_400_000;

export class Bucket {
  private tokens: number;
  private last: number;
  constructor(
    private readonly burst: number,
    private readonly perSecond: number,
    private readonly now: () => number,
  ) {
    this.tokens = burst;
    this.last = now();
  }
  take(): boolean {
    const t = this.now();
    this.tokens = Math.min(this.burst, this.tokens + ((t - this.last) / 1000) * this.perSecond);
    this.last = t;
    if (this.tokens < 1) return false;
    this.tokens -= 1;
    return true;
  }
}

export type AppOptions = {
  db: Db;
  adminUser: string;
  adminPassword: string;
  now?: () => number;
  countsBurst?: number;
  countsPerSecond?: number;
};

const assets: Record<string, [string, string]> = {
  "/assets/milo.webp": ["milo.webp", "image/webp"],
  "/assets/favicon.png": ["favicon.png", "image/png"],
  "/assets/meadow-desktop.webp": ["meadow-desktop.webp", "image/webp"],
  "/assets/meadow-mobile.webp": ["meadow-mobile.webp", "image/webp"],
};
const digest = (value: string) => createHash("sha256").update(value).digest();
const dayOf = (ms: number) => new Date(ms).toISOString().slice(0, 10);

export function parseCredentials(env: Record<string, string | undefined>) {
  const user = env.MILO_COUNTS_ADMIN_USER;
  const password = env.MILO_COUNTS_ADMIN_PASSWORD;
  if (!user || !password) throw new Error("Set both MILO_COUNTS_ADMIN_USER and MILO_COUNTS_ADMIN_PASSWORD.");
  if (!/^[A-Za-z0-9_-]{16,128}$/.test(user)) throw new Error("MILO_COUNTS_ADMIN_USER must be 16 to 128 letters, digits, hyphens or underscores.");
  if (!/^[\x21-\x7e]{32,256}$/.test(password)) throw new Error("MILO_COUNTS_ADMIN_PASSWORD must be 32 to 256 printable ASCII characters.");
  return { adminUser: user, adminPassword: password };
}

function parseCounts(text: string, today: string) {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return undefined;
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
  const keys = Object.keys(value);
  if (keys.length !== 3 || !["day", "new_install", "monthly_active"].every((k) => keys.includes(k))) return undefined;
  const { day, new_install: newInstall, monthly_active: monthlyActive } = value as Record<string, unknown>;
  if (typeof newInstall !== "boolean" || typeof monthlyActive !== "boolean") return undefined;
  if (typeof day !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(day)) return undefined;
  const ms = Date.parse(`${day}T00:00:00Z`);
  if (Number.isNaN(ms) || dayOf(ms) !== day) return undefined;
  const age = (Date.parse(`${today}T00:00:00Z`) - ms) / dayMs;
  if (age > WINDOW_DAYS_BACK || age < -WINDOW_DAYS_FORWARD) return undefined;
  return { day, newInstall, monthlyActive };
}

export function createApp(options: AppOptions) {
  const { db, adminUser, adminPassword } = options;
  const now = options.now ?? Date.now;
  const counts = new Bucket(options.countsBurst ?? 200, options.countsPerSecond ?? 50, now);
  const userDigest = digest(adminUser);
  const passwordDigest = digest(adminPassword);

  const text = (status: number, body: string, headers: Record<string, string> = {}) =>
    new Response(body, { status, headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-store", "x-content-type-options": "nosniff", ...headers } });

  const authorized = (req: Request) => {
    const header = req.headers.get("authorization") ?? "";
    const match = /^Basic ([A-Za-z0-9+/=]{1,1024})$/.exec(header);
    const decoded = match ? Buffer.from(match[1]!, "base64").toString("utf8") : "";
    const split = decoded.indexOf(":");
    const user = split < 0 ? "" : decoded.slice(0, split);
    const password = split < 0 ? "" : decoded.slice(split + 1);
    const userOk = timingSafeEqual(digest(user), userDigest);
    const passwordOk = timingSafeEqual(digest(password), passwordDigest);
    return split >= 0 && userOk && passwordOk;
  };

  async function postCounts(req: Request) {
    if (!counts.take()) return text(429, "Too many requests.\n", { "retry-after": "1" });
    if (!/^application\/json\s*(;|$)/i.test(req.headers.get("content-type") ?? "")) return text(415, "Send application/json.\n");
    const declared = req.headers.get("content-length");
    if (declared !== null && Number(declared) > MAX_BODY) return text(413, "Payload too large.\n");
    const bytes = new Uint8Array(await req.arrayBuffer());
    if (bytes.length > MAX_BODY) return text(413, "Payload too large.\n");
    const parsed = parseCounts(new TextDecoder("utf-8", { fatal: false }).decode(bytes), dayOf(now()));
    if (!parsed) return text(400, "Invalid counts.\n");
    try {
      await db.add(parsed);
    } catch {
      return text(503, "Unavailable.\n", { "retry-after": "5" });
    }
    return new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
  }

  async function dashboard(req: Request, url: URL) {
    if (!authorized(req)) {
      return text(401, "Authentication required.\n", { "www-authenticate": 'Basic realm="Milo counts", charset="UTF-8"' });
    }
    const params = [...url.searchParams.keys()];
    const requested = url.searchParams.get("days");
    const range = requested === null ? 30 : Number(requested);
    if (params.some((k) => k !== "days") || params.length > 1 || !(RANGES as readonly number[]).includes(range)) return text(400, "Use ?days=7, 30 or 90.\n");
    const todayMs = Date.parse(`${dayOf(now())}T00:00:00Z`);
    const today = dayOf(todayMs);
    const monthStarts = Array.from({ length: 6 }, (_, i) => {
      const d = new Date(todayMs);
      return dayOf(Date.UTC(d.getUTCFullYear(), d.getUTCMonth() - (5 - i), 1));
    });
    const days = Array.from({ length: range }, (_, i) => dayOf(todayMs - (range - 1 - i) * dayMs));
    try {
      const [stored, storedMonths, totalNewInstalls] = await Promise.all([
        db.range(days[0]!, today),
        db.months(monthStarts[0]!, monthStarts[5]!),
        db.totalNewInstalls(),
      ]);
      const byDay = new Map(stored.map((r) => [r.day, r]));
      const rows = days.map((day) => byDay.get(day) ?? { day, dailyActive: 0, newInstalls: 0 });
      const byMonth = new Map(storedMonths.map((m) => [m.month, m]));
      const months = monthStarts.map((month) => byMonth.get(month) ?? { month, monthlyActive: 0 });
      const nonce = Buffer.from(crypto.getRandomValues(new Uint8Array(16))).toString("base64");
      const html = renderDashboard({ rows, range, today, months, totalNewInstalls, nonce });
      return new Response(html, {
        headers: {
          "content-type": "text/html; charset=utf-8",
          "cache-control": "no-store, private, max-age=0",
          pragma: "no-cache",
          "content-security-policy": `default-src 'none'; style-src 'nonce-${nonce}'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`,
          "x-content-type-options": "nosniff",
          "x-frame-options": "DENY",
          "referrer-policy": "no-referrer",
          "x-robots-tag": "noindex, nofollow, noarchive",
        },
      });
    } catch {
      return text(503, "Unavailable.\n", { "retry-after": "5" });
    }
  }

  return {
    async fetch(req: Request): Promise<Response> {
      const url = new URL(req.url);
      const { pathname } = url;
      if (pathname === "/healthz") return req.method === "GET" || req.method === "HEAD" ? text(200, "ok\n") : text(405, "Method not allowed.\n", { allow: "GET, HEAD" });
      if (pathname === "/readyz") {
        try {
          await db.ping();
          return text(200, "ready\n");
        } catch {
          return text(503, "not ready\n");
        }
      }
      if (pathname === "/v1/counts") return req.method === "POST" ? postCounts(req) : text(405, "Method not allowed.\n", { allow: "POST" });
      if (pathname in assets) {
        if (req.method !== "GET" && req.method !== "HEAD") return text(405, "Method not allowed.\n", { allow: "GET, HEAD" });
        if (!authorized(req)) return text(401, "Authentication required.\n", { "www-authenticate": 'Basic realm="Milo counts", charset="UTF-8"' });
        const [file, type] = assets[pathname]!;
        return new Response(Bun.file(new URL(`./assets/${file}`, import.meta.url)), { headers: { "content-type": type, "cache-control": "private, max-age=3600", "x-content-type-options": "nosniff" } });
      }
      if (pathname === "/") return req.method === "GET" || req.method === "HEAD" ? dashboard(req, url) : text(405, "Method not allowed.\n", { allow: "GET, HEAD" });
      return text(404, "Not found.\n");
    },
  };
}

async function main() {
  const env = Bun.env;
  const databaseUrl = env.DATABASE_URL;
  if (!databaseUrl) throw new Error("Set DATABASE_URL.");
  const credentials = parseCredentials(env);
  const port = Number(env.PORT ?? 6130);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("PORT must be an integer from 1 through 65535.");
  const db = openDb(databaseUrl);
  await db.migrate();
  const app = createApp({ db, ...credentials });
  const server = Bun.serve({
    hostname: env.MILO_COUNTS_HOST ?? "127.0.0.1", port, maxRequestBodySize: 1024, idleTimeout: 10,
    development: false, fetch: app.fetch,
    error: () => new Response("Unavailable.\n", { status: 503, headers: { "cache-control": "no-store" } }),
  });
  console.log(`milo counts listening on port ${server.port}`);
  const stop = async () => {
    await server.stop();
    await db.close();
    process.exit(0);
  };
  process.on("SIGTERM", stop);
  process.on("SIGINT", stop);
}

if (import.meta.main) {
  main().catch(() => {
    console.error("Milo counts failed to start. Check the database, port and both admin variables.");
    process.exit(1);
  });
}
