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

**Agent patch review** is separate from usage detection: a harness that can hand over a recorded patch can use `milo review PATCH`. This shows that patch rather than treating every change in the worktree as the agent’s. See the [Milo agent skill](../skills/milo/SKILL.md).

## Agent tasks

Starting an agent is separate from reading its usage: neither needs the other. `milo task new`, or **Milo: New Agent Task…** in the Rex command palette, starts one of the agents below in a Git worktree, with the agent's own full interactive screen. The screen goes to a background Rex session of its own or, with `--here`, to the terminal you ran the command in, which needs no Rex. Milo starts only the agent you name, only if its program is installed, and passes your prompt as written. It installs no agent, signs in to none, and never reads what an agent saved to sign in. See [starting an agent task](setup.md#start-an-agent-task). On `main`; not in v0.1.7.

`milo task agents` prints this table with what is installed on your computer, and `milo doctor` names the agents whose programs are on the search path it was run with. The table is the catalog in `src/task/harness.rs`; a test keeps the two the same.

| `--agent` | Agent | Started as | Before it can reply | `milo usage` reads | Checked |
| --- | --- | --- | --- | --- | --- |
| `claude` | Claude Code | `claude -- PROMPT` | a Claude sign-in or an Anthropic API key | saved limit windows and today's transcript tokens | 2.1.311: prompt, reply and a typed follow-up |
| `codex` | Codex | `codex -- PROMPT` | a ChatGPT sign-in or an OpenAI API key | limit windows recorded in recent sessions | 0.171.0: prompt, reply and a typed follow-up |
| `omp` | oh-my-pi | `omp launch -- PROMPT` | a provider signed in to inside oh-my-pi | the Codex and Claude limit reports it saved, under those tools | 19.1.1: prompt, reply and a typed follow-up |
| `opencode` | OpenCode | `opencode --prompt=PROMPT` | a model chosen inside OpenCode | today's tokens, steps and estimated cost | 1.19.6 and 2.4.1: prompt, reply and a typed follow-up |
| `pi` | Pi | `pi -- PROMPT` | a model provider set up inside Pi | Nothing | 1.6.1: prompt, reply and a typed follow-up, from a stand-in model |
| `amp` | Amp | `amp`; no first prompt | an Amp account | Nothing | 0.0.1795608053: opened, as far as its own sign-in or setup |
| `copilot` | GitHub Copilot CLI | `copilot --interactive=PROMPT` | a GitHub account with Copilot, or a model provider set up for it | Nothing | 1.0.115: prompt, reply and a typed follow-up, from a stand-in model |
| `cursor` | Cursor CLI | `cursor-agent agent -- PROMPT` | a Cursor account | Nothing | 2026.11.22: opened, as far as its own sign-in or setup |
| `antigravity` | Antigravity CLI | `agy --prompt-interactive=PROMPT` | a Google account | Nothing | 1.5.17: opened, as far as its own sign-in or setup |
| `kiro` | Kiro CLI | `kiro-cli chat -- PROMPT` | a Kiro sign-in | Nothing | 3.6.0: opened, as far as its own sign-in or setup |
| `cline` | Cline CLI | `cline -i -- PROMPT` | a provider chosen in Cline's setup | Nothing | 3.0.75: prompt, reply and a typed follow-up, from a stand-in model |
| `droid` | Factory Droid | `droid -- PROMPT` | a Factory account | Nothing | 1.20.0: opened, as far as its own sign-in or setup |
| `goose` | Goose | `goose run --interactive --text=PROMPT` | a provider set up with goose configure | Nothing | 1.61.0: prompt, reply and a typed follow-up, from a stand-in model |
| `qwen` | Qwen Code | `qwen --prompt-interactive=PROMPT` | a provider connected inside Qwen Code | Nothing | 0.28.4: prompt, reply and a typed follow-up, from a stand-in model |

How to read it:

