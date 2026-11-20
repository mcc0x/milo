# Usage limits

`milo usage` shows how much of a reported limit is **left**, not how much has been used. It shows the tools that have something to show, and gives the whole pane to them. The two-row rail gives each of two tools one line for its most constrained window, and a tool on its own both rows. When there's room, it adds the reset countdown for a recent reading or when an older reading was last seen. Wide panes can show large figures and provider marks. Choose large figures in setup if you prefer them in a taller rail too.

A reset time is a prediction from the last report, not a new measurement. Once it passes, Milo marks the reset as elapsed until a new reading arrives. No source means **unknown**, never zero or a fresh 100%, and a tool with nothing known is left out unless you ask to always see it. Run `milo doctor` to see which local source supplied each tool's reading.

## macOS menu bar (local build)

With a menu-enabled CLI on macOS 13 or later, run `milo menubar install` to install and open `~/Applications/Milo Usage.app`. Run it again to reopen or upgrade. `milo menubar uninstall` stops the app, unregisters launch at login if enabled, and removes only the app. Neither command needs sudo or a download.

You can also manage the menu app from Home (`milo`), or under **Workflow** in terminal Settings (`milo setup`) and web Settings (`milo web`). Install opens the app. When it’s installed, the control offers removal and asks for confirmation. Removing it keeps the Milo CLI, your settings, and any unsaved Settings draft.

For the local preview, build with `sh macos/build-menu.sh` and use `target/local-menu/milo` as your CLI. The released CLI does not yet include the app. The local builder captures supported provider/config directory overrides from your shell, so rebuild if those paths change and do not distribute a personalized build. The app includes its own reader and does not require Rex.

Click the menu bar item to see the same local usage windows as the pane. The menu uses **global** provider visibility, not the preferences of whichever worktree happens to be focused. Automatic display picks the lowest fresh remaining percentage; when none is fresh it shows just Milo. Options lets you pin a provider, show only the mascot, or opt into launch at login. Quitting the app also stops its reader.

## Which tools appear

Milo gives a tool a place once it has a figure, and no place before that. If you use Codex and Claude Code but not OpenCode, the pane has two places and nothing for OpenCode: no label, no dash and no empty row. Each tool has its own choice under **Appearance** in `milo setup` and `milo web`, saved globally or for a worktree like the other pane preferences. Reopen the pane to apply a change.

| Choice | What the pane does |
| --- | --- |
| Auto, the default | Gives the tool a place while it has a figure saved on this Mac: a limit window for Codex or Claude Code, or tokens OpenCode recorded today. Without one the tool takes no room. |
| Always show | Keeps the tool's place, and says what is unknown and what to do next, such as `Claude · no source` and `Run milo setup`. It never fills in a number. A pane too small for every place still gives its rows to limits first. |
| Hide | Never shows the tool and does not look for it: its folders are not walked or watched, and its database is not opened. With Codex and Claude Code both hidden, oh-my-pi's database is not opened either. |

Whether a tool is in use here is read from files the tool itself saved. Milo does not run the tool, read its sign-in or use the network to find out.

| Tool | Found when |
| --- | --- |
| Codex | A session file `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` has something in it, or oh-my-pi has saved a Codex report. |
| Claude Code | `~/.claude.json` has something in it, or a session written today is under `~/.claude/projects`, or the status line or oh-my-pi has saved a limit report. |
| OpenCode | The file where it keeps its database is a SQLite database. |

