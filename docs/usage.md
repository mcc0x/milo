# Usage limits

`milo usage` shows how much of each reported limit has been **used**, not how much is left. For example, `5h 31%` means 31% of the five-hour window has been used. The pane can also show a weekly limit, time until reset and the age of the reading. After a reported reset time, the window shows `0%` and, when there is room, `reset` until a new reading arrives. A dash means the limit is missing, not zero.

## Where the figures come from

| Source | What milo reads |
| --- | --- |
| oh-my-pi | Saved Codex and Claude limits in `~/.omp/agent/agent.db` (or `$PI_CODING_AGENT_DIR/agent.db`), using the system `sqlite3` command to read the database |
| Codex | Limit reports in recent Codex session files under `~/.codex/sessions` |
| Claude Code | The saved usage cache in `~/.claude.json`, or the limits Claude Code sent to the status line managed by milo |

For each tool, milo uses the newest report it can read. When there is room, it also shows **tokens today** from Claude Code's local session files under `~/.claude/projects`. This is local activity since midnight, not a limit. It counts new input, output and cache writes, not cache reads. A `+` after the number means the total may be incomplete.

**Show Claude Code limits** in `milo setup` adds a status line if you do not already have one. `milo claude-status` is called by Claude Code; it shows a short line and saves the reported limits to `~/.local/state/milo/claude-limits.json` (or `$XDG_STATE_HOME/milo/claude-limits.json`). It does not save prompts or replies. You do not need to enable it if a saved Claude Code or oh-my-pi report is already available.

## What milo does not do

The usage pane only reads local files. It does not call the providers, run their command-line tools, sign in, read credentials or guess limits from token counts. Figures can be old or missing if the tools have not saved a report on this machine recently. The status-line helper writes only its own limit readings; it does not write to session logs.

## If a figure is missing

- Make the pane taller and wider to see labels, reset times and any hints. The logos and large numbers use terminal graphics where available, including in Rex; smaller panes fall back to text.
- Press `r` to read the local sources again. This does not ask a provider for a fresh reading.
- For oh-my-pi, check that `sqlite3` is installed and that the harness has saved a usage report. For Codex, look for a recent local session that reported limits. For Claude Code, see [setup](setup.md#claude-code-limits) if neither saved source has a reading.
