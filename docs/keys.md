# Keys and marks

Press `?` for help when the pane has room. `Esc`, `q` or `?` closes help; `Ctrl-C` quits milo. Text pasted through a terminal that marks pastes is not read as key presses. While you are [editing a file](#editing-in-the-reader), the keys you type are text: help is `Ctrl-G`, and `Ctrl-C` asks before it gives up unsaved changes.

A help page longer than the pane continues in a second column, and a third, once the pane is 57 columns wide or more, so a full-width pane usually shows it whole. In the shell, `milo COMMAND --help` or `milo --help COMMAND` explains one command in a few lines, and `milo --help` lists every command and key.

In standalone Home, use `↑`/`↓` and `Enter` to choose a pane; the mouse is optional. `q` in a reader opened from Outline or History returns to the list, then `q` returns to Home. A pane started directly returns to the shell instead.

Pasting is distinct from typing only when the terminal supports bracketed paste and forwards its markers. Without it, pasted characters can act as shortcuts. Use the keyboard if your terminal or multiplexer intercepts mouse reports.

If your terminal sends an unfinished reply, or an Alt key produces the same prefix as a reply, Milo drops the ambiguous input rather than typing it. Press `Ctrl-X` to end that sequence. Then carry on typing, or press `Ctrl-C` to quit.

## Moving around

These keys work in the outline tree, history and reader. The reader and help page scroll instead of moving a selection.

| Key | Action |
| --- | --- |
| Up / Down or `k` / `j` | Move one row |
| Page Up / Page Down or `Ctrl-B` / `Ctrl-F` | Move one page |
| `Ctrl-U` / `Ctrl-D` | Move half a page |
| Home / End or `g` / `G` | Go to the first or last row |
| Mouse wheel | Scroll |

### Outline

| Key | Action |
| --- | --- |
| `Enter` | Open or close a folder, or read a file |
| `a` or `Ctrl-A` | Show Actions for the selected path in the tree |
| `Ctrl-T` or `Ctrl-A` in Find | Show Actions for the highlighted result. Typing `a` adds it to the query. |
| `Ctrl-O` / `Ctrl-R` | Open the selected working file in its default Mac app / reveal it in Finder; also works on the highlighted Find result |
| `Space` | Open or close a folder |
| Left or `h` | Close an open folder, or go to the parent |
| Right or `l` | Open a closed folder |
| `{` / `}` | Previous or next visible folder |
| `K` / `J` | Previous or next item in the same folder, skipping nested rows |
| `p` | Go to the parent folder |
| `e` | Open every folder; press again to restore your choices |
| `n` / `N` | Jump to the next or previous changed file, including inside closed folders. If there’s nowhere to jump, the tree says why. |
| `c` | Switch between the whole tree and changed files only |
| `/` | Find files by name or path; arrows choose a result, `Enter` reads it and `Esc` returns to the tree |
| `r` | Reload |
| `q` | Quit |

Click a folder to open or close it, click a file to select it, or double-click a file to read it. In the short Outline strip, double-click a filename to read it; a single click doesn’t scroll the strip. There’s no keyboard cursor: arrows, `h` / `l`, `j` / `k`, the wheel and Page Up / Page Down scroll one column at a time. Home / End or `g` / `G` go to either end.

### History

| Key | Action |
| --- | --- |
| `Enter` | Show or hide a commit's files, show the rest of a long file list, or read the selected file |
| Left or `h` | From a file, return to its commit; from an open commit, close its file list |
| Right or `l` | Open the selected commit's files. In a narrow focused pane, page through a long subject first. |
| `r` | Reload |
| `q` | Quit |

Click a commit to toggle its files, click a file to select it, or double-click to read it as it was at that commit. For a merge, the file list compares the commit with its first parent.

### Reader

| Key | Action |
| --- | --- |
| `b` / `Space` | Scroll a page up or down |
| Left / Right or `h` / `l` | Scroll sideways |
| `m` in a working Markdown file | Switch between rendered text and the source/diff |
| `G` in rendered Markdown | Follow the end as the file changes. Any scroll stops following; `End` jumps once. |
| `Space` in a video | Play or pause (always muted) |
| Left / Right or `h` / `l` in a video | Seek back or forward five seconds |
| `n` / `N` | Next or previous block of added or removed lines |
| `c` | Switch between the full diff and changes with nearby context |
| `Tab`, then `Enter` | Select and expand a folded stretch of unchanged lines; clicking the fold expands it too |
| `e` | Edit the file, or say why it is read-only |
| `a` or `Ctrl-A` | Show Actions for the working file in read mode. When editing, `a` types a letter and `Ctrl-A` moves to the start of the line. |
| `Ctrl-O` / `Ctrl-R` | Open the shown working file in its default Mac app / reveal it in Finder (read mode only) |
| `q` or `Esc` | Close |

The last row shows `e edit` when the file can be edited and the row has room. Help lists the reason when it cannot, and the formatter that would run.

A [review of an agent's patch](panes.md#reviewing-an-agents-edits), opened by `milo review PATCH`, adds `]` and `[` for the next or previous file of the patch and `r` to read the files again. `i` writes a [note on a line or a range](panes.md#notes-on-the-patchs-lines) and `p` lists the notes to copy. It is always read-only.

### Editing in the reader

Press `e` in the reader, or start with `milo view FILE --edit`. The heading says `editing`, then `unsaved` once the draft differs from the file. Nothing is written until you save. See [editing files](panes.md#editing-files) for what can be edited and how a save is kept safe.

| Key | Action |
| --- | --- |
| `Ctrl-S` | Save the draft to the file |
| `Ctrl-Z` / `Ctrl-Y` | Undo or redo. A run of typing is undone a word at a time |
| `Ctrl-F` | Format the draft with an [installed formatter](panes.md#formatting). `Ctrl-Z` takes it back |
| `Esc`, `Ctrl-Q` or `Ctrl-C` | Stop editing. With unsaved changes, asks: `s` save, `d` discard, `Esc` back |
| Arrows | Move the cursor. Up and Down keep their column through shorter lines |
| Home / End or `Ctrl-A` / `Ctrl-E` | Start or end of the line |
| Page Up / Page Down | Move a page |
| `Enter` | New line, indented like the one above |
| `Tab` | Indent by a tab or by the file's own number of spaces |
| Backspace or `Ctrl-H` | Delete the character before the cursor |
| Delete or `Ctrl-D` | Delete the character under the cursor |
| `Ctrl-G` | Help |
| Click | Put the cursor there |
| Mouse wheel | Scroll without moving the cursor |

Pasted text is inserted as one change, with the file's line endings and without control characters. A paste larger than 1 MiB is refused. Every other key types itself, including `?` and `q`.

Once the floating reader in Rex holds a second file, `Ctrl-N` and `Ctrl-P` show the next and previous [tab](panes.md#tabs-in-the-floating-reader) and `Ctrl-W` closes the shown one, while reading and while editing. A click on a tab's name shows it, and a click on its `×` closes it.

Questions take the place of the last row and wait for one of the keys they list. Any other key is ignored. When the pane is two rows tall they take the heading's row instead. A narrow pane drops the words before the keys first: at 28 columns every key is still listed.

| Question | Keys |
| --- | --- |
| `Unsaved edits` | `s` save and stop editing, `d` discard the draft, `Esc` go back to it |
| `Changed on disk` | `c` compare disk and draft, `o` overwrite the confirmed version, `Esc` back without writing |
| `Kept draft found` | `r` put the kept draft in place of the text, `d` delete it, `Esc` decide later |

### Usage and setup

In Usage, `r` checks local data without bypassing the [oh-my-pi read interval](usage.md#where-the-figures-come-from), and `q` quits. In setup, use `1`–`4` or `Tab` for Layout, Appearance, Workflow and Health. Up / Down or `k` / `j` selects a row; `Enter` or `Space` activates it. In Layout, `a` tries the current draft and `u` undoes the trial. `s` asks **where** to save; choose the scope and confirm. `Esc` backs out of a confirmation without saving.

## Colours and marks

Milo uses the palette advertised by the terminal. Set `NO_COLOR=1` to suppress text colours without losing the selection indicator. Standalone panes default to plain file symbols; saved icon choices and `MILO_ICONS=plain`, `nerd` or `off` override the default. Rex keeps its Nerd Font default.

In a Git outline, **bold** means changes to commit and regular coloured names mean changes to push. Plain names have no changes marked.

If color or font weight is hard to distinguish, enable text change marks in setup. For a still outline while tools write files, choose static activity marks. Neither option changes the terminal's appearance for other programs.

| Colour | Change |
| --- | --- |
| Yellow | Edited |
| Green | Added; italic if Git does not track it yet |
| Red, struck through | Removed |
| Cyan, italic | Moved |
| Magenta | Left in conflict by a merge |

| Mark | Meaning |
| --- | --- |
| Pulsing `●` / `○` at the left edge | Being written now |
| `•` at the left edge | Changed since you last looked |
| `docs 3` on a closed folder | Three changed files inside |
| `+7 −5` | Seven lines added and five removed |
| `○5 ↑2` in a narrow outline | Five files to commit and two to push |

When focus is elsewhere, the outline can follow the file being written (for example, `following tides.ts`). It does not move the cursor while you use it or reopen a folder you closed.

Following file activity and following a shell are different choices. File activity moves the outline selection within a project; shell following changes the project shown by an attached pane. Pin a pane's project if you want to keep reading it while the shell moves elsewhere.

In history, a hollow dot means uncommitted work. With a remote branch to compare against, a bold dot marks a commit not on that branch, and a faint dot marks one already on it. A regular dot means there is no comparison. The heading says `no branch tracked` if the current branch does not track a remote branch; that does not mean there are no commits to push. A `•` beside a commit means it arrived while the pane was open. Open a commit to see what changed in each file, in the same colours as the outline.

In the reader, added lines have a green background and keep their code colours; removed lines stay in place with a red background. If only some words of a line changed, those words are highlighted.
