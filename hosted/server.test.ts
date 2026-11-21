import { expect, test } from "bun:test";
import { chmodSync, linkSync, mkdtempSync, readdirSync, renameSync, rmSync, statSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openBindings } from "./bindings";
import { healthResponse, parseRelayConfig, startRelay } from "./server";
import { telegramApi, verifyTelegramBot, type ProviderFetch } from "./providers";

const origin = "https://connect.usemilo.net";
const production = { MILO_RELAY_ORIGIN: origin, PORT: "8080", MILO_RELAY_DATA_DIR: "/data" };
const telegram = { MILO_TELEGRAM_BOT_TOKEN: "123:synthetic-fixture" };
const discord = { MILO_DISCORD_CLIENT_ID: "1234", MILO_DISCORD_CLIENT_SECRET: "synthetic-fixture" };

function relayTemp() {
  return mkdtempSync(join(tmpdir(), "milo-relay-runtime-"));
}

test("local defaults stay loopback, ignore platform PORT and opt out of polling", () => {
  expect(parseRelayConfig({ PORT: "443", ...telegram })).toMatchObject({
    hostname: "127.0.0.1", port: 6120, origin: undefined, telegram: undefined,
  });
  expect(parseRelayConfig({ ...telegram, MILO_TELEGRAM_LONG_POLL: "1" }).telegram?.token)
    .toBe(telegram.MILO_TELEGRAM_BOT_TOKEN);
  const local = "http://localhost:6120/discord/callback";
  expect(parseRelayConfig({ ...discord, MILO_DISCORD_REDIRECT_URI: local }).discord?.redirectUri).toBe(local);
  expect(() => parseRelayConfig({ ...discord, MILO_DISCORD_REDIRECT_URI: `${origin}/discord/callback` }))
    .toThrow(/exact Discord callback/);
});

test("production requires the exact origin, a valid listen port and a volume path", () => {
  expect(parseRelayConfig({ ...production, ...discord })).toMatchObject({
    hostname: "0.0.0.0", port: 8080, origin, dataDirectory: "/data",
    discord: { redirectUri: `${origin}/discord/callback` },
  });
  for (const value of ["", `${origin}/`, `${origin}:443`, "http://connect.usemilo.net", "https://other.example"]) {
    expect(() => parseRelayConfig({ ...production, ...discord, MILO_RELAY_ORIGIN: value })).toThrow(/MILO_RELAY_ORIGIN/);
  }
  for (const value of [undefined, "0", " 8080", "08080", "1e3", "65536", "-1"]) {
    expect(() => parseRelayConfig({ ...production, ...discord, PORT: value })).toThrow(/PORT/);
  }
  for (const value of [undefined, "data", "/", "/data/", "/tmp/../data"]) {
    expect(() => parseRelayConfig({ ...production, ...discord, MILO_RELAY_DATA_DIR: value })).toThrow(/MILO_RELAY_DATA_DIR/);
  }
  expect(() => parseRelayConfig({ ...production, ...discord, RAILWAY_VOLUME_MOUNT_PATH: "/other" }))
    .toThrow(/MILO_RELAY_DATA_DIR/);
});

test("production rejects incomplete providers and unsafe fetch settings", () => {
  expect(() => parseRelayConfig(production)).toThrow(/at least one provider/);
  expect(parseRelayConfig({ ...production, ...telegram }).telegram?.token).toBe(telegram.MILO_TELEGRAM_BOT_TOKEN);
  expect(() => parseRelayConfig({ ...production, ...telegram, MILO_DISCORD_CLIENT_ID: "1234" })).toThrow(/CLIENT_SECRET/);
  expect(() => parseRelayConfig({ ...production, ...discord, MILO_DISCORD_REDIRECT_URI: "http://localhost:6120/discord/callback" }))
    .toThrow(/exact Discord callback/);
  for (const token of ["", "synthetic-fixture", "9999999999999999:fixture"]) {
    expect(() => parseRelayConfig({ ...production, ...discord, MILO_TELEGRAM_BOT_TOKEN: token })).toThrow(/BOT_TOKEN/);
  }
  expect(() => parseRelayConfig({ ...production, ...telegram, MILO_TELEGRAM_LONG_POLL: "0" })).toThrow(/BOT_TOKEN/);
  expect(() => parseRelayConfig({ ...production, ...telegram, NODE_TLS_REJECT_UNAUTHORIZED: "0" })).toThrow(/TLS verification/);
  expect(() => parseRelayConfig({ ...production, ...telegram, BUN_CONFIG_VERBOSE_FETCH: "curl" })).toThrow(/logging disabled/);
});

