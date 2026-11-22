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

The preview anchor is separate from its working shell. In a disposable server, the CLI-created split remained unowned after publication until the app peer claimed it. Milo never claims it for the app. If the new tile remains unowned, the observer reports that at its 28-second startup check and stops. A waiting notice or refusal is not a placement pass.

## Placement

The default is **Right** of the main working terminal, not right of the sidebar. Choose **Preview placement** in `milo setup` → Workflow or `milo web` → Panes → Preview. These edit the same preference in the existing settings scopes. To try another placement once, without saving anything:

```sh
milo preview http://localhost:3000 --placement left
```

The four placements are tiled splits in the same tab. A saved `preview_placement = floating` from an experimental build resolves to **Right** without rewriting the file on read. New commands and settings edits accept only `right`, `left`, `above` or `below`. An ambiguous working terminal or viewer causes a refusal.

Click **Return to terminal** or press **Command-Shift-Return** while editing the page to return to the working terminal. **Command-W** in the focused preview closes it. The helper hides when Rex is not the frontmost application.

## Window ownership and permissions

Milo does not patch Rex or install a plugin. On macOS the Cargo build compiles a separate system-framework helper, embeds it in `milo` and installs that exact helper in the private macOS user cache on demand. The terminal CLI does not link AppKit or WebKit. No separate package, JavaScript runtime or browser extension is required.