- **Started as** is the vendor's documented way to open an interactive session with a first message, with the program's own name. The program is not always named like the word `--agent` takes: Cursor's is `cursor-agent`, Antigravity's `agy` and Kiro's `kiro-cli`. Amp documents no argument for a first message, so it opens idle and a `--prompt` for it is refused.
- **Before it can reply** is the agent's own requirement, in short. Milo supplies none of it. An agent that lacks it opens on its own sign-in or setup screen in the session, where you answer it.
- **`milo usage` reads** says what the usage pane shows of what that agent saved. *Nothing* means Milo has no reader for it, not that the agent used nothing: its tokens, cost and quota are unknown to Milo, in the pane and in the menu bar. The GitHub Copilot [online account](#online-provider-accounts) below is a different thing from the `copilot` program: it asks GitHub with a token you give Milo, and never uses the program's sign-in.
- **Checked** names the release and how far it was followed. *Prompt, reply and a typed follow-up* were seen in a session Milo started, with the agent's own provider. *From a stand-in model* is the same with the replies from a stand-in on the same computer, which the agent could be pointed at with no sign-in; it shows that the prompt arrived as written, not what a real model does with it. *Opened* means the release drew its interactive screen for these arguments and stopped at its own sign-in, so no reply was seen. [The evidence notes](evidence/harness-catalog/README.md) have each run, and what was not checked.

**Where it works.** What a vendor publishes and what was checked with Milo are different things, and only the second is a claim about Milo:

- **Checked with Milo: macOS on Apple Silicon only.** Every row of the table was run there. Each run used the vendor's `darwin-arm64` release.
- **Linux: published by every vendor above, started by Milo for none.** Each vendor publishes its program for Linux on x86-64 and ARM64. Milo's own panes are checked on both ([#53](https://github.com/mcc0x/milo/issues/53)), and the launcher's tests run on Linux in CI beside stand-ins for the agents. No agent's Linux release has been started by Milo, and Milo inside Rex on Linux is not verified, so a background session there is unverified too; `--here` needs no Rex.
- **WSL2: the Linux side only.** Milo there is a Linux program. It looks for an agent on the distro's own search path, and an agent installed on the Windows side is not one it starts. Nothing in the table was run under WSL2 ([#54](https://github.com/mcc0x/milo/issues/54)).
- **Windows itself: not at all.** Some of these vendors publish Windows builds. Milo does not build for Windows, so no agent can be started from it, and a vendor's Windows build says nothing about Milo.

With `--host`, the agent and the project are on that Rex host and the agent must be installed there. `--here` and `--host` cannot be combined.

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
| Milo for macOS | Optional menu app for agent quota or local CPU/memory readings, with access to settings. An opt-in **Needs you** section lists the Rex panes whose program waits for you, failed or finished, and jumps to them; it needs no messaging account. Included in the Apple Silicon release; installed separately with `milo menubar install`. | [Menu bar guide](usage.md#macos-menu-bar) and [panes that need you](usage.md#rex-panes-that-need-you) |

## Reader, media and file tools

- **Git** supplies worktree changes, commit history and historical file contents. Outline also browses plain folders. [Pane guide](panes.md)
- **Markdown** renders directly in Reader in the **v0.1.7 release**; no external previewer is required. On **main**, the [Actions menu](panes.md#file-actions) also offers an explicit handoff to a user-installed **tkmp** beside the Rex shell for eligible working Markdown files. That handoff is not in v0.1.7. Milo passes the filename only; see the [wrapper for width-capable builds](panes.md#using-the-full-pane-width-with-tkmp). A built-in fit-to-pane choice is tracked separately in [#94](https://github.com/mcc0x/milo/issues/94) and depends on [tfcace/tkmp#1](https://github.com/tfcace/tkmp/pull/1).
- **Kitty graphics** can display supported images after a positive terminal capability check. **FFmpeg and FFprobe** are optional, user-installed tools for supported MP4s. The [format matrix](panes.md#reading-pictures-video-and-markdown) lists platform fallbacks and decoder requirements.
- **Finder and default Mac apps** can reveal or open eligible working files on a verified local Mac. These actions don’t export a draft or open today’s file in place of a historical revision. [Native file actions](panes.md#opening-a-file-outside-milo-on-a-mac)
- **Formatters** run on request against the draft: rustfmt, gofmt, Ruff, Black, Prettier, Biome, Deno, Taplo, clang-format, shfmt, StyLua, swift-format and Terraform. Milo doesn’t download them or save their output without your explicit save. [File types and formatter selection](panes.md#formatting)
