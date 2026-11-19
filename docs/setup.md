# Set up milo

![Milo setup showing the available panes, the layout sketch and what saving will change](img/setup.png)

## In Rex

Run `milo setup` inside a Rex tab. Use Up and Down to select a row and `Space` or `Enter` to change it. You can choose the outline, history and usage panes and the width of the left column. Press `a` to try your choices in this tab without saving, or `s` to save them. The added panes leave the keyboard in your shell.

When there is room, **On save:** lists what will change. For a new setup with both **Arrange new shell tabs** and **Show Claude Code limits** on, it says:

- “Add Milo actions to Rex.”
- “Arrange new shell tabs.”
- “Add milo to Claude Code's status line.”

Later it can say “Update the Rex layout.” or “Remove Milo actions from Rex.” Turning off an option can show “Stop arranging new shell tabs.” or “Take milo off Claude Code's status line.”

Turn on **Arrange new shell tabs** to apply the layout to new shell tabs. Tabs opened to run a command and tabs with their own layout are left alone. Turn it off and save to stop arranging new tabs. The panes also work on their own outside Rex.

Bare `milo` offers setup once in an unconfigured Rex. Use `milo outline` to skip the offer. Outside Rex, `milo setup` only offers the Claude Code option.

## Rex actions

Saving installs these actions in the Rex command palette:

| Action | Name for a key binding |
| --- | --- |
| Milo: Arrange This Tab | `milo_layout` |
| Milo: Add the Outline | `milo_outline` |
| Milo: Add the History | `milo_history` |
| Milo: Add the Usage Strip | `milo_usage` |
| Milo: Close Panes in This Tab | `milo_close` |

Milo does not add key bindings. To add your own, put something like `rex.bind("cmd+shift+m", "milo_layout")` in Rex's `init.lua`.

Setup adds two marked lines at the top of `~/.config/rex/init.lua` (after the interpreter line, if it has one) and asks Rex to reload. The actions live in `~/.config/milo/rex.lua`; setup writes `~/.config/milo/listener.lua` when you turn on **Arrange new shell tabs**. If `$XDG_CONFIG_HOME` is an absolute path, both folders live there instead. `$REX_CONFIG` can point directly to a different settings file. Milo keeps its saved state in `~/.local/state/milo` or under an absolute `$XDG_STATE_HOME`.

Setup does not rewrite your actions or key bindings. It refuses to change read-only settings files or use a linked milo configuration folder. The settings file itself may be a symbolic link: setup edits the target and keeps the link.

## Claude Code limits

**Show Claude Code limits** adds milo to `~/.claude/settings.json` only if you do not already have a status line. Turning it off removes milo's entry, not one you added yourself. If the option is unavailable because you already have a status line, milo leaves it alone; [other sources](usage.md#where-the-figures-come-from) may already provide limits.

## Remove milo

1. To remove the Rex setup, run `milo setup` inside Rex. Turn off **Outline**, **History**, **Usage** and **Arrange new shell tabs**, then press `s`. This removes milo's lines from Rex's settings, stops the listener and removes generated files that milo still owns. A generated file you edited by hand is left in place. To close panes already open, use **Milo: Close Panes in This Tab** before removing the actions, or close the panes yourself.
2. To remove milo from the Claude Code status line too, turn off **Show Claude Code limits** and save. You can also do this outside Rex.
3. From the milo checkout, run `sh install.sh --uninstall` to remove the binary. Use the same `MILO_BIN_DIR` you installed to if you set one. This command does not change Rex or Claude Code settings or remove milo's saved state.

## Troubleshooting

- **`milo` is not found:** the installer puts it in `~/.local/bin` unless you set `MILO_BIN_DIR`. Add the install directory to your `PATH`, or run the binary by its full path.
- **Setup says milo's Rex lines were moved, copied or changed:** setup leaves the file alone. Put a single copy of the original two lines back at the top, just after any interpreter line, then reopen setup and save.
- **A save was interrupted:** reopen setup, check the choices and press `s`. If Rex still needs to reload, setup says “Rex still needs to reload. Check your choices, then press s.”
- **Another setup is saving:** only one setup can save at a time. Wait for the other save to finish, then try again.
- **Actions are missing:** run `milo setup` inside Rex and press `s`. If setup reports a check or reload error, resolve it before saving again. Run `rex actions` without selecting a client (including via `REX_CLIENT`) and look for `milo_layout` in the server's action list.
- **New shell tabs are not arranged:** check **Arrange new shell tabs** and save inside Rex. Setup normally starts and stops the listener. `milo listen` and `milo listen --stop` are the listener commands, not panes.
- **Icons appear as boxes:** install a [Nerd Font](https://www.nerdfonts.com) and select it in the terminal, or set `MILO_ICONS=plain` for ordinary symbols or `MILO_ICONS=off` to hide them.
