import { strict as assert } from "node:assert";
import { createHash } from "node:crypto";
import { readdirSync } from "node:fs";
import { setTimeout as sleep } from "node:timers/promises";
import { Database } from "bun:sqlite";
import { openBindings, verifyDataMount } from "./bindings";
import type { ProviderFetch } from "./providers";
import { parseRelayConfig, startRelay } from "./server";
import { PRODUCTION_ORIGIN } from "./service";

const directory = "/data";
const host = new URL(PRODUCTION_ORIGIN).host;
const token = "4242:synthetic-relay-benchmark";
const botId = "4242";
const idleMs = 55_000; // Eleven 5 s sweep intervals and at least two 25 s long polls.
const providerDelayMs = 25;
const bindingCount = 10_000;
const shortBody = JSON.stringify({ text: "Rex: a terminal finished" });
const largeBody = JSON.stringify({ text: `Rex: a terminal finished\n<code>${"&amp;".repeat(3000)}</code>` });
type Relay = { server: { url: URL }; stop(): Promise<void> };
const initialCounts = { bootCalls: 0, immediatePolls: 0, longPolls: 0, pollAborts: 0, sendCalls: 0, discordExchanges: 0 };
type Providers = {
  fetch: ProviderFetch;
  counts: typeof initialCounts;
  state: { gate?: Promise<void>; sendStatus: number };
};
type Stop = (phase?: string) => Promise<void>;

function output(phase: string, details: object) {
  console.log(JSON.stringify({ phase, ...details }));
}

function snapshot() {
  return { at: performance.now(), rss: process.memoryUsage().rss, ...process.resourceUsage() };
}

async function measure<T>(phase: string, task: () => Promise<T>, details?: (result: T) => object) {
  const start = snapshot();
  const result = await task();
  const end = snapshot();
  output(phase, {
    wallMs: end.at - start.at,
    userCpuMs: (end.userCPUTime - start.userCPUTime) / 1000,
    systemCpuMs: (end.systemCPUTime - start.systemCPUTime) / 1000,
    rssBeforeBytes: start.rss, rssAfterBytes: end.rss,
    fsReadBlocks: end.fsRead - start.fsRead, fsWriteBlocks: end.fsWrite - start.fsWrite,
    voluntarySwitches: end.voluntaryContextSwitches - start.voluntaryContextSwitches,
    involuntarySwitches: end.involuntaryContextSwitches - start.involuntaryContextSwitches,
    ...details?.(result),
  });
  return result;
}

const credential = (issuer: string, index: number) => `milorelay_prod_${issuer}${index.toString(36).padStart(27, "0")}`;
function seed() {
  const ledger = openBindings(botId, directory);
  const { issuer } = ledger;
  ledger.close();
  // Use one transaction outside the sample; runtime reads and mutations use the real ledger.
  using db = new Database(`${directory}/telegram-${botId}.sqlite`, { create: false });
  const insert = db.query("INSERT INTO bindings(digest, chat) VALUES (?, ?)");
  db.transaction(() => {
    for (let i = 0; i < bindingCount; i++) {
      const digest = createHash("sha256").update(credential(issuer, i)).digest("hex");
      insert.run(digest, i + 1);
    }
  }).immediate();
  return issuer;
}

