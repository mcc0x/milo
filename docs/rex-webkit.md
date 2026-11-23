# Local WebKit preview (experimental, macOS)

**Human-test build: placement is approximate.** Use one visible, local Rex window, outside full screen and wholly on one display. Leave Accessibility, Screen Recording and Rex Remote Control off. Placement and tab switching in the Mac client still need visual acceptance before merge.

Start your dev server first. In its **local Rex tab**, run:

```sh
milo preview http://localhost:3000
```

Rex owns the terminal anchor, divider and layer. A small Milo helper owns a borderless AppKit panel containing a real `WKWebView`. The page is not rendered into a bitmap, embedded in the settings dashboard or opened in the default browser. The command never searches for servers, picks a port, changes Rex settings or navigates an existing browser tab. Linux, SSH and remote sessions are not supported.

### Launching from Rex for Mac

1. In **Rex.app on the Mac**, select the local host and use [New Tab (⌘T)](https://www.superlogical.com/rex/docs/reference/mac-actions#action-client-tab-new). Use a new disposable tab for testing. This is a **Mac client action**, not `rex window new` in a shell.
2. In the new tab’s **main working terminal**, `cd` to the project. Start the dev server if needed. Run `milo preview http://localhost:3000` there. When testing a PR build, use its absolute binary path in place of `milo`; no installation is needed.
3. If Milo says the terminal **isn’t owned** by a local Mac client, do not change permissions. A connected Mac, an active tab, or a Mac-owned sidebar does **not** establish ownership of the working block. An attached phone need not be disconnected.
4. **Several sessions can stay attached to the same Mac.** With one visible Mac window, Milo opens the requested tile but leaves the page hidden. Click near the **center of the new tile**, keep the pointer there and wait for **ready to confirm**. Type `show` and press Return for a peek of up to 12 seconds. Before switching sessions, move the pointer outside and **wait until the page disappears**. Moving it back does not reopen the page.
5. **Resizing:** drag the Rex divider and release on it. Milo hides the pixels so Rex handles the drag without closing or reloading the page. Wait for `divider released`, then move back inside the preview tile. A matching mouse movement routed to that terminal can restore the remaining peek. For an expired peek, a shared tile, or a drag Milo did not see begin, the tile asks for the six digits beside **Milo** in the Mac menu bar. Keeping the pointer in the tile, type those digits there and press Return. Do not paste or share them. Releasing on the divider never shows the page.

**Reading the tile:** `ready to confirm` means the focused terminal can accept a command, not that placement has passed. Wait for it once. `confirmation accepted; checking the tile` means Milo received the complete command. `preview visible` means the native panel passed its placement check, **not** that the dev server loaded. If the tile instead reports `not shown`, use the reason it gives: refocus the tile, let a moving divider settle, uncover the pinned window, or move the window wholly onto one display. Don't change privacy permissions. The helper reports stages and reasons, not typed text, URLs or page contents.

To check from that same working shell, the following read-only commands print only the owner classification (requires `jq`):

```sh
(
  rex --autostart=false block inspect -s "$REX_SESSION" "$REX_BLOCK" --json &&
  rex --autostart=false api call -s "$REX_SESSION" session.list_clients '{}'
) | jq -ser --argjson uid "$(id -u)" '
  if length != 2 then error("incomplete Rex response") else
    .[0].block.owner as $owner |
    if $owner == null then "working block: no owner"
    elif any(.[1].clients[]; .client_id == $owner and .connection_state == "connected"
      and .info.kind == "app" and .info.platform == "macos" and .principal.kind == "peer"
      and .principal.transport == "unix" and .principal.uid == $uid and (.principal.pid // 0) > 0)
    then "working block: local Mac app owner"
    else "working block: no matching local Mac app owner" end
  end'
```

On Rex `0.1.0-preview.21`, a disposable server with a Mac-identifying Unix app peer attached left the working block from **`rex window new` unowned**. Creating the same shell through `session.new_window` on the attached app connection assigned it to that peer without an explicit claim. From inside those two PTYs, Milo’s production identification code refused the first and returned the exact block, window and app peer for the second. This checks the server contract, not a GUI keystroke. If a fresh Mac-created terminal still refuses, report the Rex version, owner classification and Milo error; do not force ownership or change privacy settings to bypass the check.

The preview anchor is separate from its working shell. In a disposable server, the CLI-created split remained unowned after publication until the app peer claimed it. Milo never claims it for the app. At a one-time check after 28 seconds, the observer reports an unclaimed tile and stops **if it can still verify that tile in its original tab**. A detached or unverifiable tile need not produce that diagnostic. A waiting notice or refusal is not a placement pass.

## Placement

The default is **Right** of the main working terminal, not right of the sidebar. Choose **Preview placement** in `milo setup` → Workflow or `milo web` → Panes → Preview. These edit the same preference in the existing settings scopes. To try another placement once, without saving anything:

```sh
milo preview http://localhost:3000 --placement left
```

The four placements are tiled splits in the same tab. A saved `preview_placement = floating` from an experimental build resolves to **Right** without rewriting the file on read. New commands and settings edits accept only `right`, `left`, `above` or `below`. An ambiguous working terminal or viewer causes a refusal.

Click **Return to terminal** or press **Command-Shift-Return** while editing the page to return to the working terminal. **Command-W** in the focused preview closes it. The helper hides when Rex is not the frontmost application.

## Window ownership and permissions

Milo does not patch Rex or install a plugin. On macOS the Cargo build compiles a separate system-framework helper, embeds it in `milo` and installs that exact helper in the private macOS user cache on demand. The terminal CLI does not link AppKit or WebKit. No separate package, JavaScript runtime or browser extension is required.

Before splitting, Milo matches the terminal owner to the local Rex app PID and pins its single eligible visible native window ID using [Core Graphics window metadata](https://developer.apple.com/documentation/coregraphics/cgwindowlistcopywindowinfo(_:_:)). The helper never captures pixels, reads terminal scrollback, asks for Accessibility, or chooses a replacement window. It reads confirmation commands only from its own preview tile, not the working shell. Its private cache location is separate from the old experimental Accessibility helper.

The Rex observer follows the published tile in the original session and tab. It sends normalized bounds when the divider moves. The helper maps those fractions into the pinned window using Rex’s [appearance preferences](https://www.superlogical.com/rex/docs/reference/mac-settings): sidebar side and width, tab bar location, toolbar behavior, pane headers, density and terminal padding. It leaves room for the sidebar whether collapsed or expanded. Header, gap and toolbar dimensions are estimates. The toolbar says **Approximate**. Unconfirmed ownership, an inactive tab, zoom, a higher visible Rex layer or an unsupported window configuration suppresses the overlay.

Milo checks Rex ownership once a second while observing. A silent observer cannot keep a visible panel authorized indefinitely: its last authorization expires after three seconds. A confirmed loss of the pinned Quartz window ends the helper; temporary occlusion, an offscreen window, or unavailable window metadata does not transfer the pin.

**What a pin cannot tell Milo:** [`client.list`](https://www.superlogical.com/rex/docs/reference/api#clientlist) reports attachments. `session.view` reports the active tab and focused block **within each session**. Neither identifies the session painted in the pinned native window. Other sessions can retain a focused block simultaneously. The Mac's `session.select` action changes the displayed session; it is not a query. Milo checks only the original server; it neither queries other hosts nor selects a session on your behalf.

Once Milo sees this Mac viewer attached to multiple sessions, the tile stays in timed-peek mode, even if other sessions later detach. Observer heartbeats cannot open the page or extend a peek. The pointer is sampled, so a quick excursion outside and back between samples can go unnoticed. A same-window keyboard switch to another session or host, or a palette drawn inside the window, can also go undetected. In timed mode the page can then remain visible for the rest of the 12-second interval. If only one attachment was observed, presentation is automatic: there is **no 12-second cap** on an undetected host switch or palette. Close an automatic preview before switching hosts. One visible window is a placement requirement, not proof of the displayed session.

Use the confirmation in a tile you control. Rex forwards input to a shared terminal; the helper cannot authenticate which keyboard entered a command. The pointer check is local to the Mac but is not proof of who typed. Leave the page hidden if someone else is operating that same tile.

Resize resumption requires a fresh input report from the original, still-focused anchor after the divider settles. A matching frame or a pointer returning to the same screen coordinates is not enough. A known tab, owner, Space, sibling or pinned-window change cancels the pause. An uncompleted drag is abandoned after 45 seconds; no page is visible while waiting. Like `show`, terminal input is shared; it does not identify the physical device that sent it.

If another app viewer can send input to this session, mouse motion never resumes the preview. To open a new peek the local user must read a one-time code from the Mac menu bar and type it into the original Rex tile. The code is not sent to the shared terminal or the page. Crossing the original deadline discards pending input and rotates the code. If the menu item is not visible, leave the page hidden; the pause ends automatically. No macOS permission is needed.

After a guarded divider interaction, canceling and typing `show` again does not bypass local confirmation: follow the tile’s menu-code prompt. The initial `show` on a newly launched helper remains a shared-terminal action. If several resize updates arrive together, Milo can retain a hidden drag but requires the code rather than assuming the intervening focus was unchanged.

## Selecting an element for an agent

Use **Select** in the preview and click the element. Review the structural path in Milo’s controls, then choose **Copy** or **Cancel**. Copying does not paste into a terminal, contact an agent or run a command. Paste it yourself if you want to share it. The picker returns a bounded path derived from tag kinds and sibling positions, not DOM content, attributes, input values, cookies, source files or screenshots. Navigation invalidates an outstanding selection.

Resizing cancels an outstanding pick or copy confirmation. If the tile cannot fit distinct review and copy controls, enlarge it to enable selection.

Milo’s picker is first-party code under this repository’s MIT license. Nothing is downloaded at runtime. It uses WebKit’s isolated content world in the explicitly opened loopback origin's main frame. The preview uses a nonpersistent WebKit data store, restricts navigation to the initial origin and does not open external windows. This is for your own dev server, **not a network sandbox for hostile applications**.

Serve the page, assets and hot-reload WebSocket from the same origin (scheme, host and port). If a framework serves its assets from a second port, proxy them through the dev server; Milo will not discover or allowlist that port automatically.

## What has been verified

The installed Apple Silicon Rex `0.1.0-preview.21` (Mac build 2061, commit `202b1b3fc0219883112258a8d0f9f95e2d47daa9`, control API revision 18) advertises terminal, onboarding and two tunnel flavors, not a browser block. The [official preview feed](https://blob.superlogical.com/public/rex-app/preview-arm64/appcast.xml), [server API](https://www.superlogical.com/rex/docs/automate/server-api), [Lua reference](https://www.superlogical.com/rex/docs/reference/lua), [Mac actions](https://www.superlogical.com/rex/docs/reference/mac-actions) and [Rust SDK](https://github.com/alpha-og/rexdk) distinguish server-created blocks from client UI. The detailed [beta changelog](https://www.superlogical.com/rex/docs/beta/community-mcp) requires membership. The overlay is a Milo design, **not a claim that Rex has added a browser API**.

* A human verified the page over **Rex.app** on the stack-fix build: it appeared and accepted clicks, typing and navigation. That build dismissed the page at the divider. Physical divider resizing and keyboard focus return to the original shell are separate acceptance checks.
* The disposable Rex layout lab checks all four tiled placements in the same tab with a sidebar present, divider changes, zoom, tab switches, server-side focus return and observer shutdown. It uses a simulated Unix viewer, not the Mac client's pixels or keyboard focus.
* With **one Mac-like Unix app connection attached to four live Rex sessions**, the same lab opened a right tile without moving focus. The observer stayed hidden for a different owner, reported guarded eligibility only for the focused, owned tile, hid on tab switch and restored eligibility (not the page) on return. It exited when the working terminal closed. Attaching another session during an ordinary preview did not change the originating session's layout revision.
* The corrected coordinator accepted a command typed across three deliberately late 700 ms samples on a real Rex PTY. A separate 700 ms delay during first WebKit construction did not require a second command; an offscreen snapshot confirmed the loopback page painted. The same test discarded a partial command across **real focus loss**, concealed on a Rex tab switch and divider move, and produced one rejection rather than a prompt loop when the modeled window stack was wrong.
* A disposable macOS PTY exercised the native confirmation reader: input queued before eligibility, a partial line and a second `show` could not open or extend a peek. The native lease fixture exercised pointer and expiry decisions; it did not move the physical pointer or switch the Mac app's displayed session.
* In the earlier confirmation build, a controlled 700 ms gap in placement samples reset the terminal reader. In a real Rex-created foreground, canonical PTY, the first part of a command typed across that gap was flushed. A complete command in the same PTY reached confirmation and created the WebKit panel, but a late sample before ordering could clear it without displaying the page. These are two distinct paths to the same symptom; the old terminal prompt could not distinguish them. The sampler timing was controlled, not recorded from a user's window.
* In a disposable AppKit panel, the production WebKit view loaded a real loopback server, accepted native keyboard/form input and hot-reloaded after a source change without a full navigation. The native smoke test exercises back/reload and select/review/cancel/copy. This tests the browser, not its registration over Rex.
* The packaged dashboard was exercised at desktop and narrow widths; its placement setting survived save and restart. Linux rejected the command without opening a browser.
* The human-test fixture loaded and navigated in the actual Milo `WKWebView` and reported its new viewport after a native resize from 510 to 350 points wide. This was a disposable nonactivating host, **not** visual acceptance over Rex.app.
* Two disposable macOS processes placed a nonactivating panel above a background fixture window using the host’s AppKit window number. A separate Quartz PID and z-order check confirmed their relative order. An exact Quartz ID lookup found an offscreen window and returned no match for the terminated peer’s window. A closed-but-retained native window can remain in the inventory; closing a Rex anchor also ends its observer.
* The native key-equivalent fixture and the Rex focus RPC cover the two ends of focus return separately. Returning from the WebKit page and typing in the original Rex shell still needs human acceptance.

### Component measurements, not an end-to-end benchmark

Apple Silicon, release-compiled helper component, local Bun dev page. Time to interactive page: 561 ms on the first measured run, then 313, 315 and 282 ms. Host RSS was 104–105 MiB. Across three five-second idle samples the host used 0.018–0.019 CPU seconds (about 0.4% of one core). In a separate four-second sample, the newly created WebContent, Networking and GPU workers that exited with the fixture totalled 66.8 MiB RSS and 0.02 CPU seconds. RSS can count shared pages more than once. These numbers exclude Rex and pane tracking; they are **not** measurements of the complete overlay. In 25 paired warm invocations, `milo --version` took a median 3.97 ms before and 3.93 ms after adding the helper.

In the approximate-placement build’s WebKit component, a Bun dev server became interactive in 341 ms in one run. Editing a source module triggered hot reload and preserved the draft field. Host RSS was 103.2 MiB; five idle seconds consumed 0.0052 CPU seconds. This excludes separate WebKit workers, the Rex observer and the native window sampler.

In one six-second idle sample with a right split, the isolated Rex observer used 0.01 CPU seconds with one attachment and 0.01 with four; its ending RSS was 38.6 and 41.6 MiB respectively. Rex server CPU was 0.03 and 0.04 seconds. Refocusing the guarded tile took 49 ms through observing eligibility in the four-session fixture. These samples exclude the Mac app, Quartz and WebKit. They measure neither time to first paint nor whole-preview idle cost.

In a separate nonactivating fixture, the sampler pinned a real macOS window by ID. Resizing and moving it changed the estimated right pane from 242 × 374 to 302 × 434 points. A second fixture window suppressed the result; closing the original did not reassign its pin. Twenty samples took 0.0170 process CPU seconds over 5.13 seconds. The measurement included window enumeration and the current Rex appearance preferences, but considered only the fixture’s windows for placement. It does not measure occlusion over Rex.

### Reproducing the checks

From a checkout, run the production browser smoke test and the isolated Rex layout lab separately:

```sh
sh macos/Preview/tests/smoke.sh
cargo test --locked --test rex_lab preview_layout_and_observer -- --exact --ignored --nocapture
cargo test --locked --test rex_lab preview_tick::preview_confirmation_uses_rex_pty -- --exact --ignored --nocapture
```

The confirmation lab combines a real Rex server, a four-session Unix viewer fixture, the terminal's actual input and the native coordinator. It uses an offscreen `WKWebView` and models window stacking, foreground and pointer position. A verified page pixel there is not an on-screen Rex screenshot; actual keyboard focus, palette stacking and physical display movement remain separate checks.

In a separate two-process check with transparent, click-through fixture windows on two 1× displays, AppKit reported the ordered panel visible and on the active Space while Quartz still listed only the pinned target. The first snapshot showing the panel above the target arrived 9–27 ms after ordering in four trials. This isolates a post-order race. The earlier refusal over Rex.app did not record the failed step, so its cause remains unknown. Milo allows **one more sample** only for an absent first panel record, with the original pin, geometry, visibility and consent still valid. The panel remains transparent and cannot receive input while waiting. A panel behind Rex, an obstruction, a changed frame, an invalid Space or a second missing record is not accepted.

### Human acceptance: start with the right split

From the checkout, serve just the supplied page (not your project files) in one shell:

```sh
site=$(mktemp -d)
cp tests/preview-tile.html "$site/index.html"
python3 -m http.server 0 --bind 127.0.0.1 --directory "$site"
```

Use the printed loopback URL in a fresh **Mac-created Rex terminal**: `/absolute/path/to/milo preview http://127.0.0.1:PORT/ --placement right`. Use `Ctrl-C` in the server shell when finished.

For the multi-session check, leave your other sessions attached. Before confirming, verify that only terminal text is visible. Confirm once, move the pointer outside and wait for the page to disappear, then bring it back: the page should stay hidden. Confirm again, leave the pointer inside and wait at least 12 seconds: the page should hide without further input. To isolate **tab switching** from pointer exit, start another peek without clicking into the page, leave the pointer still and use Rex's keyboard shortcut to switch tabs **within the same session** and immediately back. Check that the page stays hidden until a fresh `show`. Record any exposure on the other tab.

For the right-divider check, navigate to **Second page**, enter a phrase, and increment the click counter. Drag the divider, release on it, then return inside the preview tile. Check that the page and input survive and the viewport width changes. Repeat with a drag longer than 12 seconds: moving back alone must not display the page. Type the new Mac menu-bar code into the tile and press Return. Switch tabs during another drag and verify the page cannot resume from the old drag.

If the tile prints `not shown`, resolve the stated reason, keep the pointer inside the focused tile, then type a **new** `show` and press Return. A rejected command is never retried automatically. A `confirmation accepted` line distinguishes receipt by the terminal from successful placement; `preview visible` means the helper presented the panel, not that the page finished loading.

For a stacking refusal, the one `not shown` line names the failed step: `pre-order` for the initial Quartz check, `order` for AppKit placement, `post-order` for verification of the clear panel, or `reveal` for the final check before showing the page. Report that line; no window titles, screenshots, URL, page text or permission change are needed to diagnose the step.

1. Start with one ordinary Rex window. Compare all four green page edges, the preview toolbar, the Rex divider and pane header. Drag the divider in both directions; resize and move the window. Record any offset or incorrect overlap. Repeat with and without the sidebar. **Passing real resize and tab switching is a merge gate**, not a prerequisite for trying this build.
2. Type into the fixture, click the counter and navigate. Return keyboard focus to the original shell and type there. Select an element, cancel, then select again and copy. For hot reload, repeat with your own same-origin dev server and edit its source; the static geometry fixture does not implement hot reload.
3. Resize and zoom the tile. Switch tabs, open a second native window and cover Rex with another app: check that the preview hides. Separately try a same-window session or host switch and an in-window palette; record any exposure, including time to hide, rather than assuming these have a detectable window event. Check toolbar, sidebar and header variants, physical displays and Spaces.
4. Close the anchor, window and session separately; check that both the helper and its Rex observer exit. Compare startup, WebKit children, helper and Rex CPU/RSS against the same window without the preview. Capture only the disposable project.
