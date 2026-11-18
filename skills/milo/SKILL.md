---
name: milo
description: Use milo in a user's terminal to show a Git file tree, recent commits, usage, or a file from the working tree or a commit, and to guide Rex setup.
---

# milo

## Check first

- Run `command -v milo` and `milo --version` in the environment where the pane
  will run. A version confirms the binary is available, not that Rex is set up.
- In the intended Rex environment, check the server action list (`rex actions`,
  with no client selected) for `milo_layout` (Milo: Arrange This Tab). Its
  presence confirms the Rex layout actions are loaded. There is no milo setup
  status flag.
- If the user wants to configure milo, open `milo setup` inside Rex. Let the
  user choose the panes and optionally enable **Show Claude Code limits**.
  `s` saves; `a` tries the layout in this tab. To remove the Rex setup, turn off
  all three panes and **Arrange new shell tabs**, then press `s`.

## Commands

| Command | What it does |
| --- | --- |
| `milo outline [PATH]` | Shows a live file tree with changes to commit or push. |
| `milo history [PATH]` | Shows recent commits and the files they touched. |
| `milo usage` | Shows local coding activity and available usage limits. |
| `milo view FILE [--commit ID]` | Opens the read-only reader. |
| `milo setup` | Chooses a Rex layout and optionally enables Claude Code limits. |
| `milo listen [--stop]` | Starts or stops Rex's new-tab listener; normally managed by setup. |
| `milo claude-status` | Status-line helper called by Claude Code, not a pane. |

`PATH` defaults to the current directory. Use `milo outline` to avoid the
first-run setup offer that bare `milo` can show inside Rex. Use `milo --help`
for the CLI guide and `?` for keys and marks in a pane.

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
- Do not run full-screen panes in your own non-interactive shell. They need
  an interactive terminal. If you cannot target the user's terminal, give
  the user the command to run instead.
