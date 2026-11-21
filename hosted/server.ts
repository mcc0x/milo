import { resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { HOST, PORT, MAX_BODY, PRODUCTION_ORIGIN, TELEGRAM_RETRY_AFTER, createService, type Service } from "./service";
import { openBindings, verifyDataMount, type BindingStore } from "./bindings";
import type { DiscordConfig, ProviderFetch } from "./providers";

const productionHost = new URL(PRODUCTION_ORIGIN).host;
const healthcheckHost = "healthcheck.railway.app";
const callbackPath = "/discord/callback";
const telegramPollMaxAge = 60_000;

type RelayConfig = {
  origin?: typeof PRODUCTION_ORIGIN;
  hostname: string;
  port: number;
  dataDirectory?: string;
  telegram?: { token: string };
  discord?: DiscordConfig;
};

type RuntimeOptions = {
  fetch?: ProviderFetch;
  report?: (message: string) => void;
  now?: () => number;
};

export function parseRelayConfig(env: Record<string, string | undefined>): RelayConfig {
  const origin = env.MILO_RELAY_ORIGIN;
  if (origin !== undefined && origin !== PRODUCTION_ORIGIN) {
    throw new Error(`MILO_RELAY_ORIGIN must be ${PRODUCTION_ORIGIN} or unset for the local relay.`);
  }
  const dataDirectory = env.MILO_RELAY_DATA_DIR;
  const portString = env.PORT ?? "";
  let port = PORT;
  if (origin) {
    if ((env.RAILWAY_ENVIRONMENT_NAME !== undefined || env.RAILWAY_ENVIRONMENT_ID !== undefined ||
         env.RAILWAY_PROJECT_ID !== undefined || env.RAILWAY_DEPLOYMENT_ID !== undefined) &&
        env.RAILWAY_ENVIRONMENT_NAME !== "production") {
      throw new Error("The production relay may only run in the Railway production environment.");
    }
    if (env.NODE_TLS_REJECT_UNAUTHORIZED === "0" ||
        env.BUN_CONFIG_VERBOSE_FETCH === "1" || env.BUN_CONFIG_VERBOSE_FETCH === "curl") {
      throw new Error("Production requires TLS verification and fetch debug logging disabled.");
    }
    port = Number(portString);
    if (!/^[1-9]\d{0,4}$/.test(portString) || port > 65535) {
      throw new Error("PORT must be an integer from 1 through 65535 in production.");
    }
    if (!dataDirectory || !/^\/[A-Za-z0-9._/-]+$/.test(dataDirectory) ||
        dataDirectory === "/" || resolve(dataDirectory) !== dataDirectory ||
        (env.RAILWAY_VOLUME_MOUNT_PATH !== undefined && env.RAILWAY_VOLUME_MOUNT_PATH !== dataDirectory)) {
      throw new Error("Set MILO_RELAY_DATA_DIR to the absolute persistent volume mount path.");
    }
  } else if (dataDirectory !== undefined) {
    throw new Error("MILO_RELAY_DATA_DIR requires the production relay origin.");
  }

  const optIn = env.MILO_TELEGRAM_LONG_POLL;
  const token = env.MILO_TELEGRAM_BOT_TOKEN;
  if (optIn !== undefined && optIn !== "0" && optIn !== "1") {
    throw new Error("Set MILO_TELEGRAM_LONG_POLL to 0 or 1.");
  }
  if (origin && optIn === "0" && token !== undefined) {
    throw new Error("Unset MILO_TELEGRAM_BOT_TOKEN if Telegram polling is disabled.");
  }
  const pollTelegram = optIn === "1" || (origin !== undefined && token !== undefined);
  if (pollTelegram) {
    const id = /^([1-9]\d{0,15}):[A-Za-z0-9_-]{1,128}$/.exec(token ?? "")?.[1];
    if (!id || !Number.isSafeInteger(Number(id))) {
      throw new Error("Set a valid MILO_TELEGRAM_BOT_TOKEN to enable Telegram polling.");
    }
  }

  const clientId = env.MILO_DISCORD_CLIENT_ID;
  const clientSecret = env.MILO_DISCORD_CLIENT_SECRET;
  const redirectUri = env.MILO_DISCORD_REDIRECT_URI ?? (origin && `${origin}${callbackPath}`);
  let discord: DiscordConfig | undefined;
  if (clientId !== undefined || clientSecret !== undefined || env.MILO_DISCORD_REDIRECT_URI !== undefined) {
    if (!clientId || !/^[1-9]\d{0,19}$/.test(clientId) || !clientSecret ||
        !/^[\x21-\x7e]{1,256}$/.test(clientSecret)) {
      throw new Error("Set valid MILO_DISCORD_CLIENT_ID and MILO_DISCORD_CLIENT_SECRET together.");
    }
    const callbacks = origin
      ? [`${origin}${callbackPath}`]
      : [`http://${HOST}:${PORT}${callbackPath}`, `http://localhost:${PORT}${callbackPath}`];
    if (!redirectUri || !callbacks.includes(redirectUri)) {
      throw new Error("MILO_DISCORD_REDIRECT_URI must match the relay's exact Discord callback URL.");
    }
    discord = { clientId, clientSecret, redirectUri };
  }
  const telegram = pollTelegram && token ? { token } : undefined;
  if (origin && !telegram && !discord) throw new Error("Configure at least one provider for the production relay.");

  return { origin, hostname: origin ? "0.0.0.0" : HOST, port, dataDirectory, telegram, discord };
}

function json(body: object, status = 200, extra?: Record<string, string>) {
  return Response.json(body, {
    status,
    headers: { "Cache-Control": "no-store", "Referrer-Policy": "no-referrer", ...extra },
  });
}

export function healthResponse(request: Request, production: boolean, ready: boolean, localPort = PORT): Response | undefined {
  if (request.method !== "GET") return;
  const url = new URL(request.url);
  if ((url.pathname !== "/healthz" && url.pathname !== "/readyz") || url.search) return;
  const host = request.headers.get("host") ?? (production ? "" : url.host);
  if (production) {
    if (host !== url.host || (host !== productionHost && (host !== healthcheckHost || url.pathname !== "/readyz"))) return;
  } else if (host !== url.host || (host !== `${HOST}:${localPort}` && host !== `localhost:${localPort}`)) {
    return;
  }
  const ok = url.pathname === "/healthz" || ready;
  return json({ ok }, ok ? 200 : 503);
}

export function startRelay(config: RelayConfig, {
  fetch, report = console.error, now = () => performance.now(),
}: RuntimeOptions = {}) {
  let bindingStore: BindingStore | undefined;
  try {
    if (config.origin) verifyDataMount(config.dataDirectory!);
    if (config.telegram) bindingStore = openBindings(config.telegram.token.split(":", 1)[0], config.dataDirectory);
  } catch {
    throw new Error(config.origin
      ? "Cannot open production relay state. Attach a writable persistent volume with owner-only permissions."
      : "Cannot open the local Telegram binding database.");
  }

  const controller = new AbortController();
  const { signal } = controller;
  let service: Service | undefined;
  let lastTelegramPoll: number | undefined;
  function telegramCanSend() {
    return !signal.aborted && lastTelegramPoll !== undefined && now() - lastTelegramPoll < telegramPollMaxAge;
  }
  function isReady() {
    return service !== undefined && !signal.aborted && (!config.telegram || telegramCanSend());
  }
  const requests = new Set<Promise<Response>>();
  let server: Bun.Server<undefined>;
  try {
    server = Bun.serve({
      hostname: config.hostname,
      port: config.port,
      maxRequestBodySize: MAX_BODY,
      idleTimeout: 10,
      development: false,
      fetch(request) {
        const health = healthResponse(request, !!config.origin, isReady(), server.port ?? PORT);
        if (health) return health;
        if (!service || signal.aborted) {
          if (config.telegram && request.method === "POST" && new URL(request.url).pathname === "/v1/send") {
            return json({ error: "Relay is not ready.", retry_after: TELEGRAM_RETRY_AFTER },
              503, { "Retry-After": String(TELEGRAM_RETRY_AFTER) });
          }
          return json({ error: "Relay is not ready." }, 503);
        }
        // The production listener is reached through Railway's HTTPS ingress,
        // which supplies X-Real-IP. Never use the client-controlled forwarded chain.
        const clientIp = config.origin ? request.headers.get("x-real-ip") ?? undefined : undefined;
        const response = service.handle(request, clientIp);
        requests.add(response);
        void response.then(() => requests.delete(response), () => requests.delete(response));
        return response;
      },
      error: () => json({ error: "Relay error." }, 500),
    });
  } catch {
    bindingStore?.close();
    throw new Error("Could not listen on the configured relay address.");
  }

  const sweep = setInterval(() => service?.sweep(), 5_000).unref();
  const run = (async () => {
    try {
      service = await createService({
        origin: config.origin, telegram: config.telegram, discord: config.discord,
        bindingStore, fetch, report, signal,
        telegramCanSend,
        onTelegramBacklog: () => { lastTelegramPoll = undefined; },
      });
      if (!config.telegram) return;
      while (!signal.aborted) {
        const delay = await service.pollTelegram(signal);
        if (signal.aborted) return;
        if (delay === null) {
          lastTelegramPoll = undefined;
          if (config.origin) throw new Error("Telegram polling could not start or has stopped.");
          return;
        }
        if (delay === 0) lastTelegramPoll = now();
        else {
          lastTelegramPoll = undefined;
          if (delay > 0) await sleep(delay, undefined, { signal });
        }
      }
    } catch (error) {
      if (!signal.aborted) {
        lastTelegramPoll = undefined;
        throw error;
      }
    }
  })();

  let stopping: Promise<void> | undefined;
  function stop() {
    return stopping ??= (async () => {
      controller.abort();
      clearInterval(sweep);
      try {
        await Promise.allSettled([server.stop(), run, ...requests]);
      } finally {
        bindingStore?.close();
      }
    })();
  }

  return { server, run, stop };
}

if (import.meta.main) {
  try {
    const config = parseRelayConfig(Bun.env);
    process.umask(0o077);
    const relay = startRelay(config);
    console.log(config.origin
      ? `Milo production relay listening on port ${config.port}`
      : `Milo local relay listening on http://${config.hostname}:${config.port}`);
    let shutdownTask: Promise<void> | undefined;
    function shutdown() {
      return shutdownTask ??= (async () => {
        const deadline = setTimeout(() => process.exit(1), 10_000);
        try {
          await relay.stop();
        } catch {
          console.error("Could not close relay state cleanly.");
          process.exitCode = 1;
        } finally {
          clearTimeout(deadline);
          process.off("SIGTERM", shutdown);
          process.off("SIGINT", shutdown);
        }
      })();
    }
    process.once("SIGTERM", shutdown);
    process.once("SIGINT", shutdown);
    void relay.run.catch(() => {
      console.error("Relay stopped because provider initialization or polling failed.");
      process.exitCode = 1;
      return shutdown();
    });
  } catch (error) {
    console.error(error instanceof Error ? error.message : "Could not start the relay.");
    process.exitCode = 1;
  }
}
