# Milo integrations

Milo works in a supported terminal without Rex or a connected account. Pick the integration for the job: reading local usage, checking an online account, forwarding an alert, or opening a file. These are separate choices.

## Coding agents and usage

`milo usage` reads reports already on disk. It doesn’t launch a harness or sign in to discover one. **Auto** keeps tools without a reading out of the way; **Hide** stops checking that tool. See [usage settings and local sources](usage.md#which-tools-appear).

| Local source | What shows up in Milo |
| --- | --- |
| Codex | Limit windows recorded in recent sessions. |
| Claude Code | Saved limit windows and, when available, today’s transcript token count. An optional Claude Code status line can supply limit reports. |
| oh-my-pi (OMP) | Saved Codex and Claude limit reports contribute to those tools’ readings, not a separate OMP quota. |
| OpenCode | Today’s recorded tokens, steps and estimated cost from its local database. Not a remaining quota. |

**Agent tasks** are separate too. `milo task new`, or **Milo: New Agent Task…** in the Rex command palette, starts Claude Code, Codex, oh-my-pi or OpenCode in a Git worktree and a background Rex session of its own, with the agent's full interactive screen. It starts only the agent you name, only if it is installed, and passes your prompt as written. Online provider accounts are not agents and cannot be started. See [starting an agent task](setup.md#start-an-agent-task). On `main`; not in v0.1.7.

**Agent patch review** is separate from usage detection: a harness that can hand over a recorded patch can use `milo review PATCH`. This shows that patch rather than treating every change in the worktree as the agent’s. See the [Milo agent skill](../skills/milo/SKILL.md).

## Online provider accounts

These are **off by default**. Enable an account and supply its own credential to query the provider. Milo doesn’t reuse a harness login. Balances, spend and credits are not quota percentages.

| Provider | What the API reports |
| --- | --- |
| DeepSeek | Prepaid balance in the account currency. |
| OpenRouter | The key’s remaining spending limit or its spend; account credits require a management key. |
| GitHub Copilot | AI credits consumed this month, not credits remaining; access depends on the plan and token permissions. |
| Devin | Daily ACUs consumed for an eligible Enterprise organization, not an allowance remaining. |

See [account setup](usage.md#accounts-asked-online) and the [provider evidence](usage-sources.md#summary), including access restrictions and schema-only checks. Cursor, Gemini CLI and Amp are not shipped usage adapters.

## Rex, messaging and the Mac menu bar

| Integration | What it adds | Where to start |
| --- | --- | --- |
| Rex | Remembered pane layouts, floating Reader tabs, shell following, agent tasks in background sessions and access to another host’s local settings dashboard. | [Setup and hosts](setup.md) |
| Discord alerts | Forward selected Rex events to a channel. Pair through Milo or use your own webhook. Delivery goes directly from your device to Discord. | [Alert forwarding](setup.md#mirror-rex-alerts) |
| Telegram alerts | Pair with Milo’s bot in a private chat (delivery uses the hosted relay), or configure your own bot and chat without the relay. Can run alongside Discord. | [Alert forwarding](setup.md#mirror-rex-alerts) |
| Discord Rich Presence | Optionally show Rex activity in Discord. Separate from alerts; session and project names are separate opt-ins. | [Presence and privacy](presence.md) |
| Milo for macOS | Optional menu app for agent quota or local CPU/memory readings, with access to settings. Included in the Apple Silicon release; installed separately with `milo menubar install`. | [Menu bar guide](usage.md#macos-menu-bar) |

## Reader, media and file tools

- **Git** supplies worktree changes, commit history and historical file contents. Outline also browses plain folders. [Pane guide](panes.md)
- **Markdown** renders directly in Reader in the **v0.1.7 release**; no external previewer is required. On **main**, the [Actions menu](panes.md#file-actions) also offers an explicit handoff to a user-installed **tkmp** beside the Rex shell for eligible working Markdown files. That handoff is not in v0.1.7. Milo passes the filename only; see the [wrapper for width-capable builds](panes.md#using-the-full-pane-width-with-tkmp). A built-in fit-to-pane choice is tracked separately in [#94](https://github.com/mcc0x/milo/issues/94) and depends on [tfcace/tkmp#1](https://github.com/tfcace/tkmp/pull/1).
- **Kitty graphics** can display supported images after a positive terminal capability check. **FFmpeg and FFprobe** are optional, user-installed tools for supported MP4s. The [format matrix](panes.md#reading-pictures-video-and-markdown) lists platform fallbacks and decoder requirements.
- **Finder and default Mac apps** can reveal or open eligible working files on a verified local Mac. These actions don’t export a draft or open today’s file in place of a historical revision. [Native file actions](panes.md#opening-a-file-outside-milo-on-a-mac)
- **Formatters** run on request against the draft: rustfmt, gofmt, Ruff, Black, Prettier, Biome, Deno, Taplo, clang-format, shfmt, StyLua, swift-format and Terraform. Milo doesn’t download them or save their output without your explicit save. [File types and formatter selection](panes.md#formatting)