function providers(backlog = 0): Providers {
  const counts = { ...initialCounts };
  const state: Providers["state"] = { sendStatus: 200 };
  let offset = 0;
  const success = (result: unknown) => Response.json({ ok: true, result });
  const fetch: ProviderFetch = async (url, init) => {
    if (url === "https://discord.com/api/v10/oauth2/token" && init.method === "POST") {
      counts.discordExchanges++;
      return Response.json({ webhook: {
        type: 1, application_id: "12345", id: "123456", token: "synthetic-webhook-fixture", channel_id: "1234567",
      } });
    }
    const prefix = `https://api.telegram.org/bot${token}/`;
    assert.ok(url.startsWith(prefix), "Unexpected provider URL");
    switch (url.slice(prefix.length)) {
      case "getWebhookInfo": counts.bootCalls++; return success({ url: "" });
      case "getMe": counts.bootCalls++; return success({ id: Number(botId), is_bot: true, username: "MiloFixtureBot" });
      case "getUpdates": {
        const request = JSON.parse(init.body as string);
        assert.equal(request.offset, offset || undefined);
        if (request.timeout === 0) {
          counts.immediatePolls++;
          const updates = Array.from({ length: Math.min(20, backlog) }, () => ({ update_id: offset++ }));
          backlog -= updates.length;
          return success(updates);
        }
        assert.equal(request.timeout, 25);
        counts.longPolls++;
        try {
          await sleep(25_000, undefined, { signal: init.signal! });
        } catch (error) {
          counts.pollAborts++;
          throw error;
        }
        return success([]);
      }
      case "sendMessage":
        counts.sendCalls++;
        if (state.gate) await state.gate;
        else await sleep(providerDelayMs, undefined, { signal: init.signal! });
        if (state.sendStatus === 429) return Response.json({ ok: false, error_code: 429, parameters: { retry_after: 3 } }, { status: 429 });
        return state.sendStatus === 200 ? success({}) : new Response(null, { status: state.sendStatus });
      default: throw new Error("Unexpected provider operation");
    }
  };
  return { fetch, counts, state };
}

function request(relay: Relay, path: string, { ip = 0, ...init }: RequestInit & { ip?: number } = {}) {
  const headers = new Headers(init.headers);
  headers.set("Host", host);
  headers.set("X-Real-IP", `198.18.${Math.floor(ip / 254)}.${ip % 254 + 1}`);
  return fetch(new URL(path, relay.server.url), { ...init, headers, redirect: "manual" });
}

async function expectStatus(response: Response, status: number, retry?: number) {
  assert.equal(response.status, status);
  if (retry === undefined) return response.text();
  assert.equal(response.headers.get("retry-after"), String(retry));
  assert.equal((await response.json()).retry_after, retry);
}

async function waitUntil(ready: () => boolean | Promise<boolean>) {
  const deadline = performance.now() + 5_000;
  while (!(await ready())) {
    assert.ok(performance.now() < deadline, "Relay benchmark timed out waiting for its fixture");
    await sleep(1);
  }
}

async function lifecycle(label: string, fixture: Providers, telegram: boolean, work?: (relay: Relay, stop: Stop) => Promise<void>) {
  const config = parseRelayConfig({
    MILO_RELAY_ORIGIN: PRODUCTION_ORIGIN, MILO_RELAY_DATA_DIR: directory, PORT: "8080",
    ...(telegram ? { MILO_TELEGRAM_BOT_TOKEN: token } : {
      MILO_DISCORD_CLIENT_ID: "12345", MILO_DISCORD_CLIENT_SECRET: "synthetic-relay-benchmark",
    }),
  });
  let relay: Relay | undefined;
  let shutdown: Promise<void> | undefined;
  const stop: Stop = (phase = `${label}.shutdown`) => shutdown ??=
    measure(phase, () => relay!.stop(), () => ({ pollAborts: fixture.counts.pollAborts }));
  let runError: unknown;
  const reports: string[] = [];
  try {
    await measure(`${label}.startup`, async () => {
      const start = performance.now();
      const running = relay = startRelay({ ...config, hostname: "127.0.0.1", port: 0 }, {
        fetch: fixture.fetch, report: (message) => reports.push(message),
      });
      const listenMs = performance.now() - start;
      void running.run.catch((error) => { runError = error; });
      let readinessProbes = 0;
      await waitUntil(async () => {
        if (runError) throw runError;
        readinessProbes++;
        const response = await request(running, "/readyz", { headers: { Connection: "close" } });
        assert.ok(response.status === 200 || response.status === 503);
        await response.text();
        return response.status === 200;
      });
      return { listenMs, readinessProbes, ...fixture.counts };
    }, (details) => details);
    await work?.(relay!, stop);
  } finally {
    if (relay) await stop();
  }
  if (runError) throw runError;
  assert.deepEqual(reports, []);
}

