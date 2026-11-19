# Set up milo

![Milo’s layout picker with the project sidebar selected beside its split preview](img/setup.png)

## Install

The v0.1.0 prebuilt release is for Apple Silicon macOS. Use Homebrew:

```sh
brew install mcc0x/milo/milo
```

Or download the versioned release installer from GitHub:

```sh
curl -fsSLo milo-install.sh https://github.com/mcc0x/milo/releases/download/v0.1.0/install.sh &&
  sh milo-install.sh
```

The installer checks the binary against a pinned SHA-256 checksum before placing it in `~/.local/bin`. Set `MILO_BIN_DIR` to choose a different directory. The prebuilt installer does not need a compiler, Cargo or Git.

To build from source instead, install Git and Rust 1.91 or later, then run:

```sh
git clone https://github.com/mcc0x/milo.git
cd milo
sh install.sh
```

The source installer also honours `MILO_BIN_DIR` and defaults to `~/.local/bin`.

## In Rex

In a Rex shell run `milo setup`. Pick **Just the tree**, **Project sidebar**, or start from the tab you already arranged. Adjust the individual panes and appearance if needed. Trying the layout does not change your saved default; undo the trial before saving if it isn't what you want.

In Home, the welcome guide and Settings, moving the pointer over a clickable control underlines it without changing your keyboard selection. **Click once** to open a menu, switch sections or change a value. Click **Save** and choose a scope to keep your changes. Terminals without mouse-motion reporting still support the arrow keys and Enter.

In narrow Settings panes, rows use shorter labels. When there's room, the full name and value of the selected setting appear below the list.

**Unsaved changes** stays visible while you edit. If you leave Settings from Home or `milo setup`, choose **Save**, **Discard**, or **Keep editing**. Save asks for a scope for each changed group: layout, then appearance and workflow together. Claude Code choices apply to your account and do not need a scope. Each confirmed save takes effect as you go, so going back does not undo changes you have already saved. Leaving also undoes any layout trial you haven't kept by saving it.

Save for **this worktree** to keep a project-specific setup, or save a **global default** for projects without an override. Worktrees of the same repository can have different choices. Preferences live on the machine running Milo, outside the repository. A project cannot install actions or supply a command for Milo to execute.

Usage starts as a two-row strip in the shared sidebar. Drag a divider in an installed Milo sidebar and Milo remembers the proportions for that Git worktree automatically. To save a layout explicitly or make it the global default, use **Milo: Remember This Layout…**. Milo stores pane roles and proportions, not processes or live pane IDs. Mixed rails containing another app are left alone.

![A Rex tab with Outline, History and the compact Usage readout in one rail beside the shell](img/tab.png)

Turn on **Arrange new shell tabs** to apply the layout to new shell tabs, including the first tab of a new session. If Usage is sized in rows, Milo waits for a Rex client to measure the tab, so a background tab may arrange when you first show it. Tabs opened to run a command and tabs with their own layout are left alone. Turn it off and save to stop arranging new tabs. The panes also work on their own outside Rex.

Use the appearance choices to preview icons, choose text change marks, and turn animated activity marks off. Use the workflow choices to pin Outline and History to the starting project or follow the shell they were opened beside. Milo does not change Rex's font, theme, shortcuts or notification settings.

**Text zoom is separate from split size.** On Rex preview.21 (server API 18), Milo can read a terminal's cell dimensions, but creating or resizing a terminal does not set the font the Mac app renders. The app exposes relative font actions and an app-wide [starting font size](https://www.superlogical.com/rex/docs/reference/mac-settings.md#setting-typography-font-size), not an absolute per-pane zoom setting. Milo therefore remembers divider positions, not text zoom, for Outline, History and Usage.

Run `milo` for Home or `milo setup` to go directly to Settings. Both share the same first-run guide. To skip it in an install script, run `milo setup --skip-onboarding`; it records the skip and exits without requiring a terminal or installing integrations. `milo outline` and the other direct pane commands never show the guide. Terminal panes work without Rex; split geometry and shell-following require Rex.

## Rex actions

Saving installs these actions in the Rex command palette:

