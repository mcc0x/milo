# Menu “Needs you” section (#101)

`python3 docs/evidence/menu-attention/walkthrough.py` drives the menu app against a disposable Rex server and saves these pictures and [`walkthrough.log`](walkthrough.log). Run `sh macos/build-menu.sh` first, on a Mac with a menu bar and Rex installed. It takes about two minutes and shows a second Milo in the menu bar with its popover and menus open. Leave the Mac alone while it runs: a menu will not open or stay open while you type or click in another app, and the walkthrough then stops with `the menu did not open`.

It builds a separate copy of the menu app under `target/menu-attention/`, with its own bundle identifier, from the app’s own sources and [`shots.swift`](shots.swift), and puts the Milo that `build-menu.sh` built inside it. That copy runs with a home of its own under `/tmp`. The only `rex` it can find there is a wrapper pinned to a Rex server the walkthrough starts on a socket in the same folder, so the installed Milo, your settings and the Rex you work in are not read, stopped or changed. No Rex app is connected to that server and Remote Control is never touched. Usage in the pictures is made up; the CPU and memory figures are the Mac’s own.

On arm64 macOS 27.2.1 with Rex 0.1.0-preview.23:

- **Off, as installed.** The menu was as before, the quota badge showed, and no client was connected to Rex. After **Off** was chosen again at the end, the watch and its Rex script were gone.
- **Choices.** Each choice was made in the real **More options → Needs you** menu: opened as a click opens it, moved through with arrow keys and chosen with Return, the keys put in the app’s own event queue. **In this menu** listed panes with no dot; **In this menu, with a menu bar dot** added the dot. **Finished work** and **Waiting for input** were switched on and off on their own, and the rows followed.
- **Panes.** Three terminals on the lab server reported `blocked`, `error` and `done` through Rex’s program status. A report’s message (`secret prompt`) reached neither the frames nor the menu. When the waiting program reported `working`, its row left; when it asked again, the row came back.
- **Jump.** The row of `api-refactor` was jumped to while another tab of its session was active. Rex made the pane’s tab active and the pane its focused block. With no Rex app connected to the lab server, the menu said so and left the other rows alone. This is the supported fallback; see below for what was not tried.
- **Seen.** Marking `release-notes` seen removed that row only.
- **Other indicators.** With **CPU** chosen, the bar showed the CPU reading with the dot, and **This Mac** appeared under the section.
- **Rex stopped and came back.** With the server stopped, the section said Rex wasn’t running, listed nothing, and the dot went out. With a new server started, nothing was listed until a pane on it reported.

| | |
| --- | --- |
| ![Menu with the section off: only Usage, with made-up Codex limits](01-off.png) | ![More options open on Needs you: Off is checked, with In this menu, In this menu with a menu bar dot, and Include with Waiting for input and Errors checked and Finished work unchecked](03-options-off.png) |
| ![Needs you section saying No pane needs you, above Usage](04-on-empty.png) | ![Needs you listing api-refactor › Tests waiting for permission from claude, and docs-site reporting an error from codex, each just now](05-waiting-and-error.png) |
| ![Three rows after a minute: waiting for permission, reported an error, and finished, each 1m, the last two with a Mark seen button](08-with-finished.png) | ![With Waiting for input switched off: only the error and the finished row](09-errors-and-finished-only.png) |
| ![Needs you submenu with the dot choice, Errors and Finished work checked and Waiting for input unchecked](10-options-on.png) | ![After the waiting agent went back to work: its row is gone, the other two remain](11-resumed.png) |
| ![After a jump with no Rex app connected: all three rows still listed, and a note that the pane is focused in its session but no Rex app on this Mac is connected to show it](12-jump-explained.png) | ![After marking release-notes seen: the waiting and the error row remain](13-marked-seen.png) |
| ![With the CPU indicator chosen: Needs you, then This Mac with CPU and memory, then Usage](15-cpu-indicator.png) | ![With Rex stopped: Rex isn’t running on this Mac. No pane is counted until it is](16-rex-stopped.png) |
| ![After Rex came back: one row, migration waiting for sign-in from codex](18-reconnected.png) | ![Off again: the menu with only Usage](19-off-again.png) |

The menu bar item, drawn at twice its size:

| Section off | Menu only | With the dot | CPU indicator with the dot | Rex stopped |
| --- | --- | --- | --- | --- |
| ![Mascot and 64%](02-status-off.png) | ![Mascot and 64%, no dot](06-status-menu-only.png) | ![Mascot, a small dot, and 64%](07-status-dot.png) | ![Mascot, a small dot, and CPU 7%](14-status-cpu-dot.png) | ![Mascot and 64%, no dot](17-status-rex-stopped.png) |

## What these pictures are, and what was not checked

- **The popover and the menus** are the app’s own windows as the system composed them, taken by the app itself. That needs no Screen Recording permission, which this session did not have: `screencapture` could not take a picture of the screen.
- **The menu bar item is not a picture of the menu bar.** macOS draws a status item in a process of its own, so the app has no window of it to take a picture of. Those five images are the image and title the app gave the item, drawn on a flat strip of the bar’s appearance. The dot’s place among other menu bar items was not looked at.
- **A row was not clicked with the pointer.** The walkthrough called the action the row’s button and its **Mark seen** button are wired to. Keys, not the pointer, worked the options menu.
- **No Rex app was switched.** Showing the pane’s session in Rex for Mac needs an app connected to the same server with Remote Control on. The lab server has no app, and Remote Control was left as it is on this Mac, so the path that ends with Rex in front was not run against a real Rex app. `tests/menu_attention.rs` runs it against a stand-in for `rex`, including Rex refusing the request, and `tests/menu-attention.swift` checks that only a pane Rex showed is marked seen and that the app Rex names is the one brought forward.
- **VoiceOver was not run.** The rows and buttons carry labels; they were not listened to.
