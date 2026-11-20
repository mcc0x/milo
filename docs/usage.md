# Usage limits

`milo usage` shows how much of a reported limit is **left**, not how much has been used. The two-row rail gives each provider one line for its most constrained window. When there's room, it adds the reset countdown for a recent reading or when an older reading was last seen. Wide panes can show large figures and provider marks. Choose large figures in setup if you prefer them in a taller rail too.

A reset time is a prediction from the last report, not a new measurement. Once it passes, Milo marks the reset as elapsed until a new reading arrives. No source means **unknown**, never zero or a fresh 100%. Run `milo doctor` to see which local source supplied each provider's reading.

## Where the figures come from

| Source | What milo reads |
| --- | --- |
| oh-my-pi | Limits the harness saves for Codex and Claude in `~/.omp/agent/agent.db` (or `$PI_CODING_AGENT_DIR/agent.db`). Milo reads only the usage data, using `/usr/bin/sqlite3` in read-only mode. Automatic reads are at least 20 seconds apart per pane. |
| Codex | Limit reports in recent Codex session files under `~/.codex/sessions` |
| Claude Code | Its own saved limits in `~/.claude.json`, or limits sent through the status line if you turn it on in setup. |
| OpenCode | Today's activity, not limits, from its session database. See [OpenCode activity](#opencode-activity). |

For each tool, milo uses the newest report it can read. When there is room, it also shows **tokens today** from Claude Code's local session files under `~/.claude/projects`. This is local activity since midnight, not a limit. It counts new input, output and cache writes, not cache reads. A `+` after the number means the total may be incomplete.

Milo checks the tails of recent Codex logs, then resumes near the previous end as those logs grow. Ordinary session activity does not clear the last limit report. A new or replaced log, a truncation, or a large burst of new data calls for another bounded tail check.

While a Codex log has been active in the last five minutes, Milo checks known logs about every two seconds, even if the writer keeps the file open. With a native watcher, Milo groups notifications so it doesn’t walk the session directories on every write. Without a watcher, it looks for new Codex and Claude Code sessions on the roughly two-second fallback checks. Press `r` to check immediately.

Milo labels reports as older after five minutes, or ten minutes for oh-my-pi to allow for its provider refresh cadence. This uses the time the provider's window was last observed, not the time Milo read the file.

**Show Claude Code limits** in `milo setup` adds a status line if you do not already have one. Claude Code calls `milo claude-status` to show a short line and save the reported limits to `~/.local/state/milo/claude-limits.json` (or under an absolute `$XDG_STATE_HOME`). It does not save prompts or replies. You do not need to enable it if Claude Code or oh-my-pi already saves a report.

An unchanged status-line reading confirms the same window, but Milo saves that confirmation at most once a minute per window. A lower figure for the same window or an older reset does not replace or refresh the saved figure.

## OpenCode activity

OpenCode saves no limits on this machine, so Milo shows none for it. What it does save is each step's token counts and its own cost estimate. On a pane two rows taller than the key hints need, Milo adds one line above them, such as `OpenCode · 48.2k tokens today · $1.37 est.`. A narrower pane drops the cost first, then shortens the rest. The two-row rail does not show it.

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

- Make the pane taller and wider to see labels, reset times and hints. Where the terminal can show pictures, including in Rex, milo draws the logos and large figures as pictures. Otherwise it draws with text. Short panes use text either way.
- Press `r` to check local data immediately. Milo reuses any oh-my-pi check already in progress and updates the pane when it finishes. Existing figures stay visible; `checking` means a lookup is in progress and there is no figure yet. Reload does not ask a provider for a fresh reading.
- `no sessions found` means no Codex sessions were found; `no limits to show` means there are sessions but no usable limits, or that a saved report has no windows. `no saved limits found` means milo has no saved Claude Code reading. A narrow pane may shorten these hints.
- For oh-my-pi, check that `/usr/bin/sqlite3` is available and the harness has saved a report. For Claude Code, see [setup](setup.md#claude-code-limits) if neither saved source has a reading.
