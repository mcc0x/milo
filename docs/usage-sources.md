# Usage sources: what was checked

This is the evidence behind what `milo usage` reads from each coding harness and asks of each provider, and what it leaves unknown. [Usage limits](usage.md) describes the pane itself.

There are two kinds of source. A **tool** is a harness that saves usage on this machine. An **account** is a figure only a provider's service holds. They follow different rules, and neither borrows from the other.

A tool is added only when all of these hold:

- **It is already on disk.** Milo reads what the tool saved. It does not call a provider, run the tool, read its saved sign-in, or open an account page.
- **Its format was checked against a pinned release**, in source and in a database or file that release wrote, and the test fixtures come from that release.
- **Each read is bounded** in time, bytes or both, and never changes the tool's files.
- **Each figure keeps its own meaning.** The table below is the vocabulary. A figure that a source cannot support stays unknown; it is never shown as zero, estimated from another kind, or filled in from a plan's published allowance.
- **A tool that is not in use takes no room.** Whether a tool is in use on this machine is decided from the same bounded reads, apart from whether it has a figure. An adapter says what counts as found, and an empty folder or an empty file never does. [Which tools appear](usage.md#which-tools-appear) has the rule for each tool.

An account is added only when all of these hold:

- **The provider documents the endpoint** and the fields Milo reads, for the kind of key a person can make for themselves. Endpoints found by watching an app, a dashboard's own API, and anything that needs a browser session are out.
- **The user turned it on and named a key for Milo.** Nothing on the machine can say an account is in use without reading someone's secrets, so there is no detection: every account starts hidden, and a hidden account is never asked about. Milo does not reuse a key a harness saved or one in another tool's environment variable.
- **One read-only request, bounded** in time and bytes, to the provider's own host, at an interval the provider's limits allow, with any announced wait honoured.
- **The figure keeps the provider's unit.** A balance, a spend and a count of credits are not quotas. They never become a percentage, never drive the menu bar badge or an alert, and are never added to a harness's figures: the same upstream account can sit behind several harnesses.
- **The answer's shape is checked**, and anything else is unknown. Where the provider publishes only a schema and no test account was available, that is said below.

| Kind | What it is | What it is not |
| --- | --- | --- |
| Tokens | Counts the tool recorded for requests it made | A share of any limit |
| Cost | Money the tool recorded or estimated, in its own currency | A bill, or a price Milo worked out from tokens |
| Quota | A limit, what is left of it and when it resets, as the provider reported it | Anything derived from tokens or cost |
| Scope | Which account, project, session, model and period a figure covers | |
| Age | When the tool observed the figure, not when Milo read the file | |
| Provenance | The file, table and field it came from, and the release that wrote it | |
| Balance | Prepaid money the provider says is on the account | A quota, or a spend |
| Spend | Money the provider says a key or account consumed in a period | A share of a subscription |
| Credits, ACUs | A provider's own unit of consumption, counted for a period | Money, or what is left of an allowance |

## Summary

Ranked by how well the documented source fits one person's usage pane.

| Source | Kind | What can be read | Who can read it | Decision |
| --- | --- | --- | --- | --- |
| OpenCode | Harness, local | Tokens and its own cost estimate for today. Quota: unknown, never saved | Anyone, no credential | **Shipped** (earlier), for the checked releases below |
| DeepSeek | Upstream API, remote | Balance in CNY or USD, granted part, whether it can pay for requests | Any API key | **Shipped**, hidden until turned on |
| OpenRouter | Upstream API, remote | Left of the key's spending limit, or the key's spend; spend today and this month | Any API key. Account credits need a management key | **Shipped**, hidden until turned on |
| GitHub Copilot | Harness, remote | AI credits consumed this month. Allowance left: not in the API | Personal plans, fine-grained token with Plan read. Organization seats: admins only | **Shipped** as consumption, hidden until turned on. Schema only, not verified live |
| Devin | Harness, remote | ACUs consumed in the billing day. Remaining: not reported | Enterprise plans, `ViewOrgConsumption`. Others get 403 | **Shipped** as consumption, hidden until turned on. Schema only, not verified live |
| Gemini CLI | Harness, local | Tokens per message in session logs. Cost, quota: unknown | Anyone, no credential | **Deferred.** Needs the open decisions and a fixture from a real release |
| Cursor | Harness, remote | Team spend and events through the Admin API. One person's plan usage: no documented source | Enterprise team admins, with a key that can also change the team | **Deferred.** No documented personal source |
| Amp | Harness | Nothing current locally; `amp usage` asks the server with the saved key | No documented HTTP endpoint | **Deferred.** Unsupported |

## OpenCode

### Releases checked

| Release | Commit | Artifact run under a throwaway home | Session tables |
| --- | --- | --- | --- |
| [1.2.0](https://github.com/sst/opencode/tree/v1.2.0) (first release with SQLite) | `ffc000de8e446c63d41a2e352d119d9ff43530d0` | npm `opencode-darwin-arm64@1.2.0`, sha256 `502b5e02…63e85e` | `session`, `message` |
| [1.19.6](https://github.com/sst/opencode/tree/v1.19.6) (npm `latest`) | `f4e363c8295fa90a03dddc451882fffe8fbce357` | npm `opencode-darwin-arm64@1.19.6`, sha256 `7abf8124…6b3cd8` | `session`, `message` |
| [2.2.0](https://github.com/sst/opencode/releases/tag/v2.2.0) (first published 2.x) | `1097e2668b1e08ad63bdf0b1c4860b9bc01e420c` | `opencode-darwin-arm64.zip`, sha256 `c71b1cd9…00db3d` | `session_v2`, `session_message` |
| [2.3.2](https://github.com/sst/opencode/releases/tag/v2.3.2) | `c0ad18b402eb2827fc1ee71c171b1177b3fc02d1` | `opencode-darwin-arm64.zip`, sha256 `31ab5a06…fee1a` | `session_v2`, `session_message` |

The full checksums are in the headers of `src/usage/tests/opencode/*.sql`. Those files hold the table definitions copied from databases the four binaries created in an empty directory, and from the one 2.3.2 left after upgrading a 1.19.6 directory.

### What is on disk

- **Path.** `${XDG_DATA_HOME:-~/.local/share}/opencode/opencode.db`, also on macOS. `OPENCODE_DB` names another file, relative to that directory unless absolute. Builds on the `latest`, `beta` and `prod` channels share that file, as do `dev` and `next` in 2.x; a build on any other channel writes `opencode-<channel>.db`. Sources: [`database-path.ts`](https://github.com/sst/opencode/blob/c0ad18b402eb2827fc1ee71c171b1177b3fc02d1/packages/cli/src/database-path.ts), [`global-roots.ts`](https://github.com/sst/opencode/blob/c0ad18b402eb2827fc1ee71c171b1177b3fc02d1/packages/util/src/global-roots.ts), [1.x `database.ts`](https://github.com/sst/opencode/blob/f4e363c8295fa90a03dddc451882fffe8fbce357/packages/core/src/database/database.ts#L43-L55). Both `opencode db path` (1.19.6) and `opencode debug paths` (2.3.2) printed that path.
- **Journal.** WAL mode. On macOS both release lines left `opencode.db-wal` and `opencode.db-shm` in place after exiting, which Milo's read-only reader needs.
- **1.x rows.** One `message` row per message; `time_created` is the message's own creation time and `data` is JSON. An assistant step has `role: "assistant"`, `cost`, and `tokens: { input, output, reasoning, cache: { read, write } }` ([schema](https://github.com/sst/opencode/blob/f4e363c8295fa90a03dddc451882fffe8fbce357/packages/schema/src/v1/session.ts#L453-L485)). The kinds do not overlap: `input` excludes cached tokens and `output` excludes reasoning ([`getUsage`](https://github.com/sst/opencode/blob/f4e363c8295fa90a03dddc451882fffe8fbce357/packages/opencode/src/session/session.ts#L338-L378)).
- **2.x rows.** `session_message` rows with `type = 'assistant'`; `data` has the same `tokens` object and an optional `cost` ([table](https://github.com/sst/opencode/blob/c0ad18b402eb2827fc1ee71c171b1177b3fc02d1/packages/core/src/session/sql.ts), [token kinds](https://github.com/sst/opencode/blob/c0ad18b402eb2827fc1ee71c171b1177b3fc02d1/packages/core/src/session/usage.ts#L11-L19)).
- **Upgrades keep the old tables.** OpenCode 2 copies each 1.x session into the new tables and leaves the `session` and `message` rows where they were ([migration](https://github.com/sst/opencode/blob/c0ad18b402eb2827fc1ee71c171b1177b3fc02d1/packages/core/src/database/v1-migration.bun.ts)). Reading both generations would count every migrated step twice, so Milo reads the 2.x tables whenever `session_v2` exists and the 1.x tables otherwise. Late 1.x releases already create an empty `session_message`; it does not mark a 2.x database.
- **Forks copy history.** A forked session repeats the source's messages under their original times. OpenCode's own statistics skip them with `fork_session_id IS NULL OR message.time_created >= session.time_created` ([`stats.ts`](https://github.com/sst/opencode/blob/c0ad18b402eb2827fc1ee71c171b1177b3fc02d1/packages/core/src/session/stats.ts#L119-L146)). 1.x has no fork column, but a session's own messages are never older than the session, so the time rule alone does the same there. The upgrade to 2.x does not record that a 1.x session was a fork, so Milo applies the time rule to every session 1.x created, in either generation of tables.
- **Cost is an estimate.** It comes from catalogue prices unless the endpoint reported a charge, and is zero when the model has no price ([`calculateCost`](https://github.com/sst/opencode/blob/c0ad18b402eb2827fc1ee71c171b1177b3fc02d1/packages/core/src/session/usage.ts#L21-L36)).
- **Quota is not on disk.** The Go allowance and Console budgets are fetched from OpenCode's servers with the signed-in credential when asked for ([`usage-limits.ts`](https://github.com/sst/opencode/blob/c0ad18b402eb2827fc1ee71c171b1177b3fc02d1/packages/core/src/usage-limits.ts)). Nothing saves them. The same database has `account` and `credential` tables holding tokens; Milo's queries name only the session tables and return sums.

### How it was checked

Each release binary ran with `HOME` and every XDG directory pointed at a throwaway directory, never signed in, with automatic updates and model-list fetches turned off.

- `opencode import` (1.2.0 and 1.19.6) loaded a synthetic session through OpenCode's own schema validation.
- `opencode run` (1.19.6 and 2.3.2) ran one prompt against a throwaway loopback server that returned a fixed reply with 1200 prompt tokens, 200 of them cached, and 34 completion tokens, 4 of them reasoning. Both lines recorded `input 1000, output 30, reasoning 4, cache read 200`, and a cost of $0.00357 from the configured prices.
- `opencode run --fork` on both lines confirmed that copies keep their original times.
- 2.2.0 and 2.3.2 opened a copy of the 1.19.6 directory and migrated it. A database 2.x creates from nothing has no `session` or `message` table; an upgraded one keeps both.
- Milo's totals were then compared with `opencode stats --days 0 --json` from 2.2.0 and 2.3.2 on the migrated, native and forked databases. Steps, every token kind and cost matched. `--days 0` starts at local midnight, as Milo's day does.
- The one difference is a 1.x fork made on the day of the upgrade. After 2.3.2 upgraded a 1.19.6 database holding one, `opencode stats` counted the copied step again and reported 3 steps. Milo reported the 2 that ran.
- 1.19.6 then imported another session into the database 2.3.2 had upgraded. It opened it and wrote only to the old tables, which 2.3.2 no longer reads. Milo reported that database as unknown.

### What Milo reads

Two queries through `/usr/bin/sqlite3`, read-only, sharing a one-second limit and capped output: one lists the columns of the four session tables, the other returns a single row of sums for assistant steps created since local midnight. Message text is parsed inside SQLite and never leaves it. On a 420 MB database with 200,000 messages, 4,300 of them from today, both generations answered in under 0.2 seconds.

A reading is refused, and stays unknown, when:

- a generation of tables is present without every column Milo reads;
- a counted step is not valid JSON, or holds a token count that is not a whole number at or above zero, or a cost that is not a number at or above zero;
- a counted session was created by a major version other than 1 (in 1.x tables) or 1 and 2 (in 2.x tables). Preview builds record a `0.0.0-…` version, so a day with one of their sessions is unknown too;
- an upgraded database has a 1.x message from today in a session with nothing from today in the 2.x tables. That is an upgrade still copying sessions, or a 1.x binary still in use.

The checked releases are the four above. Milo accepts any 1.x or 2.x session whose tables and rows pass these checks; a release between or after them is not separately verified.

### Not covered

- Title, compaction and `generate` requests in OpenCode 2. It adds their usage to the session's running total rather than to a step, so it cannot be dated. `opencode stats` counts the same steps Milo does: it adds compaction usage only from stored events, and the local CLI does not store events ([`bus.ts`](https://github.com/sst/opencode/blob/c0ad18b402eb2827fc1ee71c171b1177b3fc02d1/packages/core/src/bus.ts#L214)). In 1.x a compaction is an ordinary assistant step and is counted.
- Releases before 1.2.0, which kept one JSON file per message.
- Databases of other channels, and a relative `XDG_DATA_HOME`.
- Platforms where SQLite removes the `-wal` and `-shm` files when OpenCode exits. Milo's reader does not create them, so there the figures show only while OpenCode is open. Only macOS was checked.
- A 1.x binary that adds steps, after an upgrade, to a session OpenCode 2 has also used today. Those steps are missed, because the session does have messages from today in the 2.x tables.
- The day of an upgrade. OpenCode 2 turns a 1.x compaction into a summary without usage, and drops some subtask steps, so those leave today's total once the database is migrated. A 1.x session whose only messages from today are ones the upgrade drops keeps the day unknown until midnight.

## Gemini CLI

Checked in source only, at [v0.70.0](https://github.com/google-gemini/gemini-cli/releases/tag/v0.70.0) (`8468962efadfbcdfd988a8fd04af4110a68c886e`, 2026-11-19), with older tags compared for format changes. The CLI was not installed or run.

- **Tokens are saved.** Session logs live under `~/.gemini/tmp/<project>/chats/session-<UTC minute>-<id>.jsonl`, with a subagent's log in a folder named after the main session. `GEMINI_CLI_HOME` moves the parent of `.gemini` ([paths](https://github.com/google-gemini/gemini-cli/blob/8468962efadfbcdfd988a8fd04af4110a68c886e/packages/core/src/config/storage.ts#L195-L234), [writer](https://github.com/google-gemini/gemini-cli/blob/8468962efadfbcdfd988a8fd04af4110a68c886e/packages/core/src/services/chatRecordingService.ts#L794-L840)). A `gemini` message can carry `tokens: { input, output, cached, thoughts, tool, total }`, a `model` and a `timestamp` ([types](https://github.com/google-gemini/gemini-cli/blob/8468962efadfbcdfd988a8fd04af4110a68c886e/packages/core/src/services/chatRecordingTypes.ts#L16-L104), [mapping](https://github.com/google-gemini/gemini-cli/blob/8468962efadfbcdfd988a8fd04af4110a68c886e/packages/core/src/services/chatRecordingService.ts#L1168-L1189)).
- **The kinds overlap.** `input` is the whole prompt including `cached`, and a field the API omitted is written as `0`. Summing them the way Milo does for Claude Code or OpenCode would be wrong.
- **The log is replayed, not counted.** The same message id is appended again when its tokens arrive, and `$set`, `$patch` and `$rewindTo` records change or remove earlier messages ([rewind](https://github.com/google-gemini/gemini-cli/blob/8468962efadfbcdfd988a8fd04af4110a68c886e/packages/core/src/services/chatRecordingService.ts#L1452-L1498)).
- **The format has moved twice in a year** and carries no version: hashed project folders became named ones at v0.29.0, and whole-file `.json` became append-only `.jsonl` at v0.39.0. Old and new files can sit side by side after a migration.
- **Retention is short.** Logs older than 30 days are removed by default, and a new session that cannot be resumed can be deleted on exit ([settings](https://github.com/google-gemini/gemini-cli/blob/8468962efadfbcdfd988a8fd04af4110a68c886e/packages/cli/src/config/settingsSchema.ts#L393-L441)).
- **No cost.** Neither the logs nor the optional telemetry file record a currency amount.
- **No quota.** `/stats` calls `retrieveUserQuota` on the Code Assist service with the signed-in account and keeps the answer in memory ([call](https://github.com/google-gemini/gemini-cli/blob/8468962efadfbcdfd988a8fd04af4110a68c886e/packages/core/src/code_assist/server.ts#L367-L374)). The credentials in `~/.gemini/oauth_creds.json` are out of bounds.
- **Telemetry file.** A user who has set `telemetry.outfile` gets token and AI-credit events appended there ([docs](https://github.com/google-gemini/gemini-cli/blob/8468962efadfbcdfd988a8fd04af4110a68c886e/docs/cli/telemetry.md#L33-L54)). It is opt-in, at a path the user chose, can hold the account's email and prompts, and its `credits_remaining` starts at zero until the server says otherwise, so a zero is not a balance.

**Feasible:** tokens today from retained logs, labelled as locally recorded and partial. **Unknown:** cost, quota, credits.

**Decisions needed before an adapter:**

1. Whether a tokens-only figure with no cost and no quota is worth a row.
2. Whether rewound turns count. The requests were made, but replaying the log as the CLI does drops them.
3. Which total to show, given that `input` already includes `cached`.
4. Whether to read every project's logs or only the current one, and whether to support the pre-v0.39.0 `.json` files.
5. A fixture written by a real release. This review read source only.

## Amp

Amp's CLI is closed source. Checked: the official docs, and the published npm packages unpacked without running them — `@ampcode/cli@0.0.1795176040-g0dd132` (2026-11-20, shasum `e624f3e3c0b090a1ab80388891c3c0e8c1bd9873`) with its `cli-darwin-arm64` binary, and the older JavaScript builds `@sourcegraph/amp@0.0.1768594800-gf294ec` (January 2026), `0.0.1774909414-gfed2d4` (March), `0.0.1778790766-g60d194` (May) and `0.0.1781034998-g7dd2da` (June). Findings from inside the bundles are readings of minified code, not documented behaviour.

- **Threads live on the server.** [Security](https://ampcode.com/security) says the server handles "thread syncing and storage, and usage tracking" and keeps conversations in its own database. The January and March builds also wrote each thread to `~/.local/share/amp/threads/T-<uuid>.json`, where an assistant message could carry `usage: { model, inputTokens, outputTokens, cacheCreationInputTokens, cacheReadInputTokens, timestamp }`. That writer is absent from the May and later builds.
- **No cost on disk.** No currency or credit field was found in what the CLI itself wrote. Third-party tools report credit fields in some old thread files; that is not confirmed from Amp's side.
- **No quota on disk.** [`amp usage`](https://ampcode.com/docs/pricing) asks the server for the balance with the account's API key, which is stored in `~/.local/share/amp/secrets.json`. That file is out of bounds.
- **Streaming JSON is output, not storage.** [`--stream-json`](https://ampcode.com/docs/cli/streaming-json) prints a `usage` object per assistant message, but only to whoever ran the command. Milo does not run Amp.
- **Logs.** `~/.cache/amp/logs/cli.log` is a diagnostic log; no usage record in it was verified.
- **No format promise.** Amp's [docs](https://ampcode.com/docs) say "No backward compatibility, no legacy features", and no schema is published for thread files.

**Feasible:** token counts from thread files that old releases left behind. That is history, not current use, so it would mislead in a pane about today. **Unknown:** cost, credits, quota, and all usage on current releases.

**Decisions needed:** none that Milo can settle. A current adapter needs Amp to save a usage snapshot locally, separate from the credentials file. Anything that reuses the API key is ruled out.

Rechecked for this review: the [pricing page](https://ampcode.com/docs/pricing) (modified 2026-11-17) still names only user settings, workspace settings and `amp usage` as ways to see a balance. Amp is a harness that resells model usage as credits and can also route to a linked ChatGPT or Claude subscription or your own keys, so a figure for Amp would be Amp credits, not the upstream account's. No HTTP endpoint for the balance is documented, and running `amp usage` would have Milo run the tool with the user's saved key. **Unsupported**; nothing was built.

## DeepSeek

An upstream API, not a harness: OpenCode, Claude Code and others can all spend the same DeepSeek balance.

- **Documented.** [`GET https://api.deepseek.com/user/balance`](https://api-docs.deepseek.com/api/get-user-balance/) with `Authorization: Bearer <API key>` returns `is_available` and `balance_infos[]`, each with `currency` (`CNY` or `USD`), `total_balance`, `granted_balance` and `topped_up_balance` as decimal strings.
- **Read.** The first currency's total as the figure, its granted part and a second currency's total as details, and `is_available: false` as "too low for requests". A currency outside the documented two, a negative amount or an empty list is unknown.
- **Not available.** Spend, tokens, a per-period figure, when a grant expires.
- **Credential.** Any API key of the account. DeepSeek has no read-only key, so the key Milo is given can also spend the balance.
- **Limits.** The [rate limit page](https://api-docs.deepseek.com/quick_start/rate_limit) documents concurrency for model requests and none for this endpoint. Milo asks at most every 5 minutes.
- **Verified.** Parsing against the documented example; the request, headers and failures against a loopback server. Not called live: no key was used.

## OpenRouter

An upstream router. Any harness given the same key spends against it, so the figure is the key's, shown under OpenRouter's name only.

- **Documented.** [`GET https://openrouter.ai/api/v1/key`](https://openrouter.ai/docs/api/api-reference/api-keys/get-current-key) with the key itself returns `data.limit`, `limit_remaining`, `limit_reset`, `usage`, `usage_daily`, `usage_weekly` and `usage_monthly`, in US dollars. [`GET /api/v1/credits`](https://openrouter.ai/docs/api/api-reference/credits/get-credits) returns the account's purchased and used credits but requires a management key.
- **Read.** `limit_remaining` when the key has a limit, else `usage`; `usage_daily` and `usage_monthly` as details. The reference does not say where OpenRouter's day and month begin, so Milo repeats them as the provider's "today" and "this month".
- **Not read.** `/credits`: a management key can create and delete keys, which is more than a usage pane should hold. The key's label, creator and workspace identifiers in the reply are not kept. A spending limit is money, so what is left of it is shown in dollars and never as a share.
- **Verified.** Parsing against the documented example with identifiers replaced; requests and failures against a loopback server. Not called live.

## GitHub Copilot

A harness with its own billing unit. Since June 2026 Copilot usage is [measured in GitHub AI credits](https://docs.github.com/en/copilot/concepts/billing/usage-based-billing-for-individuals), with a monthly allowance per plan.

- **Documented.** [`GET /users/{username}/settings/billing/ai_credit/usage`](https://docs.github.com/en/rest/billing/usage#get-billing-ai-credit-usage-report-for-a-user) returns `timePeriod` and `usageItems[]` with `unitType`, `grossQuantity`, `netQuantity` and amounts, for the current month by default. It is [open to fine-grained personal access tokens](https://docs.github.com/en/rest/authentication/endpoints-available-for-fine-grained-personal-access-tokens) with read access to the user's Plan.
- **Read.** The sum of `grossQuantity`, as AI credits consumed in the reported month, valid until that month ends in UTC. Items in more than one `unitType` are not added, and the report is then unknown.
- **Plan restriction.** The page states that user endpoints cover only usage billed to a personal account. A seat billed through an organization returns nothing here, and the organization endpoints need an administrator. An empty report is therefore shown as "no usage reported", never as zero.
- **Not available.** The allowance left and its reset: the report has consumption only, and Milo does not fill in a plan's published allowance.
- **Not used.** The `copilot_internal` endpoints that editors call. They are undocumented and take the editor's own sign-in.
- **Not verified live.** GitHub publishes a schema without example values. The fixture's `unitType`, `sku` and `model` strings are placeholders, and the product filter is not used because its accepted values are not listed. No token was used.

## Devin

A harness. ACUs are its own compute unit, not an upstream model account.

- **Documented.** [`GET https://api.devin.ai/v3/organizations/{org_id}/consumption/daily`](https://docs.devin.ai/api-reference/v3/consumption/organizations-consumption-daily) with `time_after` and `time_before` in Unix seconds returns `total_acus` and `consumption_by_date[]`. The enterprise variant advises bounds aligned to the billing day, which starts at 08:00 UTC, to match the dashboard.
- **Read.** `total_acus` for the current billing day, valid until that day ends.
- **Plan restriction.** The page states that the consumption API is for Enterprise plans and that Teams and self-serve organizations receive `403`, which Milo shows as "access was denied". It needs a service user or [personal access token](https://docs.devin.ai/api-reference/personal-access-tokens) with `ViewOrgConsumption`. A personal access token carries all of its owner's permissions, so a service user limited to that permission is the safer key.
- **Scope.** The whole organization, not one person. The per-user variant needs a user identifier Milo has no documented way to learn from the key's owner alone, so it is not used.
- **Not available.** ACUs left in a plan or cycle. The cycle list is an enterprise-level endpoint.
- **Not verified live.** Schema only; no Enterprise account was available. No rate limit is published beyond the `429` response, so Milo asks at most every 15 minutes, once more when the billing day ends, and honours `Retry-After`.

## Cursor

A harness with its own plans. Checked: the [API overview](https://cursor.com/docs/api), the [Admin API](https://cursor.com/docs/account/teams/admin-api) and the [CLI configuration reference](https://cursor.com/docs/cli/reference/configuration).

- **Documented, for teams.** `POST /teams/spend` returns each member's `spendCents`, `overallSpendCents` and spending limits for the billing cycle; `/teams/daily-usage-data` and `/teams/filtered-usage-events` return request counts and events, aggregated hourly, with polling advised at most once an hour and a default limit of 20 requests a minute.
- **Why it is not built.** The overview lists the Admin and Analytics APIs as available to Enterprise teams. Keys are made by a team administrator with the `admin:*` scope, and the same API removes members and sets spend limits. That is a team-wide, write-capable credential, and what it returns is every member's spend, not the usage left on the plan of the person at the keyboard.
- **No documented personal source.** The user API key the overview describes is for the Cloud Agents API and the SDKs. The CLI configuration reference lists no status-line command that would hand a figure to another program. Third-party guides describe one, and an in-session `/usage` command; neither is in the reference pages checked, and neither is documentation to build on. The dashboard's own requests and the editor's local databases are private and were not inspected.
- **Blocker.** A documented way for one person to read their own plan usage: a personal read-only endpoint, or a status line or file the CLI writes.

## Others considered

- **Anthropic and OpenAI organization usage and cost APIs.** Not rechecked for this review. They report an organization's API spend to its administrators. Claude Code and Codex subscription limits already come from local reports, and an API spend beside them would invite reading one as the other.
- **Windsurf, Kiro, Zed, Cline, Aider and similar.** Not assessed. Each would need the same evidence as a tool or an account above.