Empty folders, empty files, another kind of file under the same name and a Claude Code directory that holds only settings do not count. Links to individual sources or within the session trees are not followed. A report oh-my-pi saved for a tool does count, even when nothing else of the tool is on disk. The looks are the bounded reads described under [Where the figures come from](#where-the-figures-come-from).

Milo uses `$CODEX_HOME/sessions` instead of `~/.codex/sessions` when Codex has an explicit home. With `CLAUDE_CONFIG_DIR` set, it looks for `.claude.json`, `projects/` and `settings.json` inside that directory, not in the default profile. Run Milo and Claude Code with the same override to see and configure that profile's usage.

A tool that is found but has no figure yet stays out of the way of the tools that have one. When no tool has a figure, the pane shows one quiet block instead of empty places: what each found tool is waiting for, such as `Claude · no saved limits found` and `Run milo setup`, or `No usage to show yet` and the tools Milo looks for when it found none.

The frame closes up around what is shown:

- **Two tools with limits** share the pane: side by side when it is wide, one above the other on a rail.
- **One tool with limits** has the pane to itself. A rail lists its other windows under the tightest one, and a wide pane keeps its windows together at the left.
- **OpenCode with one of them** takes the second place: the other half of a wide pane, or the lower half of a rail.
- **OpenCode with both** has one row beneath them, from the third row of a rail or in a wide pane at least nine rows tall. Two rows go to the two limits.
- **OpenCode alone** fills the two-row rail with today's tokens, then its cost and steps. A larger pane draws the tokens large.

`milo doctor` follows the same rule. It reports a tool that was found or is always shown, and says nothing of one that is absent or hidden beyond the choice itself.

## Where the figures come from

| Source | What milo reads |
| --- | --- |
| oh-my-pi | Limits the harness saves for Codex and Claude in `~/.omp/agent/agent.db` (or `$PI_CODING_AGENT_DIR/agent.db`). Milo reads only the usage data, using `/usr/bin/sqlite3` in read-only mode. Automatic reads are at least 20 seconds apart per pane. |
| Codex | Limit reports in recent Codex session files under `~/.codex/sessions` |
| Claude Code | Its own saved limits in `~/.claude.json`, or limits sent through the status line if you turn it on in setup. |
| OpenCode | Today's activity, not limits, from its session database. See [OpenCode activity](#opencode-activity). |

For each tool, milo uses the newest report it can read. When there is room, it also shows **tokens today** from Claude Code's local session files under `~/.claude/projects`. This is local activity since midnight, not a limit. It counts new input, output and cache writes, not cache reads. A `+` after the number means the total may be incomplete.

A report dated more than five minutes ahead of this machine's clock is not used, whichever source it came from. Otherwise one report written while a clock was wrong would outrank every later one. Milo shows the newest of the remaining sources instead. A deferred Codex or Claude Code report becomes eligible as the clock catches up, even if the source file has not changed. For the status-line save, the next report from Claude Code replaces the misdated file.

Milo checks the tails of recent Codex logs, then resumes near the previous end as those logs grow. Ordinary session activity does not clear the last limit report. A new or replaced log, a truncation, or a large burst of new data calls for another bounded tail check.

While a Codex log has been active in the last five minutes, Milo checks known logs about every two seconds, even if the writer keeps the file open. With a native watcher, Milo groups notifications so it doesn’t walk the session directories on every write. Without a watcher, it looks for new Codex and Claude Code sessions on the roughly two-second fallback checks. Press `r` to check immediately.

Milo labels reports as older after five minutes, or ten minutes for oh-my-pi to allow for its provider refresh cadence. This uses the time the provider's window was last observed, not the time Milo read the file.

**Show Claude Code limits** in `milo setup` adds a status line if you do not already have one. Claude Code calls `milo claude-status` to show a short line and save the reported limits to `~/.local/state/milo/claude-limits.json` (or under an absolute `$XDG_STATE_HOME`). It does not save prompts or replies. You do not need to enable it if Claude Code or oh-my-pi already saves a report.
Each explicit `CLAUDE_CONFIG_DIR` has its own `claude-limits.json` under Milo's `claude-profiles/` state directory, so a reading from another profile is not used as a fallback.

An unchanged status-line reading confirms the same window, but Milo saves that confirmation at most once a minute per window. A lower figure for the same window or an older reset does not replace or refresh the saved figure.
If an update leaves out the reset time, it can update a nondecreasing figure without discarding the last known reset.

## OpenCode activity

OpenCode saves no limits on this machine, so Milo shows none for it. What it does save is each step's token counts and its own cost estimate. Beneath two tools with limits Milo gives it one line, such as `OpenCode · 48.2k tokens today · $1.37 est.`. A narrower pane drops the cost first, then shortens the rest. Beside one tool, or alone, it also shows the number of steps, when the last one was recorded and the cache reads. [Which tools appear](#which-tools-appear) says where it goes at each size.

| Figure | What it means |
| --- | --- |
| Tokens today | New input, output, reasoning and cache writes of the assistant steps OpenCode recorded since local midnight, in every project. Cache reads are left out, as they are for Claude Code. It is activity, not a share of any limit. |
| Cost | The sum OpenCode recorded for those steps, in US dollars. It is OpenCode's estimate from model prices, not a bill: a subscription or an unpriced model records zero, and Milo shows nothing rather than `$0`. A `+` means some steps recorded no cost. |
| Limits | Unknown. OpenCode asks its own servers for allowances and budgets when you open them, and does not save the answer. Milo does not ask and does not use OpenCode's saved sign-in. |

Milo reads `~/.local/share/opencode/opencode.db`, or the same file under an absolute `$XDG_DATA_HOME`, or the file `$OPENCODE_DB` names. It uses `/usr/bin/sqlite3` in read-only mode for at most a second, at least 20 seconds apart. The queries return sums and times only: message text never leaves the database, and the tables that hold OpenCode's sign-in are not read. The line can lag OpenCode by about half a minute; press `r` to read now.

The figures were checked against databases written by OpenCode 1.2.0, 1.19.6, 2.2.0 and 2.3.2. They match `opencode stats --days 0`, except that Milo does not count a forked session's copied steps again on the day OpenCode 1 is upgraded to 2. Milo shows nothing, and `milo doctor` reports OpenCode's activity as unknown, when the database has other tables, when a step from today is malformed, when one of today's sessions was created by a version other than 1.x or 2.x, or when OpenCode 1 is still writing to a database OpenCode 2 has upgraded. Title requests are not counted, nor are OpenCode 2's compaction requests, because OpenCode does not record them as steps. [Usage sources](usage-sources.md) has the evidence, the limits of this reading, and why Amp and Gemini CLI are not read.

## What milo does not do

The usage pane reads local data. It does not call the providers, run their tools, sign in, read credentials, guess limits from token counts or work out a cost from them. Figures can be old or missing if no tool has saved a report on this machine recently. The status-line helper writes its own limit readings, not the session logs.

## If a figure is missing

- A tool that is not there at all has no figure saved on this Mac, or is hidden. Run `milo doctor` to see what Milo found, or choose **Always show** for it in setup to keep its place and see what it is waiting for.
- Make the pane taller and wider to see labels, reset times and hints. Where the terminal can show pictures, including in Rex, milo draws the logos and large figures as pictures. Otherwise it draws with text. Short panes use text either way.
- Press `r` to check local data immediately. Milo reuses any oh-my-pi check already in progress and updates the pane when it finishes. Existing figures stay visible; `checking` means a lookup is in progress and there is no figure yet. Reload does not ask a provider for a fresh reading.
- A tool that is always shown, or found while no tool has a figure, says why it has none. `no sessions found` means no Codex sessions were found; `no limits to show` means there are sessions but no usable limits, or that a saved report has no windows. `no saved limits found` means milo has no saved Claude Code reading. `no tokens recorded today` means OpenCode's database has nothing from today. A narrow pane may shorten these hints.
- For oh-my-pi, check that `/usr/bin/sqlite3` is available and the harness has saved a report. For Claude Code, see [setup](setup.md#claude-code-limits) if neither saved source has a reading.
