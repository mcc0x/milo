import { createHash, randomBytes } from "node:crypto";
import { discordExchange, discardUnclaimedWebhook, object, telegramApi } from "./providers";
import type { DiscordConfig, ProviderFetch } from "./providers";
import type { BindingStore } from "./bindings";
import { resultPage } from "./result";

export const HOST = "127.0.0.1";
export const PORT = 6120;
export const MAX_BODY = 32 * 1024;
const CALLBACK = "/discord/callback";
const COOKIE = "milo_discord_browser";
const TTL = 10 * 60_000;
const LIMIT = 128;
const key = () => randomBytes(32).toString("base64url");
const keyPattern = /^[A-Za-z0-9_-]{43}$/;
const bindingPattern = /^Bearer (milorelay_[A-Za-z0-9_-]{43})$/;
const loopbackHosts = new Set([`${HOST}:${PORT}`, `localhost:${PORT}`]);
const callbackCookie = (value: string, maxAge: number) =>
  `${COOKIE}=${value}; Path=${CALLBACK}; Max-Age=${maxAge}; HttpOnly; SameSite=Lax`;
const digest = (credential: string) => createHash("sha256").update(credential).digest("hex");
const headers = {
  "Cache-Control": "no-store",
  "Referrer-Policy": "no-referrer",
  "X-Content-Type-Options": "nosniff",
  "X-Frame-Options": "DENY",
  "Content-Security-Policy": "default-src 'none'; frame-ancestors 'none'",
};
const json = (body: object, status = 200, extra?: Record<string, string>) =>
  Response.json(body, { status, headers: { ...headers, ...extra } });
const error = (status: number, message: string) => json({ error: message }, status);
const rateLimited = (seconds: number) =>
  json({ error: "Too many requests; try again later." }, 429, { "Retry-After": String(seconds) });
const plain = (body: string | null, status: number, extra?: Record<string, string>) =>
  new Response(body, { status, headers: { ...headers, "Content-Type": "text/plain; charset=utf-8", ...extra } });

const oneField = async (request: Request, field: string) => {
  if (request.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase() !== "application/json") return;
  try {
    const data = object(await request.json());
    if (data && Object.keys(data).length === 1 && typeof data[field] === "string") return data[field];
  } catch {
    return;
  }
};

function windowLimit(capacity: number, interval: number) {
  let expires = 0;
  let count = 0;
  return (now: number) => {
    if (now >= expires) {
      count = 0;
      expires = now + interval;
    }
    if (count >= capacity) return Math.ceil((expires - now) / 1000);
    count++;
    return 0;
  };
}

type Step =
  | { kind: "telegram-start"; proof: string }
  | { kind: "telegram-confirm"; proof: string; chat: number }
  | { kind: "telegram-ready"; chat: number }
  | { kind: "discord-start"; proof: string }
  | { kind: "discord-callback"; proof: string; browser: string; verifier: string }
  | { kind: "discord-exchange" }
  | { kind: "discord-ready"; credential: string; channel: string };

type Pair = { deviceKey: string; expires: number; step: Step };
type BoundChat = { chat: number; nextSend: number; digests: Set<string> };
type Options = {
  telegram?: { token: string };
  bindingStore?: BindingStore;
  discord?: DiscordConfig;
  fetch?: ProviderFetch;
  now?: () => number;
  report?: (message: string) => void;
};

export type Service = {
  handle(request: Request): Promise<Response>;
  pollTelegram(): Promise<number | null>;
  sweep(): void;
};

// Only authenticated Bot API updates get here. Match the actual human sender to a private chat.
function privateSender(fromValue: unknown, message?: Record<string, unknown>) {
  const from = object(fromValue);
  const chat = object(message?.chat);
  const id = from?.id;
  if (typeof id === "number" && Number.isSafeInteger(id) && id > 0 && from?.is_bot === false &&
      chat?.type === "private" && chat.id === id && !message?.sender_chat) return id;
}

