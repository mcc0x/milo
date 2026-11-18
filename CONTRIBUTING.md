# Contributing to milo

Thanks for wanting to help. milo is small on purpose, and the bar for a change is that it leaves the program simpler, faster or more correct without making it larger than it needs to be.

## Getting set up

You need Rust 1.91 or later, git, and a terminal.

```sh
git clone <your fork>
cd milo
cargo build --release
./target/release/milo outline    # the outline, in this repository
./target/release/milo history    # recent commits
./target/release/milo usage      # usage at a glance
./target/release/milo view src/main.rs
```

## Before you open a pull request

All four of these must be clean. They are quick: the whole test suite runs in under a second.

```sh
cargo build --release              # no warnings
cargo clippy --all-targets         # no warnings; the lint set is in Cargo.toml
cargo fmt -- --check               # no diff; the settings are in rustfmt.toml
cargo test                         # everything passes
```

Then run the program and look at the thing you changed. Tests read the screen as text, so they cannot tell you that something looks wrong.

## What a good change looks like

**It solves one thing.** A bug fix does not also rename a module. A refactoring does not also change behaviour.

**It adds no dependency.** milo depends on `libc` and nothing else. If you believe a crate would earn its place, open an issue first and say what it replaces.

**It is tested by what the user sees.** Most tests build a throwaway git repository (`src/fixture.rs`), open milo on it, and assert on the frame of text it draws. Write tests that would fail if the behaviour broke, not tests that repeat the code. A bug fix comes with the test that would have caught it.

**It keeps untrusted repositories safe.** milo is often pointed at code its user has not read. It must never run a program a repository names (hooks, filters, external diff tools, file-system monitors), never follow a link out of the project to show a file, and never write an escape sequence from a file name or a commit message to the terminal. `git/run.rs` is where git is called; nothing else should start a process except `rex.rs`.

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

The README has a one-line description of every module. The short version:

| If you want to change… | Look in… |
| --- | --- |
| what a row, the header or the hint line looks like | `src/outline/draw.rs` |
| what a key or click does in the outline | `src/outline/input.rs` |
| when the outline follows work, or how long a mark stays | `src/outline/attention.rs` |
| the history heading, commit list or opened files | `src/history.rs` |
| the reader | `src/reader.rs`, and `src/diff.rs` for how a diff becomes lines |
| which git commands run, and how safely | `src/git/run.rs` |
| Rex setup and its sketch | `src/setup.rs` |
| how setup saves both sets of settings or puts them back | `src/setup/install.rs`, `src/setup/files.rs` |
| changes to Rex settings and the generated layout | `src/setup/rex_files.rs`, `src/setup/layout.rs` |
| changes to Claude Code settings | `src/setup/claude.rs`, `src/setup/settings.rs` |
| how usage reads provider reports | `src/usage.rs`, `src/usage/files.rs`, `src/usage/omp.rs`, `src/usage/status.rs` |
| the usage strip and its figures | `src/usage/draw.rs` |
| the JSON reader shared by usage and setup | `src/json.rs` |
| guarded file opens and atomic replacements | `src/fs_ops.rs` |
| the Lua actions and the new-tab listener | `src/setup/actions.lua`, `src/setup/listener.lua` |
| what is read from git's output | `src/git/parse.rs` |
| what milo remembers about a repository | `src/git.rs` |
| how the screen is drawn and what a style is | `src/screen.rs` |
| keys and mouse input | `src/keys.rs` |
| how often milo looks for changes | `Pace` in `src/pane.rs` |
| the contract a pane implements | `Pane` in `src/pane.rs` |

### Adding a pane

Put the screen in a new module under `src/` and implement `pane::Pane`. The trait gives the shared loop a frame to draw, keys to pass on, a help page, and a place to watch and check for changes. Return `After::Leave` from a key that closes the pane, or `After::Stay` to keep it open. A pane with timed redraws can also implement `next_frame`.

Declare the module in `src/main.rs`, add the command to `Command`, its parser and `dispatch`, and list it in `--help`. Do not add a second event loop.

### Changing the Rex layout

The two Lua templates are in `src/setup/`. `layout.rs` adds the choices and
proportions above the actions in `rex.lua`, and the path to that file above
the handler in `listener.lua`. `rex_files.rs` checks both before installation.

Run `cargo test setup::` for generation, screen and installation tests. The
tests use throwaway settings and a stand-in for Rex. To check a generated file
with Rex itself, copy it to a scratch directory and run
`MILO_CHECKING_REX=1 rex config check /path/to/copy.lua`; the flag prevents the
new-tab listener from starting during the check. Check both files and try the
different pane choices. Only try a reload or the actions on a disposable Rex
configuration and session, never as part of the automated tests.

### Adding a language to the reader

Code colouring is a small scanner (`src/syntax.rs`) driven by tables (`src/syntax/langs.rs`). To add a language you should only need to touch the tables:

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
