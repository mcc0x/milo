# Using the panes

Run the panes in a Unix terminal with 256 colours. Outline and history start in the current directory unless you give them a path. Inside a Git repository they use its root, even when you start in a subfolder. In a plain folder, outline shows just the files and history says it is not a Git project. To outline a directory named like a command, give its path explicitly: `milo ./history`.

If you initialize Git in that folder, open panes pick up the repository and its saved worktree choices without restarting. Switching branches does not erase remembered folder folds or mark the newly visible timeline as newly arrived.

In Follow mode, Outline and History use the local shell's working directory, even with a coding agent in the foreground and no directory report from the prompt. If Rex cannot identify the shell or supply its directory, the panes keep the last project and show `Pinned: no shell cwd`.

## What fits in a pane

| Pane | Size | What you see |
| --- | --- | --- |
| Outline | 7 rows or fewer | A strip of changed files in a Git project, most recently written first, flowed across the width |
| Outline | 52 columns or more | In the tree, the type of change and how many lines each file gained and lost |
| Outline and history | 12 rows or more | A key hint |
| History | 3 rows or fewer | Recent commits without the heading, dots or divider |
| History | 52 columns and 8 rows or more | A row of dots when there is more than one commit |
| History | Narrow | A shorter summary, such as `↑2` for two commits to push or `no tracking` for a branch that does not track a remote branch |
| History | Narrow and focused | The selected commit's subject wraps so you can read it without widening the rail |
| Usage | Narrow | The limiting window takes priority; if its status does not fit, the provider name stays visible |
| Reader | 2 rows | The filename and one line of content; `n` and `N` show the changed line itself |

In narrow History, the wrapped subject uses only the room below a clicked row, so the row stays under the pointer. In a two-row pane, expanding a commit uses the second row for its first file or an empty-commit note. Collapse it to see the subject again.

The outline and history update on their own. macOS uses file-change notifications. Where the pane needs to watch but notifications are missing, including on Linux, milo checks about every two seconds. See [keys and marks](keys.md) for colours and symbols.

In a plain folder, Outline lists the top level and the folders you expand. It shows a partial-listing warning if those folders contain more than 50,000 entries.

If Git holds the index lock for more than a moment, Outline, History and the working-file Reader show `Waiting for Git` and pause automatic updates. They update when the lock clears; Milo never removes it.

Untracked files appear before Milo counts their lines. In a wide outline, `?` means the count is not ready, not zero. The outline counts the files in view in small batches as you scroll; a folder shows `?` while any of its new files are still uncounted.

If filtering or folding hides the selected file, Outline keeps the cursor on its nearest visible folder. Press `r` to refresh counts, including offscreen files.

## Comparing commits

History counts commits to push against the branch's tracked remote branch. `no branch tracked` means there is no tracked branch, not that all commits have been pushed. Milo can still use the remote's default branch to mark commits for comparison. `no remote` means the project has no remote configured.

A shallow clone has only part of the project's history. Milo lists the commits Git has, but it cannot list a commit's changed files if its parent is missing. In that case history shows `Git did not answer for this commit.` instead of showing every file as new.

## Reading files

Open a file from the outline or history with `Enter` or a double-click. Inside Rex it opens in a floating reader. Opening another file from that pane replaces only its previous reader, even after following the shell to another project. Any shell you split beside the reader stays open. Outside Rex the reader takes over the pane; `q` brings you back.
History catches up with repository changes when you return from a file, keeping that file selected if it is still in the timeline.

On iPhone, enable [Rex → Settings → Keyboard → Open Keyboard Manually](https://www.superlogical.com/rex/docs/reference/ios-settings#setting-keyboard-open-keyboard-manually) to browse without raising the keyboard. Tap a folder to expand it, tap a file to select it, and double-tap to open it. This setting affects **all** terminal panes on that iPhone, not just Outline. Tap the keyboard button to type in a shell. If the keyboard reopens when you return to a terminal, turn off **Keep Keyboard Open** in the same settings.

For a quick look at the tree, use **Milo: Peek Outline** in Rex. It reuses one floating outline per tab. Close the peek with `Esc` or `q` rather than removing a permanent pane.

`milo view FILE` reads a working file. `milo view FILE --commit ID` reads the version from a commit. For the latter, start at the repository root and use the file's path as it was in that commit. Milo ignores `.` components and repeated slashes in committed file paths, but rejects `..` components. Use a full or abbreviated hexadecimal commit ID, not a branch name or `HEAD`.

The reader shows the whole file with changed lines marked. It colours Rust, Python, JavaScript, TypeScript (including JSX and TSX), Go, Java, C, C++, C#, PHP, Lua, Swift, SQL, shell, JSON, YAML and TOML by file extension. It colours a file named `Dockerfile`, `Makefile` or `GNUmakefile` in any letter case, and files ending in `.dockerfile` or `.mk`. Working files and files at a commit are recognised the same way. It uses a small scanner, not a full parser. Other languages show as plain text. The working-file reader will not open a file outside the project through a linked folder.

When a removed line and the added line that replaces it look alike, the reader highlights the words that changed. A word is a run of letters, digits and underscores in any script; text written without spaces, such as Chinese or Japanese, is compared one character at a time. Highlighting only adds emphasis: the text shown is the same with or without it. Lines that share too little, lines with many separate changes and lines whose edits span more than about a hundred words keep only their red or green background, as do the remaining lines of an unusually large diff.

The reader also labels hidden differences next to the text: `[tab]` versus `[4 spaces]`, `[LF]` versus `[CRLF]`, or a mark such as `[zero-width space U+200B]`. It highlights the character affected by a changed mark, including a combining accent. Ordinary edits do not label unchanged whitespace or marks. Use the horizontal scroll keys to reach labels on long lines.

## Reading pictures

The working-file reader can show PNG, JPEG, GIF, WebP, HEIC and HEIF, TIFF, BMP, ICO and ICNS, SVG, PSD and the first page of a PDF, where conversion is supported. It fits the picture without changing its proportions. On macOS, `/usr/bin/sips` converts other formats and resizes large PNGs; other systems show only PNGs that do not need conversion. Ordinary text still shows as text if a file has a picture extension. At a commit, the reader shows the stored file as text or a note, not as a picture.

Pictures need a terminal that supports the kitty graphics protocol, such as Rex. Milo asks the terminal whether it can draw pictures and shows a message if it cannot.
