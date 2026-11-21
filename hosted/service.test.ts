import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { createService, PRODUCTION_ORIGIN } from "./service";
import type { Service } from "./service";
import type { ProviderFetch } from "./providers";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { openBindings } from "./bindings";

const origin = "http://127.0.0.1:6120";
const discord = { clientId: "100100100100100100", clientSecret: "fixture-secret", redirectUri: `${origin}/discord/callback` };
const productionDiscord = { ...discord, redirectUri: `${PRODUCTION_ORIGIN}/discord/callback` };
const client = "198.51.100.1";
const webhookId = "200200200200200200";
const webhookToken = "not-a-real-discord-webhook-token";
const webhookChannel = "300300300300300300";
const credential = `https://discord.com/api/webhooks/${webhookId}/${webhookToken}`;
const discordToken = {
  scope: "webhook.incoming", access_token: "unused-access-token", refresh_token: "unused-refresh-token",
  webhook: { type: 1, application_id: discord.clientId, id: webhookId, token: webhookToken, channel_id: webhookChannel },
};

const api = (service: Service, path: string, body: object, extra?: Record<string, string>, base = origin, clientIp?: string) =>
  service.handle(new Request(`${base}/v1/${path}`, {
    method: "POST", headers: { "Content-Type": "application/json", Host: new URL(base).host, ...extra }, body: JSON.stringify(body),
  }), clientIp);
const publicApi = (service: Service, path: string, body: object, clientIp = client, extra?: Record<string, string>) =>
  api(service, path, body, extra, PRODUCTION_ORIGIN, clientIp);
const open = (service: Service, url: string | URL, cookie?: string, clientIp?: string) =>
  service.handle(new Request(url, { headers: { Host: new URL(url).host, ...(cookie ? { Cookie: cookie } : {}) } }), clientIp);

async function begin(service: Service, provider: "telegram" | "discord", base = origin, clientIp?: string) {
  const res = await api(service, "pair", { provider }, undefined, base, clientIp);
  expect(res.status).toBe(200);
  expect(res.headers.get("cache-control")).toBe("no-store");
  return (await res.json()) as { deviceKey: string; url: string };
}

async function authorize(service: Service, link: string, clientIp?: string) {
  const res = await open(service, link, undefined, clientIp);
  expect(res.status).toBe(303);
  expect(res.headers.get("referrer-policy")).toBe("no-referrer");
  const consent = new URL(res.headers.get("location")!);
  expect(consent.origin + consent.pathname).toBe("https://discord.com/oauth2/authorize");
  expect(consent.searchParams.get("scope")).toBe("webhook.incoming");
  expect(consent.searchParams.has("client_secret")).toBe(false);
  expect(consent.searchParams.get("code_challenge_method")).toBe("S256");
  const challenge = consent.searchParams.get("code_challenge");
  expect(challenge).toMatch(/^[A-Za-z0-9_-]{43}$/);
  expect(res.headers.get("set-cookie")).toContain("HttpOnly; SameSite=Lax");
  const cookie = res.headers.get("set-cookie")!.split(";", 1)[0];
  const callback = new URL(consent.searchParams.get("redirect_uri")!);
  callback.search = new URLSearchParams({ code: "fixture-code", state: consent.searchParams.get("state")!, iss: "https://discord.com" }).toString();
  return { callback, cookie, challenge };
}

type TelegramCall = { method: string; body: Record<string, unknown> & {
  reply_markup?: { inline_keyboard: { callback_data: string }[][] };
} };
type TelegramFixture = {
  fetch: ProviderFetch;
  calls: TelegramCall[];
  message(text: string, type?: string, user?: number, chat?: number): void;
  tap(data: string, user?: number, chat?: number, messageId?: number): void;
  webhook(value: string): void;
  conflict(): void;
  confirmation(): string;
};
type TelegramResponse = (body: TelegramCall["body"]) => Promise<Response>;

