# Working on Milo

Milo is a local-first companion to Rex. Start with the [contributor guide](CONTRIBUTING.md) for the source map, commands and release checks. Use the [Milo skill](skills/milo/SKILL.md) to **operate** Milo for a user; use the [UI review skill](skills/milo-ui-review/SKILL.md) to **change** a visible surface.

## Architecture decisions to preserve

- Keep Git reads in `src/git/` and Rex calls in `src/rex.rs`. Treat the checkout, its Git settings and terminal text as untrusted. Reader edits are an explicit user save through `src/reader/disk.rs`, not a general permission to write into the project.
- The settings UI is a client, not a second settings engine. Keep validation, scope, persistence and integration side effects on the Rust side. Distinguish previews, drafts and immediate actions; never enable a provider just because setup succeeded.
- Keep the browser editor local and usable from the packaged binary without a JavaScript toolchain. The public website is a separate static export; see [site guidance](site/src/AGENTS.md). Never put private settings URLs, live usage, tokens or webhook links in screenshots, logs, tests or issues.
- Check the contract that changed, not just the code that implements it: a Rust fixture for persistence, an isolated terminal for a pane, the packaged editor for a dashboard change, the static preview for a site change. Run the [pre-PR checks](CONTRIBUTING.md#before-you-open-a-pull-request) and use the repository’s [PR template](.github/pull_request_template.md).

## Optional agent tools

No MCP server or agent account is needed to build or test Milo. Use `gh` for GitHub or optionally import the [read-only GitHub PR MCP example](docs/agent-tools/github-pr.mcp.example.json) into a client that supports HTTP MCP and OAuth. The [upstream setup guide](https://github.com/github/github-mcp-server/blob/main/docs/remote-server.md) covers client-specific formats. This example enables PR reads only; it does not deploy, merge, or grant permission to use a live provider. Keep OAuth credentials in your client, not in this repo.
