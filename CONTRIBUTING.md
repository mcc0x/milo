# Contributing to milo

Keep changes small and make milo simpler, faster or more correct.

## Getting set up

You need Rust 1.91 or later, git, and a terminal.

CI runs portable tests on Linux with Rust 1.91.0 and SQLite, Apple Silicon tests for platform-specific behavior, and a native x64 Windows workflow. Test platform-sensitive changes on the affected target; a cross-build or a passing Linux suite does not exercise the Windows console, installer or file APIs.

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

On native Windows, use an MSVC Rust toolchain and run `.\target\release\milo.exe outline` in Windows Terminal after the Cargo build. Rex and menu bar features require a supported environment.

## Before you open a pull request

Run these checks:

```sh
cargo build --release              # no warnings
cargo clippy --all-targets         # no warnings; the lint set is in Cargo.toml
cargo fmt -- --check               # no diff; the settings are in rustfmt.toml
cargo test                         # everything passes
```

After changing the source or macOS installer, also run `sh tests/install.sh`; for Linux run `sh tests/linux-install.sh`. On Windows, use the installer fixture in the [Windows workflow](.github/workflows/windows.yml) in a disposable user profile. `cargo test` alone does not exercise the installers.

Then run the program and look at what you changed. Text checks cannot tell you whether a pane looks right.

When updating the README, guides, agent skill, `llms.txt` or this file, run `python3 site/bundle-docs.py` and commit the refreshed `site/public/llms-full.txt`, `site/public/llms.txt` and `site/src/generated/install.md`, the copy of the setup guide’s Install section that builds `/install/`. CI checks that the published copies match these sources.

The website in `site/` is a Next.js static export, built with Bun 1.4 and Node.js 26. In that directory, `bun install` and `bun run dev` start it for editing. Before a pull request, run `bun test && bun run build`, then `bun run preview` and look at the built pages in a browser. Railway runs the same build and serves `site/dist` with `site/Caddyfile`.

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

The tag check is a minimum, not release qualification. For each published target, follow [release packaging](docs/release-packaging.md) for the exact versioned commit and bytes. Qualify reporting-enabled packages separately if the release is approved to report.

CI selects checks based on changed paths. Shared inputs such as the menu icon, lockup, release installer and relay text fixture still trigger Mac coverage; unknown paths and incomplete diffs default to a full run. If the commit you want to tag skipped macOS, dispatch **Release tests** on its branch and verify the run SHA before tagging. A skipped job does not use the tag ruleset’s required check name.

## What a good change looks like

**It solves one thing.** A bug fix does not also rename a module. A refactoring does not also change behaviour.

**It adds no dependency.** milo depends on `libc` and nothing else. If you believe a crate would earn its place, open an issue first and say what it replaces.

**It is tested by what the user sees.** Use throwaway Git repositories (`src/fixture.rs`) and check the frames the program draws where practical. Write tests that would fail if the behaviour broke, not tests that repeat the code. A bug fix comes with the test that would have caught it.

**It keeps untrusted repositories safe.** Keep the user's Git settings, but disable hooks, filters, external diffs, text converters, file-system monitors and on-demand fetching. Do not follow a folder link out of the project to show a file or let names and messages send terminal controls.

**It keeps subprocesses in one place per tool.** Start Git through `src/git/run.rs`, the login shell that is asked for its search path through `src/task/harness.rs`, `/usr/bin/sips` through `src/picture/sips.rs`, FFmpeg and FFprobe through `src/reader/video/decode.rs`, `/usr/bin/sqlite3` through `src/usage/sqlite.rs`, `/usr/bin/curl` through `src/usage/http.rs` or `src/alerts/send.rs`, `/usr/bin/security` through `src/alerts/secret.rs`, Rex through `src/rex.rs`, `ssh` through `src/ssh.rs`, and formatters through `src/format.rs`. Use their existing safeguards. Never run a command named by project files or Git settings.

**It writes to a project only when the user saves or starts a task.** Every git command milo runs only reads, except the `git worktree add` of `milo task new`, which goes through `change` in `src/git/run.rs` and has no other caller. The one thing that changes a project's files is `Ctrl-S` in the reader's editor, and it goes through `src/reader/disk.rs`: the file is opened without following a link, read again, and written only if it is still what the edit started from or the user says to overwrite it. Do not add a second way to write, and do not let anything else (a formatter, a cache, a temporary file) write into the project. What milo keeps for itself, such as a draft from a reader that was ended, goes in its state folder.

**It starts no program the user did not ask for.** Opening and showing a file never run anything from the project or for it. A coding agent starts only from `milo task new` or its Rex action, by the name the person gave, and never another in its place. A formatter runs only on `Ctrl-F`, is found by a fixed name in the folders of `PATH` written in full, and is given the draft on standard input. Never install, download or run a tool through a package runner.

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

A `check` runs in the pane loop, and no key is read until it returns. Where it can take long, as Git can in a large project, start the work on a thread and return: `unfinished_check` gives the loop a descriptor that becomes readable when the answer is ready, and `finish_check` takes the answer in. Show nothing of an answer until all of it can be shown, drop one that an earlier load has overtaken, and return false from `finish_check` when another look is needed. The loop takes no other look while one is away, rests after it for four times as long as it took, and looks again soon if a change was reported meanwhile. `src/outline/refresh.rs` is the example.

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

The task launcher (`src/task.rs`) is the only code that creates a session, and
only when someone starts a task. It shows, selects and renames none.

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

A second test there, on a server of its own, is for the floating reader. It
opens a file from the outline, edits it through keys Rex sends, opens a second
file and sees it become a tab of the same reader beside the draft, switches and
closes tabs, and sees another session's outline get a reader of its own. Then it
has Rex close the reader on two unsaved drafts and gets one back at the next edit.
A third hands `milo review --float` a patch from a process with no terminal,
as an agent's shell would. It sees only the handed-over edits marked beside a
person's uncommitted lines in the same file, a path outside the project
refused, and a reason in place of the diff once the agent's line is rewritten.

### Changing the output action