async function idle(label: string, fixture: Providers, duration: number) {
  const before = { ...fixture.counts };
  await measure(label, () => sleep(duration), () => ({
    immediatePolls: fixture.counts.immediatePolls - before.immediatePolls,
    longPolls: fixture.counts.longPolls - before.longPolls,
  }));
}

function latency(samples: number[]) {
  samples.sort((a, b) => a - b);
  const percentile = (p: number) => samples[Math.ceil(samples.length * p) - 1];
  return { requests: samples.length, p50Ms: percentile(0.5), p95Ms: percentile(0.95), maxMs: samples.at(-1) };
}

async function discordPair(relay: Relay, ip: number) {
  const post = (path: string, body: object) => request(relay, path, {
    ip, method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
  });
  const pair = await post("/v1/pair", { provider: "discord" });
  assert.equal(pair.status, 200);
  const { deviceKey, url } = await pair.json();
  const start = await request(relay, new URL(url).pathname, { ip });
  await expectStatus(start, 303);
  const cookie = start.headers.get("set-cookie")!.split(";", 1)[0];
  const consent = new URL(start.headers.get("location")!);
  const query = new URLSearchParams({ code: "fixture", state: consent.searchParams.get("state")!, iss: "https://discord.com" });
  await expectStatus(await request(relay, `/discord/callback?${query}`, { ip, headers: { cookie } }), 303);
  const page = await expectStatus(await request(relay, "/discord/finish", { ip, headers: { cookie } }), 200);
  const finishCode = page?.match(/<code>([A-F0-9]{8})<\/code>/)?.[1];
  assert.ok(finishCode);
  await expectStatus(await post("/v1/preview", { deviceKey, finishCode }), 200);
  await expectStatus(await post("/v1/claim", { deviceKey, finishCode }), 200);
}

