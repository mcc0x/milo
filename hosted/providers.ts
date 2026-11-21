import { setTimeout as sleep } from "node:timers/promises";

export type ProviderFetch = (url: string, init: RequestInit) => Promise<Response>;

export type DiscordConfig = { clientId: string; clientSecret: string; redirectUri: string };

type TelegramReply = { status: number; result?: unknown; retryAfter?: number };
type TelegramApi = (
  method: string, payload?: Record<string, unknown>, timeout?: number, signal?: AbortSignal,
) => Promise<TelegramReply>;

type TelegramBootstrap = {
  signal?: AbortSignal;
  report: (message: string) => void;
  wait?: (delay: number, signal: AbortSignal) => Promise<unknown>;
};

export function object(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

export function telegramApi(fetch: ProviderFetch, token: string): TelegramApi {
  return async (method: string, payload: Record<string, unknown> = {}, timeout = 8_000, signal?: AbortSignal) => {
    try {
      const response = await fetch(`https://api.telegram.org/bot${token}/${method}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
        redirect: "error",
        signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(timeout)]) : AbortSignal.timeout(timeout),
      });
      const data = object(await response.json().catch(() => undefined));
      if (response.ok && data?.ok === true) return { status: 200, result: data.result };
      const status = typeof data?.error_code === "number" ? data.error_code : response.status;
      const retryAfter = object(data?.parameters)?.retry_after;
      return {
        status: status >= 400 ? status : 502,
        retryAfter: typeof retryAfter === "number" && Number.isSafeInteger(retryAfter) && retryAfter > 0
          ? Math.min(retryAfter, 86_400)
          : undefined,
      };
    } catch {
      return { status: 502 };
    }
  };
}

export async function verifyTelegramBot(tg: TelegramApi, botId: number, {
  signal, report, wait = (delay, signal) => sleep(delay, undefined, { signal }),
}: TelegramBootstrap) {
  const budget = new AbortController();
  const deadline = setTimeout(() => budget.abort(), 45_000).unref();
  const probeSignal = signal ? AbortSignal.any([signal, budget.signal]) : budget.signal;
  async function probe(method: "getWebhookInfo" | "getMe") {
    for (let attempt = 0; attempt < 3 && !probeSignal.aborted; attempt++) {
      const reply = await tg(method, {}, undefined, probeSignal);
      if (probeSignal.aborted) break;
      if (reply.status !== 429 && (reply.status < 500 || reply.status >= 600)) return reply;
      if (attempt < 2) await wait(Math.max(reply.retryAfter ?? 0, 2 ** attempt) * 1000, probeSignal);
    }
  }
  const reportTransient = () => {
    if (!signal?.aborted) report("Telegram disabled: temporary provider errors prevented verification.");
  };
  try {
    const webhook = await probe("getWebhookInfo");
    if (!webhook) return reportTransient();
    const url = object(webhook.result)?.url;
    if (webhook.status !== 200 || url !== "") {
      report(typeof url === "string" && url !== "" ? "Telegram disabled: existing webhook left untouched."
        : "Telegram disabled: webhook status could not be verified.");
      return;
    }
    const identity = await probe("getMe");
    if (!identity) return reportTransient();
    const me = object(identity.result);
    if (identity.status === 200 && me?.is_bot === true && me.id === botId &&
        typeof me.username === "string" && /^[A-Za-z0-9_]{5,32}$/.test(me.username)) return me.username;
    report("Telegram disabled: bot identity could not be verified.");
  } catch (error) {
    if (!probeSignal.aborted) throw error;
    reportTransient();
  } finally {
    clearTimeout(deadline);
  }
}

export async function discordExchange(fetch: ProviderFetch, config: DiscordConfig, code: string, verifier: string) {
  try {
    const response = await fetch("https://discord.com/api/v10/oauth2/token", {
      method: "POST",
      body: new URLSearchParams({
        client_id: config.clientId,
        client_secret: config.clientSecret,
        grant_type: "authorization_code",
        redirect_uri: config.redirectUri,
        code,
        code_verifier: verifier,
      }),
      redirect: "error",
      signal: AbortSignal.timeout(8_000),
    });
    if (!response.ok) {
      const failure = object(await response.json().catch(() => undefined))?.error;
      if (failure === "invalid_client") return "client" as const;
      if (failure === "invalid_grant") return "grant" as const;
      return "exchange" as const;
    }
    const data = object(await response.json());
    const webhook = object(data?.webhook);
    // We use only the channel webhook Discord issued, never the OAuth tokens.
    // The token's scope metadata is not the credential used to deliver an alert.
    if (!webhook || webhook.type !== 1) return "webhook" as const;
    if (webhook.application_id !== config.clientId) return "application" as const;
    if (typeof webhook.id !== "string" ||
        !/^[1-9][0-9]{4,19}$/.test(webhook.id) || typeof webhook.token !== "string" ||
        !/^[A-Za-z0-9_-]{20,100}$/.test(webhook.token) || typeof webhook.channel_id !== "string" ||
        !/^[1-9][0-9]{0,19}$/.test(webhook.channel_id)) return "fields" as const;
    return {
      credential: `https://discord.com/api/webhooks/${webhook.id}/${webhook.token}`,
      channel: webhook.channel_id,
    };
  } catch {
    return "network" as const;
  }
}

export async function discardUnclaimedWebhook(fetch: ProviderFetch, url: string) {
  try {
    const response = await fetch(url, {
      method: "DELETE",
      redirect: "error",
      signal: AbortSignal.timeout(8_000),
    });
    await response.body?.cancel();
  } catch {
    // The operator can remove orphaned webhooks from the Discord channel's Integrations settings.
  }
}
