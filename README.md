<p align="center">
  <a href="https://usemilo.net">
    <img src="docs/img/milo-readme-banner.webp" width="960" alt="Milo — Your Git workspace, inside Rex. Browse files, commits and diffs, edit code, track agent usage, save pane layouts, and get alerts in Discord or Telegram.">
  </a>
</p>

<p align="center">
  <a href="https://github.com/mcc0x/milo/releases/latest"><img alt="Latest Milo release" src="https://img.shields.io/github/v/release/mcc0x/milo?label=version&amp;color=4178a6"></a>
  <a href="https://github.com/mcc0x/milo/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/mcc0x/milo?style=flat&amp;label=stars&amp;color=4178a6"></a>
  <a href="Cargo.toml"><img alt="Core CLI: 1 direct dependency" src="https://img.shields.io/badge/core%20CLI-1%20dependency-4178a6"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-4178a6"></a>
</p>

## Install

The v0.1.5 release is available for Apple Silicon Macs running macOS 13 or later. Install it with Homebrew:

```sh
brew install mcc0x/milo/milo
```

Without Homebrew, download and run the installer from the release tag:

```sh
curl -fsSLo milo-install.sh https://github.com/mcc0x/milo/releases/download/v0.1.5/install.sh &&
  sh milo-install.sh
```

The script verifies the release binary against its SHA-256 checksum and installs it to `~/.local/bin` by default. It does not need Rust or Git. See [setup and removal](docs/setup.md) for source installation and uninstall instructions.

Run `milo` inside Rex to choose your panes and save your workspace preferences.

## Workspace settings

Choose a tree or a project sidebar, try it in this tab, then save it globally or just for this Git worktree. Drag a divider in an installed Milo sidebar and Milo remembers the proportions for that worktree. With **Arrange new shell tabs** enabled, the next shell tab restores them; command tabs and existing layouts are left alone. With **Title Rex tabs** enabled, tabs you have not named show their project, worktree and agent.

Prefer a browser? Run `milo web` in your project. It opens the [local settings editor](docs/setup.md#browser-editor) built into the CLI. It shares settings with `milo setup`; no account or separate install is needed.

<p align="center"><img src="docs/img/web-settings.webp" width="760" alt="Milo’s browser editor with Outline, History and Usage in order, a left rail at 20% width, and a two-row Usage pane"></p>

## Outline

<p align="center">
  <img src="docs/img/outline.png" width="570" alt="Project files with colours and line counts for changes to commit or push">
</p>

See changed files in the project tree as you work. Bold colours mark changes to commit; regular colours mark changes to push. Press `Enter` on a file to read it.

## History

<p align="center">
  <img src="docs/img/history.png" width="570" alt="Recent commits, with the newest commit open to show its changed files">
</p>

See recent commits and, when the branch tracks a remote branch, how many still need to be pushed. Press `Enter` on a commit to see the files it touched, then on a file to read that version.

## Usage

<p align="center">
  <img src="docs/img/usage.png" width="860" alt="Codex and Claude Code limits showing percent left, time until reset and an Opus weekly limit">
</p>

See how much of each reported limit you have left. Milo reads local reports saved by oh-my-pi, Codex and Claude Code, and the tokens and estimated cost OpenCode recorded today. It shows the tools that have a figure on this Mac, so there is no empty place for one you do not use, and setup can always show or hide each tool. Where the terminal can show pictures, such as in Rex, the pane can draw the logos and large figures as pictures.

### In the macOS menu bar

Keep an eye on usage without switching to Rex. **Milo** shows the mascot beside your lowest fresh remaining percentage. Click it for reset times, or choose **Browser settings…** to open the browser editor.

<p align="center"><img src="docs/img/menu-bar.webp" width="312" alt="Milo’s macOS menu showing example Codex and Claude Code limits, reset times and a Settings button"></p>

On an Apple Silicon Mac running macOS 13 or later, run `milo menubar install`. The Homebrew and release binaries include the app; it only runs if you install it. See [menu bar setup and removal](docs/usage.md#macos-menu-bar).

## Discord presence

Optionally show **In a Rex session** on your Discord profile, under Milo's name and icon, while a Rex app has a session open. It is off until you turn it on with `milo presence enable`, in Settings or in the browser editor, and each of them says what becomes public first. Nothing about your sessions, projects, files or commands is sent unless you also choose to show the name of the session in front, its Git project's folder name, or both. No bot, token or sign-in is involved, and you can use a Discord application of your own instead of Milo's. See [Discord presence](docs/presence.md) for setup, privacy and limits.

## Reader

<p align="center">
  <img src="docs/img/reader.png" width="675" alt="A TypeScript file with coloured code and green and red lines showing edits">
</p>

Read a file with added and removed lines marked, or open it as it was at a commit. Press `n` or `N` to jump between changed blocks. The reader colours [about thirty languages](docs/panes.md#languages), from Rust and TypeScript to Markdown, HTML and CSS.

Press `e` to edit a working text file where you are reading it. The edit is a draft until you save with `Ctrl-S`, and a save never replaces a file that changed on disk without asking. `Ctrl-F` formats the draft with a [formatter you have installed](docs/panes.md#formatting); `Ctrl-Z` takes it back. See [editing files](docs/panes.md#editing-files).

## Commands

| Command | Opens |
| --- | --- |
| `milo` | Milo Home: Browser settings, Terminal settings, Doctor and Uninstall |
| `milo outline [PATH]` | File tree |
| `milo history [PATH]` | Recent commits |
| `milo usage` | Usage limits |
| `milo view FILE [--commit ID \| --edit]` | File reader, or its editor with `--edit` |
| `milo setup` | Setup screen |
| `milo web [--port PORT] [--no-open]` | [Local browser settings editor](docs/setup.md#browser-editor) |
| `milo web --host HOST` | [Another Rex host’s settings](docs/setup.md#open-another-hosts-settings) through a loopback Rex forward (source builds; not in v0.1.5) |
| `milo settings export GROUPS` / `import FILE` / `undo` | [Copy chosen pane settings](docs/setup.md#copy-settings-to-another-host), preview the import, and take it back (source builds; not in v0.1.5) |
| `milo menubar install` / `uninstall` | Install or remove the optional menu app (menu-enabled builds only) |
| `milo doctor` | Read-only preference and integration report |
| `milo alerts` | [Mirror Rex alerts](docs/setup.md#mirror-rex-alerts) to a Telegram chat or Discord channel; off until you turn it on |
| `milo presence [status]` / `enable` / `details` / `application` / `disable` | Show or change the optional [Discord presence](docs/presence.md) |

`milo PATH` opens Outline at that path. On a fresh install, `milo` and `milo setup` share a short welcome guide. For unattended installs, `milo setup --skip-onboarding` dismisses it and exits without opening a TUI or applying settings. Direct pane commands never start the guide.

## Documentation

[Setup and removal](docs/setup.md) · [Keys and marks](docs/keys.md) · [Usage limits](docs/usage.md) · [Discord presence](docs/presence.md) · [Pane behaviour](docs/panes.md) · [How it works](docs/internals.md)

[Contributing](CONTRIBUTING.md) · [MIT licence](LICENSE)
