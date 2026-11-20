# Set up milo

![Milo’s layout picker with the project sidebar selected beside its split preview](img/setup.png)

## Install

The v0.1.3 prebuilt release is for Apple Silicon macOS. Use Homebrew:

```sh
brew install mcc0x/milo/milo
```

Or download the versioned release installer from GitHub:

```sh
curl -fsSLo milo-install.sh https://github.com/mcc0x/milo/releases/download/v0.1.3/install.sh &&
  sh milo-install.sh
```

The installer checks the binary against a pinned SHA-256 checksum before placing it in `~/.local/bin`. Set `MILO_BIN_DIR` to choose a different directory. The prebuilt installer does not need a compiler, Cargo or Git.

To build from source instead, install Git and Rust 1.91 or later, then run:

```sh
git clone https://github.com/mcc0x/milo.git
cd milo
sh install.sh
```

The source installer builds the checkout containing the script, even when launched from another directory. A standalone or piped copy clones Milo from GitHub into a private temporary directory instead.

The source installer also honours `MILO_BIN_DIR` and defaults to `~/.local/bin`.

Current installers write a receipt for the binary and its install path. Use a current installer with `--uninstall` and the same `MILO_BIN_DIR` to remove it. If the binary has changed or the receipt is missing or invalid, the installer leaves the binary in place. Older installs may not have a receipt; verify the binary before removing it manually rather than running an older installer’s `--uninstall`.

## In Rex

In a Rex shell run `milo setup`. Pick **Just the tree**, **Project sidebar**, or start from the tab you already arranged. Adjust the individual panes and appearance if needed. Trying the layout does not change your saved default; undo the trial before saving if it isn't what you want.

In Home, the welcome guide and Settings, moving the pointer over a clickable control underlines it without changing your keyboard selection. **Click once** to open a menu, switch sections or change a value. Click **Save** and choose a scope to keep your changes. Terminals without mouse-motion reporting still support the arrow keys and Enter.

In narrow Settings panes, rows use shorter labels. When there's room, the full name and value of the selected setting appear below the list.

**Unsaved changes** stays visible while you edit. If you leave Settings from Home or `milo setup`, choose **Save**, **Discard**, or **Keep editing**. Save asks for a scope for each changed group: layout, then appearance and workflow together. Claude Code choices apply to the active profile and do not need a scope. Each confirmed save takes effect as you go, so going back does not undo changes you have already saved. Leaving also undoes any layout trial you haven't kept by saving it.

Save for **this worktree** to keep a project-specific setup, or save a **global default** for projects without an override. Worktrees of the same repository can have different choices. Preferences live on the machine running Milo, outside the repository. A project cannot install actions or supply a command for Milo to execute.

If the saved layout changes while Settings is open, Milo keeps the newer layout instead of overwriting it. Reopen Settings to edit that layout.

Saving appearance or workflow preferences changes only the settings you edited. A worktree still inherits global values for any settings it does not override, and saving global defaults does not copy worktree overrides.

If a worktree's preferences file is unreadable or invalid, panes keep using global preferences. If the global file is broken, panes use built-in defaults. Milo leaves the files alone and shows a notice; run `milo doctor` to find the file and the error, then correct it and reopen the panes.

Usage starts as a two-row strip in the shared sidebar. Drag a divider in an installed Milo sidebar and Milo remembers the proportions for that Git worktree automatically. To save a layout explicitly or make it the global default, use **Milo: Remember This Layout…**. Milo stores pane roles and proportions, not processes or live pane IDs. Mixed rails containing another app are left alone.

![A Rex tab with Outline, History and the compact Usage readout in one rail beside the shell](img/tab.png)

Turn on **Arrange new shell tabs** to apply the layout to new shell tabs, including the first tab of a new session. Save this switch globally; changing it alone does not replace either the global or worktree layout. If Usage is sized in rows, Milo waits for a Rex client to measure the tab, so a background tab may arrange when you first show it. Tabs opened to run a command and tabs with their own layout are left alone. Turn it off and save globally to stop arranging new tabs. The panes also work on their own outside Rex.

