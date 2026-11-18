# strata

strata shows a project's files in a terminal pane, coloured by their git status. Its read-only reader shows one file with code colours and the lines added or removed.

## Build and install

Build with Rust 1.91 or later:

```sh
cargo build --release
mkdir -p "$HOME/.local/bin"
cp target/release/strata "$HOME/.local/bin/"
```

Put that directory on your `PATH`, or copy the binary to a different directory already on it.

## Run

```text
strata [PATH]
strata view FILE
```

`PATH` defaults to the current directory. Inside a git repository, the outline starts at the repository root. Outside one, it shows a plain file tree without git status. `strata view FILE` opens the reader without first opening the outline.

## Colours, weights and marks

| File name colour | Meaning |
| --- | --- |
| Yellow | Edited |
| Green | New (including untracked files) |
| Red | Removed |
| Cyan | Moved |
| Magenta | In conflict |

Bold names are uncommitted; regular-weight coloured names are unpushed. Untracked and moved names are italic; removed names are struck through. Untouched names are faint when there is pending work. Icons have colours of their own for file types, not for git status.

The project heading counts files to commit and files to push (`○` and `↑` when space is short). A closed folder's number counts the pending files within it. In the tree, `●` and `○` alternate beside a recent change; `•` marks the change after it stops pulsing. The outline can follow the latest marked file when focus is elsewhere without reopening folders you closed. If the terminal does not report focus, it waits for two minutes without input before following.

In history, a hollow dot is work to commit. A bold filled dot is a commit to push; a faint one is already pushed. Without a remote to compare with, filled dots use regular weight. In the reader, `+` and `-` mark added and removed lines, and reversed text can pick out the edited part of a line. Line numbers are for the current file, so removed lines have none.

## Outline keys

| Key | Action |
| --- | --- |
| `↑` / `↓` or `k` / `j` | Move the cursor |
| `Page Up` / `Page Down` or `Ctrl-B` / `Ctrl-F` | Move by a page |
| `Ctrl-U` / `Ctrl-D` | Move by half a page |
| `Home` / `End` or `g` / `G` | Go to the first or last row |
| `Enter` | Toggle a folder or open a file |
| `Space` | Toggle a folder |
| `←` or `h` | Close an open folder or go to the parent |
| `→` or `l` | Open a closed folder |
| `{` / `}` | Previous or next visible folder |
| `K` / `J` | Previous or next item in the same folder, skipping nested rows |
| `p` | Go to the parent folder |
| `e` | Open every folder or restore earlier folder choices |
| `n` / `N` | Next or previous pending file, including inside closed folders |
| `c` | Toggle between all files and pending files only |
| `t` | Show or hide history when there is room for it |
| `r` | Reload |
| `?` | Show help |
| `q` | Quit |

Click a folder to toggle it, click a file to select it, or double-click a file to open it. The mouse wheel scrolls the tree. In the strip there is no cursor: use `←` / `→` or `h` / `l` to move one column at a time, and `Home` / `End` or `g` / `G` to go to either end.

## Reader keys

| Key | Action |
| --- | --- |
| `↑` / `↓` or `k` / `j` | Scroll one line |
| `Page Up` / `Page Down`, `b` / `Space` or `Ctrl-B` / `Ctrl-F` | Scroll by a page |
| `Ctrl-U` / `Ctrl-D` | Scroll by half a page |
| `Home` / `End` or `g` / `G` | Go to the top or bottom |
| `←` / `→` or `h` / `l` | Scroll sideways |
| `n` / `N` | Jump to the next or previous block of added or removed lines |
| `?` | Show help |
| `q` or `Esc` | Close the reader |

The mouse wheel scrolls the reader. Use the scroll keys in help and `Esc` to close it. `Ctrl-C` quits strata from either screen, even with help open.

## Pane shapes

- At seven rows or fewer, a git outline shows pending files in a strip ordered by most recent write. Outside git it stays a tree.
- A tree at least 52 columns wide shows what happened to each pending file and how many lines it gained or lost, when counts are available.
- Recent history can sit beside the outline from 90 columns wide and three rows high, or beneath the tree from 20 rows high. A side panel uses some of the pane width, so the tree must still have 52 columns of its own for file details.
- The outline needs at least 12 columns. The reader needs at least 20 columns and three rows.

## Rex

Inside Rex, `Enter` or a double-click on a file opens a floating reader using the `rex` command. Outside Rex the reader replaces the outline in the same pane; closing it returns to the outline.

## Environment

By default, icons use Nerd Font glyphs. `STRATA_ICONS=plain` uses ordinary symbols instead. `STRATA_ICONS=off` (or `none`) hides icons.

## Untrusted repositories

strata uses read-only git commands and disables repository hooks, filters and external diff programs where they could run. It shows the destination of a symbolic link in place of the target file's contents, and drops terminal control characters from displayed text.

## Requirements

Use a Unix terminal and git for repository status. On macOS, file notifications prompt updates as changes happen; on other Unix systems, strata checks periodically. It also checks periodically on macOS in case notifications miss a change.

## Source map

Paths are relative to `src/`.

- `main.rs` — command-line arguments and the terminal event loop.
- `diff.rs` — whole-file diffs and the changed parts of paired lines.
- `fixture.rs` — temporary projects and repositories for tests.
- `git.rs` — repository discovery, state, pending files and history.
- `git/parse.rs` — parsers for git status, file summaries and commit history.
- `git/run.rs` — read-only git commands, settings and resource limits.
- `help.rs` — scrollable help pages.
- `history.rs` — the recent commit panel and its timeline.
- `icons.rs` — file and folder symbols and their colours.
- `keys.rs` — keyboard input, mouse events and terminal replies.
- `outline.rs` — tree, pending files, history and recent marks for the outline.
- `outline/attention.rs` — focus, following work and fading marks.
- `outline/draw.rs` — layout and drawing for the tree, strip and heading.
- `outline/input.rs` — outline keys and mouse actions.
- `read.rs` — bounded file and symbolic-link reads.
- `reader.rs` — the read-only file screen, its diff and its keys.
- `rex.rs` — opening a file in a floating Rex window.
- `screen.rs` — styled terminal cells and incremental drawing.
- `syntax.rs` — finding the source text spans to colour.
- `syntax/langs.rs` — language rules and file extensions.
- `term.rs` — entering, waiting on and restoring the terminal.
- `tree.rs` — tree nodes, folder choices and navigation between rows.
- `ui.rs` — drawing helpers shared by the screens.
- `walk.rs` — listing a tree outside git.
- `watch.rs` — file notification scopes and the macOS watcher.
- `when.rs` — local calendar days and short commit ages.
- `width.rs` — terminal column widths and text offsets.
