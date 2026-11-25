---
name: milo
description: Use milo in a user's terminal to show a Git file tree, recent commits, usage, a file from the working tree or a commit, or only the edits you made yourself, and to guide Rex setup.
---

# milo

## Check first

- Run `command -v milo` and `milo --version` in the environment where the pane
  will run. A version confirms the binary is available, not that Rex is set up.
- Run `milo doctor` to see effective preferences, integration health and
  usage sources without changing anything. `rex actions` (without `-C` or
  `REX_CLIENT`) lists server actions such as `milo_layout`.
- Standalone Home and the rich previews below require v0.1.7 or later. On an
  older install, use a direct pane command and check its `--help`.
- Open `milo setup` for interactive configuration. Trying a layout is not
  permission to save it. Distinguish the global default from the override for
  this Git worktree. `milo_remember_layout` captures an explicitly chosen rail;
  `milo_peek_outline` opens a temporary floating outline instead of docking it.
- With **Title Rex tabs** on, Milo names tabs nobody has named for their Git
  project, worktree and agent. A tab you or the user rename keeps that name and
  Milo stops titling it. Milo never renames a session on its own.
  `milo_name_session` renames the session it is run in after the project of
  the given shell. It creates and switches to nothing. Run it only when the
  user asks for that name.
- `milo task new` adds a Git worktree and starts a coding agent in a new Rex
  session. Run it, or `milo_new_agent_task`, only when the user asks for a
  task to be started, with the agent, branch, base or worktree they named.
  Never choose a different agent when theirs is missing: report the refusal.
  It does not show the new session; leave switching to the user. It removes
  nothing, so say what a failed launch left behind.
- `milo_open_output` (**Milo: Open File from Terminal Output…**) lets the user
  open a project file named in a pane's recent output, at its line. Write
  locations as `path:line` or `path:line:column`, relative to the project, so
  that it can find them. It opens a floating reader and takes focus, so leave
  running it to the user unless they ask.
- Use `milo setup --repair` or `--remove` only when asked to change the
  integration. Do not change Rex's global theme, font or keyboard shortcuts.
- Discord presence is off unless the user turned it on, and it publishes on
  their Discord profile. `milo presence` reports it without changing anything.
  Run `milo presence enable`, `details`, `application` or `disable` only when
  the user asks, and never pass `--yes` for them: it skips the notice of what
  becomes public. Never raise the details to `session`, `project` or `both`
  on your own: they show the session's name or the project's folder name.

## Commands

| Command | What it does |
| --- | --- |
| `milo` | Opens Home. In a standalone terminal, choose Outline, History, Usage or settings without onboarding. |
| `milo setup --skip-onboarding` | Dismisses the first-run guide and exits without a TUI or installation changes. |
| `milo outline [PATH]` | Shows a live file tree with changes to commit or push. |
| `milo history [PATH]` | Shows recent commits and the files they touched. |
| `milo usage` | Shows local usage limits and today's activity for whichever of Codex, Claude Code and OpenCode have saved a figure. |
| `milo view FILE [--commit ID \| --edit \| --line LINE[:COLUMN]]` | Opens the reader. It only reads unless the user presses `e` or you pass `--edit`, which starts editing a working text file. `--line` opens a working file at that line. |
| `milo review PATCH [--from NAME] [--float]` | Shows the edits in `PATCH`, a unified diff you wrote of your own edits, and nothing else as changed. |
| `milo task new --agent AGENT (--branch NAME [--base REF] \| --worktree DIR) [--prompt TEXT]` | Starts `claude`, `codex`, `omp` or `opencode` interactively in a Git worktree and a background Rex session. Prints `worktree DIR` and `session ID`. Pass the prompt as one quoted argument. `--host HOST --project DIR` starts it on another Rex host. |
| `milo setup` | Opens terminal settings. In Rex it can also manage the sidebar layout. |
| `milo web [--port PORT] [--no-open]` | Opens the settings dashboard on loopback. |
| `milo web --host HOST [--project DIR]` | Opens another Rex host's settings dashboard on this computer through a loopback Rex forward, and closes both when done. `HOST` is a label from `rex hosts`. |
| `milo settings export GROUPS` / `import FILE [--apply]` / `undo` | Copies chosen pane settings to another host as a file. `import` only previews until `--apply`. Never carries tokens, destinations, accounts or integrations. |
| `milo presence [status]` | Says whether the optional Discord presence is on and what Discord shows now. |
| `milo listen [--stop]` | Starts or stops layout memory, automatic new-tab arrangement and tab titles; normally managed by setup. |
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

