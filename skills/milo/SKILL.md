---
name: milo
description: Use milo in a user's terminal to show a Git file tree, recent commits, usage, or a file from the working tree or a commit, and to guide Rex setup.
---

# milo

## Check first

- Run `command -v milo` and `milo --version` in the environment where the pane
  will run. A version confirms the binary is available, not that Rex is set up.
- In this checkout, `milo doctor` explains effective preferences, integration
  health and usage sources without changing anything. For an older installed
  binary, check `milo --help`; `rex actions` (without `-C` or `REX_CLIENT`)
  lists server actions such as `milo_layout`.
- Open `milo setup` for interactive configuration. Trying a layout is not
  permission to save it. Distinguish the global default from the override for
  this Git worktree. `milo_remember_layout` captures an explicitly chosen rail;
  `milo_peek_outline` opens a temporary floating outline instead of docking it.
- Use `milo setup --repair` or `--remove` only when asked to change the
  integration. Do not change Rex's global theme, font or keyboard shortcuts.

## Commands

| Command | What it does |
| --- | --- |
| `milo` | Opens the interactive home menu. |
| `milo setup --skip-onboarding` | Dismisses the first-run guide and exits without a TUI or installation changes. |
| `milo outline [PATH]` | Shows a live file tree with changes to commit or push. |
| `milo history [PATH]` | Shows recent commits and the files they touched. |
| `milo usage` | Shows local usage limits and today's activity for whichever of Codex, Claude Code and OpenCode have saved a figure. |
| `milo view FILE [--commit ID]` | Opens the read-only reader. |
| `milo setup` | Chooses a Rex layout and optionally enables Claude Code limits. |
| `milo web [--port PORT] [--no-open]` | Opens the settings dashboard on loopback. Check `milo web --help` for availability on older installs. |
| `milo listen [--stop]` | Starts or stops layout memory and automatic new-tab arrangement; normally managed by setup. |
| `milo claude-status` | Status-line helper called by Claude Code, not a pane. |

`PATH` defaults to the current directory. Use direct pane commands when launching a pane for the user; they bypass the home menu and onboarding. Use `milo --help` for the CLI guide and `?` for pane keys.

## Open a file for the user

In an interactive terminal, use `milo view "$file"` for the working copy or
`milo view "$file" --commit "$id"` for the file as of a commit. For the second
form, start at the repository root, use the path relative to that root as it
existed at the commit, and pass a full or abbreviated hex Git commit ID.

To open a reader from a non-interactive agent shell, start a terminal tab in
the user's intended Rex session. Pass the file and commit as separate quoted
arguments. For example, with the session and project directory identified:

```sh
rex run -s "$session" -c "$project_dir" -- milo view "$file"
rex run -s "$session" -c "$project_dir" -- milo view "$file" --commit "$id"
```

The user can press `n` or `N` to move between changed blocks and `q` or `Esc`
to close the reader. In Rex, opening a file from the outline or history also
opens a floating reader.

## Do not

- Do not edit the user's Rex or Claude Code configuration directly. Use
  `milo setup` and save only with the user's approval.
- Do not paste the private URL printed by `milo web` into chats, logs or issues:
  it includes the session key. For local use, let Milo open the browser.
- Do not run full-screen panes in your own non-interactive shell. They need
  an interactive terminal. If you cannot target the user's terminal, give
  the user the command to run instead.