export async function createService({
  telegram, discord, bindingStore, fetch = globalThis.fetch, now = () => performance.now(), report = () => {},
}: Options = {}): Promise<Service> {
  if (discord && ![...loopbackHosts].some((host) => discord.redirectUri === `http://${host}${CALLBACK}`)) {
    throw new Error("Invalid Discord callback configuration.");
  }
  const discordOrigin = discord && new URL(discord.redirectUri).origin;
  const tg = telegram && telegramApi(fetch, telegram.token);
  let username: string | undefined;
  if (tg) {
    const webhook = await tg("getWebhookInfo");
    const url = object(webhook.result)?.url;
    if (webhook.status === 200 && url === "") {
      const me = object((await tg("getMe")).result);
      if (me?.is_bot === true && typeof me.username === "string" && /^[A-Za-z0-9_]{5,32}$/.test(me.username)) {
        username = me.username;
      } else {
        report("Telegram disabled: bot identity could not be verified.");
      }
    } else {
      report(typeof url === "string" && url !== "" ? "Telegram disabled: existing webhook left untouched."
        : "Telegram disabled: webhook status could not be verified.");
    }
  }

  const pairs = new Map<string, Pair>();
  const proofs = new Map<string, Pair>();
  const bindings = new Map<string, BoundChat>();
  const chats = new Map<number, BoundChat>();
  function remember(hashed: string, chat: number) {
    let binding = chats.get(chat);
    if (!binding) {
      binding = { chat, nextSend: 0, digests: new Set() };
      chats.set(chat, binding);
    }
    binding.digests.add(hashed);
    bindings.set(hashed, binding);
  }
  for (const row of bindingStore?.load() ?? []) remember(row.digest, row.chat);
  const pairWindow = windowLimit(12, 60_000);
  const claimWindow = windowLimit(240, 60_000);
  const sendWindow = windowLimit(15, 1000);
  const botReplyWindow = windowLimit(12, 1000);

  function forget(pair: Pair, discard = false) {
    pairs.delete(pair.deviceKey);
    const step = pair.step;
    if ("proof" in step) proofs.delete(step.proof);
    if (discard && step.kind === "discord-ready") void discardUnclaimedWebhook(fetch, step.credential);
  }

  function sweep() {
    const time = now();
    for (const pair of pairs.values()) if (time >= pair.expires) forget(pair, true);
  }

  function transition(pair: Pair, next: Step) {
    if ("proof" in pair.step) proofs.delete(pair.step.proof);
    pair.step = next;
    if ("proof" in next) proofs.set(next.proof, pair);
  }

  function revokeChat(chat: number) {
    bindingStore?.revoke(chat);
    const binding = chats.get(chat);
    if (binding) {
      for (const hashed of binding.digests) bindings.delete(hashed);
      chats.delete(chat);
    }
    for (const pair of pairs.values()) {
      const step = pair.step;
      if ("chat" in step && step.chat === chat) forget(pair);
    }
  }

  async function start(request: Request) {
    const wait = pairWindow(now());
    if (wait) return rateLimited(wait);
    const provider = await oneField(request, "provider");
    if (provider !== "telegram" && provider !== "discord") return error(400, "Choose Telegram or Discord.");
    if ((provider === "telegram" && !username) || (provider === "discord" && !discordOrigin)) {
      return error(503, "Provider is not enabled.");
    }
    sweep();
    if (pairs.size >= LIMIT) return error(503, "Too many pending pairings.");
    const deviceKey = key();
    const proof = key();
    const step: Step = provider === "telegram" ? { kind: "telegram-start", proof } : { kind: "discord-start", proof };
    const pair = { deviceKey, expires: now() + TTL, step };
    pairs.set(deviceKey, pair);
    proofs.set(proof, pair);
    const url = provider === "telegram"
      ? `https://t.me/${username}?start=${proof}`
      : `${discordOrigin}/discord/start/${proof}`;
    return json({ deviceKey, url });
  }

  async function claim(request: Request) {
    const wait = claimWindow(now());
    if (wait) return rateLimited(wait);
    const deviceKey = await oneField(request, "deviceKey");
    if (!deviceKey || !keyPattern.test(deviceKey)) return error(400, "Invalid device key.");
    sweep();
    const pair = pairs.get(deviceKey);
    if (!pair) return error(404, "Pairing is unavailable; start again.");
    const step = pair.step;
    if (step.kind === "discord-ready") {
      forget(pair);
      return json({ provider: "discord", label: `Discord channel · ${step.channel}`, credential: step.credential, chat: "" });
    }
    if (step.kind === "telegram-ready") {
      if (bindings.size >= LIMIT) return error(503, "Too many active Telegram bindings.");
      const credential = `milorelay_${key()}`;
      const hashed = digest(credential);
      bindingStore?.insert(hashed, step.chat);
      remember(hashed, step.chat);
      forget(pair);
      return json({ provider: "telegram", label: `Telegram DM · ${step.chat}`, credential, chat: String(step.chat) });
    }
    return json({ pending: true }, 202);
  }

  function discordStart(url: URL, proof: string) {
    const pair = proofs.get(proof);
    if (!discord || url.origin !== discordOrigin || pair?.step.kind !== "discord-start") {
      return plain("This Discord pairing link is unavailable. Start again in Milo.", 404);
    }
    const state = key();
    const browser = key();
    const verifier = key();
    transition(pair, { kind: "discord-callback", proof: state, browser, verifier });
    const consent = new URL("https://discord.com/oauth2/authorize");
    consent.search = new URLSearchParams({
      response_type: "code", client_id: discord.clientId, scope: "webhook.incoming",
      redirect_uri: discord.redirectUri, state, prompt: "consent",
      code_challenge: createHash("sha256").update(verifier).digest("base64url"), code_challenge_method: "S256",
    }).toString();
    return plain(null, 303, {
      Location: consent.href, "Set-Cookie": callbackCookie(browser, Math.ceil((pair.expires - now()) / 1000)),
    });
  }

  const failureMessages = {
    denied: "Discord authorization was declined. Choose a channel and authorize the webhook to connect.",
    code: "Discord did not return an authorization code. Check the application allows the OAuth2 authorization code grant and webhook.incoming scope.",
    client: "Discord rejected the OAuth client credentials. Check the application ID and its OAuth2 client secret, not the bot token.",
    grant: "Discord rejected the authorization code. Start a fresh pairing in Milo and confirm the registered callback matches the relay URI exactly.",
    exchange: "Discord refused the token exchange. Check the OAuth application settings before starting again.",
    webhook: "Discord did not issue an incoming webhook. Select a channel where you can manage webhooks.",
    application: "Discord returned a webhook that could not be matched to the configured application.",
    fields: "Discord returned incomplete or unsupported channel webhook credentials.",
    network: "The relay could not read Discord’s token response. Check its network connection.",
    expired: "This pairing expired. Start a fresh pairing in Milo.",
  };
  async function discordCallback(url: URL, request: Request) {
    const states = url.searchParams.getAll("state");
    const state = states[0];
    const pair = states.length === 1 && keyPattern.test(state) ? proofs.get(state) : undefined;
    const issuers = url.searchParams.getAll("iss");
    const cookie = request.headers.get("cookie")?.split(";").map((item) => item.trim())
      .filter((item) => item.startsWith(`${COOKIE}=`));
    if (!discord || url.origin !== discordOrigin || pair?.step.kind !== "discord-callback" ||
        issuers.length !== 1 || issuers[0] !== "https://discord.com" ||
        cookie?.length !== 1 || cookie[0] !== `${COOKIE}=${pair.step.browser}`) {
      return resultPage(headers, "Pairing not verified", "Return to Milo and start a new Discord connection in this browser.", false, 400);
    }
    const { verifier } = pair.step;
    transition(pair, { kind: "discord-exchange" }); // Spend state before making any network request.
    const codes = url.searchParams.getAll("code");
    const result = url.searchParams.has("error")
      ? (url.searchParams.get("error") === "access_denied" ? "denied" : "code")
      : codes.length === 1 && /^[\x21-\x7e]{1,512}$/.test(codes[0])
        ? await discordExchange(fetch, discord, codes[0], verifier) : "code";
    const ready = typeof result !== "string" && now() < pair.expires && pairs.has(pair.deviceKey);
    if (typeof result !== "string" && ready) {
      transition(pair, { kind: "discord-ready", ...result });
    } else {
      forget(pair);
      if (typeof result !== "string") void discardUnclaimedWebhook(fetch, result.credential);
      report(`Discord pairing: ${typeof result === "string" ? result : "expired"}.`);
    }
    return plain(null, 303, {
      Location: ready ? "/discord/connected" : `/discord/not-connected/${typeof result === "string" ? result : "expired"}`, "Set-Cookie": callbackCookie("", 0),
    });
  }

  async function relay(path: string, request: Request) {
    const credential = bindingPattern.exec(request.headers.get("authorization") ?? "")?.[1];
    const hashed = credential && digest(credential);
    const binding = hashed ? bindings.get(hashed) : undefined;
    if (!hashed || !binding) return error(401, "Telegram binding is missing or revoked.");
    if (path === "/v1/binding") {
      bindingStore?.remove(hashed);
      bindings.delete(hashed);
      binding.digests.delete(hashed);
      if (!binding.digests.size) chats.delete(binding.chat);
      return plain(null, 204);
    }
    const text = await oneField(request, "text");
    if (!text?.trim() || text.length > 16_384) return error(400, "Invalid message text.");
    if (bindings.get(hashed) !== binding) return error(401, "Telegram binding is missing or revoked.");
    if (!tg || !username) return error(503, "Telegram is not enabled.");
    const time = now();
    if (binding.nextSend > time) return rateLimited(Math.ceil((binding.nextSend - time) / 1000));
    const wait = sendWindow(time);
    if (wait) return rateLimited(wait);
    binding.nextSend = time + 1000;
    const response = await tg("sendMessage", {
      chat_id: binding.chat, text, parse_mode: "HTML", link_preview_options: { is_disabled: true },
    });
    if (response.status === 200) return plain(null, 204);
    if (response.status === 429) {
      const retry = response.retryAfter ?? 1;
      binding.nextSend = Math.max(binding.nextSend, now() + retry * 1000);
      return rateLimited(retry);
    }
    if (response.status === 403) {
      revokeChat(binding.chat);
      return error(401, "Telegram binding is missing or revoked.");
    }
    return error(502, "Telegram could not deliver this message.");
  }

  async function handle(request: Request) {
    const url = new URL(request.url);
    if (!loopbackHosts.has(url.host) || !loopbackHosts.has(request.headers.get("host") ?? url.host)) {
      return error(403, "Loopback host required.");
    }
    const { pathname: path } = url;
    const method = request.method;
    if (path.startsWith("/v1/")) {
      if (request.headers.has("origin")) return error(403, "Use Milo's local API proxy.");
      if (Number(request.headers.get("content-length")) > MAX_BODY) return error(413, "Request is too large.");
      if (method === "POST" && path === "/v1/pair") return start(request);
      if (method === "POST" && path === "/v1/claim") return claim(request);
      if ((method === "POST" && path === "/v1/send") || (method === "DELETE" && path === "/v1/binding")) {
        return relay(path, request);
      }
      return error(404, "Unknown endpoint.");
    }
    if (method === "GET") {
      if (path === "/healthz") return json({ ok: true });
      if (path === "/milo.png") return new Response(Bun.file(new URL("../dashboard/public/favicon.png", import.meta.url)), { headers: { ...headers, "Content-Type": "image/png" } });
      if (path === "/meadow/desktop.webp" || path === "/meadow/mobile.webp") {
        return new Response(Bun.file(new URL(`../dashboard/app${path}`, import.meta.url)), { headers: { ...headers, "Content-Type": "image/webp" } });
      }
      sweep();
      if (path.startsWith("/discord/start/")) return discordStart(url, path.slice("/discord/start/".length));
      if (path === CALLBACK) return discordCallback(url, request);
      if (path === "/discord/connected") return resultPage(headers, "Discord authorized", "Switch back to Milo to finish connecting your channel. You can close this tab.", true);
      if (path.startsWith("/discord/not-connected/")) {
        const reason = path.slice("/discord/not-connected/".length);
        if (Object.hasOwn(failureMessages, reason)) return resultPage(headers, "Discord not connected", failureMessages[reason as keyof typeof failureMessages]);
      }
    }
    return error(404, "Unknown endpoint.");
  }

  async function telegramUpdate(update: Record<string, unknown>) {
    if (!tg || !username) return;
    sweep();
    const callback = object(update.callback_query);
    if (callback) {
      const message = object(callback.message);
      const chat = privateSender(callback.from, message);
      const id = callback.id;
      const proof = typeof callback.data === "string" && /^confirm_[A-Za-z0-9_-]{43}$/.test(callback.data)
        ? callback.data.slice("confirm_".length) : undefined;
      if (!chat || typeof id !== "string" || id.length > 128 || !proof) return;
      const pair = proofs.get(proof);
      const confirmed = pair?.step.kind === "telegram-confirm" && pair.step.chat === chat;
      if (confirmed) transition(pair, { kind: "telegram-ready", chat });
      if (!botReplyWindow(now())) {
        await tg("answerCallbackQuery", { callback_query_id: id,
          text: confirmed ? "Confirmed. Return to Milo to finish pairing." : "Pairing unavailable. Start again in Milo." });
      }
      const messageId = message?.message_id;
      if (confirmed && typeof messageId === "number" && Number.isSafeInteger(messageId) && messageId > 0) {
        const edited = await tg("editMessageText", { chat_id: chat, message_id: messageId,
          text: "Pairing confirmed. Return to Milo to save this chat. To revoke access later, send /stop.",
          reply_markup: { inline_keyboard: [] } });
        if (edited.status !== 200) report("Telegram could not update its confirmation message.");
      }
      return;
    }
    const message = object(update.message);
    const chat = privateSender(message?.from, message);
    if (!chat || typeof message?.text !== "string" || message.forward_origin || message.via_bot) return;
    const command = /^\/(start|help|stop|delete)(?:@([A-Za-z0-9_]+))?(?: ([A-Za-z0-9_-]{43}))?$/.exec(message.text);
    if (!command || (command[2] && command[2].toLowerCase() !== username.toLowerCase())) return;
    const [, action, , proof] = command;
    if (action === "start" && proof) {
      const pair = proofs.get(proof);
      if (pair?.step.kind !== "telegram-start" || botReplyWindow(now())) return;
      const confirmation = key();
      transition(pair, { kind: "telegram-confirm", proof: confirmation, chat });
      const result = await tg("sendMessage", {
        chat_id: chat,
        text: "Allow this Milo installation to send selected agent output to this chat if you enable forwarding in Milo? Confirm only if you requested this. To revoke access later, send /stop or /delete.",
        reply_markup: { inline_keyboard: [[{ text: "Confirm pairing", callback_data: `confirm_${confirmation}` }]] },
      });
      if (result.status !== 200) forget(pair);
    } else if (!proof && (action === "stop" || action === "delete")) {
      revokeChat(chat);
      if (!botReplyWindow(now())) {
        await tg("sendMessage", { chat_id: chat, text: "Milo access revoked and pairing data removed. Previously sent messages are not deleted." });
      }
    } else if (!proof && !botReplyWindow(now())) {
      await tg("sendMessage", { chat_id: chat, text: "Start in Milo to pair this chat. Use /stop or /delete to revoke Milo access." });
    }
  }

  let offset: number | undefined;
  async function pollTelegram() {
    if (!tg || !username) return null;
    const result = await tg("getUpdates", { offset, timeout: 25, limit: 20, allowed_updates: ["message", "callback_query"] }, 32_000);
    if (result.status === 409 || result.status === 401 || result.status === 404) {
      username = undefined;
      for (const pair of pairs.values()) if (pair.step.kind.startsWith("telegram-")) forget(pair);
      report(result.status === 409 ? "Telegram disabled: polling conflict; webhook settings left untouched."
        : "Telegram disabled: bot credentials were rejected.");
      return null;
    }
    if (!Array.isArray(result.result)) return (result.retryAfter ?? 2) * 1000;
    for (const item of result.result) {
      const update = object(item);
      const id = update?.update_id;
      if (!update || typeof id !== "number" || !Number.isSafeInteger(id) || (offset !== undefined && id < offset)) continue;
      offset = id + 1;
      await telegramUpdate(update);
    }
    return 0;
  }

  return { handle, pollTelegram, sweep };
}