function telegramFixture(operations?: { edit?: TelegramResponse; alert?: TelegramResponse; updates?: () => Promise<Response> }): TelegramFixture {
  let sequence = 1;
  let nextMessage = 17;
  const promptIds = new Map<string, number>();
  let webhook = "";
  let conflict = false;
  const updates: object[] = [];
  const calls: TelegramCall[] = [];
  const fetch: ProviderFetch = async (url, init) => {
    const endpoint = new URL(url);
    expect(endpoint.origin).toBe("https://api.telegram.org");
    const method = endpoint.pathname.split("/").at(-1)!;
    const body = JSON.parse(init.body as string);
    calls.push({ method, body });
    if (method === "getWebhookInfo") return Response.json({ ok: true, result: { url: webhook } });
    if (method === "getMe") return Response.json({ ok: true, result: { id: 4242, is_bot: true, username: "MiloFixtureBot" } });
    if (method === "getUpdates") return operations?.updates ? operations.updates()
      : conflict ? Response.json({ ok: false, error_code: 409 }, { status: 409 })
        : Response.json({ ok: true, result: updates.splice(0) });
    if (method === "answerCallbackQuery") return Response.json({ ok: true, result: true });
    if (method === "editMessageText") {
      return operations?.edit ? operations.edit(body)
        : Response.json({ ok: true, result: { message_id: body.message_id, chat: { id: body.chat_id } } });
    }
    if (method === "sendMessage") {
      if (body.parse_mode && operations?.alert) return operations.alert(body);
      const message_id = nextMessage++;
      const confirmation = body.reply_markup?.inline_keyboard[0]?.[0]?.callback_data;
      if (confirmation) promptIds.set(confirmation, message_id);
      return Response.json({ ok: true, result: { message_id, chat: { id: body.chat_id } } });
    }
    throw new Error("Unexpected provider call");
  };
  function message(text: string, type = "private", user = 123, chat = 123) {
    updates.push({ update_id: sequence++, message: { text, from: { id: user, is_bot: false }, chat: { id: chat, type } } });
  }
  function tap(data: string, user = 123, chat = user, messageId = promptIds.get(data)) {
    updates.push({ update_id: sequence++, callback_query: {
      data, id: "fixture-callback", from: { id: user, is_bot: false }, message: { message_id: messageId, chat: { id: chat, type: "private" } },
    } });
  }
  return {
    fetch, calls, message, tap, webhook(value: string) { webhook = value; }, conflict() { conflict = true; },
    confirmation() { return calls.at(-1)!.body.reply_markup!.inline_keyboard[0][0].callback_data; },
  };
}

const telegram = { token: "4242:fixture-bot-token" };

async function requestTelegramConfirmation(service: Service, fixture: TelegramFixture, base = origin) {
  const pair = await begin(service, "telegram", base, client);
  const link = new URL(pair.url);
  expect(link.origin + link.pathname).toBe("https://t.me/MiloFixtureBot");
  const start = `/start ${link.searchParams.get("start")}`;
  fixture.message(start, "group");
  fixture.message(start, "private", 456, 123);
  await service.pollTelegram();
  expect(fixture.calls.at(-1)!.method).toBe("getUpdates");
  fixture.message(start);
  await service.pollTelegram();
  expect(fixture.calls.at(-1)).toMatchObject({ method: "sendMessage", body: { chat_id: 123 } });
  return { ...pair, confirmation: fixture.confirmation() };
}

const removeBinding = (service: Service, headers: Record<string, string>, base = origin) =>
  service.handle(new Request(`${base}/v1/binding`, { method: "DELETE", headers: { Host: new URL(base).host, ...headers } }), client);

function testLedger() {
  const dir = mkdtempSync(join(tmpdir(), "milo-relay-issuer-"));
  const store = openBindings("4242", dir);
  return { store, [Symbol.dispose]() {
    store.close();
    rmSync(dir, { recursive: true, force: true });
  } };
}

test("starts offline and rejects browser API requests and invalid hosts or bodies", async () => {
  let calls = 0;
  const service = await createService({ fetch: async () => { calls++; throw new Error("Unexpected provider call"); } });
  expect((await open(service, `${origin}/healthz`)).status).toBe(200);
  expect((await api(service, "pair", { provider: "telegram" })).status).toBe(503);
  expect((await api(service, "pair", { provider: "discord" }, { Origin: "http://other.test" })).status).toBe(403);
  expect((await api(service, "pair", { provider: "discord" }, { Host: "other.test:6120" })).status).toBe(403);
  expect((await api(service, "pair", { provider: "discord" }, { "Content-Type": "text/plain" })).status).toBe(400);
  expect(await service.pollTelegram()).toBeNull();
  expect(calls).toBe(0);
});

test("does not start polling with a webhook and stops on a later conflict", async () => {
  const fixture = telegramFixture();
  fixture.webhook("https://old-webhook.test/private");
  const reports: string[] = [];
  const options = { telegram, fetch: fixture.fetch, report: (message: string) => reports.push(message) };
  const disabled = await createService(options);
  expect(await disabled.pollTelegram()).toBeNull();
  expect(fixture.calls.map(({ method }) => method)).toEqual(["getWebhookInfo"]);
  expect(reports.join("\n")).not.toContain("old-webhook.test");
  fixture.webhook("");
  const active = await createService(options);
  const pair = await begin(active, "telegram");
  fixture.conflict();
  expect(await active.pollTelegram()).toBeNull();
  expect((await api(active, "claim", { deviceKey: pair.deviceKey })).status).toBe(404);
  expect((await api(active, "pair", { provider: "telegram" })).status).toBe(503);
  expect(fixture.calls.map(({ method }) => method)).toEqual(["getWebhookInfo", "getWebhookInfo", "getMe", "getUpdates"]);
});

