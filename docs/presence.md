# Discord presence

Milo can show on your Discord profile that you are using Rex. It is off until you turn it on, and while it is off Milo starts nothing for it and never contacts Discord.

## What becomes public

While presence is on and a Rex app has a session open on this computer, the Discord app shows this as your activity:

- **Milo's name and icon**, from Milo's public Discord application
- one fixed line under it: **In a Rex session**

Anyone who can see your Discord activity sees it, and so can tell when you are using Rex. Discord decides who that is and may add how long the activity has been up. Use Discord's own activity privacy settings to narrow it.

That is all, unless you choose to show more. There are four choices of **details**, and the first is the default:

| Details | Adds a second line | For example |
| --- | --- | --- |
| `generic` | nothing | |
| `session` | the name of the Rex session in front | Session: release-notes |
| `project` | the folder name of the Git project in that session's focused pane | Project: milo |
| `both` | both names | Project: milo · Session: release-notes |

A session's name is whatever you or a coding agent called it, and often describes the work, so read it as others would before showing it. A project's name is the folder name of its Git repository: never its path, its remote address or a branch.

Whatever you choose, Milo never sends a tab title, a file name, a path, a remote address, a command, a terminal title or anything a terminal shows. Each name is cut to one short line of plain text. A name that could be a path or an address, one with a `/`, a `\` or a `user@host:` form, is left out whole.

Every place you can turn presence on, or choose to show a name, says what that makes public before it does. Turning presence on does not turn names on, and a choice saved by an earlier Milo keeps showing only what it showed then.

### Which session is “in front”

Names come only from the session the Rex app on this computer is showing: the one whose active tab has its focused pane held by that app. Rex has no call that names the session an app is looking at, so Milo goes by that. If two sessions fit, or none does, Milo shows no name at all rather than the name of the wrong session, and `milo presence` says so. This happens once the app has shown more than one session, or while only a phone or another computer is attached. Outside a Git project there is no project name.

With `generic`, Milo asks Rex one thing: whether any Rex app has a session open. It looks at no session. With a name chosen, it also asks which session is in front and, only for the names you chose, that session's label and the folder its focused pane is working in.

## Turn it on

Nothing needs creating first: Milo shows the activity under its own Discord application. An application ID is public, not a password. Milo holds no bot, token or client secret, and never signs in to Discord.

Use any one of these:

- `milo presence enable`. It says what becomes public and asks before turning anything on. `--yes` answers for you in a script.
- `milo setup`, then **Workflow → Discord presence**.
- `milo web`, then **Workflow → Discord presence → Turn on…**.

All three read and write the same saved choice, `discord.conf` in Milo's settings folder. It is one choice for your account on this computer, not a per-worktree preference, and it is saved at once without the **Save** step that pane preferences use.

To show a name, choose it in the same place: `milo presence details session`, `project`, `both` or back to `generic`; **Presence shows** in Settings; or **Shows** in the browser editor. While presence is on, a choice that adds a name asks first, and one that removes a name takes it down at once.

### Your own application

To show a name and icon of your choosing in place of Milo's:

1. Open <https://discord.com/developers/applications>, choose **New Application** and name it. The name is what Discord shows, as in “Playing Rex”. An app icon is optional. No bot is needed.
2. Copy the **Application ID** from its General Information page. It is a number of 17 to 19 digits.
3. Give it to Milo: `milo presence application APPLICATION_ID`, **Presence application** in Settings, or **Use my own** in the browser editor. A terminal's paste is not read in Settings, so the command is easier for a copied ID.

`milo presence application official`, or the same control in Settings and the browser editor, goes back to Milo's. An application ID you saved with an earlier Milo is kept until you do.

## Check it and turn it off

`milo presence` says whether presence is on, which application and details it uses, and what Discord is showing now, names included. The same line appears under **Discord presence** in Settings, in the browser editor and in `milo doctor`.

| It says | Meaning |
| --- | --- |
| Discord is showing “In a Rex session”. | The activity is up, with any names it lists. |
| …with no name: Milo has none it can be sure of. | Names are chosen, but [no one session is in front](#which-session-is-in-front) or it has no name to show. |
| Nothing is shown until a Rex app opens a session. | Rex is running, but no Rex app has a session open. |
| The Discord app is not running here. | Milo looks for it every 10 seconds while Rex is in use. |
| Discord does not know this application ID. | Check the ID, or go back to Milo's application. |
| Milo's presence helper is not running. | See [when it runs](#when-it-runs). |
| Discord did not accept the activity. | Discord answered with an error. Nothing is shown until presence is turned on again or Rex restarts. |

`milo presence disable`, or the same switch in Settings or the browser editor, takes the activity down at once, closes Milo's connection to Discord and stops the helper. Nothing then runs until you turn presence on again. Your choice of details and application is kept.

## When it runs

While presence is on, one small helper process does the work. Rex tells it when the first Rex app opens a session and when the last one leaves; it puts the activity up and takes it down to match. However many tabs, sessions and Milo panes are open, there is one helper and one connection to Discord, so nothing competes to set your activity.

With names chosen, Rex also tells the helper when a tab, a pane, a session's name or a pane's program changes. The helper waits a second for such changes to settle, and tells Discord only if the line it would show is different. Changing the details or the application replaces the helper: the old activity is taken down before the new one is put up.

The helper starts when you turn presence on while Rex is running, and whenever Rex loads Milo's saved layout, which is each time Rex starts once you have saved a layout in `milo setup` with at least one pane or with **Arrange new shell tabs** on. It ends when the Rex server stops. Without a saved layout, run `milo presence enable` again after starting Rex.

If Discord is not running, or quits, the helper waits and puts the activity back within about 10 seconds of Discord returning. Quitting Rex or turning presence off takes the activity down. So does **Uninstall Milo** in Home, which also turns presence off, so a later install starts with it off. If you remove the binary another way, run `milo presence disable` first. Rex itself is never slowed or changed by any of this: the helper only listens.

## Platforms and limits

- Presence talks to the **Discord desktop app** on the same computer and account as Milo, through the [local connection Discord documents](https://docs.discord.com/developers/topics/rpc). The browser and mobile versions of Discord have none, and nothing is shown through them alone.
- Milo's prebuilt release is for Apple Silicon Macs. A source build on Linux looks for Discord's socket in `XDG_RUNTIME_DIR` as Discord documents; a Discord installed as a Flatpak or Snap keeps its socket elsewhere and is not found.
- Only a Rex app with a session open counts. Scripts, the `rex` command and Milo's own listener do not, and a Rex app left open counts for as long as it has a session open, whether or not you are at the keyboard.
- Presence follows the Rex server on this computer. Sessions on another host do not start it there, and names are never taken from them.
- Names are shown only [when one session is plainly in front](#which-session-is-in-front). Expect the fixed line alone after you have looked at several sessions.
- The fixed line's wording cannot be changed. To change the name or icon, use [an application of your own](#your-own-application).
- If Discord rejects an application ID, Milo stops asking until you turn presence on again or Rex restarts.