async function delivery(relay: Relay, fixture: Providers, issuer: string, stop: Stop) {
  const authorization = (index: number) => `Bearer ${credential(issuer, index)}`;
  const send = (index: number, body = shortBody, ip = index) => request(relay, "/v1/send", {
    ip, method: "POST", body, headers: { "Content-Type": "application/json", Authorization: authorization(index) },
  });
  async function batch(label: string, body: string, start: number, count: number, status: number, retry?: number) {
    const calls = fixture.counts.sendCalls;
    await measure(label, async () => {
      const samples: number[] = [];
      for (let i = 0; i < count; i += 16) {
        await Promise.all(Array.from({ length: Math.min(16, count - i) }, async (_, j) => {
          const at = performance.now();
          await expectStatus(await send(start + i + j, body), status, retry);
          samples.push(performance.now() - at);
        }));
      }
      return samples;
    }, (samples) => ({ ...latency(samples), bodyBytes: body.length, status, providerCalls: fixture.counts.sendCalls - calls }));
    assert.equal(fixture.counts.sendCalls - calls, status === 204 ? count : 0);
  }
  for (const [round, body] of [shortBody, largeBody, shortBody, largeBody].entries()) {
    if (round) await sleep(1050);
    await batch(`telegram.delivery.${round}`, body, round * 8, 8, 204);
  }

  await sleep(1050);
  const gate = Promise.withResolvers<void>();
  fixture.state.gate = gate.promise;
  const calls = fixture.counts.sendCalls;
  const held: Promise<Response>[] = [];
  try {
    // Distinguish per-chat and per-client limits from the global provider cap.
    held.push(send(40));
    await waitUntil(() => fixture.counts.sendCalls === calls + 1);
    await expectStatus(await send(40, shortBody, 41), 429, 1);
    for (let i = 41; i <= 43; i++) held.push(send(i, shortBody, 40));
    await waitUntil(() => fixture.counts.sendCalls === calls + 4);
    await expectStatus(await send(44, shortBody, 40), 429, 1);
    for (let i = 44; i <= 47; i++) held.push(send(i));
    await waitUntil(() => fixture.counts.sendCalls === calls + 8);
    await batch("telegram.saturated.short", shortBody, 500, 256, 429, 1);
    await batch("telegram.saturated.large", largeBody, 800, 128, 429, 1);
  } finally {
    gate.resolve();
    fixture.state.gate = undefined;
    await Promise.all(held.map(async (response) => expectStatus(await response, 204)));
  }

  await sleep(1050);
  await measure("telegram.send-window", async () => {
    const before = fixture.counts.sendCalls;
    for (let i = 50; i < 66; i++) await expectStatus(await send(i), i === 65 ? 429 : 204, i === 65 ? 1 : undefined);
    assert.equal(fixture.counts.sendCalls - before, 15);
  }, () => ({ requests: 16, providerCalls: 15 }));
  await sleep(1050);
  await measure("telegram.upstream-backpressure", async () => {
    const before = fixture.counts.sendCalls;
    fixture.state.sendStatus = 429;
    await expectStatus(await send(70), 429, 3);
    await expectStatus(await send(70), 429, 3);
    fixture.state.sendStatus = 502;
    await expectStatus(await send(71), 502);
    assert.equal(fixture.counts.sendCalls - before, 2);
    fixture.state.sendStatus = 200;
  }, () => ({ requests: 3, providerCalls: 2 }));

  await measure("telegram.revoke", async () => {
    const samples: number[] = [];
    for (let i = 100; i < 132; i++) {
      const at = performance.now();
      await expectStatus(await request(relay, "/v1/binding", {
        ip: i, method: "DELETE", headers: { Authorization: authorization(i) },
      }), 204);
      samples.push(performance.now() - at);
    }
    return samples;
  }, latency);
  await expectStatus(await send(100), 401);
  const before = fixture.counts.sendCalls;
  const release = Promise.withResolvers<void>();
  fixture.state.gate = release.promise;
  const response = send(72);
  try {
    await waitUntil(() => fixture.counts.sendCalls === before + 1);
    const stopped = stop("telegram.shutdown-in-flight");
    await sleep(providerDelayMs);
    release.resolve();
    await expectStatus(await response, 204);
    await stopped;
  } finally {
    release.resolve();
    await response;
  }
}

verifyDataMount(directory);
assert.equal(readdirSync(directory).length, 0, "Use a fresh throwaway Docker volume for synthetic relay data");
output("config", { bun: Bun.version, arch: process.arch, idleMs, providerDelayMs, bindingCount });
const discord = providers();
await lifecycle("discord-only", discord, false, async (relay) => {
  await idle("discord-only.idle", discord, idleMs);
  await measure("discord-only.pair", async () => {
    const samples: number[] = [];
    for (let i = 0; i < 32; i++) {
      const at = performance.now();
      await discordPair(relay, i);
      samples.push(performance.now() - at);
    }
    assert.equal(discord.counts.discordExchanges, 32);
    return samples;
  }, latency);
});
await lifecycle("telegram.empty-ledger", providers(), true);
const issuer = seed();
const telegram = providers(200);
await lifecycle("telegram.full-ledger", telegram, true, async (relay, stop) => {
  assert.equal(telegram.counts.immediatePolls, 11);
  await idle("telegram.full-ledger.idle", telegram, idleMs);
  await delivery(relay, telegram, issuer, stop);
});
const reopened = openBindings(botId, directory);
try {
  assert.equal(reopened.load(bindingCount + 1).length, bindingCount - 32);
} finally {
  reopened.close();
}
