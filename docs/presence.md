# Discord presence

Milo can show on your Discord profile that you are using Rex. It is off until you turn it on, and while it is off Milo starts nothing for it and never contacts Discord.

## What becomes public

While presence is on and a Rex app has a session open on this computer, the Discord app shows this as your activity:

- the **name and icon** of a Discord application that you create and name yourself
- one fixed line under it: **In a Rex session**

Anyone who can see your Discord activity sees it, and so can tell when you are using Rex. Discord decides who that is and may add how long the activity has been up. Use Discord's own activity privacy settings to narrow it.

Milo sends nothing else. It never sends a session, tab, project or file name, a path, a command, a terminal title or anything a terminal shows, and there is no setting that makes it. The only thing Milo asks Rex is whether any Rex app has a session open.

Every place you can turn presence on shows this before it does.

## Turn it on

Milo has no Discord application of its own, so you supply one. An application ID is public, not a password: Milo needs no token, client secret or Discord sign-in, and you should not give it one.

1. Open <https://discord.com/developers/applications>, choose **New Application** and name it. The name is what Discord shows, as in “Playing Rex”. An app icon is optional.
2. Copy the **Application ID** from its General Information page. It is a number of 17 to 19 digits.
3. Turn presence on with that ID in any one of these:

   - `milo presence enable APPLICATION_ID`. It says what becomes public and asks before turning anything on. `--yes` answers for you in a script.
   - `milo setup`, then **Workflow → Discord presence**. Type the digits on the page that says what becomes public. A terminal's paste is not read there, so the command above is easier for a copied ID.
   - `milo web`, then **Workflow → Discord presence**. Paste the ID and choose **Turn on…**.

All three read and write the same saved choice, `discord.conf` in Milo's settings folder. It is one choice for your account on this computer, not a per-worktree preference, and it is saved at once without the **Save** step that pane preferences use. The ID is remembered, so later you can run `milo presence enable` without it.

## Check it and turn it off

`milo presence` says whether presence is on, which application it uses and what Discord is showing now. The same line appears under **Discord presence** in Settings, in the browser editor and in `milo doctor`.

| It says | Meaning |
| --- | --- |
| Discord is showing “In a Rex session”. | The activity is up. |
| Nothing is shown until a Rex app opens a session. | Rex is running, but no Rex app has a session open. |
| The Discord app is not running here. | Milo looks for it every 10 seconds while Rex is in use. |
| Discord does not know this application ID. | Check the ID and turn presence on again with the right one. |
| Milo's presence helper is not running. | See [when it runs](#when-it-runs). |
| Discord did not accept the activity. | Discord answered with an error. Nothing is shown until presence is turned on again or Rex restarts. |

`milo presence disable`, or the same switch in Settings or the browser editor, takes the activity down at once, closes Milo's connection to Discord and stops the helper. Nothing then runs until you turn presence on again.

## When it runs

While presence is on, one small helper process does the work. Rex tells it when the first Rex app opens a session and when the last one leaves; it puts the activity up and takes it down to match. However many tabs, sessions and Milo panes are open, there is one helper and one connection to Discord, so nothing competes to set your activity. The helper sleeps between those events and reads nothing from your sessions.

The helper starts when you turn presence on while Rex is running, and whenever Rex loads Milo's saved layout, which is each time Rex starts once you have saved a layout in `milo setup` with at least one pane or with **Arrange new shell tabs** on. It ends when the Rex server stops. Without a saved layout, run `milo presence enable` again after starting Rex.

If Discord is not running, or quits, the helper waits and puts the activity back within about 10 seconds of Discord returning. Quitting Rex or turning presence off takes the activity down. So does **Uninstall Milo** in Home, which also turns presence off, so a later install starts with it off. If you remove the binary another way, run `milo presence disable` first. Rex itself is never slowed or changed by any of this: the helper only listens.

## Platforms and limits

- Presence talks to the **Discord desktop app** on the same computer and account as Milo, through the [local connection Discord documents](https://docs.discord.com/developers/topics/rpc). The browser and mobile versions of Discord have none, and nothing is shown through them alone.
- Milo's prebuilt release is for Apple Silicon Macs. A source build on Linux looks for Discord's socket in `XDG_RUNTIME_DIR` as Discord documents; a Discord installed as a Flatpak or Snap keeps its socket elsewhere and is not found.
- Only a Rex app with a session open counts. Scripts, the `rex` command and Milo's own listener do not, and a Rex app left open counts for as long as it has a session open, whether or not you are at the keyboard.
- Presence follows the Rex server on this computer. Sessions on another host do not start it there.
- The wording is fixed. To change the name or icon, edit your application on Discord's site or use another application ID.
- If Discord rejects the application ID, Milo stops asking until you turn presence on again or Rex restarts.