Use the appearance choices to preview icons, choose text change marks, and turn animated activity marks off. Usage shows Codex, Claude Code and OpenCode only while each has a figure to show; choose **Always show** or **Hide** for a tool to [decide that yourself](usage.md#which-tools-appear). Use the workflow choices to pin Outline and History to the starting project or follow the shell they were opened beside. Milo does not change Rex's font, theme, shortcuts or notification settings.

**A sidebar that Rex restores holds shells, not Milo.** On Rex preview.21 (server API 18), stopping the server ends every program in its sessions and saves their layouts. When it starts again, each tab comes back with its panes, names and dividers where they were, but every pane is a new login shell in the folder that pane was in. The Outline, History and Usage panes keep their places and their `milo:` labels with nothing of Milo running in them. Rex decides what a restored pane runs, and Milo does not type a command into those shells: a shell at a prompt is yours and may already hold your input. To get the sidebar back in a tab, run **Milo: Close Panes in This Tab**, then **Milo: Arrange This Tab**; see [Troubleshooting](#troubleshooting) for what that closes. New shell tabs are arranged as before, because the listener starts again with Rex.

**Text zoom is separate from split size.** On Rex preview.21 (server API 18), Milo can read a terminal's cell dimensions, but creating or resizing a terminal does not set the font the Mac app renders. The app exposes relative font actions and an app-wide [starting font size](https://www.superlogical.com/rex/docs/reference/mac-settings.md#setting-typography-font-size), not an absolute per-pane zoom setting. Milo therefore remembers divider positions, not text zoom, for Outline, History and Usage.

Run `milo` for Home or `milo setup` to go directly to Settings. Both share the same first-run guide. To skip it in an install script, run `milo setup --skip-onboarding`; it records the skip and exits without requiring a terminal or installing integrations. `milo outline` and the other direct pane commands never show the guide. Terminal panes work without Rex; split geometry and shell-following require Rex.

## Tab titles and session names

Milo leaves tab and session names alone unless you ask. It builds a name from folder names and the name of an agent's process. It never reads what a terminal shows, a terminal's title or a program's status text, so nothing you type to an agent ends up in a name.

Turn on **Title Rex tabs** under **Workflow** to name tabs that nobody has named. A tab is titled after the Git project its shell is in: `project` in the repository's own folder and `project/worktree` in a linked worktree. While Claude Code, Codex or oh-my-pi runs in one of the tab's shells, the title ends with ` · claude`, ` · codex` or ` · omp`. A tab outside a Git project keeps the label Rex gave it. The setting is off by default. Save it globally, or for one worktree to title tabs only there or everywhere but there.

Milo titles a tab only while its label is Rex's own, such as `Window 89abcdef`, or the title Milo set last. Rename a tab yourself, in the app or with `rex window rename`, and Milo stops titling it. Clear the name to hand the tab back. Milo remembers the titles it set by Rex window ID in its state folder, so a name you chose is still respected after Rex or Milo's listener restarts.

A change to the setting reaches new tabs at once, and an open tab the next time its shell changes directory or an agent starts or stops in it. Turning titles off gives a tab Rex's label back at that point. Agents are recognised by the name of their program: one started under another name, such as `node`, is not shown.

The listener starts Milo to ask about a directory once, and with titles on once more for each agent that runs there. After that it asks again when your global preferences or that worktree's own have changed, or when a folder outside any project has gained a repository. So with titles off, other tabs in a directory and later visits to it normally start no further process. Milo is asked every time about a directory it cannot vouch for: one outside a project with a folder above it that other users can write to, such as `/tmp`, one reached through a link, or one where Git could not run.

**Milo: Name Session from This Project** renames the session you run it in after the project of its shell: `project`, or `project/worktree` in a linked worktree, without the agent. It is the only way Milo names a session, and it happens only when you run the action: from the command palette, with a key you bound to it, or with `rex do milo_name_session`. It creates no session and switches to none. Outside a Git project it changes nothing and says so. Milo does not remember the name or keep it up to date, so rename the session again whenever you like. The action does not depend on **Title Rex tabs**.

Both need the installed Rex integration. After upgrading Milo, run `milo setup --repair` so Rex loads the current actions and listener.

## Browser editor

Run `milo web` from the project you want to configure, or choose **Open in browser** from Milo Home. Milo opens the editor through a short-lived private file so it doesn’t pass the session key to the launcher. If the editor doesn’t open, use the private link printed in the terminal. `milo web --no-open` prints that link without opening a browser. Keep the command running while you edit. The page is served by the Milo executable on this machine, not by usemilo.net. No account, network service, or separate frontend install is needed. Don’t share the link.

The editor listens on `127.0.0.1:6110` by default. Use `milo web --port 7000` to choose another port, or `--port 0` for an available port when running a second editor.

Layout edits stay in a draft until you save. In Rex, the layout actions menu lets you copy the current tab’s layout or try the draft in that tab. Undo a trial to restore the previous layout. Save explicitly for the current worktree or as the global default. Appearance and workflow use the same preference files as terminal Settings; Claude Code choices apply to the active profile. Outside Rex you can still edit and save settings.

In **Health**, run a check of saved settings and the live layout. If the original Rex tab is no longer available, Health warns unless it finds a more serious issue. Saving settings or changing the live layout clears the old result; run the check again for an updated verdict.

While the page is open, it periodically keeps the local session alive only if it has unsaved changes, an active layout trial, or queued edits. This does not save your work, and the session can still time out if the browser suspends the page.

**Done** closes the editor only after undoing any active layout trial. If undo fails, the editor stays open so you can retry. Stopping Milo with `Ctrl-C` or letting it time out only attempts to undo the trial. Closing the browser tab alone leaves the private link usable until Milo stops or the session is idle for 15 minutes. When the session ends, unsaved drafts are discarded; saved settings stay as they are.

For UI development, run `cargo run --features web-dev -- web` from the Milo checkout and use the browser tab it opens. Edits to `src/web/app.css` apply in place; changes to `index.html`, `app.js`, or the mascot reload the page against the same in-memory settings draft. Finish any pending input first. Rust changes still require restarting the command. Normal builds serve embedded assets and do not watch the checkout.

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
| Name the session after this project | `milo_name_session` |

Milo does not install key bindings. For an optional shortcut, check Rex's shortcut settings and `rex keymap` first; the latter lists config bindings, not every Mac app shortcut. If the chord is free, put a binding such as `rex.bind("cmd+shift+m", "milo_peek_outline")` in your own Rex `init.lua`. Actions also work directly in the palette without any binding.

Peek opens a floating outline and takes focus. Invoking it again raises the same peek. Dismiss it to return to the originating pane. Adding the docked Outline instead keeps the shell focused. Your existing custom actions remain yours.

Setup adds two marked lines at the top of `~/.config/rex/init.lua` (after the interpreter line, if it has one) and asks Rex to reload. The actions live in `~/.config/milo/rex.lua`. `~/.config/milo/listener.lua` watches for divider changes even when arranging new shell tabs is off, and titles tabs when you have chosen that. If `$XDG_CONFIG_HOME` is an absolute path, both folders live there instead. `$REX_CONFIG` can point directly to a different settings file. Milo keeps its saved state in `~/.local/state/milo` or under an absolute `$XDG_STATE_HOME`.

Setup does not rewrite your actions or key bindings. It refuses to change read-only settings files or use a linked milo configuration folder. The settings file itself may be a symbolic link: setup edits the target and keeps the link.

## Claude Code limits

**Show Claude Code limits** adds milo to `~/.claude/settings.json` only if you do not already have a status line. Turning it off removes milo's entry, not one you added yourself. If the option is unavailable because you already have a status line, milo leaves it alone; [other sources](usage.md#where-the-figures-come-from) may already provide limits.

If you set `CLAUDE_CONFIG_DIR`, Milo uses that profile's `settings.json` instead and tracks its installation separately.

New setups use the full path to Milo, so Claude Code does not need `milo` on its `PATH`. If Doctor reports an old launcher, turn limits off, save, then turn them back on and save.

## Remove milo

From Home, choose **Uninstall** to review and confirm removal. **Health → Remove integration** and `milo setup --remove` only disconnect the managed integrations; they do not remove the executable. For manual removal:

1. To diagnose a setup before changing it, run `milo doctor`. To fix the managed integration after an upgrade or executable move, run `milo setup --repair` with the binary you intend to keep.
2. Before deleting the binary, run `milo setup --remove` to disconnect the managed Rex and Claude Code integrations. Saved preferences and user-edited files are not an uninstall target. Close any live Milo panes separately.
3. Remove the binary with `brew uninstall milo` if you installed with Homebrew. If you used the release installer, run `sh milo-install.sh --uninstall` with your downloaded copy. If you built from source, run `sh install.sh --uninstall` from the checkout. For either script, use the same `MILO_BIN_DIR` you installed to if you set one. Removing the binary does not change Rex or Claude Code settings or remove milo's saved state.

If Rex cannot reload during removal, Milo still removes the managed files and tells you to restart Rex. If Milo cannot safely remove its Claude Code status line, correct the reported settings problem and retry before deleting the executable.

If you set `XDG_STATE_HOME` when installing the integration, use the same value for repair and removal. Milo won’t guess which listener or Claude settings entry belongs to an installation it can’t verify.

`milo setup --remove` disconnects only the active Claude Code profile. Before removing the binary, repeat it for any alternate profiles where you enabled limits, using the same `CLAUDE_CONFIG_DIR` as when you enabled them.

## Troubleshooting

Choose **Doctor** in Home or **Read diagnostics** in Settings to check the installation. Press `Esc` to go back without waiting for the checks to finish. For a report to copy from the shell, run `milo doctor`.

- **`milo` is not found:** for Homebrew, check that Homebrew is on `PATH`. The scripts put milo in `~/.local/bin` unless you set `MILO_BIN_DIR`. Add the install directory to `PATH`, or run the binary by its full path.
- **Doctor reports a Rex loader conflict, or setup says Milo's Rex lines were moved, copied or changed:** Milo leaves your Rex settings and listener alone. In the Rex settings file named by Doctor, restore a single copy of Milo's original two lines at the top, just after any interpreter line. Then run Doctor again.
- **Doctor reports an unreadable saved layout profile:** restore a known-good copy of the named file, or back it up and move it aside. Then reopen Settings to use the fallback layout or save a new one. Integration repair does not replace layout profiles.
- **A save was interrupted or actions are missing:** run `milo doctor`. It checks the managed `rex.lua` and `listener.lua` on disk separately from the actions Rex has loaded. If it reports an out-of-date managed file, a stale launcher, a pending reload or missing managed actions, run `milo setup --repair` with the binary you intend Rex to use. If Doctor identifies a custom file, review it yourself. The **Health** section in setup offers the same diagnostics, repair and removal.
- **The sidebar opens at a different size:** run `milo doctor` from that Rex tab to compare the saved sidebar width and Usage height with what's live. If the sizes differ, check [Autobalance splits](https://www.superlogical.com/rex/docs/reference/mac-settings.md#setting-splits-focus-autobalance-splits) under **Settings → Interface → Splits & Focus**. If it is on, turn it off, then close and re-add the Milo panes to apply the saved sizes.
- **Another setup is saving:** only one setup can save at a time. Wait for it to finish before retrying.
- **New shell tabs are not arranged:** enable **Arrange new shell tabs** under **Customize panes**, then save globally. The listener is an account setting, not a per-project switch. `milo doctor` checks whether it is actually running.
- **After Rex restarts, the sidebar shows shell prompts:** Rex preview.21 restores the panes of a sidebar as shells and does not start Milo in them again. In each tab you want back, run **Milo: Close Panes in This Tab**, then **Milo: Arrange This Tab**. The first closes the three restored shells, so finish anything you started in one of them beforehand; the second builds the sidebar again at the sizes remembered for that worktree. **Arrange This Tab** on its own adds nothing while the restored panes are there, since each pane of the sidebar still exists. Until you rebuild it, dragging a restored sidebar's divider is not remembered, and Doctor compares its sizes without noticing that Milo is not running in it. Milo does not rebuild restored tabs for you, and never types into their shells.
- **A tab is not titled:** `milo doctor` shows whether **Rex tab titles** is on for this worktree and whether the listener is running. The listener runs while the saved layout has at least one pane or arranges new tabs. A tab that has a name of its own is left alone: clear the name in Rex to let Milo title it. A tab that was open when you turned titles on is titled when its shell next changes directory.
- **Icons appear as boxes:** choose plain symbols or no icons in setup, or override one launch with `MILO_ICONS=plain` or `MILO_ICONS=off`. Choose a Nerd Font in Rex only if you want one for all of your terminals.