test("Telegram verifies private sender on both steps, claims once, and relays only to the bound DM", async () => {
  const fixture = telegramFixture();
  const service = await createService({ telegram, fetch: fixture.fetch });
  const { deviceKey, confirmation } = await requestTelegramConfirmation(service, fixture);
  expect((await api(service, "claim", { deviceKey })).status).toBe(202);
  fixture.tap(confirmation, 456);
  fixture.tap(confirmation, 123, 456);
  await service.pollTelegram();
  expect(fixture.calls.some(({ method }) => method === "editMessageText")).toBe(false);
  expect((await api(service, "claim", { deviceKey })).status).toBe(202);
  fixture.tap(confirmation);
  await service.pollTelegram();
  expect(fixture.calls.at(-1)).toMatchObject({ method: "editMessageText", body: {
    chat_id: 123, message_id: 17, reply_markup: { inline_keyboard: [] }, text: expect.stringContaining("Pairing confirmed"),
  } });
  const claims = await Promise.all([api(service, "claim", { deviceKey }), api(service, "claim", { deviceKey })]);
  expect(claims.map((res) => res.status).sort()).toEqual([200, 404]);
  const body = await claims.find((res) => res.status === 200)!.json();
  expect(body).toEqual({ provider: "telegram", label: "Telegram DM · 123", credential: expect.stringMatching(/^milorelay_[A-Za-z0-9_-]{43}$/), chat: "123" });
  const auth = { Authorization: `Bearer ${body.credential}` };
  expect((await api(service, "send", { chat: "456", text: "fixture" }, auth)).status).toBe(400);
  expect((await api(service, "send", { text: "<code>fixture</code>" }, auth)).status).toBe(204);
  expect(fixture.calls.at(-1)).toEqual({ method: "sendMessage", body: {
    chat_id: 123, text: "<code>fixture</code>", parse_mode: "HTML", link_preview_options: { is_disabled: true },
  } });
  expect((await api(service, "send", { text: "fixture" }, auth)).status).toBe(429);
  expect((await removeBinding(service, auth)).status).toBe(204);
  expect((await removeBinding(service, auth)).status).toBe(401);
});

async function claimTelegram(service: Service, fixture: TelegramFixture) {
  const pair = await requestTelegramConfirmation(service, fixture);
  fixture.tap(pair.confirmation);
  await service.pollTelegram();
  const response = await api(service, "claim", { deviceKey: pair.deviceKey });
  expect(response.status).toBe(200);
  const { credential } = await response.json();
  return { Authorization: `Bearer ${credential}` };
}

