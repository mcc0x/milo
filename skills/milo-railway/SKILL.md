---
name: milo-railway
description: Review changes to Milo's Railway-hosted website and its infrastructure as code. Use alongside Railway's upstream use-railway skill.
---

Milo's production project is `Milo`, environment `production`, service `site` (`usemilo.net`). The hosted web app builds from `/site` on `main`. The local settings dashboard and the loopback pairing relay are not public Railway services. Do not expose either by changing its bind address.

Use the official `use-railway` skill for platform operations (`railway skills` installs it). Keep project-specific infrastructure in `.railway/railway.ts`. Do not create a second Railway JSON/TOML configuration. Variables are deliberately unmanaged: never export values when importing config.

From the repository root, run `railway link --project Milo --environment production --service site` if this checkout isn't already linked. Run `cd .railway && bun install --frozen-lockfile && railway config plan`. Include the intended plan or no-change result in the PR. A PR is not authorization to apply, deploy, or change domains. Confirm the specific plan with the maintainer first.
