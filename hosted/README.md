# Milo pairing relay

Only **Connect with Milo** needs the relay. The settings dashboard remains local. Discord uses the relay for initial OAuth; subsequent alerts go directly from the device to the authorized webhook. Telegram alerts pass through the relay to the confirmed private DM. Manual bot and webhook setup do not need the relay.

## Production operator checklist

- Production accepts the canonical origin `https://connect.usemilo.net`. Register `https://connect.usemilo.net/discord/callback` on the production Discord application, enable `webhook.incoming`, and publish the privacy and terms URLs. Use a different bot and application for development. Do not forward production tokens to a PR environment.
- Build `hosted/Dockerfile` with the repository root as build context. Mount a dedicated writable volume at `/data`. Configure one replica with no sleeping or deployment overlap. Never run two long pollers for the same Telegram bot. Use `/readyz` as the deployment gate, not a successful TCP connection.
- The production image sets `MILO_RELAY_ORIGIN=https://connect.usemilo.net` and `MILO_RELAY_DATA_DIR=/data`. Set `RAILWAY_RUN_UID=0` for the volume bootstrap; it must drop privileges before the HTTP service starts. Railway supplies `PORT`. Set at least one provider: `MILO_TELEGRAM_BOT_TOKEN` enables production polling; `MILO_DISCORD_CLIENT_ID` **and** `MILO_DISCORD_CLIENT_SECRET` enable Discord with its canonical callback. Keep these variables on `connect`, not `site`. Do not copy `.env`, databases, or credentials into images or commit them to IaC.
- Before making the feature available, verify certificate issuance, authorized Discord channel selection, Telegram confirmation and delivery, individual revocation, a restart with the volume retained, and healthy delivery by the other provider. Use private test destinations with non-sensitive content. Never paste pairing links, callback URLs, request bodies, cookies or bearer headers into logs, issues or screenshots.
- Restoring an old database can restore revoked bindings. If the current volume is lost or compromised, start with an **empty** volume and have Telegram users pair again; rotate compromised provider secrets separately. Rolling the server binary back does not revoke credentials. Stop the relay rather than falling back to a version that does not enforce the same pairing and revocation rules.
- After such a loss, each Milo installation still holds a Telegram connection that the new volume never issued. The relay answers its sends and its revocation with `503`, and Milo never drops a connection it could not revoke: it will not replace or forget that one, nor remove its own integrations. Tell those users to run `milo alerts forget telegram --local-only` and then connect again. The command exists only on the command line. It asks no relay, turns Telegram forwarding off before it removes the connection from that machine, and leaves Discord as it is. It revokes nothing and says so; where the old ledger may still exist, the user sends `/stop` to the bot, which revokes every connection to that DM.

## Run locally

Use Bun 1.4.5 or later. Start the local relay as below. Launch the development Milo dashboard with `MILO_RELAY_ORIGIN=http://127.0.0.1:6120 milo web`; a normally launched client uses the official HTTPS endpoint.

```sh
cd hosted
bun run server.ts
```

`curl --fail http://127.0.0.1:6120/healthz` checks the listener, not provider readiness. For provider configuration, create an owner-readable file in `hosted/` using `(umask 077; touch .env)`, then `chmod 600 .env`. Bun reads `.env` in this directory. This file and `.env.*` are ignored by Git; never commit, paste into logs, or put the server credentials in the dashboard. Restart after changing configuration. Leave entries for unused providers unset.

### Telegram: verified private chat only

