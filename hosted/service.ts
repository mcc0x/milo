import { createHash, createHmac, randomBytes, timingSafeEqual } from "node:crypto";
import { discordExchange, discardUnclaimedWebhook, object, telegramApi, verifyTelegramBot } from "./providers";
import type { DiscordConfig, ProviderFetch } from "./providers";
import type { BindingStore } from "./bindings";
import { resultPage } from "./result";
import { clientNetwork, requestJson, validTelegramText } from "./edge";

export const HOST = "127.0.0.1";
export const PORT = 6120;
export const MAX_BODY = 32 * 1024;
const MAX_PAIR_BODY = 1024;
export const PRODUCTION_ORIGIN = "https://connect.usemilo.net";
export const TELEGRAM_RETRY_AFTER = 5;
const CALLBACK = "/discord/callback";
const FINISH = "/discord/finish";
const TTL = 10 * 60_000;
const LIMIT = 128;
const WINDOW = 60_000;
const CLIENT_LIMIT = 2048;
const REQUEST_LIMIT = 64;
const PROVIDER_LIMIT = 8;
const FINISH_ATTEMPTS = 5;
const key = () => randomBytes(32).toString("base64url");
const keyPattern = /^[A-Za-z0-9_-]{43}$/;
const finishPattern = /^[A-F0-9]{8}$/;
const loopbackHosts = new Set([`${HOST}:${PORT}`, `localhost:${PORT}`]);
const productionHost = new URL(PRODUCTION_ORIGIN).host;
const digest = (credential: string) => createHash("sha256").update(credential).digest("hex");
const baseHeaders = {
  "Cache-Control": "no-store",
  "Referrer-Policy": "no-referrer",
  "X-Content-Type-Options": "nosniff",
  "X-Frame-Options": "DENY",
  "Content-Security-Policy": "default-src 'none'; frame-ancestors 'none'",
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

type Route = "pair" | "claim" | "browser" | "binding";
type Client = {
  expires: number;
  pendingLimit: number;
  pending: number;
  active: number;
  counts: Partial<Record<Route, number>>;
};
type FinishProof = { hash: Buffer; attempts: number };

type Claim = { credential: string; digest?: string };

type Step =
  | { kind: "telegram-start"; proof: string }
  | { kind: "telegram-confirm"; proof: string; chat: number; message: number }
  | { kind: "telegram-delivering"; chat: number }
  | { kind: "telegram-ready"; chat: number; finish?: FinishProof }
  | { kind: "discord-start"; proof: string }
  | { kind: "discord-callback"; proof: string; browser: string; verifier: string }
  | { kind: "discord-exchange" }
  | { kind: "discord-ready"; credential: string; channel: string; browser?: string; finish?: FinishProof };

type Pair = { device: string; expires: number; client: Client; step: Step; reviewed?: true; claimed?: Claim };
type BoundChat = { chat: number; nextSend: number; sending: boolean; digests: Set<string> };
type Options = {
  origin?: string;
  signal?: AbortSignal;
  telegram?: { token: string };
  telegramCanSend?: () => boolean;
  onTelegramBacklog?: () => void;
  bindingStore?: BindingStore;
  discord?: DiscordConfig;
  fetch?: ProviderFetch;
  now?: () => number;
  report?: (message: string) => void;
};

export type Service = {
  handle(request: Request, clientIp?: string): Promise<Response>;
  pollTelegram(signal?: AbortSignal): Promise<number | null>;
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
  origin, signal, telegram, telegramCanSend, onTelegramBacklog, discord, bindingStore,
  fetch = globalThis.fetch, now = () => performance.now(), report = () => {},
}: Options = {}): Promise<Service> {
  if (origin !== undefined && origin !== PRODUCTION_ORIGIN) throw new Error("Invalid relay origin.");
  const production = origin === PRODUCTION_ORIGIN;
  if (discord && (production ? discord.redirectUri !== `${PRODUCTION_ORIGIN}${CALLBACK}`
    : ![...loopbackHosts].some((host) => discord.redirectUri === `http://${host}${CALLBACK}`))) {
    throw new Error("Invalid Discord callback configuration.");
  }
  const discordOrigin = discord && new URL(discord.redirectUri).origin;
  const cookieName = production ? "__Host-milo_discord_browser" : "milo_discord_browser";
  const callbackCookie = (value: string, maxAge: number) =>
    `${cookieName}=${value}; Path=${production ? "/" : CALLBACK}; Max-Age=${maxAge}; HttpOnly; SameSite=Lax${production ? "; Secure" : ""}`;
  const headers = production ? { ...baseHeaders, "Strict-Transport-Security": "max-age=31536000" } : baseHeaders;
  const json = (body: object, status = 200, extra?: Record<string, string>) =>
    Response.json(body, { status, headers: { ...headers, ...extra } });
  const error = (status: number, message: string) => json({ error: message }, status);
  // Milo's sender reads the wait from the body, as it does for Telegram and Discord.
  const rateLimited = (seconds: number) =>
    json({ error: "Too many requests; try again later.", retry_after: seconds }, 429, { "Retry-After": String(seconds) });
  const plain = (body: string | null, status: number, extra?: Record<string, string>) =>
    new Response(body, { status, headers: { ...headers, "Content-Type": "text/plain; charset=utf-8", ...extra } });
  const absolute = (path: string) => production ? `${PRODUCTION_ORIGIN}${path}` : path;
  const finishSecret = production ? randomBytes(32) : undefined;
  const finishMac = (value: string) => createHmac("sha256", finishSecret!).update(value).digest();
  const finishProof = (code: string): FinishProof => ({ hash: finishMac(`code:${code}`), attempts: 0 });
  const browserFinish = (browser: string) => finishMac(`browser:${browser}`).subarray(0, 4).toString("hex").toUpperCase();
  const botId = telegram && /^([1-9]\d{0,15}):[A-Za-z0-9_-]{1,128}$/.exec(telegram.token)?.[1];
  if (telegram && (!botId || !Number.isSafeInteger(Number(botId)))) throw new Error("Invalid Telegram bot configuration.");
  if (production && telegram && !/^[A-Za-z0-9_-]{16}$/.test(bindingStore?.issuer ?? "")) {
    throw new Error("Production Telegram requires a persistent binding issuer.");
  }
  const tg = telegram && telegramApi(fetch, telegram.token);
  let username = tg && await verifyTelegramBot(tg, Number(botId), { signal, report });
  let telegramCaughtUp = false;

  const pairs = new Map<string, Pair>();
  const recoveries = new Map<string, Pair>();
  const completions = new Map<string, Pair>();
  const clients = new Map<string, Client>();
  const cleanup: string[] = [];
  let cleanupActive = 0;
  let exchangeActive = 0;
  let sendActive = 0;
  let requestsActive = 0;
  let nextSweep = 0;
  const bindingLimit = production ? 10_000 : LIMIT;
  const bindingPrefix = production ? "milorelay_prod_" : "milorelay_";
  const verifiedClientLimits: Record<Route, number> = { pair: 12, claim: 240, browser: 60, binding: 120 };
  const unknownClientLimits: Record<Route, number> = { pair: 4, claim: 20, browser: 12, binding: 30 };
  const proofs = new Map<string, Pair>();
  const bindings = new Map<string, BoundChat>();
  const chats = new Map<number, BoundChat>();
  function remember(hashed: string, chat: number) {
    let binding = chats.get(chat);
    if (!binding) {
      binding = { chat, nextSend: 0, sending: false, digests: new Set() };
      chats.set(chat, binding);
    }
    binding.digests.add(hashed);
    bindings.set(hashed, binding);
  }
  const saved = bindingStore?.load(bindingLimit + 1) ?? [];
  if (saved.length > bindingLimit) throw new Error("Too many stored Telegram bindings.");
  for (const row of saved) {
    if (!/^[a-f0-9]{64}$/.test(row.digest) || !Number.isSafeInteger(row.chat) || row.chat <= 0) {
      throw new Error("Invalid stored Telegram binding.");
    }
    remember(row.digest, row.chat);
  }
  const sendWindow = windowLimit(15, 1000);
  const botReplyWindow = windowLimit(12, 1000);

  function drainCleanup() {
    while (cleanupActive < 4 && cleanup.length) {
      cleanupActive++;
      void discardUnclaimedWebhook(fetch, cleanup.shift()!).finally(() => {
        cleanupActive--;
        drainCleanup();
      });
    }
  }

  function forget(pair: Pair, discard = false) {
    if (!pairs.delete(pair.device)) {
      recoveries.delete(pair.device);
      return;
    }
    pair.client.pending--;
    const step = pair.step;
    if ("proof" in step) proofs.delete(step.proof);
    if (step.kind === "discord-ready") {
      if (step.browser) completions.delete(step.browser);
      if (discard) {
        cleanup.push(step.credential);
        drainCleanup();
      }
    }
  }

  function retainClaim(pair: Pair, claimed: Claim) {
    forget(pair);
    pair.claimed = claimed;
    recoveries.set(pair.device, pair);
  }

  function sweep() {
    const time = now();
    if (time < nextSweep) return;
    nextSweep = time + 1000;
    for (const pair of pairs.values()) {
      if (time >= pair.expires && pair.step.kind !== "discord-exchange") forget(pair, true);
    }
    for (const pair of recoveries.values()) if (time >= pair.expires) forget(pair);
    for (const [address, client] of clients) {
      if (time >= client.expires && !client.active && !client.pending) clients.delete(address);
    }
  }

  function activePair(pair: Pair | undefined) {
    if (!pair) return;
    if (now() < pair.expires) return pair;
    if (pair.step.kind !== "discord-exchange") forget(pair, true);
  }

  async function limited(route: Route, clientIp: string | undefined, respond: (client: Client) => Response | Promise<Response>) {
    const address = production ? clientNetwork(clientIp) : "local";
    const time = now();
    const limits = address ? verifiedClientLimits : unknownClientLimits;
    let client = clients.get(address);
    if (!client) {
      if (clients.size >= CLIENT_LIMIT) {
        sweep();
        if (clients.size >= CLIENT_LIMIT) return rateLimited(1);
      }
      client = { counts: {}, expires: time + WINDOW, active: 0, pending: 0, pendingLimit: production ? limits.pair : LIMIT };
      clients.set(address, client);
    } else if (time >= client.expires) {
      client.counts = {};
      client.expires = time + WINDOW;
    }
    const count = client.counts[route] ?? 0;
    if (count >= limits[route]) return rateLimited(Math.ceil((client.expires - time) / 1000));
    if ((production && client.active >= 4) || requestsActive >= REQUEST_LIMIT) return rateLimited(1);
    client.counts[route] = count + 1;
    client.active++;
    requestsActive++;
    try {
      return await respond(client);
    } finally {
      client.active--;
      requestsActive--;
    }
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
    for (const pair of recoveries.values()) {
      if (pair.step.kind === "telegram-ready" && pair.step.chat === chat) forget(pair);
    }
  }

  function invalidBody(status: number) {
    return error(status, status === 413 ? "Request is too large." : status === 408 ? "Request timed out." : "Invalid request body.");
  }

  async function start(request: Request, client: Client) {
    const body = await requestJson(request, MAX_PAIR_BODY);
    if (typeof body === "number") return invalidBody(body);
    const provider = Object.keys(body).length === 1 ? body.provider : undefined;
    if (provider !== "telegram" && provider !== "discord") return error(400, "Choose Telegram or Discord.");
    if ((provider === "telegram" && !username) || (provider === "discord" && !discordOrigin)) {
      return error(503, "Provider is not enabled.");
    }
    sweep();
    if (client.pending >= client.pendingLimit) return rateLimited(60);
    if (pairs.size + cleanup.length + cleanupActive >= LIMIT) return error(503, "Too many pending pairings.");
    const deviceKey = key();
    const proof = key();
    const hashed = digest(proof);
    const step: Step = provider === "telegram" ? { kind: "telegram-start", proof: hashed } : { kind: "discord-start", proof: hashed };
    const pair = { device: digest(deviceKey), expires: now() + TTL, client, step };
    client.pending++;
    pairs.set(pair.device, pair);
    proofs.set(hashed, pair);
    const url = provider === "telegram"
      ? `https://t.me/${username}?start=${proof}`
      : `${discordOrigin}/discord/start/${proof}`;
    return json({ deviceKey, url });
  }

  async function claim(request: Request, review: boolean) {
    const body = await requestJson(request, MAX_PAIR_BODY);
    if (typeof body === "number") return invalidBody(body);
    const { deviceKey, finishCode } = body;
    if (Object.keys(body).length !== (production ? 2 : 1) ||
        typeof deviceKey !== "string" || !keyPattern.test(deviceKey) ||
        (production && (typeof finishCode !== "string" || !finishPattern.test(finishCode)))) {
      return error(400, "Invalid pairing claim.");
    }
    const device = digest(deviceKey);
    const pair = activePair(pairs.get(device) ?? recoveries.get(device));
    if (!pair) return error(404, "Pairing is unavailable; start again.");
    const step = pair.step;
    if (step.kind !== "discord-ready" && step.kind !== "telegram-ready") return json({ pending: true }, 202);
    if (production && (!step.finish || !timingSafeEqual(step.finish.hash, finishMac(`code:${finishCode}`)))) {
      if (!step.finish || ++step.finish.attempts >= FINISH_ATTEMPTS) {
        forget(pair, true);
        return error(404, "Pairing is unavailable; start again.");
      }
      return error(400, "Finish code does not match.");
    }
    const metadata = step.kind === "discord-ready"
      ? { provider: "discord", label: `Discord channel · ${step.channel}`, chat: "" }
      : { provider: "telegram", label: `Telegram DM · ${step.chat}`, chat: String(step.chat) };
    if (review) {
      pair.reviewed = true;
      return json(metadata);
    }
    if (production && !pair.reviewed) return error(409, "Review the destination before saving.");
    if (production && step.kind === "telegram-ready" && (!telegramCaughtUp || telegramCanSend?.() === false)) {
      return error(503, "Telegram is not ready; try again later.");
    }
    if (pair.reviewed) {
      if (pair.claimed) return json({ ...metadata, credential: pair.claimed.credential });
      sweep();
      if (recoveries.size >= 256) return error(503, "Too many recently saved pairings; try again later.");
    }
    if (step.kind === "discord-ready") {
      if (pair.reviewed) retainClaim(pair, { credential: step.credential });
      else forget(pair);
      return json({ ...metadata, credential: step.credential });
    }
    if (bindings.size >= bindingLimit || (production && (chats.get(step.chat)?.digests.size ?? 0) >= 8)) {
      return error(503, "Too many active Telegram bindings.");
    }
    const credential = production ? `${bindingPrefix}${bindingStore!.issuer}${randomBytes(20).toString("base64url")}` : `${bindingPrefix}${key()}`;
    const hashed = digest(credential);
    bindingStore?.insert(hashed, step.chat);
    remember(hashed, step.chat);
    if (pair.reviewed) retainClaim(pair, { credential, digest: hashed });
    else forget(pair);
    return json({ ...metadata, credential });
  }

  function discordStart(url: URL, proof: string) {
    const pair = keyPattern.test(proof) ? activePair(proofs.get(digest(proof))) : undefined;
    if (!discord || (production ? url.search !== "" : url.origin !== discordOrigin) || pair?.step.kind !== "discord-start") {
      return plain("This Discord pairing link is unavailable. Start again in Milo.", 404);
    }
    const state = key();
    const browser = key();
    const verifier = key();
    transition(pair, { kind: "discord-callback", proof: digest(state), browser: digest(browser), verifier });
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

  function browserProof(request: Request) {
    const header = request.headers.get("cookie");
    if (!header || header.length > 8192) return;
    const name = `${cookieName}=`;
    let value: string | undefined;
    for (const part of header.split(";")) {
      const item = part.trim();
      if (!item.startsWith(name)) continue;
      if (value !== undefined) return;
      value = item.slice(name.length);
    }
    if (value && keyPattern.test(value)) return digest(value);
  }

  function discordFinish(request: Request) {
    const browser = browserProof(request);
    const pair = browser ? activePair(completions.get(browser)) : undefined;
    if (!browser || pair?.step.kind !== "discord-ready") {
      return resultPage(headers, "Pairing not verified", "Return to Milo and start a new Discord connection in this browser.", false, 404);
    }
    return resultPage(headers, "Discord authorized",
      "Enter this finish code in the Milo window where you started pairing. Do not share the code.", true, 200, browserFinish(browser));
  }
  async function discordCallback(url: URL, request: Request) {
    const states = url.searchParams.getAll("state");
    const state = states[0];
    const pair = states.length === 1 && keyPattern.test(state) ? activePair(proofs.get(digest(state))) : undefined;
    const issuers = url.searchParams.getAll("iss");
    const browser = browserProof(request);
    if (!discord || !browser || (!production && url.origin !== discordOrigin) || pair?.step.kind !== "discord-callback" ||
        issuers.length !== 1 || issuers[0] !== "https://discord.com" || browser !== pair.step.browser) {
      return resultPage(headers, "Pairing not verified", "Return to Milo and start a new Discord connection in this browser.", false, 400);
    }
    const codes = url.searchParams.getAll("code");
    const errors = url.searchParams.getAll("error");
    const validCode = errors.length === 0 && codes.length === 1 && /^[\x21-\x7e]{1,512}$/.test(codes[0]);
    if (validCode && exchangeActive >= PROVIDER_LIMIT) return rateLimited(1);
    const { verifier } = pair.step;
    transition(pair, { kind: "discord-exchange" }); // Spend state before making any network request.
    let failure: keyof typeof failureMessages = codes.length === 0 && errors.length === 1 &&
      errors[0] === "access_denied" ? "denied" : "code";
    if (validCode) {
      exchangeActive++;
      try {
        const result = await discordExchange(fetch, discord, codes[0], verifier);
        if (typeof result === "string") {
          failure = result;
        } else if (now() >= pair.expires) {
          cleanup.push(result.credential);
          drainCleanup();
          failure = "expired";
        } else if (production) {
          transition(pair, { kind: "discord-ready", ...result, browser, finish: finishProof(browserFinish(browser)) });
          completions.set(browser, pair);
          return plain(null, 303, { Location: absolute(FINISH) });
        } else {
          transition(pair, { kind: "discord-ready", ...result });
          return plain(null, 303, { Location: "/discord/connected", "Set-Cookie": callbackCookie("", 0) });
        }
      } finally {
        exchangeActive--;
      }
    }
    forget(pair);
    report(`Discord pairing: ${failure}.`);
    return plain(null, 303, { Location: absolute(`/discord/not-connected/${failure}`), "Set-Cookie": callbackCookie("", 0) });
  }

  async function relay(path: string, request: Request) {
    const authorization = request.headers.get("authorization") ?? "";
    const prefix = `Bearer ${bindingPrefix}`;
    const token = authorization.startsWith(prefix) && authorization.slice(prefix.length);
    const hashed = typeof token === "string" && keyPattern.test(token) ? digest(authorization.slice(7)) : undefined;
    if (!hashed) return error(401, "Telegram binding is missing or revoked.");
    const issuer = production && typeof token === "string" ? token.slice(0, 16) : undefined;
    if (production && (!bindingStore || !issuer || issuer !== bindingStore.issuer)) {
      return error(503, "The issuing Telegram store is unavailable.");
    }
    const binding = bindings.get(hashed);
    if (path === "/v1/binding") {
      if (production) {
        try {
          if (!bindingStore!.removeIssued(hashed, issuer!)) return error(503, "The issuing Telegram store is unavailable.");
        } catch {
          report("Telegram binding revocation could not reach its issuing store.");
          return error(503, "The issuing Telegram store is unavailable.");
        }
      } else {
        if (!binding) return error(401, "Telegram binding is missing or revoked.");
        bindingStore?.remove(hashed);
      }
      for (const pair of recoveries.values()) if (pair.claimed?.digest === hashed) forget(pair);
      bindings.delete(hashed);
      binding?.digests.delete(hashed);
      if (binding && !binding.digests.size && !binding.sending) chats.delete(binding.chat);
      return plain(null, 204);
    }
    if (!binding) return error(401, "Telegram binding is missing or revoked.");
    const body = await requestJson(request, MAX_BODY);
    if (typeof body === "number") return invalidBody(body);
    const text = Object.keys(body).length === 1 ? body.text : undefined;
    if (typeof text !== "string" || !validTelegramText(text, production)) return error(400, "Invalid message text.");
    if (bindings.get(hashed) !== binding) return error(401, "Telegram binding is missing or revoked.");
    if (!tg || !username) return error(503, "Telegram is not enabled.");
    if (production && (!telegramCaughtUp || telegramCanSend?.() === false)) {
      return json({ error: "Telegram is not ready; try again later.", retry_after: TELEGRAM_RETRY_AFTER },
        503, { "Retry-After": String(TELEGRAM_RETRY_AFTER) });
    }
    const time = now();
    if (binding.nextSend > time) return rateLimited(Math.ceil((binding.nextSend - time) / 1000));
    if (binding.sending || sendActive >= PROVIDER_LIMIT) return rateLimited(1);
    const wait = sendWindow(time);
    if (wait) return rateLimited(wait);
    binding.nextSend = time + 1000;
    binding.sending = true;
    sendActive++;
    try {
      // Not ended by a stop: Telegram may already hold the message, and its answer is the only
      // way to say so. The call's own eight seconds end inside the time a stop waits for requests.
      const response = await tg("sendMessage", {
        chat_id: binding.chat, text, parse_mode: "HTML", link_preview_options: { is_disabled: true },
      });
      if (response.status === 200) return plain(null, 204);
      if (response.status === 429 && response.confirmedError) {
        const retry = response.retryAfter ?? 1;
        binding.nextSend = Math.max(binding.nextSend, now() + retry * 1000);
        return rateLimited(retry);
      }
      if (response.status === 403 && response.confirmedError) {
        if (chats.get(binding.chat) === binding) revokeChat(binding.chat);
        return error(401, "Telegram binding is missing or revoked.");
      }
      if (response.confirmedError && response.status < 500) return error(422, "Telegram rejected this message.");
      // A timeout, unreadable answer or upstream server error may follow a successful send.
      return error(502, "Telegram delivery could not be confirmed.");
    } finally {
      sendActive--;
      binding.sending = false;
      if (!binding.digests.size && chats.get(binding.chat) === binding) chats.delete(binding.chat);
    }
  }

  async function handle(request: Request, clientIp?: string) {
    const url = new URL(request.url);
    const host = request.headers.get("host")?.toLowerCase();
    // Railway terminates TLS; Bun may use http internally to build request.url.
    if (production ? (host !== productionHost || url.host !== productionHost ||
                      (url.protocol !== "http:" && url.protocol !== "https:"))
      : (url.protocol !== "http:" || !loopbackHosts.has(url.host) || (host !== undefined && host !== url.host))) {
      return error(403, production ? "Canonical relay host required." : "Loopback host required.");
    }
    if (url.pathname.length + url.search.length > 4096) return error(414, "Request URL is too long.");
    const { pathname: path } = url;
    const method = request.method;
    if (path.startsWith("/v1/")) {
      if (request.headers.has("origin")) return error(403, "Use Milo's local API proxy.");
      if (url.search) return error(400, "Unexpected query parameters.");
      if (method === "POST" && path === "/v1/pair") return limited("pair", clientIp, (client) => start(request, client));
      const review = path === "/v1/preview";
      if (method === "POST" && (review || path === "/v1/claim")) return limited("claim", clientIp, () => claim(request, review));
      if ((method === "POST" && path === "/v1/send") || (method === "DELETE" && path === "/v1/binding")) {
        return limited("binding", clientIp, () => relay(path, request));
      }
      return error(404, "Unknown endpoint.");
    }
    if (method === "GET") {
      if (path === "/healthz") return json({ ok: true });
      if (path === "/milo.png") return new Response(Bun.file(new URL("../dashboard/public/favicon.png", import.meta.url)), { headers: { ...headers, "Content-Type": "image/png" } });
      if (path === "/meadow/desktop.webp" || path === "/meadow/mobile.webp") {
        return new Response(Bun.file(new URL(`../dashboard/app${path}`, import.meta.url)), { headers: { ...headers, "Content-Type": "image/webp" } });
      }
      if (path.startsWith("/discord/start/")) return limited("browser", clientIp, () =>
        discordStart(url, path.slice("/discord/start/".length)));
      if (path === CALLBACK) return limited("browser", clientIp, () => discordCallback(url, request));
      if (production && path === FINISH && !url.search) return limited("browser", clientIp, () => discordFinish(request));
      if (path === "/discord/connected") return production
        ? resultPage(headers, "Finish in Milo", "If you authorized Discord, enter the finish code shown in that browser.")
        : resultPage(headers, "Discord authorized", "Switch back to Milo to finish connecting your channel. You can close this tab.", true);
      if (path.startsWith("/discord/not-connected/")) {
        const reason = path.slice("/discord/not-connected/".length);
        if (Object.hasOwn(failureMessages, reason)) return resultPage(headers, "Discord not connected", failureMessages[reason as keyof typeof failureMessages]);
      }
    }
    return error(404, "Unknown endpoint.");
  }

  async function telegramUpdate(update: Record<string, unknown>, pollSignal?: AbortSignal) {
    if (!tg || !username) return;
    const callback = object(update.callback_query);
    if (callback) {
      const message = object(callback.message);
      const chat = privateSender(callback.from, message);
      const id = callback.id;
      const proof = typeof callback.data === "string" && /^confirm_[A-Za-z0-9_-]{43}$/.test(callback.data)
        ? callback.data.slice("confirm_".length) : undefined;
      if (!chat || typeof id !== "string" || !id || id.length > 128 || !proof) return;
      const messageId = message?.message_id;
      const pair = activePair(proofs.get(digest(proof)));
      if (pair?.step.kind !== "telegram-confirm" || pair.step.chat !== chat || pair.step.message !== messageId) {
        if (!botReplyWindow(now())) await tg("answerCallbackQuery", { callback_query_id: id,
          text: "Pairing unavailable. Start again in Milo." }, undefined, pollSignal);
        return;
      }
      transition(pair, { kind: "telegram-delivering", chat });
      if (!botReplyWindow(now())) await tg("answerCallbackQuery", { callback_query_id: id,
        text: "Check the confirmation message to continue." }, undefined, pollSignal);
      if (pollSignal?.aborted || !activePair(pairs.get(pair.device))) {
        forget(pair);
        return;
      }
      const code = production ? randomBytes(4).toString("hex").toUpperCase() : undefined;
      const edited = await tg("editMessageText", { chat_id: chat, message_id: messageId,
        text: code ? `Pairing confirmed. Enter this finish code only in the Milo window where you started pairing: ${code}. Do not share it. To revoke access, send /stop.`
          : "Pairing confirmed. Return to Milo to save this chat. To revoke access later, send /stop.",
        reply_markup: { inline_keyboard: [] },
      }, undefined, pollSignal);
      const editedMessage = object(edited.result);
      const delivered = edited.status === 200 && editedMessage?.message_id === messageId && object(editedMessage?.chat)?.id === chat;
      if (!pollSignal?.aborted && activePair(pairs.get(pair.device)) && (delivered || !production)) {
        transition(pair, { kind: "telegram-ready", chat, finish: code ? finishProof(code) : undefined });
      } else {
        forget(pair);
      }
      if (!delivered && !pollSignal?.aborted) report("Telegram could not update its confirmation message.");
      return;
    }
    const message = object(update.message);
    const chat = privateSender(message?.from, message);
    if (!chat || typeof message?.text !== "string" || message.text.length > 90 || message.forward_origin || message.via_bot) return;
    const command = /^\/(start|help|stop|delete)(?:@([A-Za-z0-9_]{5,32}))?(?: ([A-Za-z0-9_-]{43}))?$/.exec(message.text);
    if (!command || (command[2] && command[2].toLowerCase() !== username.toLowerCase())) return;
    const [, action, , proof] = command;
    if (action === "start" && proof) {
      const pair = activePair(proofs.get(digest(proof)));
      if (pair?.step.kind !== "telegram-start" || botReplyWindow(now())) return;
      const confirmation = key();
      const result = await tg("sendMessage", {
        chat_id: chat,
        text: "Allow this Milo installation to send selected agent output to this chat if you enable forwarding in Milo? Confirm only if you requested this. To revoke access later, send /stop or /delete.",
        reply_markup: { inline_keyboard: [[{ text: "Confirm pairing", callback_data: `confirm_${confirmation}` }]] },
      }, undefined, pollSignal);
      const sent = object(result.result);
      const messageId = sent?.message_id;
      if (pollSignal?.aborted || !activePair(pairs.get(pair.device)) || result.status !== 200 ||
          typeof messageId !== "number" || !Number.isSafeInteger(messageId) || messageId <= 0 ||
          object(sent?.chat)?.id !== chat) {
        forget(pair);
        return;
      }
      transition(pair, { kind: "telegram-confirm", proof: digest(confirmation), chat, message: messageId });
    } else if (!proof && (action === "stop" || action === "delete")) {
      revokeChat(chat);
      if (!botReplyWindow(now())) {
        await tg("sendMessage", { chat_id: chat, text: "Milo access revoked and pairing data removed. Previously sent messages are not deleted." }, undefined, pollSignal);
      }
    } else if (!proof && !botReplyWindow(now())) {
      await tg("sendMessage", { chat_id: chat, text: "Start in Milo to pair this chat. Use /stop or /delete to revoke Milo access." }, undefined, pollSignal);
    }
  }

  let offset: number | undefined;
  let polling = false;
  async function pollTelegram(pollSignal = signal) {
    if (pollSignal?.aborted) {
      telegramCaughtUp = false;
      return null;
    }
    if (!tg || !username) return null;
    if (polling) return 1000;
    polling = true;
    try {
      // Telegram answers an idle long poll only when its timeout ends. Until the queue is known to be
      // empty, ask without waiting, so that a start or a failed poll does not hold sends back that long.
      const result = await tg("getUpdates", {
        offset, timeout: telegramCaughtUp ? 25 : 0, limit: 20, allowed_updates: ["message", "callback_query"],
      }, 32_000, pollSignal);
      if (pollSignal?.aborted) return null;
      if (!Array.isArray(result.result) || result.result.length) telegramCaughtUp = false;
      if (result.status === 409 || result.status === 401 || result.status === 404) {
        username = undefined;
        for (const pair of pairs.values()) if (pair.step.kind.startsWith("telegram-")) forget(pair);
        report(result.status === 409 ? "Telegram disabled: polling conflict; webhook settings left untouched."
          : "Telegram disabled: bot credentials were rejected.");
        return null;
      }
      if (!Array.isArray(result.result) || result.result.length > 20) return (result.retryAfter ?? 2) * 1000;
      if (result.result.length) onTelegramBacklog?.();
      for (const item of result.result) {
        if (pollSignal?.aborted) return null;
        const update = object(item);
        const id = update?.update_id;
        if (!update || typeof id !== "number" || !Number.isSafeInteger(id) || id < 0 ||
            id === Number.MAX_SAFE_INTEGER || (offset !== undefined && id < offset)) continue;
        await telegramUpdate(update, pollSignal);
        if (pollSignal?.aborted) return null;
        offset = id + 1;
      }
      telegramCaughtUp = result.result.length < 20;
      return telegramCaughtUp ? 0 : -1;
    } finally {
      if (pollSignal?.aborted) telegramCaughtUp = false;
      polling = false;
    }
  }

  return { handle, pollTelegram, sweep };
}
