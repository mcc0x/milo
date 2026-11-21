# Contributing to milo

Keep changes small and make milo simpler, faster or more correct.

## Getting set up

You need Rust 1.91 or later, git, and a terminal.

CI checks every target and runs the portable test suite on Linux with Rust 1.91.0 and SQLite installed. Keep the Apple Silicon job too: filesystem notifications, terminal images and the packaged browser editor have platform-specific behavior.

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

Describe the pull request under the headings in `.github/pull_request_template.md`: **What changed** and **Verification**. GitHub fills them in when you open a pull request in the browser. A body passed on the command line (`gh pr create --body` or `--body-file`) replaces the template, so write the same headings yourself. Add **Compatibility** when a user or packager has to act.

A pull request that changes anything you can see (the website, a terminal pane, the macOS app or any other visible surface) must show real screenshots of the affected UI under **Screenshots** in its description. Show before and after when it changes an existing design, and desktop and mobile when both are affected. A change with nothing to see needs no screenshot; leave the section out.

Give every screenshot alt text that says what it shows, for people who cannot see the image: `![Changes pane after the fix, with the deleted file listed once](…)`. GitHub's default, the file name, says nothing. Replace it, in `alt="…"` too when GitHub inserts an `<img>` tag.

Label the pull request with the existing labels that fit, not every one that could. Usually that is one kind and one or two areas:

- **Kind:** `bug`, `enhancement` or `documentation`.
- **Area:** `ux` for what the user sees or does, `integrations` for Rex, coding agents and usage sources, `reliability` for recovery and regression checks, `performance` for speed and resource use, `accessibility` for barriers to people with disabilities.
- **State:** `in review` once it is ready to be read.

Nothing adds these for you, except that Dependabot labels its own pull requests. If GitHub does not let you set labels, say which fit and a maintainer will add them. Do not create new labels in a pull request.

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

An installation keeps the Lua files it was saved with until its next save, and
a save replaces only files it recognises as Milo's. Before changing
`actions.lua` or `listener.lua`, add the fingerprint (`fingerprint` in
`layout.rs`, the 64-bit FNV-1a of the file) of the version being replaced to
the list beside it, or installations holding that version are told their file
was left alone. `listener_0_1_2.lua` is the listener of releases 0.1.1 and
0.1.2, kept so that a test can save over it.

`titles.rs` names tabs and sessions for the listener and the session action,
and keeps the titles Milo set by Rex window ID in the state folder. A title
may use folder names and an agent's process name. Never read a terminal's
title, screen or status text for it, and never replace a label that is neither
Rex's default nor Milo's own. Only `milo_name_session` names a session, and
only the one it is run in: never rename, create or switch a session from the
listener or as a side effect of another action.

The listener keeps the answers of `milo tab-title title` by directory, so that
most events start no process. Each answer lists its grounds, the files that
decide it, and is reused while they read the same: a project's two preference
files, or outside a project the places where a repository could appear. When
neither says enough, as when Git could not run, list none and the listener
asks again. The listener opens every file listed, so never list a path that
another user could create.

Run `cargo test setup::` for generation, screen and installation tests. The
tests use throwaway settings and a stand-in for Rex. With Lua 5.1 installed,
run `cargo test --locked --bin milo rex_lua_fixtures -- --ignored` for the
action and listener scenarios. Set `LUA51` if the executable is not `lua5.1`.
No running Rex session is needed. To check a generated
file with Rex itself, run `MILO_CHECKING_REX=1 rex config check /path/to/copy.lua`
on a scratch copy. The flag prevents the actions file starting a listener
during the check. Check both files and try the pane choices. Only try a
reload or the actions on a disposable Rex configuration and session, never
as part of `cargo test`.

With Rex installed, `cargo test --test rex_lab -- --ignored --nocapture` does
that for you. It starts a Rex server of its own in a throwaway folder, with its
own home, settings, state and socket. There it saves a layout and follows two
Git worktrees through new tabs, divider drags, renamed tabs and sessions, and a
restart of the server. Every `rex` the test runs names that socket and cannot
start another server, and it checks that the lab's terminals and listener use
the lab's home and server too, so the Rex you are working in is never asked
anything. On macOS a shell tab runs under `login`, which hands it your home
again, so the lab sets `ZDOTDIR` and checks that each shell read the lab's
startup file and not yours. That file makes the shell copy itself a few times
as it starts, which is what prompts and plugins do to a shell while its new
tab is being arranged. Set `MILO_TEST_REX` if `rex` is not on your path. The test prints
whether the restarted server ran Milo again in the panes it restored; that is
Rex's choice. Where it restored shells instead, the test rebuilds one such rail
with the two actions `docs/setup.md` gives for that, and sees its drags
remembered again. A new shell tab that does not get its rail fails the test: none
is opened in its place. That holds for the tabs the test opens one at a time
and for those it opens back to back, four with no pause between them, three
times over.