test("a confirmed Telegram DM survives restart but a forgotten binding stays revoked", async () => {
  const dir = mkdtempSync(join(tmpdir(), "milo-relay-"));
  const fixture = telegramFixture();
  let bindingStore = openBindings("123", dir);
  const start = () => createService({ telegram, bindingStore, fetch: fixture.fetch });
  try {
    const auth = await claimTelegram(await start(), fixture);
    bindingStore.close();
    bindingStore = openBindings("123", dir);
    const resumed = await start();
    expect((await api(resumed, "send", { text: "after restart" }, auth)).status).toBe(204);
    expect((await removeBinding(resumed, auth)).status).toBe(204);
    bindingStore.close();
    bindingStore = openBindings("123", dir);
    expect((await api(await start(), "send", { text: "after revoke" }, auth)).status).toBe(401);
  } finally {
    bindingStore.close();
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a second Telegram claim preserves the first binding and shares its DM send limit", async () => {
  const fixture = telegramFixture();
  let now = 0;
  const service = await createService({ telegram, fetch: fixture.fetch, now: () => now });
  const existing = await claimTelegram(service, fixture);
  const candidate = await claimTelegram(service, fixture);
  expect(existing.Authorization).not.toBe(candidate.Authorization);
  expect((await api(service, "send", { text: "fixture" }, existing)).status).toBe(204);
  expect((await api(service, "send", { text: "fixture" }, candidate)).status).toBe(429);
  expect((await removeBinding(service, candidate)).status).toBe(204);
  expect((await api(service, "send", { text: "fixture" }, candidate)).status).toBe(401);
  now += 1000;
  expect((await api(service, "send", { text: "fixture" }, existing)).status).toBe(204);
});

test("Telegram /stop and /delete revoke from the verified DM, and expiry never mints a binding", async () => {
  const fixture = telegramFixture();
  let now = 0;
  const service = await createService({ telegram, fetch: fixture.fetch, now: () => now });
  for (const action of ["/stop", "/delete"]) {
    now += 1000;
    const auth = await claimTelegram(service, fixture);
    const second = await claimTelegram(service, fixture);
    const pending = await requestTelegramConfirmation(service, fixture);
    fixture.message(action, "group");
    await service.pollTelegram();
    expect((await api(service, "send", { text: "fixture" }, auth)).status).toBe(204);
    fixture.message(action);
    fixture.tap(pending.confirmation);
    await service.pollTelegram();
    expect((await api(service, "claim", { deviceKey: pending.deviceKey })).status).toBe(404);
    expect((await api(service, "send", { text: "fixture" }, auth)).status).toBe(401);
    expect((await api(service, "send", { text: "fixture" }, second)).status).toBe(401);
  }
  const expired = await requestTelegramConfirmation(service, fixture);
  fixture.tap(expired.confirmation);
  await service.pollTelegram();
  now += 600_000;
  expect((await api(service, "claim", { deviceKey: expired.deviceKey })).status).toBe(404);
});

test("Discord requires matching state, browser cookie and issuer, then exchanges and claims only once", async () => {
  let exchanges = 0;
  const exchange = Promise.withResolvers<Response>();
  const started = Promise.withResolvers<void>();
  let submittedVerifier: string | null = null;
  const service = await createService({ discord, fetch: async (url, init) => {
    expect(url).toBe("https://discord.com/api/v10/oauth2/token");
    expect((init.body as URLSearchParams).get("client_secret")).toBe(discord.clientSecret);
    expect((init.body as URLSearchParams).get("redirect_uri")).toBe(discord.redirectUri);
    submittedVerifier = (init.body as URLSearchParams).get("code_verifier");
    expect((init.body as URLSearchParams).get("code")).toBe("fixture-code");
    exchanges++;
    started.resolve();
    return exchange.promise;
  } });
  const { deviceKey, url } = await begin(service, "discord");
  expect(url).toMatch(/^http:\/\/127\.0\.0\.1:6120\/discord\/start\/[A-Za-z0-9_-]{43}$/);
  const { callback, cookie, challenge } = await authorize(service, url);
  expect((await open(service, url)).status).toBe(404);
  const wrongState = new URL(callback);
  wrongState.searchParams.set("state", deviceKey);
  expect((await open(service, wrongState, cookie)).status).toBe(400);
  const wrongIssuer = new URL(callback);
  wrongIssuer.searchParams.set("iss", "https://other.test");
  expect((await open(service, wrongIssuer, cookie)).status).toBe(400);
  expect((await open(service, callback)).status).toBe(400);
  expect(exchanges).toBe(0);
  const pending = open(service, callback, cookie);
  await started.promise;
  expect(submittedVerifier).toMatch(/^[A-Za-z0-9_-]{43}$/);
  expect(createHash("sha256").update(submittedVerifier!).digest("base64url")).toBe(challenge);
  expect((await open(service, callback, cookie)).status).toBe(400);
  const pendingClaim = await api(service, "claim", { deviceKey });
  expect(pendingClaim.status).toBe(202);
  expect(await pendingClaim.json()).toEqual({ pending: true });
  exchange.resolve(Response.json(discordToken));
  const success = await pending;
  expect(success.headers.get("location")).toBe("/discord/connected");
  expect(success.headers.get("set-cookie")).toContain("Max-Age=0");
  expect(await (await api(service, "claim", { deviceKey })).json()).toEqual({ provider: "discord", label: `Discord channel · ${webhookChannel}`, credential, chat: "" });
  expect((await api(service, "claim", { deviceKey })).status).toBe(404);
  expect(exchanges).toBe(1);
});

test("only a Discord-issued incoming webhook for this app can complete pairing", async () => {
  for (const [payload, outcome] of [
    [{ scope: "", webhook: discordToken.webhook }, "/discord/connected"],
    [{ scope: "webhook.incoming" }, "/discord/not-connected/webhook"],
    [{ scope: "", webhook: { ...discordToken.webhook, application_id: "42" } }, "/discord/not-connected/application"],
    [{ scope: "", webhook: { ...discordToken.webhook, token: "" } }, "/discord/not-connected/fields"],
  ] as const) {
    const service = await createService({ discord, fetch: async () => Response.json(payload) });
    const { url } = await begin(service, "discord");
    const { callback, cookie } = await authorize(service, url);
    expect((await open(service, callback, cookie)).headers.get("location"))
      .toBe(outcome);
  }
});

test("a rejected exchange explains the failure without exposing provider details", async () => {
  const service = await createService({ discord, fetch: async () =>
    Response.json({ error: "invalid_client", error_description: "private detail" }, { status: 401 }) });
  const { url } = await begin(service, "discord");
  const { callback, cookie } = await authorize(service, url);
  const path = (await open(service, callback, cookie)).headers.get("location");
  expect(path).toBe("/discord/not-connected/client");
  const text = await (await open(service, origin + path)).text();
  expect(text).toContain("client secret");
  expect(text).not.toContain("private detail");
});

test("Discord abandons denied flows and removes unclaimed expired webhooks; pairing starts are rate-limited", async () => {
  let now = 0;
  const deleted: string[] = [];
  const service = await createService({ discord, now: () => now, fetch: async (url, init) => {
    if (init.method === "DELETE") {
      deleted.push(url);
      return new Response(null, { status: 204 });
    }
    expect(url).toBe("https://discord.com/api/v10/oauth2/token");
    return Response.json(discordToken);
  } });
  const denied = await begin(service, "discord");
  const { callback: deniedCallback, cookie: deniedCookie } = await authorize(service, denied.url);
  deniedCallback.searchParams.set("error", "access_denied");
  deniedCallback.searchParams.delete("code");
  expect((await open(service, deniedCallback, deniedCookie)).headers.get("location")).toBe("/discord/not-connected/denied");
  expect((await api(service, "claim", { deviceKey: denied.deviceKey })).status).toBe(404);
  const expired = await begin(service, "discord");
  const { callback, cookie } = await authorize(service, expired.url);
  expect((await open(service, callback, cookie)).headers.get("location")).toBe("/discord/connected");
  for (let i = 0; i < 10; i++) await begin(service, "discord");
  const limited = await api(service, "pair", { provider: "discord" });
  expect(limited.status).toBe(429);
  expect(limited.headers.get("retry-after")).toBe("60");
  now += 600_000;
  service.sweep();
  expect(deleted).toEqual([credential]);
  expect((await api(service, "claim", { deviceKey: expired.deviceKey })).status).toBe(404);
});

test("production accepts only the canonical authority and rejects oversized streaming bodies", async () => {
  const service = await createService({ origin: PRODUCTION_ORIGIN });
  await expect(createService({ origin: `${PRODUCTION_ORIGIN}/` })).rejects.toThrow("Invalid relay origin.");
  await expect(createService({ origin: PRODUCTION_ORIGIN, discord })).rejects.toThrow("Invalid Discord callback");
  const forwarded = { "X-Forwarded-Host": new URL(PRODUCTION_ORIGIN).host, "X-Forwarded-Proto": "https" };
  expect((await api(service, "pair", { provider: "discord" }, forwarded, "https://other.test", client)).status).toBe(403);
  expect((await publicApi(service, "pair", { provider: "discord" }, client, { Host: "other.test", ...forwarded })).status).toBe(403);
  expect((await service.handle(new Request(`${PRODUCTION_ORIGIN}/healthz`))).status).toBe(403);
  expect((await publicApi(service, "pair", { provider: "discord" }, client, { Origin: PRODUCTION_ORIGIN })).status).toBe(403);
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new Uint8Array(1024));
      controller.enqueue(new Uint8Array(1));
      controller.close();
    },
  });
  const large = await service.handle(new Request(`${PRODUCTION_ORIGIN}/v1/pair`, {
    method: "POST", body, headers: { Host: "connect.usemilo.net", "Content-Type": "application/json" },
  }), client);
  expect(large.status).toBe(413);
  expect((await open(service, `${PRODUCTION_ORIGIN}/healthz`, undefined, client)).headers.get("strict-transport-security")).toBe("max-age=31536000");
});

