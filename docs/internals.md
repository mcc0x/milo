# How milo works

Milo is read-only when looking at a project. It disables Git hooks, repository content filters, external diff tools, file-system monitors and on-demand fetching. It limits how long Git may run and how much output it reads. It does not follow a symbolic link to display its target, open a device or pipe as a normal file, or pass control characters from file names, branch names and commit messages to the terminal.

## Updates and drawing

- One terminal loop handles terminal replies, input, help and when to check. Panes own what they show; Git supplies the repository state. A project still opening after a short wait shows its name and “Reading…”.
- On macOS, file-system notifications wake the Git panes. The outline and history leave out known ignored directories. The working reader uses its file's status and watches the wider project rather than reading all of Git status for exclusions. Watched panes check about every 30 seconds as a safety net; without notifications they check about every two seconds.
- A pane with nothing to watch, like setup or a reader at a commit, sleeps until input, a resize or a signal. Panes can also schedule a redraw without a file change. An ignored key does not rebuild the frame; terminal replies, resizes and scheduled checks do.
- After a check, milo waits at least four times as long as that check took, so repeated changes do not keep it busy indefinitely.
- The tracked file list is kept until the index changes, unpushed work is kept until the comparison changes, and commit file lists are cached once opened.
- Only changed screen cells are sent to the terminal, in one synchronised update. Milo asks for the background colour and uses it to shade the cursor and added or removed lines.

The crate depends only on `libc`. The original outline measurements on an Apple Silicon laptop were about 55 ms to the first frame and 6 MB of memory on a normal project, or 160 ms and 20 MB on a 50,000-file project. Over 90% of startup time was Git. Colouring 2 MB of Rust took about 10 ms. These are historical measurements, not current guarantees.

## Source map

Paths below are relative to [`src/`](../src/). See [contributing](../CONTRIBUTING.md) for build and test instructions.

| File | What it does |
| --- | --- |
| `main.rs` | Command selection |
| `pane.rs` | Shared pane interface and event loop |
| `outline.rs`, `outline/draw.rs`, `outline/input.rs`, `outline/attention.rs` | Outline state, tree and strip drawing, input, and following changes |
| `history.rs` | Commit list and the files each commit touched |
| `reader.rs`, `diff.rs` | Read-only file view and whole-file diffs |
| `syntax.rs`, `syntax/langs.rs` | Code colouring and the rules for each language |
| `usage.rs`, `usage/` | Usage strip and the Claude Code status-line helper |
| `setup.rs`, `setup/install.rs`, `setup/files.rs` | Setup screen and installation or removal |
| `setup/layout.rs`, `setup/rex_files.rs`, `setup/claude.rs` | Layout choices and changes to Rex and Claude Code configuration |
| `setup/actions.lua`, `setup/listener.lua` | Rex action and new-tab templates |
| `setup/settings.rs`, `setup/lock.rs` | Claude Code settings and the Rex listener lock |
| `paths.rs`, `rex.rs` | Milo's settings and state paths, and calls to Rex |
| `help.rs`, `ui.rs` | Help overlay and row builders, and common drawing tools |
| `git.rs`, `git/run.rs`, `git/parse.rs` | Repository state, guarded Git commands and output parsing |
| `tree.rs`, `walk.rs`, `watch.rs` | Folder rows, walking plain folders and file-system notifications |
| `screen.rs`, `keys.rs`, `term.rs`, `width.rs` | Screen cells, input, terminal control and text width |
| `icons.rs`, `read.rs`, `picture.rs` | File icons, file reading and picture conversion |
| `when.rs`, `fixture.rs` | Dates and ages, and throwaway test repositories |