Before splitting, Milo matches the terminal owner to the local Rex app PID and pins its single eligible visible native window ID using [Core Graphics window metadata](https://developer.apple.com/documentation/coregraphics/cgwindowlistcopywindowinfo(_:_:)). The helper never captures pixels, reads terminal contents, asks for Accessibility, or chooses a replacement window. Its private cache location is separate from the old experimental Accessibility helper.

The Rex observer follows the published tile in the original session and tab. It sends normalized bounds when the divider moves. The helper maps those fractions into the pinned window using Rex’s [appearance preferences](https://www.superlogical.com/rex/docs/reference/mac-settings): sidebar position, width and collapsed state, tab bar location, toolbar behavior, pane headers, density and terminal padding. Header, gap and toolbar dimensions are estimates. The panel’s toolbar says **Approximate**. Unconfirmed ownership, an inactive tab, zoom, a higher visible Rex layer or an unsupported window configuration suppresses the overlay.

Milo checks Rex ownership once a second while observing. A silent observer cannot keep a visible panel authorized indefinitely: its last authorization expires after three seconds. A confirmed loss of the pinned Quartz window ends the helper; temporary occlusion, an offscreen window, or unavailable window metadata does not transfer the pin.

**What a pin cannot tell Milo:** Rex reports attachments, not the session currently displayed by a native window. Milo checks the app connections on the original server for another session attached to the same local Mac process; it never queries other hosts. Switching hosts in the same native window can still be invisible to Milo. A palette drawn within an existing window may not change its metadata either. If the overlay covers the wrong surface, close it. No privacy permission resolves this ambiguity.

## Selecting an element for an agent

Use **Select** in the preview and click the element. Review the structural path in Milo’s controls, then choose **Copy** or **Cancel**. Copying does not paste into a terminal, contact an agent or run a command. Paste it yourself if you want to share it. The picker returns a bounded path derived from tag kinds and sibling positions, not DOM content, attributes, input values, cookies, source files or screenshots. Navigation invalidates an outstanding selection.

Resizing cancels an outstanding pick or copy confirmation. If the tile cannot fit distinct review and copy controls, enlarge it to enable selection.

Milo’s picker is first-party code under this repository’s MIT license. Nothing is downloaded at runtime. It uses WebKit’s isolated content world in the explicitly opened loopback origin's main frame. The preview uses a nonpersistent WebKit data store, restricts navigation to the initial origin and does not open external windows. This is for your own dev server, **not a network sandbox for hostile applications**.

Serve the page, assets and hot-reload WebSocket from the same origin (scheme, host and port). If a framework serves its assets from a second port, proxy them through the dev server; Milo will not discover or allowlist that port automatically.

## What has been verified

The installed Apple Silicon Rex `0.1.0-preview.21` (Mac build 2061, commit `202b1b3fc0219883112258a8d0f9f95e2d47daa9`, control API revision 18) advertises terminal, onboarding and two tunnel flavors, not a browser block. The [official preview feed](https://blob.superlogical.com/public/rex-app/preview-arm64/appcast.xml), [server API](https://www.superlogical.com/rex/docs/automate/server-api), [Lua reference](https://www.superlogical.com/rex/docs/reference/lua), [Mac actions](https://www.superlogical.com/rex/docs/reference/mac-actions) and [Rust SDK](https://github.com/alpha-og/rexdk) distinguish server-created blocks from client UI. The detailed [beta changelog](https://www.superlogical.com/rex/docs/beta/community-mcp) requires membership. The overlay is a Milo design, **not a claim that Rex has added a browser API**.

* The disposable Rex layout lab checks all four tiled placements in the same tab with a sidebar present, divider changes, zoom, tab switches, server-side focus return and observer shutdown. It uses a simulated Unix viewer, not the Mac client's pixels or keyboard focus.
* In a disposable AppKit panel, the production WebKit view loaded a real loopback server, accepted native keyboard/form input and hot-reloaded after a source change without a full navigation. The native smoke test exercises back/reload and select/review/cancel/copy. This tests the browser, not its registration over Rex.
* The packaged dashboard was exercised at desktop and narrow widths; its placement setting survived save and restart. Linux rejected the command without opening a browser.
* The human-test fixture loaded and navigated in the actual Milo `WKWebView` and reported its new viewport after a native resize from 510 to 350 points wide. This was a disposable nonactivating host, **not** visual acceptance over Rex.app.
* Two disposable macOS processes placed a nonactivating panel above a background fixture window using the host’s AppKit window number. A separate Quartz PID and z-order check confirmed their relative order. An exact Quartz ID lookup found an offscreen window and returned no match for the terminated peer’s window. A closed-but-retained native window can remain in the inventory; closing a Rex anchor also ends its observer.
* Native keyboard acceptance is still open: the separate nonactivating smoke panel loaded its loopback page and connected its hot-reload WebSocket, but did not become key in this launch context. The key-equivalent fixture and the Rex focus RPC are not substitutes for typing into a real Rex-hosted preview and returning to the original shell.

### Component measurements, not an end-to-end benchmark

Apple Silicon, release-compiled helper component, local Bun dev page. Time to interactive page: 561 ms on the first measured run, then 313, 315 and 282 ms. Host RSS was 104–105 MiB. Across three five-second idle samples the host used 0.018–0.019 CPU seconds (about 0.4% of one core). In a separate four-second sample, the newly created WebContent, Networking and GPU workers that exited with the fixture totalled 66.8 MiB RSS and 0.02 CPU seconds. RSS can count shared pages more than once. These numbers exclude Rex and pane tracking; they are **not** measurements of the complete overlay. In 25 paired warm invocations, `milo --version` took a median 3.97 ms before and 3.93 ms after adding the helper.

In the approximate-placement build’s WebKit component, a Bun dev server became interactive in 341 ms in one run. Editing a source module triggered hot reload and preserved the draft field. Host RSS was 103.2 MiB; five idle seconds consumed 0.0052 CPU seconds. This excludes separate WebKit workers, the Rex observer and the native window sampler.

In a separate nonactivating fixture, the sampler pinned a real macOS window by ID. Resizing and moving it changed the estimated right pane from 242 × 374 to 302 × 434 points. A second fixture window suppressed the result; closing the original did not reassign its pin. Twenty samples took 0.0170 process CPU seconds over 5.13 seconds. The measurement included window enumeration and the current Rex appearance preferences, but considered only the fixture’s windows for placement. It does not measure occlusion over Rex.

### Reproducing the checks

From a checkout, run the production browser smoke test and the isolated Rex layout lab separately:

```sh
sh macos/Preview/tests/smoke.sh
cargo test --locked --test rex_lab preview_layout_and_observer -- --exact --ignored --nocapture
```

The Rex lab uses a real server and a simulated Unix viewer. It cannot verify pixels, native keyboard focus, palette stacking or physical display movement.

### Human acceptance: start with the right split

From the checkout, serve just the supplied page (not your project files) in one shell:

```sh
site=$(mktemp -d)
cp tests/preview-tile.html "$site/index.html"
python3 -m http.server 0 --bind 127.0.0.1 --directory "$site"
```

Use the printed loopback URL in a fresh **Mac-created Rex terminal**: `/absolute/path/to/milo preview http://127.0.0.1:PORT/ --placement right`. Use `Ctrl-C` in the server shell when finished.

1. Start with one ordinary Rex window. Compare all four green page edges, the preview toolbar, the Rex divider and pane header. Drag the divider in both directions; resize and move the window. Record any offset or incorrect overlap. Repeat with and without the sidebar. **Passing real resize and tab switching is a merge gate**, not a prerequisite for trying this build.
2. Type into the fixture, click the counter and navigate. Return keyboard focus to the original shell and type there. Select an element, cancel, then select again and copy. For hot reload, repeat with your own same-origin dev server and edit its source; the static geometry fixture does not implement hot reload.
3. Resize and zoom the tile. Switch tabs and sessions in the same window, then open a second native window. Reveal and hide the sidebar, toolbar, header and Rex palette. Cover Rex with another app and move the window within and between displays and Spaces. The helper must hide rather than cover another surface when ownership is uncertain.
4. Close the anchor, window and session separately; check that both the helper and its Rex observer exit. Compare startup, WebKit children, helper and Rex CPU/RSS against the same window without the preview. Capture only the disposable project.