Working images, supported MP4s and Markdown open in the Reader. `m` switches
Markdown between preview and source/diff. Videos start paused; `Space` plays
or pauses without audio, and Left/Right seek five seconds. Decoder and terminal
support vary. Committed versions still show source/diffs or a binary-file
note, never today's working file.

The reader does not change a file unless the user edits and saves it. `e`
starts an edit of a working text file, `Ctrl-S` saves, `Ctrl-F` formats the
draft with a formatter already installed, and `Esc` stops, asking first if
there are unsaved changes. Use `milo view "$file" --edit` only when the user
asked to edit that file. Do not type into the user's editor or answer its
questions for them; to change a file yourself, use your own tools.

## Show the user your own edits

When the user asks to see what you changed ("use Milo and show me the edits"),
hand Milo a patch of the edits you made and nothing else. Milo shows each file
as it is now with only those lines marked, so the user's own uncommitted work
and other agents' edits in the same file read as unchanged. Do this only when
asked.

The patch must come from a record of your own edits, never from the project's
state:

- **A harness that records which lines you wrote.** In OMP, run
  `d = await commit.draft("self")` and write the text of `await d.diff()` to a
  file. Nothing is committed. Files it lists under Review, such as ones a
  shell command wrote, are not in that patch; say so.
- **Any other harness.** Record each edit as you make it, from your first edit
  of the task. Immediately before you edit `FILE`, copy it aside; immediately
  after, append the difference to the patch:

  ```sh
  record=$(mktemp -d)                 # once per task, outside the project
  cp "$file" "$record/before"         # before your edit; skip for a file you create
  # ... make your edit with your own tool ...
  diff -u --label "a/$file" --label "b/$file" "$record/before" "$file" >>"$record/edits.patch"
  # for a file you created:
  diff -u --label /dev/null --label "b/$file" /dev/null "$file" >>"$record/edits.patch"
  ```

  Use the path relative to the repository root in the labels. `diff` exits
  with 1 when it found a difference; that is not a failure.

**Never** build the patch from `git diff`, `git status`, `git stash` or a list
of files you touched, and never rebuild it afterwards from memory. Those hold
everyone's uncommitted work, not yours. If you did not record an edit when you
made it, say that you cannot show only your own edits. You may offer
`milo outline`, named as every uncommitted change in the project, not as
yours.

Then, from the repository root:

```sh
milo review "$record/edits.patch" --from "Claude Code" --float
```

`--float` opens a floating reader over the Rex tab you run in and needs
`REX_BLOCK` in your environment, which Rex sets in every terminal. Rex before
preview.23 also set `REX_SESSION`, and Milo uses it when it is there. It
prints how many files it shows and one `Not shown:` line with the reason for
each file it cannot
show; pass those on to the user as they are. Without Rex, or if `--float`
fails, give the user `milo review PATCH` to run in their terminal. Keep the
patch outside the project and leave it in place while the review is open.

`--from` is a label, not proof. Milo checks that each file still holds the
lines your patch says it left there, not who wrote them. A file whose lines
were changed since shows a reason and no diff. Do not regenerate the patch
from the current files to make it fit.

## Do not

- Do not edit the user's Rex or Claude Code configuration directly. Use
  `milo setup` and save only with the user's approval.
- Do not paste the private URL printed by `milo web` into chats, logs or issues:
  it includes the session key. For local use, let Milo open the browser.
- Do not run full-screen panes in your own non-interactive shell. They need
  an interactive terminal. If you cannot target the user's terminal, give
  the user the command to run instead.
- Do not present `git diff`, `milo outline` or `milo view FILE` as "your
  edits". They show all uncommitted or unpushed work in the project.