test("production budgets are fair across trusted networks and do not honor request-provided identity", async () => {
  let now = 0;
  const service = await createService({ origin: PRODUCTION_ORIGIN, discord: productionDiscord, now: () => now });
  for (let i = 0; i < 12; i++) {
    expect((await publicApi(service, "pair", { provider: "discord" })).status).toBe(200);
  }
  const same = await publicApi(service, "pair", { provider: "discord" }, `::ffff:${client}`);
  expect(same.status).toBe(429);
  expect(same.headers.get("retry-after")).toBe("60");
  expect((await publicApi(service, "pair", { provider: "discord" }, "198.51.100.2")).status).toBe(200);
  now += 60_000;
  expect((await publicApi(service, "pair", { provider: "discord" })).status).toBe(429);
  for (let i = 0; i < 5; i++) {
    const result = await api(service, "pair", { provider: "discord" }, { "X-Real-IP": `198.51.100.${i + 3}` }, PRODUCTION_ORIGIN);
    expect(result.status).toBe(i < 4 ? 200 : 429);
  }
  now = 600_000;
  expect((await publicApi(service, "pair", { provider: "discord" })).status).toBe(200);
});

async function finishDiscord(service: Service, deviceIp = client, browserIp = "198.51.100.10") {
  const pair = await begin(service, "discord", PRODUCTION_ORIGIN, deviceIp);
  const launch = await open(service, pair.url, undefined, browserIp);
  expect(launch.status).toBe(303);
  const cookie = launch.headers.get("set-cookie")!;
  expect(cookie).toMatch(/^__Host-milo_discord_browser=[A-Za-z0-9_-]{43}; Path=\/; Max-Age=\d+; HttpOnly; SameSite=Lax; Secure$/);
  const browser = cookie.split(";", 1)[0];
  const consent = new URL(launch.headers.get("location")!);
  const callback = new URL(consent.searchParams.get("redirect_uri")!);
  callback.search = new URLSearchParams({ code: "fixture-code", state: consent.searchParams.get("state")!, iss: "https://discord.com" }).toString();
  const finishUrl = `${PRODUCTION_ORIGIN}/discord/finish`;
  expect((await open(service, finishUrl, browser, browserIp)).status).toBe(404);
  callback.protocol = "http:";
  const result = await open(service, callback, browser, browserIp);
  expect(result.status).toBe(303);
  expect(result.headers.get("location")).toBe(finishUrl);
  expect(await result.text()).toBe("");
  expect((await open(service, finishUrl, undefined, browserIp)).status).toBe(404);
  const finish = await open(service, finishUrl, browser, browserIp);
  expect(finish.status).toBe(200);
  expect(finish.headers.get("cache-control")).toBe("no-store");
  const finishCode = (await finish.text()).match(/<code>([A-F0-9]{8})<\/code>/)?.[1];
  expect(finishCode).toBeDefined();
  expect(await (await open(service, `${PRODUCTION_ORIGIN}/discord/connected`, browser, browserIp)).text()).not.toContain(`<code>${finishCode}</code>`);
  return { ...pair, finishCode: finishCode!, finishUrl, browser };
}

