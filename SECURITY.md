# Reporting a security issue

Please report suspected vulnerabilities privately. This includes the Milo CLI and menu bar app, local settings dashboard, hosted alert relay, and release artifacts. For an ordinary bug that does not put people or their data at risk, use the [bug tracker](https://github.com/mcc0x/milo/issues).

Open the repository’s [Security tab](https://github.com/mcc0x/milo/security). If **Report a vulnerability** is available, use it. Otherwise, [open an issue](https://github.com/mcc0x/milo/issues/new) titled **Request for private security contact** with no technical details or identifying information. Wait for a maintainer to arrange a private channel before sharing the report. Do not open a public proof-of-concept PR.

In private, tell us which version or deployment is affected, what you expected, what happened, and who could be affected. If possible, include a minimal example using your own test data. Never send live credentials, webhook URLs, pairing codes, or real chat contents. If a secret was exposed, revoke it with the provider rather than waiting for a fix.

Maintainers will use the private channel to clarify the report and coordinate a fix and disclosure. Please do not test against other users’ sessions, accounts, or destinations.