test("production will not poll or initialize SQLite in an unmounted directory", () => {
  const directory = relayTemp();
  let providerCalls = 0;
  try {
    const config = parseRelayConfig({ ...production, ...telegram, MILO_RELAY_DATA_DIR: directory });
    expect(() => startRelay(config, { fetch: async () => { providerCalls++; throw new Error("Unexpected provider call"); } }))
      .toThrow(/persistent volume/);
    expect(providerCalls).toBe(0);
    expect(readdirSync(directory)).toEqual([]);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("Railway health host is a GET readiness exception only", async () => {
  const railway = "healthcheck.railway.app";
  const probe = (host: string, path: string, ready: boolean, method = "GET", header = host) =>
    healthResponse(new Request(`http://${host}${path}`, { method, headers: { Host: header } }), true, ready);
  const startup = probe(railway, "/readyz", false)!;
  expect(startup.status).toBe(503);
  expect(startup.headers.get("Cache-Control")).toBe("no-store");
  expect(await startup.json()).toEqual({ ok: false });
  expect(probe(railway, "/readyz", true)?.status).toBe(200);
  expect(probe("connect.usemilo.net", "/healthz", false)?.status).toBe(200);
  expect(probe("connect.usemilo.net", "/readyz", false)?.status).toBe(503);
  expect(probe("other.example", "/readyz", true)).toBeUndefined();
  expect(probe(railway, "/readyz", true, "POST")).toBeUndefined();
  expect(probe(railway, "/readyz", true, "GET", "other.example")).toBeUndefined();
  for (const path of ["/healthz", "/v1/pair", "/discord/callback", "/readyz?extra=1"]) {
    expect(probe(railway, path, true)).toBeUndefined();
  }
});

test("SQLite bindings persist with private permissions and bounded reads", () => {
  const directory = relayTemp();
  const path = join(directory, "telegram-123.sqlite");
  try {
    const store = openBindings("123", directory);
    try {
      store.insert("digest-a", 123);
      store.insert("digest-b", 456);
    } finally {
      store.close();
    }
    expect(statSync(directory).mode & 0o7777).toBe(0o700);
    expect(statSync(path).mode & 0o7777).toBe(0o600);
    chmodSync(directory, 0o755);
    chmodSync(path, 0o644);
    const reopened = openBindings("123", directory);
    expect(reopened.issuer).toMatch(/^[A-Za-z0-9_-]{16}$/);
    expect(reopened.issuer).toBe(store.issuer);
    try {
      expect(reopened.load(1)).toHaveLength(1);
      expect(reopened.load(2).map(({ digest }) => digest).sort()).toEqual(["digest-a", "digest-b"]);
      expect(statSync(directory).mode & 0o7777).toBe(0o700);
      expect(statSync(path).mode & 0o7777).toBe(0o600);
    } finally {
      reopened.close();
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("only the current matching SQLite ledger can confirm revocation", () => {
  const directory = relayTemp();
  const path = join(directory, "telegram-123.sqlite");
  const ledger = openBindings("123", directory);
  const otherBot = openBindings("234", directory);
  try {
    ledger.insert("same-digest", 123);
    otherBot.insert("same-digest", 234);
    expect(otherBot.issuer).not.toBe(ledger.issuer);
    expect(otherBot.removeIssued("same-digest", ledger.issuer)).toBe(false);
    expect(otherBot.load(1)).toEqual([{ digest: "same-digest", chat: 234 }]);
    expect(ledger.removeIssued("same-digest", ledger.issuer)).toBe(true);
    expect(ledger.load(1)).toEqual([]);
    expect(ledger.removeIssued("same-digest", ledger.issuer)).toBe(true);
    renameSync(path, `${path}.detached`);
    const replaced = openBindings("123", directory);
    try {
      expect(replaced.issuer).not.toBe(ledger.issuer);
      expect(ledger.removeIssued("same-digest", ledger.issuer)).toBe(false);
      expect(() => ledger.insert("new-digest", 123)).toThrow(/ledger no longer matches/);
      expect(() => ledger.revoke(123)).toThrow(/ledger no longer matches/);
      expect(replaced.removeIssued("same-digest", ledger.issuer)).toBe(false);
    } finally {
      replaced.close();
    }
  } finally {
    ledger.close();
    otherBot.close();
    rmSync(directory, { recursive: true, force: true });
  }
});

test("the binding store refuses symlinks and hard-linked state files", () => {
  const directory = relayTemp();
  try {
    openBindings("123", directory).close();
    const path = join(directory, "telegram-123.sqlite");
    const alias = join(directory, "telegram-234.sqlite");
    symlinkSync(path, alias);
    expect(() => openBindings("234", directory)).toThrow();
    rmSync(alias);
    linkSync(path, alias);
    expect(() => openBindings("234", directory)).toThrow();
    rmSync(alias);
    symlinkSync(path, `${path}-journal`);
    expect(() => openBindings("123", directory)).toThrow();
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

function telegramTransport(
  updates: (signal: AbortSignal) => Promise<Response>, send?: (signal: AbortSignal) => Promise<Response>,
): ProviderFetch {
  return async (url, init) => {
    switch (new URL(url).pathname.split("/").at(-1)) {
      case "getWebhookInfo": return Response.json({ ok: true, result: { url: "" } });
      case "getMe": return Response.json({ ok: true, result: { id: 123, is_bot: true, username: "MiloFixtureBot" } });
      case "getUpdates": return updates(init.signal!);
      case "sendMessage": if (send) return send(init.signal!); break;
      default: break;
    }
    throw new Error("Unexpected provider call");
  };
}

test("startup hints a retry for pre-send Telegram requests only", async () => {
  const directory = relayTemp();
  const bootstrap = Promise.withResolvers<Response>();
  const config = { ...parseRelayConfig({ ...telegram, MILO_TELEGRAM_LONG_POLL: "1" }), port: 0, dataDirectory: directory };
  const runtime = startRelay(config, { fetch: () => bootstrap.promise, report: () => {} });
  try {
    const send = await fetch(new URL("/v1/send", runtime.server.url), { method: "POST" });
    expect(send.status).toBe(503);
    expect(send.headers.get("retry-after")).toBe("5");
    expect(await send.json()).toHaveProperty("retry_after", 5);
    const binding = await fetch(new URL("/v1/binding", runtime.server.url), { method: "DELETE" });
    expect(binding.status).toBe(503);
    expect(binding.headers.get("retry-after")).toBeNull();
    expect(await binding.json()).not.toHaveProperty("retry_after");
  } finally {
    bootstrap.resolve(Response.json({ ok: true, result: { url: "https://fixture.invalid" } }));
    await runtime.stop();
    rmSync(directory, { recursive: true, force: true });
  }
});

test("readiness waits for catch-up, expires, recovers and polling aborts on shutdown", async () => {
  const directory = relayTemp();
  let time = 1000;
  let next = 0;
  const replyStarted = Promise.withResolvers<void>();
  const reply = Promise.withResolvers<Response>();
  const polls = Array.from({ length: 5 }, () => Promise.withResolvers<{
    signal: AbortSignal;
    succeed(updates?: unknown[]): void;
  }>());
  const config = { ...parseRelayConfig({ ...telegram, MILO_TELEGRAM_LONG_POLL: "1" }), port: 0, dataDirectory: directory };
  const runtime = startRelay(config, {
    now: () => time,
    fetch: telegramTransport((signal) => {
      const { promise, resolve, reject } = Promise.withResolvers<Response>();
      signal.addEventListener("abort", () => reject(signal.reason), { once: true });
      polls[next++].resolve({
        signal, succeed: (result = []) => resolve(Response.json({ ok: true, result })),
      });
      return promise;
    }, (signal) => {
      signal.addEventListener("abort", () => reply.reject(signal.reason), { once: true });
      replyStarted.resolve();
      return reply.promise;
    }),
  });
  const probe = async (path: string) => {
    const response = await fetch(new URL(path, runtime.server.url));
    await response.text();
    return response.status;
  };
  try {
    const initialPoll = await polls[0].promise;
    expect(await probe("/healthz")).toBe(200);
    expect(await probe("/readyz")).toBe(503);
    initialPoll.succeed();
    const nextPoll = await polls[1].promise;
    expect(await probe("/readyz")).toBe(200);
    nextPoll.succeed([
      { update_id: 1, message: { text: "/help", from: { id: 123, is_bot: false }, chat: { id: 123, type: "private" } } },
      ...Array.from({ length: 19 }, (_, i) => ({ update_id: i + 2 })),
    ]);
    await replyStarted.promise;
    expect(await probe("/readyz")).toBe(503);
    reply.resolve(Response.json({ ok: true, result: true }));
    const fullPageFollowup = await polls[2].promise;
    expect(await probe("/readyz")).toBe(503);
    fullPageFollowup.succeed();
    const freshPoll = await polls[3].promise;
    expect(await probe("/readyz")).toBe(200);
    time += 60_001;
    expect(await probe("/healthz")).toBe(200);
    expect(await probe("/readyz")).toBe(503);
    freshPoll.succeed();
    const lastPoll = await polls[4].promise;
    expect(await probe("/readyz")).toBe(200);
    await runtime.stop();
    expect(lastPoll.signal.aborted).toBe(true);
    expect(next).toBe(5);
  } finally {
    await runtime.stop();
    rmSync(directory, { recursive: true, force: true });
  }
});

test("transient poll failures immediately lose readiness, and shutdown interrupts retry backoff", async () => {
  const directory = relayTemp();
  const first = Promise.withResolvers<void>();
  const next = Promise.withResolvers<void>();
  const rejectPoll = Promise.withResolvers<Response>();
  let polls = 0;
  const config = { ...parseRelayConfig({ ...telegram, MILO_TELEGRAM_LONG_POLL: "1" }), port: 0, dataDirectory: directory };
  const runtime = startRelay(config, {
    fetch: telegramTransport(async (signal) => {
      if (++polls === 1) return Response.json({ ok: true, result: [] });
      signal.addEventListener("abort", () => rejectPoll.reject(signal.reason), { once: true });
      next.resolve();
      return rejectPoll.promise;
    }),
  });
  const probe = async () => {
    const response = await fetch(new URL("/readyz", runtime.server.url));
    await response.text();
    return response.status;
  };
  try {
    await next.promise;
    expect(await probe()).toBe(200);
    const response = new Response(null, { status: 429 });
    response.json = async () => {
      // Resume the test after the retry response has propagated through the poll loop.
      setImmediate(first.resolve);
      return { ok: false, error_code: 429, parameters: { retry_after: 3600 } };
    };
    rejectPoll.resolve(response);
    await first.promise;
    expect(await probe()).toBe(503);
    await runtime.stop();
    expect(polls).toBe(2);
  } finally {
    await runtime.stop();
    rmSync(directory, { recursive: true, force: true });
  }
});

test("the canonical relay refuses Railway preview or unnamed deployment environments", () => {
  expect(parseRelayConfig({ ...production, ...telegram, RAILWAY_ENVIRONMENT_NAME: "production" }).origin).toBe(origin);
  for (const name of [undefined, "", "staging", "production-pr-7"]) {
    expect(() => parseRelayConfig({
      ...production, ...telegram, RAILWAY_ENVIRONMENT_ID: "fixture", RAILWAY_ENVIRONMENT_NAME: name,
    })).toThrow(/Railway production environment/);
  }
});

test("Telegram bootstrap retries network, 5xx and provider backoff before both probes succeed", async () => {
  const methods: string[] = [];
  const waits: number[] = [];
  const reports: string[] = [];
  const transport = telegramApi(async (url) => {
    const method = new URL(url).pathname.split("/").at(-1)!;
    methods.push(method);
    switch (methods.length) {
      case 1: throw new Error("Synthetic network failure");
      case 2: return new Response(null, { status: 503 });
      case 3: return Response.json({ ok: true, result: { url: "" } });
      case 4: return Response.json({
        ok: false, error_code: 429, parameters: { retry_after: 7 },
      }, { status: 429 });
      default: return Response.json({ ok: true, result: { id: 123, is_bot: true, username: "MiloFixtureBot" } });
    }
  }, telegram.MILO_TELEGRAM_BOT_TOKEN);
  const username = await verifyTelegramBot(transport, 123, {
    report: (message) => reports.push(message),
    wait: async (delay) => { waits.push(delay); },
  });
  expect(username).toBe("MiloFixtureBot");
  expect(methods).toEqual(["getWebhookInfo", "getWebhookInfo", "getWebhookInfo", "getMe", "getMe"]);
  expect(waits).toEqual([1000, 2000, 7000]);
  expect(reports).toEqual([]);
});

test("Telegram bootstrap bounds retries and never retries definitive conflicts or invalid identities", async () => {
  for (const [webhook, status, expectedCalls, expectedWaits] of [
    [undefined, 503, 3, 2],
    [undefined, 409, 1, 0],
    [undefined, 401, 1, 0],
    ["https://fixture.invalid/registered-webhook", 200, 1, 0],
    ["", 200, 2, 0],
  ] as const) {
    let calls = 0;
    let waits = 0;
    const reports: string[] = [];
    const result = await verifyTelegramBot(async (method) => {
      calls++;
      if (method === "getMe") return { status: 200, result: { id: 999, is_bot: true, username: "WrongFixtureBot" } };
      return { status, result: { url: webhook } };
    }, 123, { report: (message) => reports.push(message), wait: async () => { waits++; } });
    expect(result).toBeUndefined();
    expect(calls).toBe(expectedCalls);
    expect(waits).toBe(expectedWaits);
    expect(reports).toHaveLength(1);
  }
});

test("shutdown cancels Telegram bootstrap backoff without another provider call", async () => {
  const controller = new AbortController();
  const waiting = Promise.withResolvers<AbortSignal>();
  let calls = 0;
  const reports: string[] = [];
  const verification = verifyTelegramBot(async () => {
    calls++;
    return { status: 429, retryAfter: 3600 };
  }, 123, {
    signal: controller.signal,
    report: (message) => reports.push(message),
    wait: (_, signal) => {
      const { promise, reject } = Promise.withResolvers<void>();
      signal.addEventListener("abort", () => reject(signal.reason), { once: true });
      waiting.resolve(signal);
      return promise;
    },
  });
  const signal = await waiting.promise;
  controller.abort();
  expect(await verification).toBeUndefined();
  expect(signal.aborted).toBe(true);
  expect(calls).toBe(1);
  expect(reports).toEqual([]);
});
