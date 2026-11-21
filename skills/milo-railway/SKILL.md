---
name: milo-railway
description: Review changes to Milo's Railway-hosted website and its infrastructure as code. Use alongside Railway's upstream use-railway skill.
---

Use project `Milo`, environment `production`. The marketing site (`site`, `usemilo.net`) builds from `/site` on `main`. The `connect` service is a separate pairing relay, not a hosted copy of the local settings dashboard. Never expose the settings dashboard or reuse development bot credentials in production.

Use the official `use-railway` skill for platform operations (`railway skills` installs it). Keep project-specific infrastructure in `.railway/railway.ts`. Do not create a second Railway JSON/TOML configuration. Variables are deliberately unmanaged: never export values when importing config.

From the repository root, run `railway link --project Milo --environment production --service site` if this checkout isn't already linked. Run `cd .railway && bun install --frozen-lockfile && railway config plan --environment production`. Include the intended plan or no-change result in the PR. A PR is not authorization to apply, deploy, or change domains. Confirm the specific plan with the maintainer first.

The production relay and its `telegram-bindings` volume are already provisioned. `connect.usemilo.net` is registered on `connect` and routes to port `8080`; keep that mapping in IaC. Do not register it again or edit DNS for the source branch switch. Keep one production Telegram poller and retain the `/data` mount. Provider credentials stay in unmanaged `connect` variables, with the bot token and Discord client secret sealed; see `hosted/README.md` for runtime requirements.

Production initially deployed from `feat/production-relay` with Wait for CI disabled. The checked-in config targets `main` with Wait for CI enabled. Once both the relay and its IaC are on `main`, plan again from `main` against `Milo` / `production`. The cutover should change only `connect`'s source branch and Wait for CI setting, with no resource additions or removals. Do not proceed if the plan changes anything else, including the site, domain, variables, or volume.

Railway CLI 5.75 can display the move to `main` as `source.branch (… → null)` in the human-readable diff. Before approving the cutover, confirm the desired source explicitly: `railway config plan --environment production --json | jq '.desiredGraph.resources[] | select(.name == "connect") | .source'` should show `"branch": "main"` and `"checkSuites": true`.

After explicit approval, apply the reviewed cutover plan. Verify the intended `main` commit passes CI and reaches `SUCCESS` on `connect`. If switching the source does not deploy that commit, use **Deploy Latest Commit** on `connect` after CI passes. Keep the bootstrap branch until the `main` deployment is verified, then re-plan to confirm no drift. Verify TLS, `/readyz`, pairing, test delivery, and revocation with dedicated test destinations before tagging a client release.

To suspend the relay, stop the deployment and preserve the service and attached volume in IaC. Do not revert the resource declarations and apply: refuse any ordinary rollback plan that deletes the volume. Verify that the site and manual forwarding are unaffected. Monitor readiness after rollout; a successful deployment check is not a continuous monitor.