test("production Discord reviews without exposing credentials and retries Save within the original lifetime", async () => {
  let now = 0;
  const reports: string[] = [];
  const service = await createService({ origin: PRODUCTION_ORIGIN, discord: productionDiscord,
    now: () => now, fetch: async () => Response.json(discordToken), report: (message) => reports.push(message) });
  const { deviceKey, url, finishCode, finishUrl, browser } = await finishDiscord(service);
  expect(url).toMatch(/^https:\/\/connect\.usemilo\.net\/discord\/start\/[A-Za-z0-9_-]{43}$/);
  expect((await publicApi(service, "preview", { deviceKey })).status).toBe(400);
  const wrong = `${finishCode[0] === "0" ? "1" : "0"}${finishCode.slice(1)}`;
  expect((await publicApi(service, "preview", { deviceKey, finishCode: wrong })).status).toBe(400);
  const proof = { deviceKey, finishCode };
  expect((await publicApi(service, "claim", proof)).status).toBe(409);
  const review = await publicApi(service, "preview", proof);
  const metadata = { provider: "discord", label: `Discord channel · ${webhookChannel}`, chat: "" };
  expect(await review.json()).toEqual(metadata);
  const claims = await Promise.all([publicApi(service, "claim", proof), publicApi(service, "claim", proof)]);
  expect(claims.map((result) => result.status)).toEqual([200, 200]);
  expect(await Promise.all(claims.map((result) => result.json()))).toEqual([
    { ...metadata, credential }, { ...metadata, credential },
  ]);
  expect((await open(service, finishUrl, browser, "198.51.100.10")).status).toBe(404);
  now = 600_000;
  expect((await publicApi(service, "claim", proof)).status).toBe(404);
  expect(reports.join("\n")).not.toContain(finishCode);
});

test("production Discord discards a webhook when the finish proof expires or runs out of attempts", async () => {
  let now = 0;
  const deleted: string[] = [];
  const service = await createService({ origin: PRODUCTION_ORIGIN, discord: productionDiscord, now: () => now, fetch: async (url, init) => {
    if (init.method === "DELETE") {
      deleted.push(url);
      return new Response(null, { status: 204 });
    }
    return Response.json(discordToken);
  } });
  const exhausted = await finishDiscord(service);
  const wrong = `${exhausted.finishCode[0] === "0" ? "1" : "0"}${exhausted.finishCode.slice(1)}`;
  for (let i = 0; i < 5; i++) {
    expect((await publicApi(service, "claim", { deviceKey: exhausted.deviceKey, finishCode: wrong })).status).toBe(i < 4 ? 400 : 404);
  }
  expect((await publicApi(service, "claim", { deviceKey: exhausted.deviceKey, finishCode: exhausted.finishCode })).status).toBe(404);
  const expired = await finishDiscord(service);
  expect((await publicApi(service, "preview", { deviceKey: expired.deviceKey, finishCode: expired.finishCode })).status).toBe(200);
  now = 600_000;
  expect((await open(service, expired.finishUrl, expired.browser)).status).toBe(404);
  expect((await publicApi(service, "claim", { deviceKey: expired.deviceKey, finishCode: expired.finishCode })).status).toBe(404);
  expect(deleted).toEqual([credential, credential]);
});