Set `MILO_TELEGRAM_LONG_POLL=1` and `MILO_TELEGRAM_BOT_TOKEN` to the token of a dedicated test bot from [@BotFather](https://t.me/BotFather). Polling is disabled if `MILO_TELEGRAM_LONG_POLL` is unset or `0`, even with a token present. The relay checks `getWebhookInfo` and `getMe` before starting. It refuses to poll a bot with a webhook and stops if Telegram later reports a conflict, including another poller. It never deletes or changes webhook settings. Use a dedicated bot with no webhook or other poller; opt-in polling consumes its Telegram updates.

In Milo, start Telegram pairing, open the official `t.me` link, press **Start** in the private chat, and then press **Confirm pairing** on the bot’s message. The sender and chat must match on both steps. Only that DM can receive relayed messages; groups, channels, and client-supplied destinations are not supported. Return to Milo and save the proposed destination. Turning forwarding on remains a separate action.

Send `/stop` or `/delete` to the bot from that DM to revoke all its relay capabilities and remove associated pending confirmations. Removing the binding from a Milo installation revokes only that installation’s capability. A new claim does not invalidate an existing binding. Revocation does not delete messages already delivered to Telegram or cancel a send already in flight.

`milo alerts forget telegram`, replacing the connection and removing Milo's integrations all stop, with the connection kept, when the relay cannot confirm its revocation. `milo alerts forget telegram --local-only` is the way out when the relay that issued it is gone for good; see the operator checklist for what it does and does not do.

Register your own accessible privacy policy link in [@BotFather](https://t.me/BotFather) before operating a bot for anyone else. Review the [Telegram Bot Developer Terms](https://telegram.org/tos/bot-developers#4-privacy), the [standard bot privacy policy](https://telegram.org/privacy-tpa), and the [Bot API](https://core.telegram.org/bots/api). Do not rely on the standard policy unless it actually describes your deployment. BotFather’s group privacy setting is not a privacy policy link.

### Discord: incoming channel webhook only

In the [Discord Developer Portal](https://discord.com/developers/applications), create a dedicated application. Under **OAuth2**, register `http://127.0.0.1:6120/discord/callback` as a redirect URI. If using `localhost` instead, register `http://localhost:6120/discord/callback` and use that exact URL in the environment. Set the following server-side values together:

- `MILO_DISCORD_CLIENT_ID`: the OAuth2 client ID.
- `MILO_DISCORD_CLIENT_SECRET`: the OAuth2 client secret (not a bot token).
- `MILO_DISCORD_REDIRECT_URI`: the exact registered callback for this environment.

Milo opens Discord’s consent dialog for `webhook.incoming` only. Choose an authorized server and channel. Complete one pairing at a time in the same browser; the callback checks single-use OAuth state and a short-lived, HttpOnly browser cookie. The relay exchanges the code on the server with PKCE and gives Milo only the selected channel’s webhook credential. It never installs a bot, reads channel history, or keeps OAuth access or refresh tokens. Complete the confirmation in the initiating dashboard before saving, then enable forwarding separately.

To revoke a claimed webhook, remove it from the channel’s **Edit Channel → Integrations → Webhooks** settings; clearing the local credential alone does not delete the Discord webhook. Read [Discord OAuth2 for webhooks](https://docs.discord.com/developers/topics/oauth2#webhooks), the [Developer Policy](https://support-dev.discord.com/hc/en-us/articles/8563934450327-Discord-Developer-Policy), and the [Developer Terms](https://support-dev.discord.com/hc/en-us/articles/8562894815383-Discord-Developer-Terms-of-Service). Add your public Privacy Policy and Terms of Service URLs under the application’s **General Information** before making it available to users.

## Data lifetime and request limits

- Keep pairing URLs and finish codes private. An unfinished pairing expires after ten minutes. Production uses an eight-character code delivered to the consenting Discord browser or verified Telegram DM. Five incorrect codes invalidate the pairing. Checking a connection only previews its destination; the device obtains a credential when you choose Save. A lost response may be retried within the pairing lifetime.
- Confirmed Telegram bindings survive restarts. The relay stores capability digests and verified DM IDs in a private, bot-specific SQLite file on the volume (locally under `~/.local/state/milo/relay`). No bot tokens, message bodies, or raw capabilities are stored there. Forget a destination to revoke one binding, or send `/stop` or `/delete` to revoke the DM. Pending pairs are memory-only.
- The relay attempts to delete an expired, unclaimed Discord webhook. If it is stopped or Discord rejects deletion, remove orphaned webhooks manually in the channel settings. A claimed webhook stays with the device and is not affected by relay restart. Providers and recipients may retain previously sent messages.
- Pairing and delivery are bounded and rate-limited. A throttled request receives `429` with `Retry-After` and the same number of seconds as `retry_after` in its JSON body, which is where Milo reads it. Treat ingress, credential storage, the TLS provider and relay host as trusted infrastructure. Per-client throttling does not replace upstream DDoS protection. Restrict operator access to logs and volumes; do not enable request body or authorization-header capture.
- The public listener expects [Railway's TLS ingress and `X-Real-IP` header](https://docs.railway.com/networking/public-networking/specs-and-limits). Do not add a raw TCP proxy or untrusted private-network peers: the application cannot authenticate a proxy by checking for an HTTP header.
- The JSON API is called by the Milo backend, not directly from a browser origin. These checks do not protect against other processes with access to your local session, or compromised bot credentials or devices.

## Test without provider access

```sh
cd hosted
bun test
```

The tests use local fetch stand-ins only; no credentials, accounts, or provider messages are needed.

`milo-alert-texts.json` lists the Telegram messages Milo words. These tests put each one to the production grammar, and `cargo test alerts` in the repository root checks that the list is what Milo sends. Change a wording in Milo, the list and, if it must, the grammar together.
