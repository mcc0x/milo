# Milo local pairing relay

This prototype runs alongside the local Milo dashboard on the same machine as the browser. It listens only on `127.0.0.1:6120`. Do not deploy it, expose it through a tunnel or reverse proxy, or point it at shared production provider apps. No provider calls are made with an unconfigured environment. Milo’s existing manual Telegram and Discord setup still works without this process.

## Run locally

Use a current Bun release. From the repository root:

```sh
cd hosted
bun run server.ts
```

`curl --fail http://127.0.0.1:6120/healthz` checks the listener, not provider readiness. For provider configuration, create an owner-readable file in `hosted/` using `(umask 077; touch .env)`, then `chmod 600 .env`. Bun reads `.env` in this directory. This file and `.env.*` are ignored by Git; never commit, paste into logs, or put the server credentials in the dashboard. Restart after changing configuration. Leave entries for unused providers unset.

### Telegram: verified private chat only

Set `MILO_TELEGRAM_LONG_POLL=1` and `MILO_TELEGRAM_BOT_TOKEN` to the token of a dedicated test bot from [@BotFather](https://t.me/BotFather). Polling is disabled if `MILO_TELEGRAM_LONG_POLL` is unset or `0`, even with a token present. The relay checks `getWebhookInfo` and `getMe` before starting. It refuses to poll a bot with a webhook and stops if Telegram later reports a conflict, including another poller. It never deletes or changes webhook settings. Use a dedicated bot with no webhook or other poller; opt-in polling consumes its Telegram updates.

In Milo, start Telegram pairing, open the official `t.me` link, press **Start** in the private chat, and then press **Confirm pairing** on the bot’s message. The sender and chat must match on both steps. Only that DM can receive relayed messages; groups, channels, and client-supplied destinations are not supported. Return to Milo and save the proposed destination. Turning forwarding on remains a separate action.

Send `/stop` or `/delete` to the bot from that DM to revoke all its relay capabilities and remove associated pending confirmations. Removing the binding from a Milo installation revokes only that installation’s capability. A new claim does not invalidate an existing binding. Revocation does not delete messages already delivered to Telegram or cancel a send already in flight.

Register your own accessible privacy policy link in [@BotFather](https://t.me/BotFather) before operating a bot for anyone else. Review the [Telegram Bot Developer Terms](https://telegram.org/tos/bot-developers#4-privacy), the [standard bot privacy policy](https://telegram.org/privacy-tpa), and the [Bot API](https://core.telegram.org/bots/api). Do not rely on the standard policy unless it actually describes your deployment. BotFather’s group privacy setting is not a privacy policy link.

### Discord: incoming channel webhook only

In the [Discord Developer Portal](https://discord.com/developers/applications), create a dedicated application. Under **OAuth2**, register `http://127.0.0.1:6120/discord/callback` as a redirect URI. If using `localhost` instead, register `http://localhost:6120/discord/callback` and use that exact URL in the environment. Set the following server-side values together:

- `MILO_DISCORD_CLIENT_ID`: the OAuth2 client ID.
- `MILO_DISCORD_CLIENT_SECRET`: the OAuth2 client secret (not a bot token).
- `MILO_DISCORD_REDIRECT_URI`: the exact registered loopback callback above.

Milo’s single-use local launcher opens Discord’s consent dialog for `webhook.incoming` only. Choose an authorized server and channel in the dialog. Complete one pairing at a time in the same browser; the callback checks single-use OAuth state and a short-lived, HttpOnly browser cookie. The relay exchanges the code on the server with PKCE and gives Milo only the selected channel’s webhook credential. It never installs a bot, reads messages, or keeps OAuth access or refresh tokens. Review the destination and save it in Milo, then enable forwarding separately.

To revoke a claimed webhook, remove it from the channel’s **Edit Channel → Integrations → Webhooks** settings; clearing the local credential alone does not delete the Discord webhook. Read [Discord OAuth2 for webhooks](https://docs.discord.com/developers/topics/oauth2#webhooks), the [Developer Policy](https://support-dev.discord.com/hc/en-us/articles/8563934450327-Discord-Developer-Policy), and the [Developer Terms](https://support-dev.discord.com/hc/en-us/articles/8562894815383-Discord-Developer-Terms-of-Service). Add your public Privacy Policy and Terms of Service URLs under the application’s **General Information** before making it available to users.

## Data lifetime and request limits

- Keep pairing URLs private; they are single-use. A pairing expires after ten minutes and its result can be claimed only once. A confirmed but unclaimed Telegram pairing creates no relay capability.
- Confirmed Telegram bindings survive relay restarts. The relay stores only capability digests and verified DM IDs in a private, bot-specific SQLite file under `~/.local/state/milo/relay`. No bot tokens, message bodies, or raw capabilities are stored there. Forget a destination to revoke one binding, or send `/stop` or `/delete` to the bot to revoke the whole DM. Pending pairs are memory-only; restart their setup after a relay restart.
- The relay attempts to delete an expired, unclaimed Discord webhook. If it is stopped or Discord rejects deletion, remove orphaned webhooks manually in the channel settings. A claimed webhook stays with the device and is not affected by relay restart. Providers and recipients may retain previously sent messages.
- The single local listener allows at most 128 in-progress pairs and 128 active Telegram capabilities. It rate-limits starts (12/minute), claims (240/minute), and sends (one per second per DM across its capabilities, plus a shared cap). A throttled API call gets `429` with `Retry-After`.
- This is a trusted-machine prototype. The loopback JSON API rejects browser `Origin` headers and requires Milo’s same-origin local proxy for dashboard calls. It does not defend against hostile local processes. Don’t share machine or browser access with untrusted users while it is running.

## Test without provider access

```sh
cd hosted
bun test service.test.ts
```

The tests use local fetch stand-ins only; no credentials, accounts, or provider messages are needed.
