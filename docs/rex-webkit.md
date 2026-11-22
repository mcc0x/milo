# Local WebKit preview (experimental, macOS)

**Release blocker:** there is no permission-free placement implementation. If the launcher's Accessibility preflight fails, Milo creates no pane. If the live helper loses access after the anchor is created, it stops; the terminal may briefly show a diagnostic. Do not grant Rex or its server Accessibility as a workaround.

Start your dev server first. In its **local Rex tab**, run:

```sh
milo preview http://localhost:3000
```

Rex owns the terminal anchor, divider and layer. A small Milo helper owns a borderless AppKit panel containing a real `WKWebView`. The page is not rendered into a bitmap, embedded in the settings dashboard or opened in the default browser. The command never searches for servers, picks a port, changes Rex settings or navigates an existing browser tab. Linux, SSH and remote sessions are not supported.

### Launching from Rex for Mac

1. In **Rex.app on the Mac**, select the local host and use [New Tab (⌘T)](https://www.superlogical.com/rex/docs/reference/mac-actions#action-client-tab-new). Use a new disposable tab for testing. This is a **Mac client action**, not `rex window new` in a shell.
2. In the new tab’s **main working terminal**, `cd` to the project. Start the dev server if needed. Run `milo preview http://localhost:3000` there. When testing a PR build, use its absolute binary path in place of `milo`; no installation is needed.
3. If Milo says the terminal **isn’t owned** by a local Mac client, stop before troubleshooting Accessibility. A connected Mac, an active tab, or a Mac-owned sidebar does **not** establish ownership of the working block. An attached phone need not be disconnected.

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

## Placement

The default is **Right** of the main working terminal, not right of the sidebar. Choose **Preview placement** in `milo setup` → Workflow or `milo web` → Panes → Preview. These edit the same preference in the existing settings scopes. To try another placement once, without saving anything:

```sh
milo preview http://localhost:3000 --placement left
```

The four placements are tiled splits in the same tab. A saved `preview_placement = floating` from an experimental build resolves to **Right** without rewriting the file on read. New commands and settings edits accept only `right`, `left`, `above` or `below`. An ambiguous working terminal or viewer causes a refusal.

Click **Return to terminal** or press **Command-Shift-Return** while editing the page to return to the working terminal. **Command-W** in the focused preview closes it. The helper hides when Rex is not the frontmost application.

## Window ownership and permissions

Milo does not patch Rex or install a plugin. On macOS the Cargo build compiles a separate system-framework helper, embeds it in `milo` and installs that exact helper in the private macOS user cache on demand. The terminal CLI does not link AppKit or WebKit. No separate package, JavaScript runtime or browser extension is required.

The helper needs macOS **Accessibility** access to measure a pane. Milo checks access without prompting. There is no Screen Recording requirement for the preview itself. Rex layout events identify the visible block, not its screen coordinates. A random marker written in the anchor's terminal, the local viewer PID, the focused accessibility window and macOS window order must agree before the helper may draw. If Rex does not expose the required accessibility information, the view stays hidden. There is no fallback to a guessed rectangle. Moving between displays requires fresh proof; windows straddling or on mirrored displays are intentionally unsupported.

Adding the cached executable to Accessibility did **not** authorize a real Rex-shell launch in testing. The helper uses Apple’s nonprompting [`AXIsProcessTrusted`](https://developer.apple.com/documentation/applicationservices/1460720-axisprocesstrusted); this tells it whether the **calling process** is trusted, not which executable to add to Settings. Stop on denial. Do not keep adding executables, restart Rex or grant a terminal or server access.

The build embeds a bare executable, not an independently launched `.app`. In a private copy of the arm64 release build on macOS 27.2.1, `codesign --verify --strict` passed, but the signing identifier was `Milo Preview`, the signature was linker-generated ad hoc, the embedded Info.plist was not bound, there was no Team ID, and the designated requirement was a code hash. An isolated `--check-access` returned 77 without opening a window or prompting. Neither result identifies the responsible process on another Mac. A stable cache path or plist identifier is not a stable signing identity; see [Apple TN3127](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements).

If an Accessibility-based design is revisited, the candidate for helper-only consent is an independently user-opened, consistently Developer ID–signed app in a durable location. That **same live process** would check trust and measure the pane; the Rex-launched child must not borrow its grant or expose a general Accessibility service. Before offering this onboarding, verify in a clean disposable account that only the helper is authorized, denial creates no pane, an update preserves identity and revocation hides the overlay. This is a proposal, not an available workaround.

**What Accessibility reads:** to find the marker, the helper compares the first 32 visible characters of candidate terminal text areas in the focused Rex window. It does not request the whole terminal value, persist those prefixes or send them to the dev page, agent or clipboard.

**Overlay limits:** Rex and the panel are separate processes, so changes are not atomic. A tab, palette or moving divider can change between two observations. The helper hides when it detects uncertainty and waits for stable bounds before showing again. Rex UI drawn inside the pane without a corresponding accessibility element cannot be reliably detected with this method. Native full-screen placement is declined; palette safety and focus while the panel is key remain GUI acceptance gates, not guarantees.

### Why a pinned native window is not enough

On Rex `0.1.0-preview.21` (API 18), a disposable server reported a right-split leaf rectangle of `[0.5, 0, 0.5, 1]`. The same local Unix app peer owned both working terminals in two sessions, each with its own `active_window_id`. These fields identify a block within its Rex session, not the session displayed in a macOS window.

A disposable AppKit client fixture used that same peer PID and gave the observer its precise `CGWindowID` up front. No Accessibility calls, screen capture, window titles or pixels were used. `CGWindowListCopyWindowInfo` distinguished two overlapping native windows in z-order and reported when the pinned window went away. But its PID, window number, bounds and order, and the two Rex session views, **did not change** when the fixture:

* added a 100-point sidebar (the actual right body narrowed from 300 to 250 points);
* revealed a 30-point header (the body height changed from 360 to 330 points);
* added an overlapping sibling view to model a palette, or hid the body to model a same-window session switch. The fixture did not render a second Rex session.

These were measured on-screen **fixture views**, not observations of Rex.app rendering a palette. Rex’s [Mac settings](https://www.superlogical.com/rex/docs/reference/mac-settings) include hover-revealed headers, toolbar placement and a sidebar that can resize panes while preserving window width. Preferences describe behavior, not the current pane viewport or transient overlays. Even an already correctly pinned window can contain the wrong surface. Milo does not use the first window for a PID, guess offsets, or ship this experiment as a fallback.

The missing client signal is a read-only presentation snapshot and invalidation event: local client, currently displayed host/session/tab, native window ID, block ID, clipped pane-body rectangle with coordinate space, and whether overlapping client chrome makes it unsafe to draw. The overlay must hide on ambiguity, a lost window or loss of that proof. Actual Rex placement, session switching, palette occlusion and multi-display movement remain manual acceptance gates.

## Selecting an element for an agent

Use **Select** in the preview and click the element. Review the structural path in Milo’s controls, then choose **Copy** or **Cancel**. Copying does not paste into a terminal, contact an agent or run a command. Paste it yourself if you want to share it. The picker returns a bounded path derived from tag kinds and sibling positions, not DOM content, attributes, input values, cookies, source files or screenshots. Navigation invalidates an outstanding selection.

Milo’s picker is first-party code under this repository’s MIT license. Nothing is downloaded at runtime. It uses WebKit’s isolated content world in the explicitly opened loopback origin's main frame. The preview uses a nonpersistent WebKit data store, restricts navigation to the initial origin and does not open external windows. This is for your own dev server, **not a network sandbox for hostile applications**.

Serve the page, assets and hot-reload WebSocket from the same origin (scheme, host and port). If a framework serves its assets from a second port, proxy them through the dev server; Milo will not discover or allowlist that port automatically.

## What has been verified

The installed Apple Silicon Rex `0.1.0-preview.21` (Mac build 2061, commit `202b1b3fc0219883112258a8d0f9f95e2d47daa9`, control API revision 18) advertises terminal, onboarding and two tunnel flavors, not a browser block. The [official preview feed](https://blob.superlogical.com/public/rex-app/preview-arm64/appcast.xml), [server API](https://www.superlogical.com/rex/docs/automate/server-api), [Lua reference](https://www.superlogical.com/rex/docs/reference/lua), [Mac actions](https://www.superlogical.com/rex/docs/reference/mac-actions) and [Rust SDK](https://github.com/alpha-og/rexdk) distinguish server-created blocks from client UI. The detailed [beta changelog](https://www.superlogical.com/rex/docs/beta/community-mcp) requires membership. The overlay is a Milo design, **not a claim that Rex has added a browser API**.

* The disposable Rex layout lab checks all four tiled placements in the same tab with a sidebar present, divider changes, zoom, tab switches, server-side focus return and observer shutdown. It uses a simulated Unix viewer, not the Mac client's pixels or keyboard focus.
* In a disposable AppKit panel, the production WebKit view loaded a real loopback server, accepted native keyboard/form input and hot-reloaded after a source change without a full navigation. The native smoke test exercises back/reload and select/review/cancel/copy. This tests the browser, not its registration over Rex.
* The packaged dashboard was exercised at desktop and narrow widths; its placement setting survived save and restart. Linux rejected the command without opening a browser.
* A real, undisplayed disposable Rex session refused preview without creating another block. On the two-display macOS test host, Accessibility and Screen Recording were not granted and Rex Remote Control was off. No privacy settings were changed. **No integrated Rex screenshot or successful GUI placement is claimed.**

### Component measurements, not an end-to-end benchmark

Apple Silicon, release-compiled helper component, local Bun dev page. Time to interactive page: 561 ms on the first measured run, then 313, 315 and 282 ms. Host RSS was 104–105 MiB. Across three five-second idle samples the host used 0.018–0.019 CPU seconds (about 0.4% of one core). In a separate four-second sample, the newly created WebContent, Networking and GPU workers that exited with the fixture totalled 66.8 MiB RSS and 0.02 CPU seconds. RSS can count shared pages more than once. These numbers exclude Rex and pane tracking; they are **not** measurements of the complete overlay. In 25 paired warm invocations, `milo --version` took a median 3.97 ms before and 3.93 ms after adding the helper.

### Reproducing the checks

From a checkout, run the production browser smoke test and the isolated Rex layout lab separately:

```sh
sh macos/Preview/tests/smoke.sh
cargo test --locked --test rex_lab preview_layout_and_observer -- --exact --ignored --nocapture
```

The Rex lab uses a real server and a simulated Unix viewer. It cannot verify pixels, native keyboard focus, palette stacking or physical display movement.

### Acceptance check on a permitted Mac

Use a disposable Rex window and project, with a real server showing an editable field and a visibly changing source string. Do not use an existing work tab. Exercise the following before treating the feature as supported:

1. Start with a right split beside the main shell, with Accessibility and Screen Recording off. A refusal is not a placement pass. On a build with verified client presentation data, check all four directions with and without a Milo sidebar; confirm that the WebKit bounds exclude the divider and header.
2. Navigate, type, scroll and change source on disk. Confirm hot reload preserves the field. Select an element, cancel, then select again and copy. Return keyboard focus to the original shell and type into it.
3. Resize and zoom the tile. Switch tabs and sessions in the same window, then open a second native window. Reveal and hide the sidebar, toolbar, header and Rex palette. Cover Rex with another app and move the window within and between displays and Spaces. The helper must hide rather than cover another surface when ownership is uncertain.
4. Close the anchor, window and session separately; check that both the helper and its Rex observer exit. Compare startup, WebKit children, helper and Rex CPU/RSS against the same window without the preview. Capture only the disposable project.
