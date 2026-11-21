# Using the panes

Run the panes in a Unix terminal with 256 colours. Outline and history start in the current directory unless you give them a path. Inside a Git repository they use its root, even when you start in a subfolder. In a plain folder, outline shows just the files and history says it is not a Git project. To outline a directory named like a command, give its path explicitly: `milo ./history`.

If you initialize Git in that folder, open panes pick up the repository and its saved worktree choices without restarting. Switching branches does not erase remembered folder folds or mark the newly visible timeline as newly arrived.

In Follow mode, Outline and History use the local shell's working directory, even with a coding agent in the foreground and no directory report from the prompt. If the directory is missing, the pane says **Waiting for shell**; if it cannot open the reported folder, it says **Shell folder unavailable**. It keeps showing the last project and names it when space permits. Neither message changes your project preference to Pin.
If Git temporarily cannot identify the shell's project, the pane keeps the last project and retries without another `cd`.
If shell tracking itself stops, the notice changes to **Shell tracking unavailable**. Reopen the pane to reconnect.

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

In a plain folder, Outline lists the top level and the folders you expand. A partial-listing warning means Milo reached the 50,000-entry limit or couldn't read every entry. In both plain folders and Git worktrees, Outline warns and omits names that aren't valid UTF-8 rather than showing a name that could open a different file.

If Git holds the index lock for more than a moment, Outline, History and the working-file Reader show `Waiting for Git` and pause automatic updates. They update when the lock clears; Milo never removes it.

Untracked files appear before Milo counts their lines. In a wide outline, `?` means the count is not ready, not zero. The outline counts the files in view in small batches as you scroll; a folder shows `?` while any of its new files are still uncounted.

If filtering or folding hides the selected file, Outline keeps the cursor on its nearest visible folder. Press `r` to refresh counts, including offscreen files.

## Comparing commits

History counts commits to push against the branch's tracked remote branch. `no branch tracked` means there is no tracked branch, not that all commits have been pushed. Milo can still use the remote's default branch to mark commits for comparison. `no remote` means the project has no remote configured.

Expand a commit to see its seven-character ID beside the file count. Narrow panes keep that space for the subject instead.

History lists the latest 300 commits. If there are older ones, `G` takes you to the oldest displayed commit, with `Older commits not shown.` underneath when there is room.

A shallow clone has only part of the project's history. If you expand a commit whose parent is beyond the shallow boundary, History says `Shallow history: parent unavailable.` It does not show every file as new. After fetching more history, press `r` to reload. A genuine first commit still shows its added files.

## Reading files

Open a file from the outline or history with `Enter` or a double-click. Inside Rex it opens in a floating reader. Opening another file from that pane replaces only its previous reader, even after following the shell to another project. Any shell you split beside the reader stays open. Outside Rex the reader takes over the pane; `q` brings you back.
History catches up with repository changes when you return from a file, keeping that file selected if it is still in the timeline.

