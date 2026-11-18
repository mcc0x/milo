# Keys and marks

Press `?` for help in any pane. `Esc`, `q` or `?` closes the help page; `Ctrl-C` quits milo.

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
| `Space` | Open or close a folder |
| Left or `h` | Close an open folder, or go to the parent |
| Right or `l` | Open a closed folder |
| `{` / `}` | Previous or next visible folder |
| `K` / `J` | Previous or next item in the same folder, skipping nested rows |
| `p` | Go to the parent folder |
| `e` | Open every folder; press again to restore your choices |
| `n` / `N` | Next or previous changed file, including in closed folders |
| `c` | Switch between the whole tree and changed files only |
| `r` | Reload |
| `q` | Quit |

Click a folder to open or close it, click a file to select it, or double-click a file to read it. In the short outline strip there is no cursor: Left / Right or `h` / `l` scroll by one column, and Home / End or `g` / `G` go to either end. The arrow keys, `j` / `k`, the wheel and Page Up / Page Down also scroll the strip one column at a time.

### History

| Key | Action |
| --- | --- |
| `Enter` | Show or hide a commit's files, show the rest of a long file list, or read the selected file |
| Left or `h` | From a file, return to its commit; from an open commit, close its file list |
| `r` | Reload |
| `q` | Quit |

Click a commit to toggle its files, click a file to select it, or double-click to read it as it was at that commit. For a merge, the file list compares the commit with its first parent.

### Reader

| Key | Action |
| --- | --- |
| `b` / `Space` | Scroll a page up or down |
| Left / Right or `h` / `l` | Scroll sideways |
| `n` / `N` | Next or previous block of added or removed lines |
| `q` or `Esc` | Close |

### Usage and setup

In usage, `r` reads the local sources again and `q` quits. In setup, Up / Down or `k` / `j` move between choices, `Enter` or `Space` changes a choice, `s` saves and `a` tries the layout in this Rex tab without saving. `q` or `Esc` leaves setup; if bare `milo` offered setup, it opens the outline instead.

## Colours and marks

In a Git outline, **bold** means changes to commit and regular coloured names mean changes to push. Plain names have no changes marked. When there is nothing to compare with on a remote, history says so.

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

In history, a hollow dot means uncommitted work. A bold dot is a commit to push, a faint dot is already pushed, and a regular dot means there is no remote comparison. A `•` beside a commit means it arrived while the pane was open. Open a commit to see what changed in each file, in the same colours as the outline.

In the reader, added lines have a green background and keep their code colours; removed lines stay in place with a red background. If only part of a line changed, that part is highlighted.
