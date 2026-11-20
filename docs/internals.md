# How milo works

Milo is read-only when looking at a project. It keeps the user's Git settings, such as global ignore rules, but overrides settings that could run a program. That includes hooks, content filters, external diff and text converters, file-system monitors and on-demand fetching. It caps Git's time and output. Git reports a change to the commit recorded for a submodule; milo does not inspect uncommitted or untracked files inside it.

The reader can show where a file link points without reading its target. It refuses a path through a folder link that leaves the project and does not read devices or pipes as files. Text from the project cannot send terminal controls.

## Updates and drawing

- One terminal loop handles replies, input, help and checks for changes. It ignores bracketed paste rather than treating pasted text as keys. If opening takes more than a moment, it draws `Reading…`; when it can identify a project, it shows its name too.
- On macOS, file-system notifications wake the Git panes. Outline and History filter writes to files and directories Git has reported as ignored. FSEvents excludes up to eight directories directly; Milo filters the remaining paths before waking a pane. The working reader uses its file's status and watches the wider project rather than reading all of Git status for exclusions. Watched panes check about every 30 seconds as a safety net; without notifications they check about every two seconds.
- A pane with nothing to watch, like setup or a reader at a commit, sleeps until input, a resize or a signal. Panes can also schedule a redraw without a file change. An ignored key does not rebuild the frame; terminal replies, resizes and scheduled checks do.
- After a check, milo waits at least four times as long as that check took, so repeated changes do not keep it busy indefinitely.
- The tracked file list is kept until the index changes, unpushed work is kept until the comparison changes, and commit file lists are cached once opened. Adding an unrelated untracked file does not require recounting edits in tracked files. A working reader reuses the Git status from its current refresh.
- Only changed screen cells are sent to the terminal, in one synchronised update. Milo asks for the background colour and uses it to shade the cursor and added or removed lines.
- Printable bases use Unicode 17.0.0 East Asian Width: wide and fullwidth characters take two cells; ambiguous characters take one. Combining marks stay with their base where applicable. Milo omits presentation modifiers such as emoji variation selectors, so they do not change the width.

The crate depends only on `libc`.

## Source map

The [source map](../CONTRIBUTING.md#source-map) lists every source and test file.