test("production Telegram enables claims only after delivering the finish code to the verified DM", async () => {
  let now = 0;
  const ready = Promise.withResolvers<TelegramCall["body"]>();
  const delivery = Promise.withResolvers<Response>();
  const sendReady = Promise.withResolvers<void>();
  const send = Promise.withResolvers<Response>();
  const fixture = telegramFixture({
    edit: (body) => { ready.resolve(body); return delivery.promise; },
    alert: () => { sendReady.resolve(); return send.promise; },
  });
  using ledger = testLedger();
  const service = await createService({ origin: PRODUCTION_ORIGIN, telegram, bindingStore: ledger.store, fetch: fixture.fetch, now: () => now });
  const { deviceKey, confirmation } = await requestTelegramConfirmation(service, fixture, PRODUCTION_ORIGIN);
  fixture.tap(confirmation, 123, 123, 999);
  await service.pollTelegram();
  expect(fixture.calls.some(({ method }) => method === "editMessageText")).toBe(false);
  fixture.tap(confirmation);
  const poll = service.pollTelegram();
  const message = await ready.promise;
  expect(message.chat_id).toBe(123);
  const finishCode = String(message.text).match(/pairing: ([A-F0-9]{8})\./)?.[1];
  expect(finishCode).toBeDefined();
  expect((await publicApi(service, "preview", { deviceKey, finishCode })).status).toBe(202);
  delivery.resolve(Response.json({ ok: true, result: { message_id: message.message_id, chat: { id: message.chat_id } } }));
  await poll;
  expect((await publicApi(service, "preview", { deviceKey })).status).toBe(400);
  const proof = { deviceKey, finishCode };
  const review = await publicApi(service, "preview", proof);
  expect(await review.json()).toEqual({ provider: "telegram", label: "Telegram DM · 123", chat: "123" });
  expect(ledger.store.load(10)).toEqual([]);
  const claim = await publicApi(service, "claim", proof);
  expect(claim.status).toBe(200);
  const binding = await claim.json();
  expect(binding.credential).toMatch(/^milorelay_prod_[A-Za-z0-9_-]{43}$/);
  expect(binding.credential.startsWith(`milorelay_prod_${ledger.store.issuer}`)).toBe(true);
  expect(await (await publicApi(service, "claim", proof)).json()).toEqual(binding);
  expect(ledger.store.load(10)).toHaveLength(1);
  const auth = { Authorization: `Bearer ${binding.credential}` };
  const text = "Rex: <code>example.org</code> is waiting for permission\n<code>Use &lt;literal&gt; &amp; reply</code>";
  const wrongMode = { Authorization: `Bearer ${binding.credential.replace("milorelay_prod_", "milorelay_")}` };
  expect((await publicApi(service, "send", { text }, client, wrongMode)).status).toBe(401);
  expect((await publicApi(service, "send", { text: "Plain note from Rex" }, client, auth)).status).toBe(400);
  expect((await publicApi(service, "send", { text: "Rex: <b>Rex</b> finished" }, client, auth)).status).toBe(400);
  expect((await publicApi(service, "send", { text, parse_mode: "Markdown" }, client, auth)).status).toBe(400);
  expect((await publicApi(service, "send", { text: `Rex: a program finished\n<code>${"x".repeat(4097)}</code>` }, client, auth)).status).toBe(400);
  expect(fixture.calls.filter(({ body }) => body.parse_mode)).toHaveLength(0);
  const sending = publicApi(service, "send", { text }, client, auth);
  await sendReady.promise;
  now += 1000;
  expect((await publicApi(service, "send", { text }, client, auth)).status).toBe(429);
  send.resolve(Response.json({ ok: true, result: true }));
  expect((await sending).status).toBe(204);
  expect(fixture.calls.filter(({ body }) => body.parse_mode)).toEqual([{ method: "sendMessage", body: {
    chat_id: 123, text, parse_mode: "HTML", link_preview_options: { is_disabled: true },
  } }]);
  expect((await removeBinding(service, auth, PRODUCTION_ORIGIN)).status).toBe(204);
  expect((await publicApi(service, "claim", proof)).status).toBe(404);
});

test("production Telegram discards a pairing if its finish delivery fails", async () => {
  const fixture = telegramFixture({ edit: async () => Response.json({ ok: false, error_code: 400 }, { status: 400 }) });
  using ledger = testLedger();
  const service = await createService({ origin: PRODUCTION_ORIGIN, telegram, bindingStore: ledger.store, fetch: fixture.fetch });
  const { deviceKey, confirmation } = await requestTelegramConfirmation(service, fixture, PRODUCTION_ORIGIN);
  fixture.tap(confirmation);
  await service.pollTelegram();
  const finishCode = String(fixture.calls.at(-1)?.body.text).match(/pairing: ([A-F0-9]{8})\./)?.[1];
  expect(finishCode).toBeDefined();
  expect((await publicApi(service, "preview", { deviceKey, finishCode })).status).toBe(404);
});

test("Telegram polling stays single-flight and can be cancelled", async () => {
  const gate = Promise.withResolvers<Response>();
  const fixture = telegramFixture({ updates: () => gate.promise });
  const service = await createService({ telegram, fetch: fixture.fetch });
  const abort = new AbortController();
  const pending = service.pollTelegram(abort.signal);
  expect(await service.pollTelegram()).toBe(1000);
  abort.abort();
  gate.resolve(Response.json({ ok: true, result: [] }));
  expect(await pending).toBeNull();
  expect(fixture.calls.filter(({ method }) => method === "getUpdates")).toHaveLength(1);
});

