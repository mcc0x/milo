# Contributing to milo

Keep changes small and make milo simpler, faster or more correct.

## Getting set up

You need Rust 1.91 or later, git, and a terminal.

Set `fork_url` to the URL of a fork you can access, then run:

```sh
git clone "$fork_url" milo
cd milo
cargo build --release
./target/release/milo outline    # the outline, in this repository
./target/release/milo history    # recent commits
./target/release/milo usage      # usage at a glance
./target/release/milo view src/main.rs
```

## Before you open a pull request

Run these checks:

```sh
cargo build --release              # no warnings
cargo clippy --all-targets         # no warnings; the lint set is in Cargo.toml
cargo fmt -- --check               # no diff; the settings are in rustfmt.toml
cargo test                         # everything passes
```

After changing `install.sh`, also run `sh tests/install.sh`; `cargo test` does not run it.

Then run the program and look at what you changed. Text checks cannot tell you whether a pane looks right.

When updating the README, guides, agent skill or this file, run `python3 site/bundle-docs.py` and commit the refreshed `site/public/llms-full.txt`. CI checks that the published copy matches these sources.

## Tagging a release

Push the release commit to `main`, then wait for **Apple Silicon release tests** to pass on that exact commit before creating its tag. On an arm64 macOS runner, GitHub Actions runs `sh tests/install.sh` and `cargo test --release --locked`, then drives the browser editor of a release build in Chromium with `tests/web-smoke.cjs`. GitHub requires that check for tag creation and updates, including when a release creates its tag through the web UI. If releasing from another branch, run **Release tests** on that branch from the Actions tab first.

## What a good change looks like

**It solves one thing.** A bug fix does not also rename a module. A refactoring does not also change behaviour.

**It adds no dependency.** milo depends on `libc` and nothing else. If you believe a crate would earn its place, open an issue first and say what it replaces.

**It is tested by what the user sees.** Use throwaway Git repositories (`src/fixture.rs`) and check the frames the program draws where practical. Write tests that would fail if the behaviour broke, not tests that repeat the code. A bug fix comes with the test that would have caught it.

**It keeps untrusted repositories safe.** Keep the user's Git settings, but disable hooks, filters, external diffs, text converters, file-system monitors and on-demand fetching. Do not follow a folder link out of the project to show a file or let names and messages send terminal controls.

**It keeps subprocesses in one place per tool.** Start Git through `src/git/run.rs`, `/usr/bin/sips` through `src/picture.rs`, `/usr/bin/sqlite3` through `src/usage/sqlite.rs`, and Rex through `src/rex.rs`. Use their existing safeguards. Never run a command named by project files or Git settings.

**It stays read-only.** milo never changes a repository. Every git command it runs only reads.

## Style

The code is written to be read. A few habits keep it that way:

- **Comments say what and why, in plain words.** Write for someone who knows Rust but has never seen this file. Do not describe how the code used to be, and do not restate the line below.
- **One word per idea.** *Pending* is anything not yet pushed; *uncommitted* is the part of that not yet committed; *unpushed* is the part committed but still local. `check` takes in changes made outside; `poll` asks git whether anything moved; `load` rebuilds what a screen shows. If you need a new word, define it in a doc comment where the idea is introduced.
- **Small functions with one job.** If a function runs past about forty lines and is not a plain table, it is probably doing two things.
- **Names over positions.** Prefer a small struct or enum to a tuple or a bare `bool` argument when a reader would otherwise have to remember an order or a meaning.
- **No leftovers.** No commented-out code, no `TODO`, no parameter that is always the same value, no helper kept for a caller that is gone.
- **`unsafe` explains itself.** Every block has a `// SAFETY:` comment directly above it giving the actual argument. The lints enforce that the comment exists; reviewers check that it is true.
- **On screen, plain words.** Text the user reads says "changed", "to commit", "to push". Words such as *pending* are for the code.

## Finding your way around

