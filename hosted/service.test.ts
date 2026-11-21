import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { createService } from "./service";
import type { Service } from "./service";
import type { ProviderFetch } from "./providers";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { openBindings } from "./bindings";

const origin = "http://127.0.0.1:6120";
const discord = { clientId: "100100100100100100", clientSecret: "fixture-secret", redirectUri: `${origin}/discord/callback` };
const webhookId = "200200200200200200";
const webhookToken = "not-a-real-discord-webhook-token";
const webhookChannel = "300300300300300300";
const credential = `https://discord.com/api/webhooks/${webhookId}/${webhookToken}`;
const discordToken = {
  scope: "webhook.incoming", access_token: "unused-access-token", refresh_token: "unused-refresh-token",
  webhook: { type: 1, application_id: discord.clientId, id: webhookId, token: webhookToken, channel_id: webhookChannel },
};

const api = (service: Service, path: string, body: object, extra?: Record<string, string>) =>
  service.handle(new Request(`${origin}/v1/${path}`, {
    method: "POST", headers: { "Content-Type": "application/json", ...extra }, body: JSON.stringify(body),
  }));
const open = (service: Service, url: string | URL, cookie?: string) =>
  service.handle(new Request(url, { headers: cookie ? { Cookie: cookie } : undefined }));

async function begin(service: Service, provider: "telegram" | "discord") {
  const res = await api(service, "pair", { provider });
  expect(res.status).toBe(200);
  expect(res.headers.get("cache-control")).toBe("no-store");
  return (await res.json()) as { deviceKey: string; url: string };
}

async function authorize(service: Service, link: string) {
  const res = await open(service, link);
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
  tap(data: string, user?: number, chat?: number): void;
  webhook(value: string): void;
  conflict(): void;
  confirmation(): string;
};

function telegramFixture(): TelegramFixture {
  let sequence = 1;
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
    if (method === "getMe") return Response.json({ ok: true, result: { is_bot: true, username: "MiloFixtureBot" } });
    if (method === "getUpdates") return conflict ? Response.json({ ok: false, error_code: 409 }, { status: 409 })
      : Response.json({ ok: true, result: updates.splice(0) });
    if (["sendMessage", "answerCallbackQuery", "editMessageText"].includes(method)) return Response.json({ ok: true, result: true });
    throw new Error("Unexpected provider call");
  };
  function message(text: string, type = "private", user = 123, chat = 123) {
    updates.push({ update_id: sequence++, message: { text, from: { id: user, is_bot: false }, chat: { id: chat, type } } });
  }
  function tap(data: string, user = 123, chat = user) {
    updates.push({ update_id: sequence++, callback_query: {
      data, id: "fixture-callback", from: { id: user, is_bot: false }, message: { message_id: 17, chat: { id: chat, type: "private" } },
    } });
  }
  return {
    fetch, calls, message, tap, webhook(value: string) { webhook = value; }, conflict() { conflict = true; },
    confirmation() { return calls.at(-1)!.body.reply_markup!.inline_keyboard[0][0].callback_data; },
  };
}

const telegram = { token: "fixture" };

async function requestTelegramConfirmation(service: Service, fixture: TelegramFixture) {
  const pair = await begin(service, "telegram");
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

const removeBinding = (service: Service, headers: Record<string, string>) =>
  service.handle(new Request(`${origin}/v1/binding`, { method: "DELETE", headers }));

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
