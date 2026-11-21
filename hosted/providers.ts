export type ProviderFetch = (url: string, init: RequestInit) => Promise<Response>;

export type DiscordConfig = { clientId: string; clientSecret: string; redirectUri: string };

export function object(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

export function telegramApi(fetch: ProviderFetch, token: string) {
  return async (method: string, payload: Record<string, unknown> = {}, timeout = 8_000) => {
    try {
      const response = await fetch(`https://api.telegram.org/bot${token}/${method}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
        redirect: "error",
        signal: AbortSignal.timeout(timeout),
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
