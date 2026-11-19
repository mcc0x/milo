# Usage limits

`milo usage` shows how much of each limit has been **used**, not how much is left. For example, `5h 31%` means 31% of the five-hour window has been used. Milo shows only the windows reported for the account. When there is room, it also shows their reset times, the age of the reading and any limit for a model's week, such as `Opus week` as a third figure. After a window's reset time it shows `0%` and, when there is room, `reset` until a new reading arrives. A dash means a tool has no figure to show, not that it used zero.

## Where the figures come from

| Source | What milo reads |
| --- | --- |
| oh-my-pi | Limits the harness saves for Codex and Claude in `~/.omp/agent/agent.db` (or `$PI_CODING_AGENT_DIR/agent.db`). Milo reads only the usage data, using `/usr/bin/sqlite3` in read-only mode at most once every 20 seconds per pane. |
| Codex | Limit reports in recent Codex session files under `~/.codex/sessions` |
| Claude Code | Its own saved limits in `~/.claude.json`, or limits sent through the status line if you turn it on in setup. |

For each tool, milo uses the newest report it can read. When there is room, it also shows **tokens today** from Claude Code's local session files under `~/.claude/projects`. This is local activity since midnight, not a limit. It counts new input, output and cache writes, not cache reads. A `+` after the number means the total may be incomplete.

**Show Claude Code limits** in `milo setup` adds a status line if you do not already have one. Claude Code calls `milo claude-status` to show a short line and save the reported limits to `~/.local/state/milo/claude-limits.json` (or under an absolute `$XDG_STATE_HOME`). It does not save prompts or replies. You do not need to enable it if Claude Code or oh-my-pi already saves a report.

## What milo does not do

The usage pane reads local data. It does not call the providers, run their tools, sign in, read credentials or guess limits from token counts. Figures can be old or missing if no tool has saved a report on this machine recently. The status-line helper writes its own limit readings, not the session logs.

## If a figure is missing

- Make the pane taller and wider to see labels, reset times and hints. Where the terminal can show pictures, including in Rex, milo draws the logos and large figures as pictures. Otherwise it draws with text. Short panes use text either way.
- Press `r` to check local data again. It does not bypass the 20-second wait between oh-my-pi reads or ask a provider for a fresh reading.
- `no sessions found` means no Codex sessions were found; `no limits to show` means there are sessions but no usable limits, or that a saved report has no windows. `no saved limits found` means milo has no saved Claude Code reading. A narrow pane may shorten these hints.
- For oh-my-pi, check that `/usr/bin/sqlite3` is available and the harness has saved a report. For Claude Code, see [setup](setup.md#claude-code-limits) if neither saved source has a reading.
