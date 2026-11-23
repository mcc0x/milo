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
4. Leave the working shell focused. Once Rex publishes the local tile and Milo verifies its placement, the page appears. You can type in the shell, leave the pointer there, or drag the Rex divider. None of those actions dismisses the page.
5. **Close the preview before switching sessions or hosts in the same Mac window.** Several sessions can stay attached; Rex does not reliably identify which one is painted. The overlay can remain over another session indefinitely. See [window ownership and permissions](#window-ownership-and-permissions).

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

Before splitting, Milo matches the terminal owner to the local Rex app PID and pins its single eligible visible native window ID using [Core Graphics window metadata](https://developer.apple.com/documentation/coregraphics/cgwindowlistcopywindowinfo(_:_:)). The helper never captures pixels, reads terminal input or scrollback, asks for Accessibility, or chooses a replacement window.

Pinning identifies a window; it does not authorize showing a page. A foreign surface over Rex’s titlebar or sibling terminal does not prevent the split. After Rex publishes the intended tile, Milo checks the projected tile against the window stack. If the tile starts covered, its anchor waits before showing WebKit. If a loaded page becomes covered, Milo hides it without discarding its URL or form. In either case it rechecks the frame and stacking before showing the page automatically. A second visible normal Rex window is still ambiguous; Milo never chooses between them by stacking order.

The Rex observer follows the published tile in the original session and tab. It sends normalized bounds when the divider moves. The helper maps those fractions into the pinned window using Rex’s [appearance preferences](https://www.superlogical.com/rex/docs/reference/mac-settings): sidebar side and width, tab bar location, toolbar behavior, pane headers, density and terminal padding. It leaves room for the sidebar whether collapsed or expanded. Header, gap and toolbar dimensions are estimates. The toolbar says **Approximate**. Unconfirmed ownership, an inactive tab, zoom, a higher visible Rex layer or an unsupported window configuration suppresses the overlay.

Milo checks Rex ownership once a second while observing. A silent observer cannot keep a visible panel authorized indefinitely: its last authorization expires after three seconds. A confirmed loss of the pinned Quartz window ends the helper; temporary occlusion, an offscreen window, or unavailable window metadata does not transfer the pin.

If the Mac’s clock moves backward far enough to make queued observations ambiguous, Milo keeps the page loaded but hides it. It waits for the clock to pass its previous time and for new geometry evidence before showing it again. Closing and reopening the preview starts a new observation.

**What a pin cannot tell Milo:** [`client.list`](https://www.superlogical.com/rex/docs/reference/api#clientlist) reports attachments. `session.view` reports the active tab and focused block **within each session**. Neither identifies the session painted in the pinned native window. Other sessions can retain a focused block simultaneously. The Mac's `session.select` action changes the displayed session; it is not a query. Milo checks only the original server; it neither queries other hosts nor selects a session on your behalf.

One visible window is a placement requirement, not proof of the displayed session. If Rex draws a palette within that window, or changes the displayed session or host without changing the original session's active tab, there may be no observable change in window geometry, order or ownership. **The pane can remain visible and interactive over unrelated content until closed.** Use it only in a local window you control. Close it before switching hosts or sessions or opening an in-window palette. Do not rely on the pointer or a timeout to hide it.

Divider changes update the same WebKit view. During continuous movement the panel can stay hidden until the divider pauses or settles long enough to verify the new frame and window order. It then returns automatically, including if the pointer is still on the divider. The URL, history and form are not reset. Closing the anchor is different: it ends the preview rather than moving the page elsewhere.

## Selecting an element for an agent

Use **Select** in the preview and click the element. Review the structural path in Milo’s controls, then choose **Copy** or **Cancel**. Copying does not paste into a terminal, contact an agent or run a command. Paste it yourself if you want to share it. The picker returns a bounded path derived from tag kinds and sibling positions, not DOM content, attributes, input values, cookies, source files or screenshots. Navigation invalidates an outstanding selection.

Resizing cancels an outstanding pick or copy confirmation. If the tile cannot fit distinct review and copy controls, enlarge it to enable selection.

Milo’s picker is first-party code under this repository’s MIT license. Nothing is downloaded at runtime. It uses WebKit’s isolated content world in the explicitly opened loopback origin's main frame. The preview uses a nonpersistent WebKit data store, restricts navigation to the initial origin and does not open external windows. This is for your own dev server, **not a network sandbox for hostile applications**.

Serve the page, assets and hot-reload WebSocket from the same origin (scheme, host and port). If a framework serves its assets from a second port, proxy them through the dev server; Milo will not discover or allowlist that port automatically.

## What has been verified

The installed Apple Silicon Rex `0.1.0-preview.21` (Mac build 2061, commit `202b1b3fc0219883112258a8d0f9f95e2d47daa9`, control API revision 18) advertises terminal, onboarding and two tunnel flavors, not a browser block. The [official preview feed](https://blob.superlogical.com/public/rex-app/preview-arm64/appcast.xml), [server API](https://www.superlogical.com/rex/docs/automate/server-api), [Lua reference](https://www.superlogical.com/rex/docs/reference/lua), [Mac actions](https://www.superlogical.com/rex/docs/reference/mac-actions) and [Rust SDK](https://github.com/alpha-og/rexdk) distinguish server-created blocks from client UI. The detailed [beta changelog](https://www.superlogical.com/rex/docs/beta/community-mcp) requires membership. The overlay is a Milo design, **not a claim that Rex has added a browser API**.

* A human verified **clicking, typing and navigation over Rex.app** on an earlier build. Physical dragging on the previous builds dismissed the page. Those results do not verify divider or focus behavior on the persistent implementation.
* The browser component has been exercised in a disposable AppKit panel against a real loopback dev server: keyboard input, navigation, source hot reload retaining a draft, and select/review/cancel/copy. The packaged settings UI was checked at desktop and narrow widths; the saved placement survived restart. Linux refused the preview without opening a browser.
* The layout/observer lab opened each of the four placements beside the working terminal with a sidebar present. Separately, the **visible WebKit test used a right split** and one Unix app peer attached to four real Rex sessions. The sibling PTY executed commands before, during and after a drag. An AppKit divider responder streamed measured positions into `session.resize_pane`; the drag started outside the old page and lasted over 13 seconds. The same WebKit view retained the form, navigation history and URL, with a wider CSS viewport.
* With the test pointer parked in the sibling, the page remained visible throughout a separate 13-second idle interval. The original Rex tab disappearing, an overlapping fixture window and loss of foreground suspended the panel; it restored without input. A replacement window did not acquire the pin, and closing the original working block ended the helper and observer.
* In one four-session run, a separate nonactivating AppKit window was placed over the sibling, the titlebar and the intended right tile. All three preflights returned the same Rex window ID. The split started with the tile covered: no panel was ordered and input was refused. Moving the cover to the sibling revealed the page; titlebar overlap left it visible. Covering the loaded tile again and moving the cover away restored the same view, URL, history and draft before the divider and idle checks. This exercises the native sampler, not the tester’s desktop. Occlusion is sampled, not a synchronous notification of every window movement.
* This test used real nonactivating AppKit windows, Quartz order and a loopback `WKWebView`. It modeled the Mac viewer identity, foreground, chrome and parked pointer and filtered the stack to its own windows. It did **not** move a physical pointer, drive the Rex.app divider, or capture a user's desktop.
* Physical acceptance of the persistent panel in Rex.app is separate from the disposable fixtures below. No screenshot of an offscreen WebKit view establishes registration over Rex. Actual client focus return, palettes, Spaces and mixed-scale display movement still need manual verification.

### Persistent right-split fixture measurement

One Apple Silicon run with sustained divider movement: 1.43 s from creating the Rex split to visible WebKit; 1.88 s to the navigated form with a draft. Across 13.35 s idle, the native fixture used 0.052 CPU seconds (0.39% of one core); RSS was 103.0 MiB at both endpoints. These include test instrumentation and exclude Rex.app, the server, observer and separate WebKit workers. They are not whole-application measurements.

### Earlier component measurements, not an end-to-end benchmark

Apple Silicon, release-compiled helper component, local Bun dev page. Time to interactive page: 561 ms on the first measured run, then 313, 315 and 282 ms. Host RSS was 104–105 MiB. Across three five-second idle samples the host used 0.018–0.019 CPU seconds (about 0.4% of one core). In a separate four-second sample, the newly created WebContent, Networking and GPU workers that exited with the fixture totalled 66.8 MiB RSS and 0.02 CPU seconds. RSS can count shared pages more than once. These numbers exclude Rex and pane tracking; they are **not** measurements of the complete overlay. In 25 paired warm invocations, `milo --version` took a median 3.97 ms before and 3.93 ms after adding the helper.

In the approximate-placement build’s WebKit component, a Bun dev server became interactive in 341 ms in one run. Editing a source module triggered hot reload and preserved the draft field. Host RSS was 103.2 MiB; five idle seconds consumed 0.0052 CPU seconds. This excludes separate WebKit workers, the Rex observer and the native window sampler.

In a separate nonactivating fixture, the sampler pinned a real macOS window by ID. Resizing and moving it changed the estimated right pane from 242 × 374 to 302 × 434 points. A second fixture window suppressed the result; closing the original did not reassign its pin. Twenty samples took 0.0170 process CPU seconds over 5.13 seconds. The measurement included window enumeration and the current Rex appearance preferences, but considered only the fixture’s windows for placement. It does not measure occlusion over Rex.

### Reproducing the checks

From a checkout, run the production browser smoke test and the isolated Rex layout lab separately:

```sh
sh macos/Preview/tests/smoke.sh
cargo test --locked --test rex_lab preview_layout_and_observer -- --exact --ignored --nocapture
cargo test --locked --test rex_lab preview_tick::persistent_preview_coexists_with_shell -- --exact --ignored --nocapture
```

The divider fixture sends drag and release events through AppKit’s `NSSplitView` responder in its own process. It never moves the system pointer. The layout lab uses a disposable Rex server and Mac-identifying Unix app peer; it does not drive a user's Rex.app.

### Human acceptance: start with the right split

From the checkout, serve just the supplied page (not your project files) in one shell:

```sh
site=$(mktemp -d)
cp tests/preview-tile.html "$site/index.html"
python3 -m http.server 0 --bind 127.0.0.1 --directory "$site"
```

Use the printed loopback URL in a fresh **Mac-created Rex terminal**: `/absolute/path/to/milo preview http://127.0.0.1:PORT/ --placement right`. Use `Ctrl-C` in the server shell when finished.

1. Keep the other sessions attached. Navigate to **Second page**, enter a phrase and increment the counter. Click the sibling shell, park the pointer there, type a command and wait at least 15 seconds. The page should stay visible without stealing the keyboard.
2. Starting on the Rex divider **outside the old page**, drag in both directions. Try a long drag and release on the divider. The page should follow without a click or command, retain the phrase and counter, and report its new viewport width. Compare its green edges with the Rex header, sidebar and divider.
3. Switch to a different **tab within the original session** and back. The page should hide on that tab and return with its state intact. Try covering the tile, zooming another block and moving the pinned window. Check for stale pixels and an overlay intercepting input outside the tile. Never use a sensitive page or an existing project to test these boundaries.
4. Use **Return to terminal** after typing in the page, type in the shell, and try the picker’s Cancel and Copy separately. Close the anchor and confirm the helper and observer exit. Repeat with the other placements and chrome variants; test different displays and Spaces separately. Physical right-divider resizing and tab switching in Rex.app remain merge gates.
