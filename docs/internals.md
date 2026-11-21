# How milo works

Milo only reads a project, with one exception: the reader writes a file when you save an edit of it. It keeps the user's Git settings, such as global ignore rules, but overrides settings that could run a program. That includes hooks, content filters, external diff and text converters, file-system monitors and on-demand fetching. It caps Git's time and output. Git reports a change to the commit recorded for a submodule; milo does not inspect uncommitted or untracked files inside it.

The reader can show where a file link points without reading its target. It refuses a path through a folder link that leaves the project and does not read devices or pipes as files. Text from the project cannot send terminal controls.

## Writing

An edit is a draft in memory. A save opens the file from the project folder the reader holds, without following a link, checks that it is a regular file, and reads it again. It writes only if the file is the one the edit was read from and still holds the same bytes, or if the user answers the question that says it does not. The write goes into the open file, so its permissions, owner, other names and attributes are not touched, and each line keeps the ending it was read with. See [editing files](panes.md#editing-files).

A formatter is a program of a fixed name, found in the folders of `PATH` that are written in full. It runs only when asked for, in a process group of its own, for at most ten seconds, with the draft on its standard input. It is not given the file to write. See [formatting](panes.md#formatting).

Outside the project, milo keeps two things in its state folder for the editor: the draft of a reader that was ended with unsaved changes, and, in Rex, a mark that a floating reader has unsaved changes so that another pane does not replace it. The mark names the reader's process and is ignored once that process is gone.

## Updates and drawing

- One terminal loop handles replies, input, help and checks for changes. Pasted text is never read as keys: it is skipped, or handed whole to a pane that is taking text. If opening takes more than a moment, it draws `Reading…`; when it can identify a project, it shows its name too. A blocked loading-frame write yields once the pane is ready.
- On macOS, file-system notifications wake the Git panes. Outline and History filter writes to files and directories Git has reported as ignored. FSEvents excludes up to eight directories directly; Milo filters the remaining paths before waking a pane. The working reader uses its file's status and watches the wider project rather than reading all of Git status for exclusions. While the editor is open, the reader pauses Git refreshes until it returns to the diff; saving still opens and checks the file. Watched panes check about every 30 seconds as a safety net; without notifications they check about every two seconds.
- A pane with nothing to watch, like setup or a reader at a commit, sleeps until input, a resize or a signal. Panes can also schedule a redraw without a file change. An ignored key does not rebuild the frame; terminal replies, resizes and scheduled checks do.
- After a check, milo waits at least four times as long as that check took, so repeated changes do not keep it busy indefinitely.
- The outline asks Git on a separate thread, so its keys are answered while Git reads a large project. It goes on showing what Git last said and takes the new answer in whole, never part of it. A change reported while Git was being asked brings another check, a reload with `r` replaces an answer still on its way, and an answer that arrives while Git holds the index is dropped and asked for again. History and the reader still refresh Git in the loop.
- The tracked file list is kept until the index changes, unpushed work is kept until the comparison changes, and commit file lists are cached once opened. Adding an unrelated untracked file does not require recounting edits in tracked files. With the same tracked status and comparison base, an ordinary save recounts only the edited files; changes that could affect rename detection or other files use the full comparison. A working reader reuses the Git status from its current refresh.
- History reads a commit's file list on a thread of its own, one commit at a time. A key waits 40 milliseconds for it, which most commits need less than; after that the pane says it is reading, goes on taking keys, and looks for the list every 10 milliseconds until it arrives, also when the pane follows a shell. A list that arrives late is kept with its commit, found again by its hash, and is never shown under another.
- Usage does not query a tool's SQLite database again while its files are as a successful read left them: the same file, size, times and header for the database and its write-ahead log. That holds only if the files had been left alone for two seconds before the read and did not change during it, and only until a saved record dated ahead of the clock comes due or the local day ends. A failed read, a missing file and `r` always read, and an unchanged database is still read every five minutes. Limits get less latitude than activity: oh-my-pi's database is left unread only without a write-ahead log, because with one a commit can reach readers without its files changing. In write-ahead mode it is read at every look, as before.
- `milo web` prints its link before it reads the settings. If asked to open the browser, it first checks where the Rex apps are connected, then starts the browser while it reads Git, the Rex tab and the settings. The listener holds a browser's first requests until the editor is whole, so no page is served from a half-read state.
- In the editor, undo and redo replace only the rows spanned by the recorded byte range. Syntax state above that range stays cached; a changed row can still change how later rows are coloured.
- Only changed screen cells are sent to the terminal, in one synchronised update. Milo asks for the background colour and uses it to shade the cursor and added or removed lines.
- Printable bases use Unicode 17.0.0 East Asian Width: wide and fullwidth characters take two cells; ambiguous characters take one. Combining marks stay with their base where applicable. Milo omits default-ignorable formatting characters, including emoji variation selectors, but preserves spacing Hangul fillers. Width is measured by scalar, not by full grapheme cluster.

The crate depends only on `libc`.

## Source map

The [source map](../CONTRIBUTING.md#source-map) lists every source and test file.