test("Discord bounds concurrent exchanges and drains expired webhook cleanup in batches", async () => {
  let now = 0;
  let exchanges = 0;
  let deletes = 0;
  const accepted = Promise.withResolvers<void>();
  const exchange = Promise.withResolvers<void>();
  const cleanup = Promise.withResolvers<void>();
  const drained = Promise.withResolvers<void>();
  const service = await createService({ origin: PRODUCTION_ORIGIN, discord: productionDiscord, now: () => now,
    fetch: async (_url, init) => {
      if (init.method === "DELETE") {
        if (++deletes === 9) drained.resolve();
        await cleanup.promise;
        return new Response(null, { status: 204 });
      }
      if (++exchanges === 8) accepted.resolve();
      await exchange.promise;
      return Response.json(discordToken);
    },
  });
  const attempts = [];
  for (let i = 0; i < 9; i++) {
    const browserIp = `198.51.100.${i + 10}`;
    const { url } = await begin(service, "discord", PRODUCTION_ORIGIN, client);
    const { callback, cookie } = await authorize(service, url, browserIp);
    attempts.push({ callback, cookie, browserIp });
  }
  const callbacks = attempts.slice(0, 8).map(({ callback, cookie, browserIp }) => open(service, callback, cookie, browserIp));
  await accepted.promise;
  const { callback, cookie, browserIp } = attempts[8];
  expect((await open(service, callback, cookie, browserIp)).status).toBe(429);
  exchange.resolve();
  expect((await Promise.all(callbacks)).map((response) => response.status)).toEqual(Array(8).fill(303));
  expect((await open(service, callback, cookie, browserIp)).status).toBe(303);
  now = 600_000;
  service.sweep();
  expect(deletes).toBe(4);
  cleanup.resolve();
  await drained.promise;
});

test("production revocation must reach the exact issuing ledger even if Telegram is disabled", async () => {
  using ledger = testLedger();
  using replacement = testLedger();
  const credential = `milorelay_prod_${ledger.store.issuer}${"x".repeat(27)}`;
  const digest = createHash("sha256").update(credential).digest("hex");
  ledger.store.insert(digest, 123);
  const auth = { Authorization: `Bearer ${credential}` };
  const offline = await createService({ origin: PRODUCTION_ORIGIN });
  const replaced = await createService({ origin: PRODUCTION_ORIGIN, bindingStore: replacement.store });
  const unreadable = await createService({ origin: PRODUCTION_ORIGIN, bindingStore: {
    ...ledger.store, removeIssued() { throw new Error("inaccessible ledger"); },
  } });
  for (const service of [offline, replaced, unreadable]) {
    expect((await removeBinding(service, auth, PRODUCTION_ORIGIN)).status).toBe(503);
  }
  expect(ledger.store.load(10)).toEqual([{ digest, chat: 123 }]);
  const active = await createService({ origin: PRODUCTION_ORIGIN, bindingStore: ledger.store });
  expect((await removeBinding(active, auth, PRODUCTION_ORIGIN)).status).toBe(204);
  expect(ledger.store.load(10)).toEqual([]);
  expect((await removeBinding(active, auth, PRODUCTION_ORIGIN)).status).toBe(204);
});

test("production Telegram pauses sends until its poll catches up and applies verified stops", async () => {
  using ledger = testLedger();
  const credential = `milorelay_prod_${ledger.store.issuer}${"x".repeat(27)}`;
  const auth = { Authorization: `Bearer ${credential}` };
  ledger.store.insert(createHash("sha256").update(credential).digest("hex"), 123);
  const fixture = telegramFixture();
  let backlogs = 0;
  let fresh = true;
  const service = await createService({ origin: PRODUCTION_ORIGIN, bindingStore: ledger.store, telegram,
    fetch: fixture.fetch, telegramCanSend: () => fresh, onTelegramBacklog: () => { backlogs++; } });
  const text = { text: "Rex: a program finished" };
  expect((await publicApi(service, "send", text, client, auth)).status).toBe(503);
  for (let i = 0; i < 20; i++) fixture.message("A normal private message");
  expect(await service.pollTelegram()).toBe(-1);
  expect(backlogs).toBe(1);
  expect((await publicApi(service, "send", text, client, auth)).status).toBe(503);
  expect(await service.pollTelegram()).toBe(0);
  fresh = false;
  expect((await publicApi(service, "send", text, client, auth)).status).toBe(503);
  fresh = true;
  expect((await publicApi(service, "send", text, client, auth)).status).toBe(204);
  fixture.message("/stop");
  expect(await service.pollTelegram()).toBe(0);
  expect(backlogs).toBe(2);
  expect(ledger.store.load(10)).toEqual([]);
  expect((await publicApi(service, "send", text, client, auth)).status).toBe(401);
});

test("local preview returns only destination metadata and allows the reviewed claim to be retried", async () => {
  const fixture = telegramFixture();
  const service = await createService({ telegram, fetch: fixture.fetch });
  const { deviceKey, confirmation } = await requestTelegramConfirmation(service, fixture);
  const proof = { deviceKey };
  expect((await api(service, "preview", proof)).status).toBe(202);
  fixture.tap(confirmation);
  await service.pollTelegram();
  const metadata = { provider: "telegram", label: "Telegram DM · 123", chat: "123" };
  expect(await (await api(service, "preview", proof)).json()).toEqual(metadata);
  const claim = await (await api(service, "claim", proof)).json();
  expect(claim).toEqual({ ...metadata, credential: expect.stringMatching(/^milorelay_[A-Za-z0-9_-]{43}$/) });
  expect(await (await api(service, "claim", proof)).json()).toEqual(claim);
  expect((await removeBinding(service, { Authorization: `Bearer ${claim.credential}` })).status).toBe(204);
  expect((await api(service, "claim", proof)).status).toBe(404);
});
