# Account fixtures

Replies in the shape each provider documents, with made-up identifiers and amounts. None was captured from a real account, and no test contacts a provider: the tests serve these files from a loopback port.

| File | Shape taken from |
| --- | --- |
| `deepseek-balance.json` | The example in [Get User Balance](https://api-docs.deepseek.com/api/get-user-balance/) |
| `openrouter-key.json` | The example in [Get current API key](https://openrouter.ai/docs/api/api-reference/api-keys/get-current-key), with the label and identifiers replaced |
| `devin-consumption-daily.json` | The `ConsumptionResponse` schema of [Get Daily Consumption](https://docs.devin.ai/api-reference/v3/consumption/organizations-consumption-daily) |
| `github-ai-credit-usage.json` | The response schema of [Get billing AI credit usage report for a user](https://docs.github.com/en/rest/billing/usage#get-billing-ai-credit-usage-report-for-a-user) |

The Devin and GitHub documents publish a schema without example values, so the `unitType`, `sku` and `model` strings here are placeholders. Milo reads none of them except to check that every item of a GitHub report uses one unit.