### Adding a usage source

[Usage sources](docs/usage-sources.md) has the rules a source must meet and the evidence for each harness looked at so far. Before writing a reader, check the tool's format against a pinned release: in its source, and in a file that release wrote under a throwaway home. Keep tokens, cost and limits as separate values, and leave unknown whatever the tool does not save. Copy the structure of test fixtures from the release, and record the release and its checksum beside them. Read databases through `src/usage/sqlite.rs` and session files through `src/usage/files.rs` so every read stays bounded.

A tool also needs a rule for when it is found on this machine, apart from whether it has a figure, and an entry in `UsageTool` in `src/preferences.rs` so that it can be always shown or hidden. Decide it from files the tool saved, never by running the tool. A tool that is absent or hidden must take no room in the pane, in its help or in `milo doctor`, and a hidden one must not be looked for. `src/usage/tests/tools.rs` checks this at every size class.

### Adding a language to the reader

Code colouring uses `src/syntax.rs` for scanning, `src/syntax/langs.rs` for language tables, and `src/syntax/delimiters.rs` for strings and comments whose closing marks depend on the opener. Adding a language normally means changing only the tables:

1. Add a `Lang` constant: its line and block comments, its quotes (list a longer opener before any opener it starts with), its keywords and constants.
2. Map its file extensions in `FILE_LANGS`, or a file name such as `Makefile` in `NAMED_LANGS`.
3. Add a test with a few idiomatic lines and the classes you expect.

The scanner is not a parser and is not meant to become one. It knows comments, strings, numbers and reserved words, and guesses types and calls from how a name is written. If a language needs more than a table can say, it is fine for milo to colour it imperfectly.

### Checking failures and speed

`src/usage/tests/failures.rs` breaks each local usage source in five ways: missing, rotated, locked, stale, and written in a shape Milo does not know. The pane may then show only a figure the source reported, marked as old once it is; a source that cannot be read is unknown. A new source goes through the same five tests with its own files, using the helpers there.

`tests/reader_bench.rs` times the reader of a release build on generated files: long lines, comments and strings that span lines, and diffs just under the size Milo loads. It measures the first frame that shows the file and the frame after each key, through a real terminal:

```sh
cargo test --release --locked --test reader_bench -- --ignored --nocapture
sh tests/reader-compare.sh REVISION > report.tsv    # this checkout beside that revision
```

`tests/reader-compare.sh` exports the revision, builds it in a temporary folder and times both builds turn about, so the two columns come from the same machine in the same minutes. Compare ratios, not times from different machines or days. `tests/reader_bench/baseline-6199085.tsv` is such a report for the revision in its name; its header names the machine. Set `MILO_BENCH_MAX_RATIO=1.25` to fail the run when a median is more than a quarter slower than the base's and the difference is more than noise: half a millisecond for a first frame, twenty microseconds for a key.

`tests/reader_bench.rs` and `tests/rex_lab.rs` use only what a built Milo offers from outside: its commands, what it draws in a terminal and, in the lab, the requests its browser editor accepts. So they can check a build they were not compiled with: set `MILO_TEST_BINARY` to a Milo built from another branch. Where a check depends on a feature that only some builds have, ask the build rather than the branch. The reader report's `syntax` column, for instance, says whether each build coloured each file as code, so a file in a language one build knows and the other does not is timed as code for the first and as plain text for the second.

## Commit messages

One line in the imperative that says what the commit does for someone using or reading milo ("Leave closed folders closed when work lands inside them"), then a short list of the notable changes if there are several. No ticket prefixes are needed.

## Reporting a bug

Use the **Bug report** form on GitHub; it adds the `bug` label. Say what you ran, what you saw and what you expected. If it depends on a repository's state, the smallest sequence of git commands that reproduces it is worth more than a description. A screenshot or pasted output helps but is optional; remove anything private first, such as tokens, account names, paths and repository contents, and give images alt text. For anything that looks like a security problem (milo running something, or showing something, that it should not), please report it privately to the maintainer rather than in a public issue.

To ask for something new, use the **Feature request** form, which adds `enhancement`. Describe the problem and the outcome you want; a design is not needed.

## Licence

By contributing you agree that your work is released under the MIT licence that covers the project (see `LICENSE`).

## Source map