On iPhone, enable [Rex → Settings → Keyboard → Open Keyboard Manually](https://www.superlogical.com/rex/docs/reference/ios-settings#setting-keyboard-open-keyboard-manually) to browse without raising the keyboard. Tap a folder to expand it, tap a file to select it, and double-tap to open it. This setting affects **all** terminal panes on that iPhone, not just Outline. Tap the keyboard button to type in a shell. If the keyboard reopens when you return to a terminal, turn off **Keep Keyboard Open** in the same settings.

For a quick look at the tree, use **Milo: Peek Outline** in Rex. It reuses one floating outline per tab. Close the peek with `Esc` or `q` rather than removing a permanent pane.

`milo view FILE` reads a working file. `milo view FILE --commit ID` reads the version from a commit. Give the file's path as it was in that commit, relative to your current directory or as an absolute path. For example, from `src`, use `milo view main.rs --commit ID`. Milo ignores `.` components and repeated slashes, but rejects `..`. Use a full or abbreviated hexadecimal commit ID, not a branch name or `HEAD`.

Committed-file lookup requires a UTF-8 path. A working file with a non-UTF-8 name can still be read directly, without a Git diff.

The reader shows the whole file with changed lines marked, and colours the languages below. Working files and files at a commit are recognised the same way, by file name, in any letter case. It uses a small scanner, not a full parser. Other files show as plain text, as does a script whose `#!` line names a different language from its extension. The working-file reader will not open a file outside the project through a linked folder.

### Languages

| Language | Files | Coloured across lines |
| --- | --- | --- |
| Rust | `.rs` | Nested block comments, strings, raw strings |
| Python | `.py`, `.pyi` | Triple-quoted strings |
| JavaScript, TypeScript | `.js`, `.mjs`, `.cjs`, `.jsx`, `.ts`, `.mts`, `.cts`, `.tsx` | Block comments, template strings |
| Go | `.go` | Block comments, raw strings |
| Java | `.java` | Block comments, text blocks |
| Kotlin | `.kt`, `.kts` | Nested block comments, raw strings |
| Scala | `.scala`, `.sc`, `.sbt` | Nested block comments, raw strings |
| Dart | `.dart` | Nested block comments, triple-quoted strings |
| C, C++ | `.c`, `.h`, `.cc`, `.cpp`, `.cxx`, `.hpp`, `.hh` | Block comments, C++ raw strings |
| C# | `.cs` | Block comments, verbatim and raw strings |
| Swift | `.swift` | Nested block comments, multiline and raw strings |
| PHP | `.php` | Block comments, strings, heredocs |
| Ruby | `.rb`, `.rake`, `.gemspec`, `.ru`, `Gemfile`, `Rakefile` | `=begin` comments, heredocs |
| Elixir | `.ex`, `.exs` | Triple-quoted strings |
| Lua | `.lua` | Long comments and long strings |
| Shell | `.sh`, `.bash`, `.zsh` | Quoted strings |
| SQL | `.sql` | Block comments |
| HTML, SVG, Vue, Svelte | `.html`, `.htm`, `.xhtml`, `.svg`, `.vue`, `.svelte` | Comments, tags, attribute values, and the JavaScript and CSS inside `script` and `style` |
| XML | `.xml` | Comments, tags, attribute values |
| CSS, SCSS, Less | `.css`, `.scss`, `.less` | Block comments |
| Markdown | `.md`, `.markdown`, `.mdx` | Fenced code blocks, comments |
| JSON | `.json`, `.jsonc` | Block comments |
| YAML | `.yaml`, `.yml` | Quoted strings |
| TOML | `.toml` | Multiline strings |
| Terraform, HCL | `.tf`, `.tfvars`, `.hcl` | Block comments, heredocs |
| Dockerfile | `Dockerfile`, `.dockerfile` | |
| Makefile | `Makefile`, `GNUmakefile`, `.mk` | |

Every language has its comments, strings, numbers and reserved words coloured. Markup has tag names, attributes and entities coloured instead, and text between tags is left plain. Markdown has headings, list marks, links, inline code, code blocks and quotes coloured; emphasis and tables stay plain, and a code block is one colour whatever language it holds. Ruby and Elixir strings in quotes are coloured one line at a time. Colour never changes the text shown, and the marks for added, removed and changed words are drawn over it.

When a removed line and the added line that replaces it look alike, the reader highlights the words that changed. A word is a run of letters, digits and underscores in any script; text written without spaces, such as Chinese or Japanese, is compared one character at a time. Highlighting only adds emphasis: the text shown is the same with or without it. Lines that share too little, lines with many separate changes and lines whose edits span more than about a hundred words keep only their red or green background, as do the remaining lines of an unusually large diff.

The reader also labels hidden differences next to the text: `[tab]` versus `[4 spaces]`, `[LF]` versus `[CRLF]`, or a mark such as `[zero-width space U+200B]`. It highlights the character affected by a changed mark, including a combining accent. Ordinary edits do not label unchanged whitespace or marks. Use the horizontal scroll keys to reach labels on long lines.

Press `n` or `N` to jump to the next or previous change. The Reader briefly shows your position (for example, `Change 2 of 5`) or `No changes` if there is nothing to jump to. Navigation wraps in both directions.

## Editing files

The reader only reads until you ask it to edit. Press `e`, or open the file with `milo view FILE --edit`, and the file's text becomes a draft with a cursor. It works the same in the reader Outline opens in its own pane, in the floating reader in Rex and in `milo view`. The last row lists the main keys, `Ctrl-G` opens help, and [keys and marks](keys.md#editing-in-the-reader) has the whole list.

The draft is yours alone until you save: the file on disk is not touched by typing, pasting, undoing or formatting. While you edit, the reader shows the draft with its code colours and no change marks. When you stop, it shows the file again at the same lines, with a fresh diff, and Outline still has the file selected.

The editor is for small changes. It has one cursor and no selection or search, and it edits a file that exists: it does not make a new one. The cursor moves one character at a time, so an accent written as a separate mark takes a key press of its own.

### What can be edited

A working file can be edited when it is a regular file, is reached without passing through a link, holds UTF-8 text with no NUL bytes, is no larger than 2 MiB and can be written by you. A byte-order mark is allowed. Anything else stays read-only. The last row then never has `e edit`, and `e` or help says why. A narrow pane can also lack room for `e edit` on a file that can be edited; `e` works all the same.

| It says | Because |
| --- | --- |
| `this is a commit` | The reader shows the file as of a commit, from History or `--commit` |
| `this is a link` | The file is a link; the reader shows where it points |
| `reached through a link` | A folder on the way to the file is a link |
| `the project moved` | The project's folder was renamed or replaced after the reader opened |
| `outside the project` | The path leads out of the project |
| `the file is gone` | The file was removed; the reader may still show its removed lines |
| `not a regular file` | It is a folder, a device or a pipe |
| `not text` | It holds NUL bytes or is not UTF-8, which a save could not keep, or Git treats it as binary |
| `shown as a picture` | The reader is showing it as a picture |
| `it cannot be shown` | The reader shows a note in its place, such as `Too large to show.` |
| `too large` | It is larger than 2 MiB, though the reader can show its changes |
| `no write permission` | You may read it but not write it |
| `it cannot be read` | The file or a folder on the way could not be opened |

A draft cannot grow past 2 MiB either: a key or paste that would take it there is refused.

### Saving

`Ctrl-S` writes the draft into the file that was read. Milo does not put a new file in its place, so the file keeps its permissions, owner, other names and attributes. The bytes you did not edit stay the same: a byte-order mark is kept, every line keeps its own ending when a file mixes `LF` and `CRLF`, and a file with no final line break is not given one. A new line takes the ending most of the file uses.

Before it writes, milo opens the file again and reads it. Nothing is overwritten unexpectedly:

- If the file no longer holds what your edit started from, or another file has been put in its place, nothing is written and the pane asks `Changed on disk: o overwrite · Esc back`. Only `o` writes your draft over it.
- If the file is gone, it is not made again.
- If the file has become a link, or a folder on the way has become a link or moved, the save is refused. Milo never writes through a link.
- If you no longer have permission to write, the save is refused.

In each case the draft stays open, and the last row says what happened. If the write itself fails part of the way, milo puts the old contents back where it can and says whether it could.

A save writes in place, not through a temporary file; that is what keeps the file's links and attributes. A crash or power loss in the middle of a save can therefore leave a partly written file.

### Leaving unsaved changes

`Esc`, `Ctrl-Q` and `Ctrl-C` stop editing. With unsaved changes they ask `Unsaved edits: s save · d discard · Esc back` first. `q` is a letter while you edit. If the terminal has stopped taking output, `Ctrl-C` ends the reader at once, as it always has; the draft is then kept as described next.

A reader can also be ended from outside: its terminal is closed, the process is asked to stop, or Rex closes its pane. If it has unsaved changes, milo writes the draft to the `drafts` folder of its state folder (`~/.local/state/milo`, or `milo` under an absolute `$XDG_STATE_HOME`), readable only by you, and prints one line about it once the terminal is yours again. The project is not written to. If milo has no state folder, because neither that variable nor `HOME` names one, the line says the edits were lost.

The next time you edit that file, the pane asks `Kept draft found: r restore · d delete · Esc later`. Restoring puts the kept text in place of the file's as one change, so `Ctrl-Z` shows the file as it is now. The kept copy is removed when you delete it or save the restored text, not before: saving other edits, or undoing the restore and then saving or leaving, keeps it. One draft is kept for each file, so a reader that is ended again with unsaved changes to that file replaces it. A process that is killed outright, or a machine that loses power, keeps nothing.

In Rex, opening another file from Outline or History replaces the floating reader. While that reader has unsaved changes, the pane says `The open reader has unsaved edits.` and leaves it alone.

## Formatting

While editing, `Ctrl-F` gives the draft to a formatter and puts its answer in place of the draft as one change. The file is not touched: look the result over, press `Ctrl-Z` to take it back, or discard it with the draft. Only `Ctrl-S` writes it. Keys pressed after `Ctrl-F` wait for the formatter and act on what it answered. If the formatter fails, the draft is left exactly as it was and the last row shows the first line of what the formatter said.

Milo runs a formatter you have installed. It does not install one, download one or run one through a package runner. For each kind of file, the first of these found is used:

| Files | Formatter | Run as |
| --- | --- | --- |
| `.rs` | rustfmt | `rustfmt --edition E --emit stdout` |
| `.go` | gofmt | `gofmt` |
| `.py`, `.pyi` | ruff, then black | `ruff format --no-cache --stdin-filename ./NAME -`<br>`black --quiet --stdin-filename ./NAME -` |
| `.js`, `.mjs`, `.cjs`, `.jsx`, `.ts`, `.mts`, `.cts`, `.tsx`, `.json`, `.jsonc`, `.css` | prettier, then biome, then deno | `prettier --stdin-filepath ./NAME`<br>`biome format --stdin-file-path ./NAME`<br>`deno fmt --ext EXT -` |
| `.scss`, `.less`, `.html`, `.md`, `.yaml`, `.yml` | prettier, then deno | As above |
| `.vue` | prettier | As above |
| `.toml` | taplo | `taplo format --stdin-filepath ./NAME -` |
| `.c`, `.h`, `.cc`, `.cpp`, `.cxx`, `.hpp`, `.hh` | clang-format | `clang-format --assume-filename=NAME` |
| `.sh`, `.bash` | shfmt | `shfmt --filename ./NAME` |
| `.lua` | stylua | `stylua --stdin-filepath ./NAME -` |
| `.swift` | swift-format | `swift-format format --assume-filename ./NAME` |
| `.tf`, `.tfvars` | terraform | `terraform fmt -no-color -` |

`NAME` is the file's name and `EXT` its extension. For Rust, `E` is the edition in the nearest `Cargo.toml` at or above the file within the project, or 2021; the option is left out when a `rustfmt.toml` there sets an edition. Other kinds of file, including some the reader colours such as Kotlin, Ruby, Java, C#, PHP and SQL, have no formatter: `Ctrl-F` says `No formatter for this kind of file.` and the last row does not offer it.

How a formatter is run:

- **Only when you press `Ctrl-F`.** Opening, reading and editing a file never start one. Help looks for the program so that it can name it, and does not run it.
- **By name, from where you installed it.** The program is looked for in the folders of `PATH` that are written in full. A folder written relative to where milo runs, such as `.` or `node_modules/.bin`, is skipped, so a project cannot supply the program. The formatter's own `PATH` has those folders taken out too, so a launcher that starts `node` cannot find one in the project. If none is installed, the last row says which to install, for example `Not installed: ruff or black.`
- **On the draft, not the file.** The draft goes in on standard input and the formatted text comes back on standard output. The formatter is not asked to write anything.
- **In the file's folder, with the file's name.** That is how a formatter finds the project's settings, such as `rustfmt.toml`, `pyproject.toml` or `.prettierrc`. Those settings belong to the project, and some formatters load code named in them: Prettier loads plugins and JavaScript settings files. Format files only in a project you trust, as you would in an editor.
- **For at most ten seconds.** After that the formatter and the programs it started are stopped. A formatter that leaves a helper running with its output still open is waited for just as long, and its answer is not used. An answer that is empty, is not UTF-8 text or is larger than 2 MiB is refused.
- **Without downloads.** Milo installs nothing. A `rustfmt` installed by rustup is told not to fetch the toolchain a project's `rust-toolchain.toml` names: if that toolchain is missing, the last row says so.
- **Keeping the file's line endings.** If the file uses `CRLF` and the formatter answers with `LF`, the draft gets `CRLF` back. A byte-order mark is not given to the formatter and is kept.

The pane shows `Formatting with NAME…` and does not redraw until the formatter is done.

## Reading pictures

The working-file reader can show PNG, JPEG, GIF, WebP, HEIC and HEIF, TIFF, BMP, ICO and ICNS, SVG, PSD and the first page of a PDF, where conversion is supported. It fits the picture without changing its proportions. On macOS, `/usr/bin/sips` converts other formats and resizes large PNGs; other systems show only PNGs that do not need conversion. Ordinary text still shows as text if a file has a picture extension. At a commit, the reader shows the stored file as text or a note, not as a picture.

Pictures need a terminal that supports the kitty graphics protocol, such as Rex. Milo asks the terminal whether it can draw pictures and shows a message if it cannot.