| Action | Name for a key binding |
| --- | --- |
| Milo: Arrange This Tab | `milo_layout` |
| Milo: Add the Outline | `milo_outline` |
| Milo: Add the History | `milo_history` |
| Milo: Add the Usage | `milo_usage` |
| Milo: Close Panes in This Tab | `milo_close` |
| Milo: Peek Outline | `milo_peek_outline` |
| Close the outline peek | `milo_close_peek` |
| Remember the current rail | `milo_remember_layout` |
| Reset a remembered layout | `milo_reset_layout` |

Milo does not install key bindings. For an optional shortcut, check Rex's shortcut settings and `rex keymap` first; the latter lists config bindings, not every Mac app shortcut. If the chord is free, put a binding such as `rex.bind("cmd+shift+m", "milo_peek_outline")` in your own Rex `init.lua`. Actions also work directly in the palette without any binding.

Peek opens a floating outline and takes focus. Invoking it again raises the same peek. Dismiss it to return to the originating pane. Adding the docked Outline instead keeps the shell focused. Your existing custom actions remain yours.

Setup adds two marked lines at the top of `~/.config/rex/init.lua` (after the interpreter line, if it has one) and asks Rex to reload. The actions live in `~/.config/milo/rex.lua`. `~/.config/milo/listener.lua` watches for divider changes even when arranging new shell tabs is off. If `$XDG_CONFIG_HOME` is an absolute path, both folders live there instead. `$REX_CONFIG` can point directly to a different settings file. Milo keeps its saved state in `~/.local/state/milo` or under an absolute `$XDG_STATE_HOME`.

Setup does not rewrite your actions or key bindings. It refuses to change read-only settings files or use a linked milo configuration folder. The settings file itself may be a symbolic link: setup edits the target and keeps the link.

## Claude Code limits

**Show Claude Code limits** adds milo to `~/.claude/settings.json` only if you do not already have a status line. Turning it off removes milo's entry, not one you added yourself. If the option is unavailable because you already have a status line, milo leaves it alone; [other sources](usage.md#where-the-figures-come-from) may already provide limits.

## Remove milo

From Home, choose **Uninstall** to review and confirm removal. **Health → Remove integration** and `milo setup --remove` only disconnect the managed integrations; they do not remove the executable. For manual removal:

1. To diagnose a setup before changing it, run `milo doctor`. To fix the managed integration after an upgrade or executable move, run `milo setup --repair` with the binary you intend to keep.
2. Before deleting the binary, run `milo setup --remove` to disconnect the managed Rex and Claude Code integrations. Saved preferences and user-edited files are not an uninstall target. Close any live Milo panes separately.
3. Remove the binary with `brew uninstall milo` if you installed with Homebrew. If you used the release installer, run `sh milo-install.sh --uninstall` with your downloaded copy. If you built from source, run `sh install.sh --uninstall` from the checkout. For either script, use the same `MILO_BIN_DIR` you installed to if you set one. Removing the binary does not change Rex or Claude Code settings or remove milo's saved state.

## Troubleshooting

- **`milo` is not found:** for Homebrew, check that Homebrew is on `PATH`. The scripts put milo in `~/.local/bin` unless you set `MILO_BIN_DIR`. Add the install directory to `PATH`, or run the binary by its full path.
- **Setup says milo's Rex lines were moved, copied or changed:** setup leaves the file alone. Put a single copy of the original two lines back at the top, just after any interpreter line, then reopen setup and save.
- **A save was interrupted or actions are missing:** run `milo doctor`. If it reports a stale launcher, pending reload or missing managed actions, run `milo setup --repair` with the binary you intend Rex to use. The **Health** section in setup offers the same diagnostics, repair and removal.
- **The sidebar opens at a different size:** run `milo doctor` from that Rex tab to compare the saved sidebar width and Usage height with what's live. If the sizes differ, check [Autobalance splits](https://www.superlogical.com/rex/docs/reference/mac-settings.md#setting-splits-focus-autobalance-splits) under **Settings → Interface → Splits & Focus**. If it is on, turn it off, then close and re-add the Milo panes to apply the saved sizes.
- **Another setup is saving:** only one setup can save at a time. Wait for it to finish before retrying.
- **New shell tabs are not arranged:** enable **Arrange new shell tabs** under **Customize panes**, then save globally. The listener is an account setting, not a per-project switch. `milo doctor` checks whether it is actually running.
- **Icons appear as boxes:** choose plain symbols or no icons in setup, or override one launch with `MILO_ICONS=plain` or `MILO_ICONS=off`. Choose a Nerd Font in Rex only if you want one for all of your terminals.
