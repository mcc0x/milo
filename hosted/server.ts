import { HOST, PORT, MAX_BODY, createService } from "./service";
import { openBindings } from "./bindings";

async function main() {
  const env = Bun.env;
  const optIn = env.MILO_TELEGRAM_LONG_POLL;
  const token = env.MILO_TELEGRAM_BOT_TOKEN;
  if (optIn !== undefined && optIn !== "0" && optIn !== "1") {
    console.error("Set MILO_TELEGRAM_LONG_POLL to 0 or 1.");
    return 1;
  }
  if (optIn === "1" && !/^\d+:[A-Za-z0-9_-]+$/.test(token ?? "")) {
    console.error("Set MILO_TELEGRAM_BOT_TOKEN to enable Telegram polling.");
    return 1;
  }
  const clientId = env.MILO_DISCORD_CLIENT_ID;
  const clientSecret = env.MILO_DISCORD_CLIENT_SECRET;
  const redirectUri = env.MILO_DISCORD_REDIRECT_URI;
  if ((clientId || clientSecret || redirectUri) &&
      (!clientId || !/^[1-9][0-9]{0,19}$/.test(clientId) || !clientSecret || !redirectUri)) {
    console.error("Set MILO_DISCORD_CLIENT_ID, MILO_DISCORD_CLIENT_SECRET, and MILO_DISCORD_REDIRECT_URI together.");
    return 1;
  }

  const report = (message: string) => console.error(message);
  process.umask(0o077);
  const service = await createService({
    telegram: optIn === "1" && token ? { token } : undefined,
    bindingStore: optIn === "1" && token ? openBindings(token.split(":", 1)[0]) : undefined,
    discord: clientId && clientSecret && redirectUri ? { clientId, clientSecret, redirectUri } : undefined,
    report,
  });
  const server = Bun.serve({
    hostname: HOST,
    port: PORT,
    maxRequestBodySize: MAX_BODY,
    fetch: service.handle,
    error: () => Response.json({ error: "Local relay error." }, {
      status: 500,
      headers: { "Cache-Control": "no-store", "Referrer-Policy": "no-referrer" },
    }),
  });
  setInterval(service.sweep, 5000).unref();
  console.log(`Milo local relay listening on http://${HOST}:${PORT}`);
  if (optIn === "1") {
    void (async () => {
      for (;;) {
        const delay = await service.pollTelegram();
        if (delay === null) return;
        if (delay) await Bun.sleep(delay);
      }
    })().catch(() => {
      report("Local relay stopped because Telegram polling failed unexpectedly.");
      server.stop(true);
      process.exit(1);
    });
  }
  return 0;
}

try {
  process.exitCode = await main();
} catch {
  console.error("Could not start Milo's local relay. Check the callback URL and that port 6120 is free.");
  process.exitCode = 1;
}