**Milo: Open File from Terminal Output…** reads what a pane printed, which no other action does. [Opening a file from a terminal's output](docs/panes.md#opening-a-file-from-a-terminals-output) says what a person sees. Keep four things true.

It reads on demand and keeps nothing. `actions.lua` asks Rex for the end of one pane's output when the action is run, bounded by `output_bytes`, and hands it to `milo output-files list` on standard input. Do not read a pane from the listener, a timer or another action, and do not write the output or what was found in it to the state folder or a log.

The pane is the one in the action's context. Take the block, its tab and its program's folder from `ctx` before anything is asked, and never from the focus afterwards. Where the folder or the computer is in doubt, as with `ssh` in the pane, a block of another host or a program whose folder cannot be read, refuse and say why.

The output is untrusted. `src/output.rs` only looks paths up: it runs nothing, opens no address and reads no file's contents. A path counts only if it leads, links followed, to a file under the worktree's root and under no other `.git`. `open` looks the chosen place up again, because the choice came back through a Rex app. What goes to Rex is plain text with no control characters.

It never costs a draft. The action closes no pane. A reader is reused only if it noted the same project beside its pipe (`src/reader/inbox.rs`), and a request with a place moves the reader's view or a draft's cursor, never its text.

`cargo test --bin milo output:: reader::` runs the unit tests, and the Lua fixtures cover the action with a stand-in for Rex. With Rex installed, `cargo test --test rex_lab -- --ignored files_named` runs it in shell panes of a disposable server. That test loads this checkout's `actions.lua` with `rex do FILE --action`, so with `MILO_TEST_BINARY` it checks that build's `output-files` and reader under this checkout's action. No palette is shown there: check the list itself in a Rex app, on a disposable session.

### Changing SSH following

[Following a shell over SSH](docs/setup.md#follow-a-shell-to-another-computer-over-ssh) says what a person sees. Keep these true.

It is asked for, once. Only `milo ssh HOST`, typed in a Rex shell, takes a tab's panes to another computer. Never intercept plain `ssh`, read a prompt, a title, an OSC 7 report or any other terminal output for a host or a folder, or follow a second hop. A *link* in the state folder (`src/ssh/link.rs`) names the host and the connection, and counts only while the `milo ssh` that wrote it leads the program Rex reports in front of that one shell. Its words reach a remote shell unquoted, so keep the host, token and helper path checks.

It logs in once, as the person. `src/ssh.rs` runs the person's `ssh` with their configuration and prompts, as a master holding one connection with agent forwarding off. Every other session (the probe, the shell, each pane) goes over that connection with `ControlMaster=no`, and those that are not the shell with `BatchMode=yes`, so none can ask for or carry a credential. Do not copy keys, tokens or usage between the computers, add a forwarding, or change an SSH or shell setting on either side.

It installs nothing. Milo on the host is the helper, found by a probe in a session with no terminal. `ssh-helper shell` notes its own process in the host's state folder and becomes the login shell, so the noted process is the shell; panes on the host read its folder from the system (`src/ssh/process.rs`). Without Milo there the shell starts as plain ssh would start it.

Panes on the host only read. `ssh-helper pane` calls `reader::forbid_edits` before anything opens. Usage never moves on its own: Tab asks for the host's.

It gives back what it borrowed. A pane lends its terminal (`Pane::lend`, `Session::lend`) and keeps everything it held. The link is removed before the connection is closed, so a pane whose lent session ends can tell a logout or a lost connection from a person quitting the host's pane.

`cargo test --bin milo ssh:: workflow:: term::` runs the unit tests. With Rex and sshd installed, `cargo test --test rex_lab -- --ignored ssh::` starts an sshd of its own on 127.0.0.1 with its own keys and a host home in the lab, and never uses your SSH files or agent. It runs both computers' sides on this one, so check a real second host by hand before claiming one. Set `MILO_TEST_SSH_EVIDENCE` to a folder to keep its frames as HTML.

### Changing alert mirroring

`src/alerts.rs` and the files under `src/alerts/` mirror Rex alerts to Telegram or Discord. An *alert* is an event Rex itself draws attention to; `event.rs` lists them and is the only place that decides. Hear Rex only through `rex events`, as `src/rex.rs` starts it: never read a terminal's screen, a system notification or Rex's socket for an alert.

Mirroring stays off until the user turns it on. The setup screen and the browser editor say what will be sent and ask first; `milo alerts` says it before and `milo alerts on` as it starts. A new field leaves the machine only behind a choice that is off by default, and `message.rs` says so in `sends`. Whatever a program supplied, its short name included, is untrusted: send it through `literal`, and its text through `plain` first.

A secret goes from standard input or the browser editor to `secret.rs` and from there to curl's standard input. Never put one in an argument, a settings file, a status note, a log, or anything sent back to a settings page. Destinations are fixed hosts; do not add one the user can type.

Start every program the mirroring process runs with `fs_ops::spawn`, never `Command::spawn`, `output` or `status`. The worker delivers to each destination on a thread of its own, and macOS has no way to make a pipe that other programs cannot inherit from the start. A program one thread starts while another is making a pipe keeps that pipe open for as long as it runs: a held Telegram request then holds up reading the Discord secret, and with it Discord's delivery.

Lifecycle notice consent is bound to the exact provider credential and, for Telegram, the chat. The versioned local tag is salted separately for each approval; it is not a credential and cannot be used to send a message. Do not accept a missing, unknown, or mismatched tag. If Rust changes the hash algorithm used by the tag, notices require explicit reapproval; regular forwarding and saved connections are unchanged.

`cargo test alerts` runs the unit tests and `cargo test --test alerts` runs the real `milo alerts` against stand-ins: a `rex` that prints the events a test writes, and a server on the loopback address for both destinations. `MILO_ALERTS_TEST_ENDPOINT` names that server, and is refused unless it is `http://127.0.0.1:PORT`. On macOS `MILO_ALERTS_KEYCHAIN` names a keychain file of the test's own, so that tests never touch yours. Only a development build reads either variable: a release build has no setting that changes where a secret is kept or sent, so with `--release` these tests do nothing. No test may use a real token, webhook or chat, or post to Telegram or Discord.

`cargo test --test alerts real_rex -- --ignored` exercises the real event subscription against an installed Rex, still with a loopback destination and a throwaway home, settings, state, keychain and socket. Set `MILO_TEST_REX` if the executable isn't on `PATH`. The test stops and restarts only its own server, then checks that a fresh program-status alert arrives exactly once without re-enabling forwarding. The non-ignored tests use the controllable stream to exercise flapping, worker exits and a pause during backoff.

### Changing support for several Rex hosts

[Several Rex hosts](docs/setup.md#several-rex-hosts) says what a person sees. Three parts stay separate.

`src/host.rs` reports the computer and its Rex. `CHECKED_API` is the control API revision Milo's Lua files and calls were checked against: raise it only after running the Lua fixtures and both Rex labs against that Rex. A newer server is a note, never a warning. The computer's name is shown on that computer and in pages its editor serves, and goes nowhere else.

`src/web/remote.rs` opens another host's editor here. The host's `milo web --forwarded-port N` accepts exactly one authority on the computer carrying the forward, `127.0.0.1:N` (or `127.0.0.1` for HTTP port 80), in place of its own and never beside it. Do not add a second accepted name, a wildcard, or a bind address other than loopback at either end. Keep the session key out of arguments: it reaches this computer only by reading the terminal the host's editor printed it in. Wait for Rex's forward process to confirm the expected listener before exposing that link; another local process listening there is not confirmation. Every Rex call for it is in `src/rex/server.rs`, never starts a server, and keeps only whole IDs, numbers and plain version words from what a server answers. Whichever way `remote::run` leaves, dropping `Carried` ends the forward and the host's session.

`src/setup/transfer.rs` writes and reads bundles. A key travels only if a `Group` lists it. Never list an account, a token, a destination, an integration switch or anything from the state folder, and keep the test that seeds those and searches the bundle for them. An import changes files only through `files::install`, so that it is all or nothing and stops when a file is not what the preview read; its undo record may name only files below the settings folder. A project is matched by `git::root_commit` or by the person, never by a name or a path.

Use `cargo test --bin milo FILTER` with `host::`, `rex::server`, `transfer::`, or `web::remote` to run the corresponding unit tests. With Rex installed, `cargo test --test rex_hosts -- --ignored` starts two disposable Rex servers with a home, settings and socket each, and follows Doctor, the forward and its cleanup, and a bundle from one to the other. `rex server stop` and `rex hosts` do not follow `REX_SERVER`, so that test names each socket with `-S` and never runs `rex hosts`. A development build takes the computer's name from `MILO_TEST_HOST_NAME`, for a test or a screenshot; a release build does not read it.

### Changing Discord presence

[Discord presence](docs/presence.md) publishes an activity to Discord. It stays off until someone turns it on, and off means no process, no socket and no timer: `tests/presence.rs` checks that. The activity is the fixed line in `src/presence/discord.rs`, under Milo's public application ID or one the person gave. Milo holds no bot, token or secret for either, and none may be added.

A second line is shown only for the *details* a person chose: the name of the session in front, the folder name of the Git project in its focused pane, or both. The default shows neither, and a choice saved by an earlier release must go on showing only what it showed. `src/rex_presence.lua` may look at sessions only for a name that was chosen, and never reads a tab's or terminal's title, a screen, a command line or a status text. Every name goes through `src/presence/label.rs`, which keeps one short line of plain text and drops whole anything that could be a path or an address. Never show a path, a remote address, a branch, a file name or a command, by default or behind a setting. When the script cannot tell which one session is in front, it reports none, and no name is shown. `presence::public` says what each choice shows, and every surface shows that text before turning presence on or adding a name.

`presence::Control` is the only way to read or change the choice, so `milo presence`, Settings and the browser editor cannot disagree. A change names the choice its surface had shown, and `Control::apply` refuses it when another surface has saved a different one since: what the person was told would become public is then no longer what would. Turning presence off needs no such check. One helper holds `presence.lock` for as long as it runs; a second gives up rather than set the activity in turn. A change to the details or the application replaces the helper, which takes the old activity down first.

No test may reach a real Discord. The helper looks for Discord's socket in one folder, the first of `XDG_RUNTIME_DIR`, `TMPDIR`, `TMP` and `TEMP` that is set, so a test that starts a helper gives it an environment in which the first of those names a folder of the test's own, and listens there as a stand-in. Do the same when trying a build by hand, and do not publish to your own Discord account from a test. With Rex installed, `cargo test --test rex_lab -- --ignored discord_presence` checks the watch on a disposable Rex server. What a real Discord app then displays can only be checked by hand with a test application and account.

### Changing the menu’s Needs you section

[Rex panes that need you](docs/usage.md#rex-panes-that-need-you) says what a person sees. *Attention* is a Rex terminal whose program status says it waits for the person, failed or finished. Keep four things true.

It is off until someone turns it on, and off means no process, no Rex call and no timer. The menu app starts `milo menubar attention watch` only while the choice in `menubar.conf` is `menu` or `dot`, and the watch ends itself when that choice turns off or cannot be read. However the watch ends, the Rex script it started ends with it: `tests/menu_attention.rs` checks each way. The choice is separate from alert mirroring and from low-quota notifications; do not read either one’s settings for it or add a sound or a notification.

It reads Rex’s report and nothing else. `src/rex_attention.lua` passes on a status record’s ID, state, kind, program name and time, and the session’s and tab’s labels. It must never read a record’s title or message, a terminal’s title, screen or scrollback, or send anything to a terminal. It answers only for the Rex server on this computer, reached over its local socket, and only for terminals that server runs. Everything it prints is untrusted: `board.rs` bounds it and makes labels plain before the menu shows them.

It shows what is true now. `board.rs` keeps no history: a record that goes back to work, is cleared or loses its terminal leaves the board, and a lost Rex leaves nothing listed. *Seen* is kept in the watch’s memory only, for one record in one state, and only a finished or failed record can be seen. Marking one row seen, or jumping to it, must never clear another.

A jump changes nothing it has not checked. `rex::jump` takes the host ID the pane was reported under and the pane’s whole block ID. It asks the local server who it is, finds the block wherever it is now, and only then focuses it and asks the one Rex app of this person on this computer to select its session. Never match a pane by label, position or ID suffix, never ask a server on another host, and never turn on Rex’s Remote Control or any other Rex setting: when Rex refuses, say so and stop.

Rex’s client list also holds command-line clients, scripts and entries with no `info` at all. Only an entry that says it is an app is a candidate, so whatever the others leave out must not end the scan: that once hid a connected Rex for Mac behind an earlier entry with no description. An app entry is the opposite. It counts only with a whole client ID, a local-socket principal of this user and a process ID, and one that could be this person’s app but lacks any of those means no app is switched.

`cargo test --bin milo attention` runs the unit tests, and `cargo test --test menu_attention` runs the real commands against a stand-in for `rex`. With Rex installed, `cargo test --test menu_attention real_rex -- --ignored` follows waiting, resumed, finished and failed panes, a jump and a server restart on a disposable Rex server with a socket of its own. `sh tests/menubar.sh` runs the menu app’s side in `tests/menu-attention.swift`. To look at the menu itself, `python3 docs/evidence/menu-attention/walkthrough.py` drives a separate copy of the app against such a server and saves pictures; see [its notes](docs/evidence/menu-attention/README.md) for what that does and does not exercise. No test may ask the Rex you work in.

### Adding a usage source

[Usage sources](docs/usage-sources.md) has the rules a source must meet and the evidence for each harness looked at so far. Before writing a reader, check the tool's format against a pinned release: in its source, and in a file that release wrote under a throwaway home. Keep tokens, cost and limits as separate values, and leave unknown whatever the tool does not save. Copy the structure of test fixtures from the release, and record the release and its checksum beside them. Read databases through `src/usage/sqlite.rs` and session files through `src/usage/files.rs` so every read stays bounded.

A tool also needs a rule for when it is found on this machine, apart from whether it has a figure, and an entry in `UsageTool` in `src/preferences.rs` so that it can be always shown or hidden. Decide it from files the tool saved, never by running the tool. A tool that is absent or hidden must take no room in the pane, in its help or in `milo doctor`, and a hidden one must not be looked for. `src/usage/tests/tools.rs` checks this at every size class.

An account is different: its figure exists only on a provider's servers, so nothing on disk says it is in use. It needs an entry in `UsageAccount`, which starts hidden, and an adapter in `src/usage/accounts.rs` that names the documented endpoint, the `MILO_…` settings it reads, how often it may be asked, and how to read the reply into a `Figure` with its own `Kind` and `Unit`. Never give an account a limit window: a balance, a spend or a count of credits must not become a percentage or reach the menu bar badge. Never read a key another tool saved, and never test against the real provider. Unit tests serve fixtures from a loopback port through `Sources::test`. That stand-in and the plain-HTTP address it uses are compiled only under `cfg(test)`: an installed binary has no setting, variable or file that sends a key anywhere but the provider's HTTPS host, and `tests/pane.rs` checks the built binary for that. Do not add one.

### Changing the patch review

`milo review` shows a *handed-over* patch: a unified diff a coding agent wrote of its own edits. [Reviewing an agent's edits](docs/panes.md#reviewing-an-agents-edits) says what a person sees. Keep three things true.

It never guesses. `src/handoff/` works only from the patch and the file's text now, and `src/review.rs` asks Git for nothing but the project's top folder. Do not add a fallback to Git's diff, status or a list of changed paths: those say what is uncommitted, not who wrote it. A file the patch does not fit shows its reason and no diff.

It claims no more than it checked. Milo checks that a file still holds the patch's lines, not who wrote them, so the screen says `patch from NAME` and the help says the rest. Do not word it as the agent's edits being verified. A new harness goes in the table in `docs/panes.md` only with the version and what was observed in a throwaway repository that had someone else's uncommitted work in the same file.

The patch is untrusted and the review writes nothing. Paths go through `safe` in `patch.rs` before any file is looked for, and files are read through `src/reader/working_path.rs` with no link followed. Keep the work limit in `fit.rs`. Do not copy the patch into the state folder, run a program for it, or install anything into an agent's settings.

Notes leave only as a copy. A note on the patch's lines (`src/review/`) is kept in memory with the lines as they were shown, and `Screen::copy` asks the review's own terminal to put its text on the clipboard. Do not have the review type into a pane (`rex send`, `send-key`, a terminal's `write`, a paste event), press Return, or call a harness to deliver a note, and do not keep notes in the state folder: they quote the patch. A direct send needs two things shown in a disposable session first: the harness itself, not the patch, `--from`, the focused pane or a session's name, says which running agent handed the patch over; and the delivery cannot join, submit or replace words already typed at that agent's prompt, nor run when the agent is gone. [The record for each harness](docs/evidence/review-notes/README.md) says where each one fell short; add to it rather than to the code until one passes both.

`cargo test --bin milo handoff::` and `cargo test --bin milo review::` run the unit tests and the pane tests, which mix a person's, the agent's and another agent's uncommitted edits in one file. `cargo test --test pane a_review_note` writes and copies a note through a real terminal. With Rex installed, `cargo test --test rex_lab -- --ignored a_floating_review` opens the floating review on a disposable Rex server, and `cargo test --test rex_lab -- --ignored review_notes` checks there that copied notes reach the review's clipboard and that panes holding a draft are left as they were.

### Changing the task launcher

[Starting an agent task](docs/setup.md#start-an-agent-task) says what a person sees. `milo task new` and the Rex action **Milo: New Agent Task…** both end in `launch` in `src/task.rs`. Keep it that way: the action in `src/setup/actions.lua` only asks for what was not passed, through `rex.ui`, and runs `milo task new` with each answer as an argument of its own. Do not give the launcher a pane, a Home entry or a page in the browser editor.

Keep the order. Everything that can be refused is checked before anything is created; then the worktree is added, then the session. Nothing that was made is removed, and a failure after the worktree exists says what exists. A person closing a question in Rex ends the action before Milo is run. Never switch the checkout the task was asked from, reuse a worktree that was not named, or choose a branch's starting commit for the person.

The session starts in the background. Do not select it, focus it, rename another session, type into it or answer an agent's trust question. Do not push, open a pull request, merge or remove a worktree.

A harness goes in `HARNESSES` in `src/task/harness.rs` only with the interactive invocation its documentation gives, checked against a real install: that the agent's full screen appears in the worktree, that a prompt with shell syntax and a leading dash arrives as written, and that a follow-up can be typed. Never use a print, headless or one-shot mode, and never start one harness in place of another. Add the version to the table in `docs/setup.md`. An online usage account is not a harness.

`cargo test --bin milo task:: worktrees::` runs the unit tests and `cargo test --test task` runs the real `milo task new` beside stand-ins for `rex` and the agents. With Lua 5.1, the fixtures above cover the action's questions, including closing each one. With Rex installed, `cargo test --test rex_lab -- --ignored a_task_starts` starts tasks from the command line and through the action on a disposable server. No test starts a real agent: do that by hand on a disposable Rex server and repository, and say in the pull request which agents and versions you ran. Only a Rex app can answer `rex.ui` questions, so say whether you answered them in an app or passed every choice as an argument.

### Changing reader tab lists

[Reopening tabs and named sets](docs/panes.md#reopening-tabs-and-named-sets) says what a person sees. `src/reader/sets.rs` keeps the lists, `src/reader/reopen.rs` is the screen over them, and `src/reader/tabs.rs` records the last tabs and reopens what was chosen. Keep four things true.

A list holds references only: a path inside the worktree, a full commit ID, a line and which tab was shown. Never write a file's text, a draft, a command or a terminal's contents into one, and never write a list into the project.

Nothing reopens unasked. A reader records its tabs as the last tabs and shows no list until `Ctrl-T`; only `milo tabs` and the two Rex actions start at the lists. Every reopening goes through the preview, and a named set changes only through its own save, replace, rename and delete.

A tab is opened as `milo view` would open it: a working file through `WorkingTarget`, a file at a commit through `CommittedTarget` with the full ID that was kept. When that fails the tab stays closed and says so. Do not fall back from a commit to the working file, or from a missing file to another path.

A tab that is open is left alone. Reopening matches each kept tab against the open ones before loading anything, so a draft, its undo history and a reading position are never replaced.

`cargo test --bin milo reader::` runs the unit tests and `cargo test --test pane the_tabs_a_reader` runs `milo tabs` in a terminal. With Rex installed, `cargo test --test rex_lab -- --ignored reader_tabs_reopen` follows two worktrees through panes of a disposable server: sets saved, replaced, renamed and deleted, a reopen over a draft, a deleted file and a dropped commit. It starts each reader with `milo view` and loads this checkout's `actions.lua` with `rex do FILE --action`, so it needs no saved layout. Set `MILO_TEST_READER_EVIDENCE` to a folder to keep its frames as HTML; `tests/rex_lab/capture_png.swift` turns one into a picture.

### Adding a language to the reader

Code colouring uses `src/syntax.rs` for scanning, `src/syntax/langs.rs` for language tables, and `src/syntax/delimiters.rs` for strings and comments whose closing marks depend on the opener. `src/syntax/markup.rs` reads HTML and XML, and `src/syntax/markdown.rs` reads Markdown; those two are not code and have walkers of their own. Adding a language normally means changing only the tables:

1. Add a `Lang` constant: its line and block comments, its quotes (list a longer opener before any opener it starts with), its keywords and constants.
2. Map its file extensions in `FILE_LANGS`, or a file name such as `Makefile` in `NAMED_LANGS`.
3. Add a test with a few idiomatic lines and the classes you expect.
4. Add its row to the language table in `docs/panes.md`.

The scanner is not a parser and is not meant to become one. It knows comments, strings, numbers and reserved words, and guesses types and calls from how a name is written. If a language needs more than a table can say, it is fine for milo to colour it imperfectly. A search for a closer must not start again from every opener on a line: a line can be megabytes long. For the same reason a comment or a string is looked for only at a byte that can begin one: the first byte of each mark in the tables, and those `Dialect::openers` lists for a dialect, which a new opener in `delimiters.rs` must be added to.

### Adding a formatter

`src/format.rs` has the table of formatters: `KINDS` maps file extensions to the tools tried in order, and each `Tool` is a program's name and its arguments. A formatter belongs there only if it reads the text on standard input and writes the formatted text to standard output, leaves the file alone, needs no network and exits with a failure when it cannot format. Give it the file's name with `Name`, which is passed as `./name` so that it cannot be read as an option.

1. Check the command against the real program, including what it does with text it cannot parse and whether it writes a cache into the folder it runs in.
2. Add the `Tool` and its row in `KINDS`, with the names as they read in `Not installed: …`. Keep those names within 24 characters: they are a row of the help page.
3. Add the row to the formatter table in `docs/panes.md`.

The tests use stand-ins written by `format::tests::stand_in`, so they pass on a machine with no formatter installed. `rustfmt_formats_a_rust_draft_where_it_is_installed` in `tests/pane.rs` runs the real program where there is one.

### Checking failures and speed

`src/usage/tests/failures.rs` breaks each local usage source in five ways: missing, rotated, locked, stale, and written in a shape Milo does not know. The pane may then show only a figure the source reported, marked as old once it is; a source that cannot be read is unknown. A new source goes through the same five tests with its own files, using the helpers there.

`src/usage/tests/accounts.rs` does the same for accounts against a loopback server: a missing key, a settings file others can read, a refused key, a rate limit, an unreachable provider, a missing curl, an old answer and a reply in an unknown shape.

`tests/reader_bench.rs` times the reader of a release build on generated files: long lines, comments and strings that span lines, and diffs just under the size Milo loads. It measures the first frame and the frame after each key through a real terminal, including typing, splitting a line, undo and redo near the end of a large draft, above lines far wider than the pane and in a file that is one line:

```sh
cargo test --release --locked --test reader_bench -- --ignored --nocapture
sh tests/reader-compare.sh REVISION > report.tsv    # this checkout beside that revision
```

`tests/reader-compare.sh` exports the revision, builds it in a temporary folder and times both builds turn about, so the two columns come from the same machine in the same minutes. Compare ratios, not times from different machines or days. `tests/reader_bench/baseline-6199085.tsv` is such a report for the revision in its name; its header names the machine. Set `MILO_BENCH_MAX_RATIO=1.25` to fail the run when a median is more than a quarter slower than the base's and the difference is more than noise: half a millisecond for a first frame, twenty microseconds for a key.

`tests/reader_bench.rs` and `tests/rex_lab.rs` use only what a built Milo offers from outside: its commands, what it draws in a terminal and, in the lab, the requests its browser editor accepts. So they can check a build they were not compiled with: set `MILO_TEST_BINARY` to a Milo built from another branch. Where a check depends on a feature that only some builds have, ask the build rather than the branch. The reader report's `syntax` column, for instance, says whether each build coloured each file as code, so a file in a language one build knows and the other does not is timed as code for the first and as plain text for the second.

Four more timing tests follow the reader's. `tests/bench/report.rs` holds what they share: the variables above, the turn-about order and the report. `tests/web_bench/`, `tests/history_bench/`, `tests/open_bench/` and `tests/usage_idle/` each hold such a report against the revision in its name.

```sh
cargo test --release --locked --test web_bench -- --ignored --nocapture
cargo test --release --locked --test history_bench -- --ignored --nocapture
cargo test --release --locked --test open_bench -- --ignored --nocapture
cargo test --release --locked --bin milo usage::tests::idle -- --ignored --nocapture
```

`tests/web_bench.rs` starts `milo web --no-open` in a throwaway home, outside Rex and beside a stand-in for Rex that answers at once or after a pause. It times the link, the first authenticated state, and that state when a stand-in for a browser asks 100 milliseconds after the link. `tests/history_bench.rs` opens a five-file commit and a six-thousand-file one in a generated repository, and times the frame that lists the files, the frame after Down sent with Enter, and Down alone; in a second repository of commits with two-megabyte subjects it times the first frame and Down. Both take `MILO_BENCH_BASE`. `src/usage/tests/idle.rs` is inside the crate, because only there can it run the pane's timer twenty times without waiting twenty intervals: it counts the `sqlite3` processes an idle pane starts for a small and a large pair of generated databases, and the looks until a new row shows. Compare its figures between two checkouts.

`tests/open_bench.rs` generates an ordinary repository of a few hundred files and a large one of fifty thousand, each with an upstream, unpushed commits, edited files and new ones. It times the first frame of the outline, the history and the reader, which is mostly the Git commands a pane runs before it can draw, then opening the outline's search and a letter typed into it. In the history it rewrites a file and times the slowest key pressed while the pane takes that in. It takes `MILO_BENCH_BASE` too. A pane's first frame varies by a few milliseconds from run to run and more on a busy machine, so compare medians of ten runs or more.

To compare backend changes, run each build alone on the same machine:

```sh
cargo test --release --locked --bin milo usage_profile -- --ignored --nocapture --test-threads=1
cargo build --release --locked
python3 tests/settings-bench.py target/release/milo
```

The usage fixtures time transcript appends and SQLite reads with a writer still connected. They check token totals, window freshness and how often four loopback account providers are asked, including after a rate limit and a reopen. The settings fixture times reads during a competing save as well as independent edits and startup; its timings include loopback HTTP and Python JSON decoding. Neither fixture needs provider credentials. For delivery, saturation and idle measurements of the hosted relay, use its [isolated container benchmark](hosted/README.md#benchmark-the-shipped-relay).

For repeated work after a pane has opened, use a release build and run these separately:

```sh
python3 tests/backend_refresh.py target/release/milo
cargo test --release --locked --bin milo adopting_edits_in_a_large_worktree -- --ignored --nocapture
python3 tests/backend_reader.py target/release/milo
```

The first counts Git commands through discovery, saves, a 24-save burst, staging and a commit. It waits for the changed counts or subject on screen. `--fixture sparse-checkout` tests a sparse index; `--idle-seconds 31.5` includes the macOS backup check. Linux polls instead of receiving FSEvents, so compare counts on the same platform. The Rust fixture separates the cost of launching a refresh, reading Git on the worker and taking its result into Outline. It repeatedly edits a path near the beginning of a 50,000-entry index; a path near the end need not cost the same.

The reader probe uses a private inbox and real terminal, not a Rex server. It times showing, switching and closing large working and committed tabs, paging a handed patch and rereading a changed file. It reports resident memory separately from Git subprocess counts, and holds a Git command to see whether input is answered during a load. Pass `--base /path/to/earlier/milo` to alternate the builds on the same files. These are new pane processes and warm filesystem caches, not measurements from a freshly booted machine.

`tests/menu-bench.sh` times the macOS menu app with made-up frames, on a Mac with a menu bar to draw in:

```sh
sh tests/menu-bench.sh
```

It builds a separate app under `target/menu-bench/` from the menu app's own sources and `tests/menu-bench.swift`, and runs it once. That app shows its own menu bar item and popover for about a minute. It starts no reader, reads no usage, and neither stops nor replaces an installed Milo. Each row gives the time the main thread was held and, for frames, the CPU the whole process spent per frame, which also counts what SwiftUI draws afterwards. The rows cover a frame equal to the last, one that changes the badge, and one that changes only the popover, each before the popover was first opened, with it closed and with it open. The popover opens without its animation, so opening it is timed whole. Runs differ by more than a build does: a whole run can take two or three times the CPU of the next, so run each build several times turn about, and compare a frame's CPU with the `idle` row of the same run. The last line is a check, and the run fails without it: a row whose reset passes must be redrawn with no new frame to say so, in an open panel and in one opened afterwards. `tests/menu-bench/` holds such a report for the revision in its name.

`sh tests/menu-bench.sh --check` runs that check alone, in about ten seconds. The copy holds its popover open itself, so a click elsewhere cannot end a row or the check early.

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
| `src/alerts.rs` | Keeps the alert mirroring choices and applies each change to them. |
| `src/alerts/cli.rs` | Runs `milo alerts` and reads a secret without showing it. |
| `src/alerts/consent.rs` | Captures what a confirmation showed without exposing the saved credentials. |
| `src/alerts/event.rs` | Reads Rex's event stream and decides which events are alerts, once each. |
| `src/alerts/forward.rs` | Follows Rex as the one mirroring process, with retries and rate limits. |
| `src/alerts/message.rs` | Words an alert, strips a program's text of mentions, and says what is sent. |
| `src/alerts/relay.rs` | Pairs a destination and forwards Telegram alerts through Milo's relay, or a loopback one in development. |
| `src/alerts/secret.rs` | Keeps a destination's secret in the macOS Keychain or a private file. |
| `src/alerts/send.rs` | Posts one message with curl and classifies the answer. |
| `src/alerts/telegram.rs` | Finds the chats in a bot's recent updates, within limits, to offer as destinations. |
| `src/alerts/tests.rs` | Checks alerts, messages, settings, secrets and the request. |
| `src/diff.rs` | Parses whole-file diffs and marks what changed within a line. |
| `src/diff/words.rs` | Finds the words that differ between paired lines, within fixed limits. |
| `src/fixture.rs` | Makes throwaway projects and Git repositories for tests. |
| `src/format.rs` | Finds an installed formatter for a kind of file and runs it over a draft, within limits. |
| `src/fs_ops.rs` | Opens files without following final links, stages atomic replacements, and starts programs one at a time so that none inherits another's pipe. |
| `src/git.rs` | Finds repositories and keeps their status, file lists, and history. |
| `src/git/parse.rs` | Reads Git status, diff, and log output. |
| `src/git/pending.rs` | Counts lines in pending changes and reuses tracked-file counts across unrelated edits. |
| `src/git/run.rs` | Runs bounded, read-only Git commands with unsafe settings overridden, and the one command that adds a worktree. |
| `src/git/worktrees.rs` | Lists a repository's worktrees and branches, and adds a worktree for a task. |
| `src/handoff.rs` | Works out what a handed-over patch changed in a file as it is now, or why that cannot be told. |
| `src/handoff/fit.rs` | Finds a patch's lines in the file and takes them back out in memory, within a fixed amount of work. |
| `src/handoff/patch.rs` | Reads the files and hunks of a unified diff and refuses names that could leave the project. |
| `src/help.rs` | Draws and scrolls the in-pane help page. |
| `src/history.rs` | Keeps commits, opened file lists, and selection state. |
| `src/history/draw.rs` | Draws history headings, dots, commit rows, and file previews. |
| `src/history/input.rs` | Handles history keys and mouse clicks. |
| `src/history/listing.rs` | Reads one commit's files off the pane's loop. |
| `src/history/refresh.rs` | Refreshes the history's Git state away from the pane loop and takes the answer in whole. |
| `src/history/tests.rs` | Checks history rows, tracking, and opening files at commits. |
| `src/home.rs` | Runs Home, the picker `milo` opens, with settings and maintenance in the same terminal session. |
| `src/home/mascot.rs` | Draws the mascot in coloured cells at four sizes. |
| `src/icons.rs` | Chooses file and folder symbols and their colours. |
| `src/json.rs` | Validates JSON and lets callers read fields without copying values. |
| `src/keys.rs` | Decodes keys, mouse events, pasted text, and terminal replies. |
| `src/main.rs` | Parses and dispatches commands and supplies CLI help. |
| `src/menubar.rs` | Stores the profile-wide menu choices in one file and manages the optional macOS app in menu-enabled builds. |
| `src/menubar/attention.rs` | Keeps the choice for the menu's Needs you section and runs `milo menubar attention`. |
| `src/menubar/attention/board.rs` | Keeps what each Rex terminal last reported as needing the person, what was marked seen, and writes the menu's rows. |
| `src/menubar/attention/watch.rs` | Streams those rows to the menu app while the section is on, and ends with its Rex script. |
| `src/menubar.sh` | Verifies, installs, upgrades and removes that app for `menubar.rs`. It is embedded, never downloaded. |
| `src/outline.rs` | Updates the file tree, pending changes, and recent marks. |
| `src/outline/attention.rs` | Uses focus and recent activity to follow writes and fade marks. |
| `src/outline/draw.rs` | Lays out and draws the full tree and short strip. |
| `src/outline/input.rs` | Handles outline navigation, folds, and file opening. |
| `src/outline/refresh.rs` | Refreshes the outline's Git state away from the pane loop and takes the answer in whole. |
| `src/outline/untracked.rs` | Fills in line counts for new files after the first frame, without following links. |
| `src/output.rs` | Finds the project files a terminal's recent output names, within its worktree, for the Rex action that opens one in the reader. |
| `src/output/tests.rs` | Checks which mentions are offered, their order and context, and what is refused. |
| `src/pane.rs` | Defines the pane interface and runs the shared terminal loop. |
| `src/paths.rs` | Finds milo's configuration and state folders. |
| `src/picture.rs` | Loads pictures and converts them to PNG for the terminal. |
| `src/preferences.rs` | Stores personal preferences outside repositories, per worktree over global defaults, and folder choices in local state. |
| `src/presence.rs` | Keeps the one Discord presence choice, says what each choice makes public, and turns it on, off, and into a status for every settings surface. |
| `src/presence/discord.rs` | Speaks Discord's local RPC protocol: the socket, the frames, the fixed activity and its optional second line. |
| `src/presence/helper.rs` | Runs the single background process that shows the activity while a Rex app has a session open, and the chosen names of what is in front. |
| `src/presence/label.rs` | Makes a session or project name fit to show, or leaves it out. |
| `src/read.rs` | Reads regular files and the names that file links point to. |
| `src/reader.rs` | Keeps reader state, handles navigation and updates, and starts and ends an edit. |
| `src/reader/buffer.rs` | Holds the lines of a draft with their endings, and its undo history. |
| `src/reader/content.rs` | Locates files and loads text, diffs, or pictures. |
| `src/reader/disk.rs` | Reads a file to edit and saves a draft only into the file it was read from. |
| `src/reader/draw.rs` | Draws files, change marks, and reader headings. |
| `src/reader/edit.rs` | Edits a draft: cursor, keys, questions, formatting, and drawing. |
| `src/reader/edit_tests.rs` | Checks editing, saving, conflicts, read-only files, kept drafts, and formatting. |
| `src/reader/inbox.rs` | Carries a request to open one more file, at a line if one is named, to a reader already running in a Rex block, through a pipe in the state folder, and notes that reader's project beside it. |
| `src/reader/handed.rs` | Loads a file with only a handed-over patch's edits marked, through no link and never to edit. |
| `src/reader/reopen.rs` | Shows a worktree's last tabs and named sets, previews one with each tab's version and whether it is still there, and saves, renames, replaces and deletes a set. |
| `src/reader/pick.rs` | Moves a line cursor over a handed-over patch and reports the lines under it, for a note. |
| `src/reader/rescue.rs` | Keeps the draft of a reader ended from outside, and marks a Rex reader that has unsaved edits. |
| `src/reader/sets.rs` | Keeps a worktree's last tabs and named sets in the state folder: paths, full commit IDs, lines and the shown tab, never a file's text. |
| `src/reader/tabs.rs` | Holds the files open in one reader as tabs, draws their strip, asks about every unsaved draft before a tab or the reader closes, remembers the tabs as the worktree's last, and reopens the tabs chosen from a list. |
| `src/reader/tests.rs` | Checks reader text, diffs, paths, pictures, and keys. |
| `src/reader/working_path.rs` | Finds a file from the held project folder without following a final link. |
| `src/host.rs` | Names the computer Milo runs on and says whether its Rex server and command are ones Milo was checked with. |
| `src/review.rs` | Runs `milo review`: a read-only reader over each file of a handed-over patch, its notes, and its floating form for a program with no terminal. |
| `src/review/compose.rs` | Holds the text of a note while it is written: wrapping, the cursor and its keys. |
| `src/review/list.rs` | Lists a review's notes with the text each is copied as. |
| `src/review/note_tests.rs` | Checks what notes hold, what is copied and when, and that the reading position survives. |
| `src/review/notes.rs` | Keeps a review's notes with the lines they quote, and words them for copying. |
| `src/rex.rs` | Calls Rex for setup, floating readers, the listener, and the presence helper's watch. |
| `src/rex/task.rs` | Creates a task's background session, watches for an agent that exits at once, and runs the launcher on another host. |
| `src/rex/attention.rs` | Starts the script that reports terminals needing the person, and shows one pane in Rex after checking its host and block. |
| `src/rex/server.rs` | Asks a Rex server for its version, host ID and connected apps, and starts, forwards and ends another host's settings editor. |
| `src/rex_attention.lua` | Tells the menu's watch which terminals of the local Rex server hold a blocked, failed or finished program status, never its text. |
| `src/rex_presence.lua` | Tells the presence helper whether any Rex app has a session open and, only for the names chosen, which session is in front. |
| `src/rex_reader.lua` | Opens a floating reader in Rex and closes the one it replaces only after the new one exists. |
| `src/rex_reader_test.lua` | Checks that script with a stand-in for Rex. |
| `src/rex_reader_show.lua` | Brings a floating reader forward for a file it was asked to show in a tab, and names it after that file. |
| `src/rex_shell.lua` | Reports the directory of the one shell a managed pane follows and the processes in front of it, and nothing from other terminals. |
| `src/screen.rs` | Stores styled cells and sends changes and pictures to the terminal. |
| `src/setup.rs` | Runs the setup screen: its choices, layout trials, scoped saves, and maintenance. |
| `src/setup/actions.lua` | Defines Rex actions that arrange, add, or close milo panes, name a session for its project, open a file named in a pane's recent output, ask for a new agent task, or open the lists of Reader tabs. |
| `src/setup/actions_test.lua` | Checks those actions with a stand-in for Rex. |
| `src/setup/claude.rs` | Plans changes to the Claude Code status line and tracks ownership. |
| `src/setup/doctor.rs` | Inspects the installation without starting Rex, and repairs or removes the integration with setup's ownership checks. |
| `src/setup/draw.rs` | Draws setup in a full tab or a narrow rail. |
| `src/setup/files.rs` | Checks, replaces, and restores files changed by setup. |
| `src/setup/install.rs` | Coordinates setup saves, Rex reloads, and listener changes. |
| `src/setup/layout.rs` | Reads and writes layout choices and generates Lua files. |
| `src/setup/listener.lua` | Remembers divider changes on managed rails and optionally arranges new shell tabs and titles unnamed ones. |
| `src/setup/listener_0_1_2.lua` | Keeps the listener of releases 0.1.1 and 0.1.2 for the test that saves over it. |
| `src/setup/listener_test.lua` | Checks the new-tab, new-session and tab-title handlers with a stand-in for Rex. |
| `src/setup/lock.rs` | Keeps saves, listeners and presence helpers from overlapping and stops the listener or helper. |
| `src/setup/preview.rs` | Sketches, in proportion, the layout Milo would build around a shell. |
| `src/setup/profiles.rs` | Saves a rail's roles, side, order and proportions globally or for a worktree, never Rex block IDs, and undoes a trial. |
| `src/setup/rex_files.rs` | Plans edits to Rex settings and checks the generated layouts. |
| `src/setup/settings.rs` | Reads and edits the top-level Claude Code status-line setting. |
| `src/setup/transfer.rs` | Exports chosen preferences and layouts as a bundle, plans and applies an import, matches a project by its repository, and takes the last import back. |
| `src/setup/transfer/tests.rs` | Checks what a bundle holds and refuses, project matching, a host changed after a preview, and undo. |
| `src/setup/tests.rs` | Checks the setup screen's frames and clicks, scoped saves, trials, and installation with throwaway settings and a stand-in for Rex. |
| `src/setup/web.rs` | Exposes drafts, scoped saves, and Rex trials to the browser editor. |
| `src/setup/welcome.rs` | Guides a first run without changing settings until the user chooses Save or Skip. |
| `src/syntax.rs` | Scans source text to find spans to colour. |
| `src/syntax/delimiters.rs` | Handles string and comment closers that depend on their openers. |
| `src/syntax/markdown.rs` | Colours Markdown headings, code, links, list marks and quotes. |
| `src/syntax/markup.rs` | Colours HTML and XML tags, and the scripts and styles inside them. |
| `src/syntax/langs.rs` | Defines language rules and maps them to file names and extensions. |
| `src/task.rs` | Runs `milo task new`: checks a request, adds or finds the worktree, starts the agent's session, and tells the Rex action what it can offer. |
| `src/task/harness.rs` | Lists the coding agents a task can start, finds their programs, and gives each its interactive invocation. |
| `src/task/tests.rs` | Checks requests, worktree plans and refusals, another host's reply, and what the action is told. |
| `src/term.rs` | Sets and restores terminal modes and waits for events. |
| `src/titles.rs` | Names Rex tabs and sessions after a Git project and remembers the titles Milo set. |
| `src/tree.rs` | Builds file and folder rows, folds, and changed-file counts. |
| `src/ui.rs` | Shares headings, status styles, icons, and scroll marks. |
| `src/uninstall.rs` | Uninstalls the running copy of Milo once confirmed, not the first `milo` on `PATH`. |
| `src/usage.rs` | Collects the latest limits and today's Claude Code and OpenCode activity, and works out which tools are in use. |
| `src/usage/accounts.rs` | Asks providers about accounts the user turned on, and keeps each figure in its own unit. |
| `src/usage/draw.rs` | Lays out the tools that have a place, with their figures, reset times, and hints. |
| `src/usage/draw/digits.rs` | Draws large figures from text blocks when pictures are unavailable. |
| `src/usage/draw/figure.rs` | Builds coloured PNG figures from the stored masks. |
| `src/usage/files.rs` | Scans session files and bounds how much a refresh reads. |
| `src/usage/marks/Inter-OFL.txt` | Records the licence for the font used in usage figures. |
| `src/usage/marks/LobeHub-LICENSE.txt` | Records the licence of the marks taken from LobeHub Icons. |
| `src/usage/marks/README.md` | Records artwork sources and figure generation instructions. |
| `src/usage/marks/claude.png` | Embeds the Claude mark shown in the pane. |
| `src/usage/marks/claude.svg` | Stores the SVG source for the Claude mark. |
| `src/usage/marks/codex.png` | Embeds the Codex mark shown in the pane. |
| `src/usage/marks/codex.svg` | Wraps the original Codex bitmap in SVG. |
| `src/usage/marks/copilot.png`, `deepseek.png`, `devin.png`, `opencode.png`, `openrouter.png` | Hold the marks the menu app shows for those tools and accounts. |
| `src/usage/marks/figures.alpha` | Stores compressed alpha masks for usage figures. |
| `src/usage/marks/generate-figures.py` | Regenerates the figure masks from the pinned Inter font. |
| `src/usage/http.rs` | Sends one bounded GET through curl with the key on standard input. |
| `src/usage/menu.rs` | Streams the pane's readings as one JSON snapshot per line for the menu app, using global preferences. |
| `src/usage/omp.rs` | Reads saved oh-my-pi limit reports through SQLite. |
| `src/usage/opencode.rs` | Reads today's token and cost totals from OpenCode's session database. |
| `src/usage/signature.rs` | Tells from a database's files whether it was written since the last read. |
| `src/usage/sqlite.rs` | Runs bounded, read-only SQLite queries off the pane loop. |
| `src/usage/status.rs` | Reads Claude Code's saved cache and handles status-line reports. |
| `src/usage/tests.rs` | Shares usage fixtures and checks oh-my-pi ingestion. |
| `src/usage/tests/accounts.rs` | Checks accounts against a loopback provider: units, visibility, keys, limits, and stale answers. |
| `src/usage/tests/accounts/` | Holds replies in the shape each provider documents, with made-up values. |
| `src/usage/tests/activity.rs` | Checks Claude Code's daily token totals across sessions. |
| `src/usage/tests/codex.rs` | Checks Codex session reports, order, and missing data. |
| `src/usage/tests/failures.rs` | Checks each usage source when missing, rotated, locked, stale, or in an unknown shape. |
| `src/usage/tests/idle.rs` | Counts the SQLite reads of an idle pane and how soon a new row shows, at two database sizes. |
| `src/usage/tests/layout.rs` | Checks usage frames at different sizes and times. |
| `src/usage/tests/menu_stream.rs` | Checks that the stream agrees with the pane, follows global preferences, and stops when its reader is gone. |
| `src/usage/tests/opencode/` | Holds the session tables each checked OpenCode release creates. |
| `src/usage/tests/opencode_activity.rs` | Checks OpenCode totals, unknown states, and the pane's row against those tables. |
| `src/usage/tests/status_line.rs` | Checks Claude Code status-line and saved-cache readings. |
| `src/usage/tests/tools.rs` | Checks which tools have a place, and the frame for each combination and choice. |
| `src/usage/tests/unchanged.rs` | Checks when an unchanged database is left unread, and each write, removal, failure and passing of time that reads it again. |
| `src/usage/time.rs` | Parses reported timestamps with enough precision to order them. |
| `src/walk.rs` | Lists files and folders outside Git. |
| `src/watch.rs` | Delivers macOS file-change notifications to the pane loop. |
| `src/web.rs`, `src/web/` | Serves the embedded settings editor on loopback and authenticates its requests. `remote.rs` opens another host's editor here through a Rex forward and ends both. |
| `dashboard/app/`, `dashboard/components/` | Next.js App Router entry point and React settings sections. Run `bun run build` in `dashboard/` after changing the UI, and commit its generated `dashboard/bundle/` alongside the source. Cargo embeds that bundle without installing frontend tooling. |
| `src/when.rs` | Works out local dates and wording for ages and durations. |
| `src/width.rs` | Measures and shortens text in terminal columns. |
| `src/width/data.rs` | Lists the Unicode 17.0.0 ranges of nonspacing marks and wide characters. |
| `src/workflow.rs` | Retargets a pane to the worktree of the shell it was opened beside, when the user chose to follow it, and lends its terminal to the same pane on the host that shell reached with `milo ssh`. |
| `src/ssh.rs` | Runs `milo ssh`: logs in with the person's ssh, holds one connection for the tab, asks the host where Milo is, and starts the shell's and panes' sessions over that connection. |
| `src/ssh/link.rs` | Keeps the private note that tells a tab's panes which host its shell reached, and checks that its `milo ssh` still leads what is in front of that shell. |
| `src/ssh/helper.rs` | Milo's side on the host: says where it is, notes the shell's process and becomes the login shell, and opens a read-only pane that follows that shell. |
| `src/ssh/process.rs` | Names a process by its ID and start time, finds its parents, and reads its folder from the system. |
| `src/ssh/watch.rs` | Follows the folder of a shell on this computer for a pane on the host. |
| `src/ssh/usage.rs` | Labels Usage beside a shell on another host, offers that host's usage on Tab, and labels the host's usage. |
| `tests/alerts.rs` | Starts real workers with disposable Rex and destination stand-ins; checks flaps, crashes, pause during retries, isolated providers, cancellations and notice counts. |
| `tests/alerts/real_rex.rs` | Repeats the disconnect/reconnect scenario with a disposable Rex server, private Unix socket and a loopback destination. With Rex installed, run `cargo test --test alerts real_rex -- --ignored`. |
| `tests/backend_reader.py` | Profiles reader tabs, large handed patches, memory and input during a held Git read in a disposable terminal. |
| `tests/backend_refresh.py` | Counts real Git processes through project discovery, warm refresh and watcher bursts in synthetic worktrees. |
| `tests/bench/report.rs` | Chooses the builds a timing test runs and writes its report beside a base build's. |
| `tests/bench/terminal.rs` | Runs a pane in a terminal for a timing test and finds its frames. |
| `tests/history_bench.rs` | Times listing a small and a large commit's files in History, and Down meanwhile. |
| `tests/history_bench/baseline-fa34ca4.tsv` | Records those times beside that revision's, on the machine named in its header. |
| `tests/install.sh` | Checks both installers with stand-ins for Git, Cargo and curl: which source is built, the macOS and checksum checks, and removal only with a matching receipt. |
| `tests/listener.rs` | Runs listener locking and stopping with real processes and a Rex stand-in. |
| `tests/menu_attention.rs` | Runs `milo menubar attention` against a Rex stand-in: off asks nothing, rows by chosen state, seen, a lost Rex, and a jump's checks. With Rex installed, an ignored test repeats it on a disposable server. |
| `tests/menu-alerts.swift` | Checks when the menu app raises a low-quota notification, with a stand-in for the system. |
| `tests/menu-attention.swift` | Checks the menu app's Needs you choices, wording, watch lifetime and jump reports with stand-ins for the bundled Milo. |
| `tests/menu-badge.swift` | Checks which provider and window the menu bar badge shows. |
| `tests/menu-resources.swift` | Checks system CPU deltas, discontinuities and physical RAM accounting with synthetic counters. |
| `tests/menu-bench.sh` | Builds a throwaway copy of the menu app and times frames and opening the popover in it. |
| `tests/menu-bench.swift` | Feeds that copy made-up frames and reports the times. |
| `tests/menu-bench/baseline-d7be344.tsv` | Records those times for one revision on the machine named in its header. |
| `tests/menu-handoff.swift` | Checks that the menu app hands the browser its private link through an owner-only file. |
| `tests/menubar.sh` | Verifies the built menu app’s signature, exercises its Swift fixtures, then installs, upgrades and removes stand-in apps in a throwaway home without Launch Services. |
| `tests/open_bench.rs` | Times the first frame of the outline, history and reader on an ordinary and a large project, the outline's search, and History's keys during a refresh. |
| `tests/open_bench/baseline-7d2a6f4.tsv` | Records those times beside that revision's, on the machine named in its header. |
| `tests/pane.rs` | Checks pane startup, input, editing, and cleanup through a real terminal. |
| `tests/presence.rs` | Runs `milo presence` and its helper against stand-ins for Discord and Rex, frame by frame. |
| `tests/reader-compare.sh` | Builds another revision and times its reader beside this checkout's. |
| `tests/reader_bench.rs` | Times the reader's first frame and scrolling on generated files. |
| `tests/reader_bench/baseline-6199085.tsv` | Records those times for one revision on the machine named in its header. |
| `tests/relay-container.sh` | Runs the relay image without a network: no start without its volume, no root, and pairing only for the canonical host. |
| `tests/rex_hosts.rs` | Follows Doctor, a forwarded settings editor and a settings bundle across two disposable Rex servers, and checks the first host's files stay as they were. |
| `tests/rex_lab.rs` | Exercises Rex layouts and presence on a disposable server, edits files in floating reader tabs, opens a floating review of an agent-supplied patch, copies review notes without touching other panes, starts agent tasks from the command line and the Rex action, and opens files named in a pane's output. |
| `tests/settings-bench.py` | Times settings requests, a competing save and first state beside a responding Rex stand-in. |
| `tests/task.rs` | Runs `milo task new` beside stand-ins for Rex and the four agents: the session asked for, literal prompts, refusals that create nothing, and what a late failure reports. |
| `tests/usage_idle/baseline-fa34ca4.tsv` | Records what `src/usage/tests/idle.rs` counted, in both journal modes, beside that revision's counts. |
| `tests/rex_lab/app.lua` | Stands in for a connected Rex app on that server. |
| `tests/rex_lab/view.lua` | Prints that server's sessions, tabs, and panes for the test. |
| `tests/rex_lab/tab_sets.rs` | Saves, reopens, replaces, renames and deletes Reader tab sets in panes of that server, in two worktrees, over a draft and with a deleted file and a dropped commit. |
| `tests/rex_lab/ssh.rs` | Logs in with `milo ssh` to a disposable sshd on the loopback address and follows the shell from B to C in panes of that server, beside an unchanged second tab, a reader draft and Usage, through a logout, a dropped connection and a host without Milo. |
| `tests/web.rs` | Runs `milo web` beside a Rex stand-in that answers late: the link first, the state only once whole, a clean end in between. |
| `tests/web_bench.rs` | Times the settings link and the first state, outside Rex and beside a slow one. |
| `tests/web_bench/baseline-fa34ca4.tsv` | Records those times beside that revision's, on the machine named in its header. |
| `tests/web-smoke.cjs` | Saves layout and usage tool choices, turns Discord presence, its names and its application on and off, and exports, previews, imports and undoes a settings bundle under Hosts in the browser editor of a built binary in Chromium, then closes it. |