The [source map](#source-map) lists the files under `src/` and `tests/`.

### Adding a pane

Put the pane in a module under `src/` and implement `pane::Pane`: `draw` fills a `Screen`, `help` returns its own `Page::new` guide, and `key` handles pane-specific input. Return `After::Leave` to close, `After::Stay` to draw again, or `After::Ignored` to keep the frame. For outside changes, add `watch_scope` and `check`; for timed redraws, add `next_frame`. A pane with no watch or timer sleeps until input, resize or a signal.

Declare the module in `src/main.rs`. Add the command to `Command`, the argument parser, `dispatch` and `--help`. Use the shared loop; choose whether the pane starts loaded, does a first check or shows `Reading…` on a slow open.

### Changing the Rex layout

The two Lua templates are in `src/setup/`. `layout.rs` adds the choices and
proportions above the actions in `rex.lua`, and the path to that file above
the handler in `listener.lua`. `rex_files.rs` asks Rex to check changed
generated files before writing settings. The listener snapshots existing rails
after Rex attaches, without replacing a baseline already captured from an event.

`setup/profiles.rs` stores rail geometry separately from those integration files.
Persist roles, side, order and proportions, never Rex block IDs. A trial is the
exception: its IDs are transient and are the only blocks its undo may close.
`preferences.rs` resolves pane settings outside the repository, per worktree
over global defaults. `workflow.rs` and `rex_shell.lua` follow only the shell
explicitly supplied by a managed pane. Ordinary manual pane launches stay pinned.

Run `cargo test setup::` for generation, screen and installation tests. The
tests use throwaway settings and a stand-in for Rex. With Lua 5.1 installed,
run `cargo test --locked --bin milo rex_lua_fixtures -- --ignored` for the
action and listener scenarios. Set `LUA51` if the executable is not `lua5.1`.
No running Rex session is needed. To check a generated
file with Rex itself, run `MILO_CHECKING_REX=1 rex config check /path/to/copy.lua`
on a scratch copy. The flag prevents the actions file starting a listener
during the check. Check both files and try the pane choices. Only try a
reload or the actions on a disposable Rex configuration and session, never
as part of the automated tests.

### Adding a language to the reader

Code colouring uses `src/syntax.rs` for scanning, `src/syntax/langs.rs` for language tables, and `src/syntax/delimiters.rs` for strings whose closing marks depend on the opener. Adding a language normally means changing only the tables:

1. Add a `Lang` constant: its line and block comments, its quotes (list a longer opener before any opener it starts with), its keywords and constants.
2. Map its file extensions in `for_file`.
3. Add a test with a few idiomatic lines and the classes you expect.

The scanner is not a parser and is not meant to become one. It knows comments, strings, numbers and reserved words, and guesses types and calls from how a name is written. If a language needs more than a table can say, it is fine for milo to colour it imperfectly.

## Commit messages

One line in the imperative that says what the commit does for someone using or reading milo ("Leave closed folders closed when work lands inside them"), then a short list of the notable changes if there are several. No ticket prefixes are needed.

## Reporting a bug

Say what you ran, what you saw and what you expected. If it depends on a repository's state, the smallest sequence of git commands that reproduces it is worth more than a description. For anything that looks like a security problem (milo running something, or showing something, that it should not), please report it privately to the maintainer rather than in a public issue.

## Licence

By contributing you agree that your work is released under the MIT licence that covers the project (see `LICENSE`).

## Source map

| File | Purpose |
| --- | --- |
| `src/diff.rs` | Parses whole-file diffs and marks what changed within a line. |
| `src/fixture.rs` | Makes throwaway projects and Git repositories for tests. |
| `src/fs_ops.rs` | Opens files without following final links and stages atomic replacements. |
| `src/git.rs` | Finds repositories and keeps their status, file lists, and history. |
| `src/git/parse.rs` | Reads Git status, diff, and log output. |
| `src/git/pending.rs` | Counts lines in pending changes and reuses tracked-file counts across unrelated edits. |
| `src/git/run.rs` | Runs bounded, read-only Git commands with unsafe settings overridden. |
| `src/help.rs` | Draws and scrolls the in-pane help page. |
| `src/history.rs` | Keeps commits, opened file lists, and selection state. |
| `src/history/draw.rs` | Draws history headings, dots, commit rows, and file previews. |
| `src/history/input.rs` | Handles history keys and mouse clicks. |
| `src/history/tests.rs` | Checks history rows, tracking, and opening files at commits. |
| `src/icons.rs` | Chooses file and folder symbols and their colours. |
| `src/json.rs` | Validates JSON and lets callers read fields without copying values. |
| `src/keys.rs` | Decodes keys, mouse events, paste markers, and terminal replies. |
| `src/main.rs` | Parses and dispatches commands and supplies CLI help. |
| `src/outline.rs` | Updates the file tree, pending changes, and recent marks. |
| `src/outline/attention.rs` | Uses focus and recent activity to follow writes and fade marks. |
| `src/outline/draw.rs` | Lays out and draws the full tree and short strip. |
| `src/outline/input.rs` | Handles outline navigation, folds, and file opening. |
| `src/pane.rs` | Defines the pane interface and runs the shared terminal loop. |
| `src/paths.rs` | Finds milo's configuration and state folders. |
| `src/picture.rs` | Loads pictures and converts them to PNG for the terminal. |
| `src/read.rs` | Reads regular files and the names that file links point to. |
| `src/reader.rs` | Keeps reader state and handles navigation and updates. |
| `src/reader/content.rs` | Locates files and loads text, diffs, or pictures. |
| `src/reader/draw.rs` | Draws files, change marks, and reader headings. |
| `src/reader/tests.rs` | Checks reader text, diffs, paths, pictures, and keys. |
| `src/rex.rs` | Calls Rex for setup, floating readers, and the listener. |
| `src/screen.rs` | Stores styled cells and sends changes and pictures to the terminal. |
| `src/setup.rs` | Draws setup choices, the save preview, and the layout sketch. |
| `src/setup/actions.lua` | Defines Rex actions that arrange, add, or close milo panes. |
| `src/setup/claude.rs` | Plans changes to the Claude Code status line and tracks ownership. |
| `src/setup/files.rs` | Checks, replaces, and restores files changed by setup. |
| `src/setup/install.rs` | Coordinates setup saves, Rex reloads, and listener changes. |
| `src/setup/layout.rs` | Reads and writes layout choices and generates Lua files. |
| `src/setup/listener.lua` | Remembers divider changes on managed rails and optionally arranges new shell tabs. |
| `src/setup/listener_test.lua` | Checks the new-tab and new-session handlers with a stand-in for Rex. |
| `src/setup/lock.rs` | Keeps saves and listeners from overlapping and stops the listener. |
| `src/setup/rex_files.rs` | Plans edits to Rex settings and checks the generated layouts. |
| `src/setup/settings.rs` | Reads and edits the top-level Claude Code status-line setting. |
| `src/setup/web.rs` | Exposes drafts, scoped saves, and Rex trials to the browser editor. |
| `src/syntax.rs` | Scans source text to find spans to colour. |
| `src/syntax/delimiters.rs` | Handles string closers that depend on their openers. |
| `src/syntax/langs.rs` | Defines language rules and maps them to file extensions. |
| `src/term.rs` | Sets and restores terminal modes and waits for events. |
| `src/tree.rs` | Builds file and folder rows, folds, and changed-file counts. |
| `src/ui.rs` | Shares headings, status styles, icons, and scroll marks. |
| `src/usage.rs` | Collects the latest limits and today's Claude Code activity. |
| `src/usage/draw.rs` | Lays out usage figures, reset times, and hints. |
| `src/usage/draw/digits.rs` | Draws large figures from text blocks when pictures are unavailable. |
| `src/usage/draw/figure.rs` | Builds coloured PNG figures from the stored masks. |
| `src/usage/files.rs` | Scans session files and bounds how much a refresh reads. |
| `src/usage/marks/Inter-OFL.txt` | Records the licence for the font used in usage figures. |
| `src/usage/marks/README.md` | Records artwork sources and figure generation instructions. |
| `src/usage/marks/claude.png` | Embeds the Claude mark shown in the pane. |
| `src/usage/marks/claude.svg` | Stores the SVG source for the Claude mark. |
| `src/usage/marks/codex.png` | Embeds the Codex mark shown in the pane. |
| `src/usage/marks/codex.svg` | Wraps the original Codex bitmap in SVG. |
| `src/usage/marks/figures.alpha` | Stores compressed alpha masks for usage figures. |
| `src/usage/marks/generate-figures.py` | Regenerates the figure masks from the pinned Inter font. |
| `src/usage/omp.rs` | Reads saved oh-my-pi limit reports through SQLite. |
| `src/usage/sqlite.rs` | Runs bounded, read-only SQLite queries off the pane loop. |
| `src/usage/status.rs` | Reads Claude Code's saved cache and handles status-line reports. |
| `src/usage/tests.rs` | Shares usage fixtures and checks oh-my-pi ingestion. |
| `src/usage/tests/activity.rs` | Checks Claude Code's daily token totals across sessions. |
| `src/usage/tests/codex.rs` | Checks Codex session reports, order, and missing data. |
| `src/usage/tests/layout.rs` | Checks usage frames at different sizes and times. |
| `src/usage/tests/status_line.rs` | Checks Claude Code status-line and saved-cache readings. |
| `src/usage/time.rs` | Parses reported timestamps with enough precision to order them. |
| `src/walk.rs` | Lists files and folders outside Git. |
| `src/watch.rs` | Delivers macOS file-change notifications to the pane loop. |
| `src/web.rs`, `src/web/` | Serves the embedded settings editor on loopback and authenticates its requests. |
| `src/when.rs` | Works out local dates and wording for ages and durations. |
| `src/width.rs` | Measures and shortens text in terminal columns. |
| `tests/install.sh` | Checks which source the installer builds, with stand-ins for Git and Cargo. |
| `tests/listener.rs` | Runs listener locking and stopping with real processes and a Rex stand-in. |
| `tests/pane.rs` | Checks pane startup, input, and cleanup through a real terminal. |
| `tests/web-smoke.cjs` | Saves and closes the browser editor of a built binary in Chromium. |