| File | Purpose |
| --- | --- |
| `src/diff.rs` | Parses whole-file diffs and marks what changed within a line. |
| `src/diff/words.rs` | Finds the words that differ between paired lines, within fixed limits. |
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
| `src/setup/actions.lua` | Defines Rex actions that arrange, add, or close milo panes or name a session for its project. |
| `src/setup/claude.rs` | Plans changes to the Claude Code status line and tracks ownership. |
| `src/setup/files.rs` | Checks, replaces, and restores files changed by setup. |
| `src/setup/install.rs` | Coordinates setup saves, Rex reloads, and listener changes. |
| `src/setup/layout.rs` | Reads and writes layout choices and generates Lua files. |
| `src/setup/listener.lua` | Remembers divider changes on managed rails and optionally arranges new shell tabs and titles unnamed ones. |
| `src/setup/listener_0_1_2.lua` | Keeps the listener of releases 0.1.1 and 0.1.2 for the test that saves over it. |
| `src/setup/listener_test.lua` | Checks the new-tab, new-session and tab-title handlers with a stand-in for Rex. |
| `src/setup/lock.rs` | Keeps saves and listeners from overlapping and stops the listener. |
| `src/setup/rex_files.rs` | Plans edits to Rex settings and checks the generated layouts. |
| `src/setup/settings.rs` | Reads and edits the top-level Claude Code status-line setting. |
| `src/setup/web.rs` | Exposes drafts, scoped saves, and Rex trials to the browser editor. |
| `src/syntax.rs` | Scans source text to find spans to colour. |
| `src/syntax/delimiters.rs` | Handles string and comment closers that depend on their openers. |
| `src/syntax/langs.rs` | Defines language rules and maps them to file names and extensions. |
| `src/term.rs` | Sets and restores terminal modes and waits for events. |
| `src/titles.rs` | Names Rex tabs and sessions after a Git project and remembers the titles Milo set. |
| `src/tree.rs` | Builds file and folder rows, folds, and changed-file counts. |
| `src/ui.rs` | Shares headings, status styles, icons, and scroll marks. |
| `src/usage.rs` | Collects the latest limits and today's Claude Code and OpenCode activity, and works out which tools are in use. |
| `src/usage/draw.rs` | Lays out the tools that have a place, with their figures, reset times, and hints. |
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
| `src/usage/opencode.rs` | Reads today's token and cost totals from OpenCode's session database. |
| `src/usage/sqlite.rs` | Runs bounded, read-only SQLite queries off the pane loop. |
| `src/usage/status.rs` | Reads Claude Code's saved cache and handles status-line reports. |
| `src/usage/tests.rs` | Shares usage fixtures and checks oh-my-pi ingestion. |
| `src/usage/tests/activity.rs` | Checks Claude Code's daily token totals across sessions. |
| `src/usage/tests/codex.rs` | Checks Codex session reports, order, and missing data. |
| `src/usage/tests/failures.rs` | Checks each usage source when missing, rotated, locked, stale, or in an unknown shape. |
| `src/usage/tests/layout.rs` | Checks usage frames at different sizes and times. |
| `src/usage/tests/opencode/` | Holds the session tables each checked OpenCode release creates. |
| `src/usage/tests/opencode_activity.rs` | Checks OpenCode totals, unknown states, and the pane's row against those tables. |
| `src/usage/tests/status_line.rs` | Checks Claude Code status-line and saved-cache readings. |
| `src/usage/tests/tools.rs` | Checks which tools have a place, and the frame for each combination and choice. |
| `src/usage/time.rs` | Parses reported timestamps with enough precision to order them. |
| `src/walk.rs` | Lists files and folders outside Git. |
| `src/watch.rs` | Delivers macOS file-change notifications to the pane loop. |
| `src/web.rs`, `src/web/` | Serves the embedded settings editor on loopback and authenticates its requests. |
| `src/when.rs` | Works out local dates and wording for ages and durations. |
| `src/width.rs` | Measures and shortens text in terminal columns. |
| `tests/install.sh` | Checks which source the installer builds, with stand-ins for Git and Cargo. |
| `tests/listener.rs` | Runs listener locking and stopping with real processes and a Rex stand-in. |
| `tests/pane.rs` | Checks pane startup, input, and cleanup through a real terminal. |
| `tests/reader-compare.sh` | Builds another revision and times its reader beside this checkout's. |
| `tests/reader_bench.rs` | Times the reader's first frame and scrolling on generated files. |
| `tests/reader_bench/baseline-6199085.tsv` | Records those times for one revision on the machine named in its header. |
| `tests/rex_lab.rs` | Follows the Rex layout through tabs, drags, renames, and a restart on a disposable Rex server. |
| `tests/rex_lab/app.lua` | Stands in for a connected Rex app on that server. |
| `tests/rex_lab/view.lua` | Prints that server's sessions, tabs, and panes for the test. |
| `tests/web-smoke.cjs` | Saves layout and usage tool choices in the browser editor of a built binary in Chromium, then closes it. |
