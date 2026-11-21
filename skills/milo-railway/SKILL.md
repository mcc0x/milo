---
name: milo-railway
description: Review changes to Milo's Railway-hosted website and its infrastructure as code. Use alongside Railway's upstream use-railway skill.
---

Use project `Milo`, environment `production`. The marketing site (`site`, `usemilo.net`) builds from `/site` on `main`. The `connect` service is a separate pairing relay, not a hosted copy of the local settings dashboard. Never expose the settings dashboard or reuse development bot credentials in production.

Use the official `use-railway` skill for platform operations (`railway skills` installs it). Keep project-specific infrastructure in `.railway/railway.ts`. Do not create a second Railway JSON/TOML configuration. Variables are deliberately unmanaged: never export values when importing config.

From the repository root, run `railway link --project Milo --environment production --service site` if this checkout isn't already linked. Run `cd .railway && bun install --frozen-lockfile && railway config plan`. Include the intended plan or no-change result in the PR. A PR is not authorization to apply, deploy, or change domains. Confirm the specific plan with the maintainer first.

Merge the server/container to `main` **before** applying the reviewed production plan. It should create only `connect` and `telegram-bindings`. Variables are unmanaged; the first deploy without provider credentials should refuse to start. Stage all required variables together (see `hosted/README.md` on the server revision), seal the bot token and Discord client secret so they are not copied into PR or duplicate environments, then deploy once. Use exactly one production Telegram poller and persistent `/data`.

Register `connect.usemilo.net` on `connect` after explicit rollout approval and configure the DNS records Railway provides. The CLI rejects creating this custom domain from source. Immediately after registration, add `domains: ["connect.usemilo.net"]` to the service definition and confirm a no-change plan **before any later apply**. Verify TLS, readiness, pairing, test delivery, and revocation with dedicated test destinations. Do not tag a client release until this gate passes.

To suspend the relay, stop the deployment and preserve the service and attached volume in IaC. Do not revert the resource declarations and apply: refuse any ordinary rollback plan that deletes the volume. Verify that the site and manual forwarding are unaffected. Monitor readiness after rollout; a successful deployment check is not a continuous monitor.
