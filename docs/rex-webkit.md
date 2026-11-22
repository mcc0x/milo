# Local WebKit preview (experimental, macOS)

Start your dev server first. In its **local Rex tab**, run:

```sh
milo preview http://localhost:3000
```

Rex owns the terminal anchor, divider and layer. A small Milo helper owns a borderless AppKit panel containing a real `WKWebView`. The page is not rendered into a bitmap, embedded in the settings dashboard or opened in the default browser. The command never searches for servers, picks a port, changes Rex settings or navigates an existing browser tab. Linux, SSH and remote sessions are not supported.

## Placement

The default is **Right** of the main working terminal, not right of the sidebar. Choose **Preview placement** in `milo setup` → Workflow or `milo web` → Panes → Preview. These edit the same preference in the existing settings scopes. To try another placement once, without saving anything:

```sh
milo preview http://localhost:3000 --placement left
```

The choices are `left`, `right`, `above`, `below` and `floating`. Floating requests a Rex floating layer **in the same tab**; it does not fall back to a split. An ambiguous main terminal or viewer is not a placement hint: Milo should refuse rather than attach to a different window.

## Window ownership and permissions

Milo does not patch Rex or install a plugin. On macOS the Cargo build compiles a separate system-framework helper, embeds it in `milo` and installs that exact helper in the private macOS user cache on demand. The terminal CLI does not link AppKit or WebKit. No separate package, JavaScript runtime or browser extension is required.

The helper needs macOS **Accessibility** access to measure a pane. Milo checks access without prompting. There is no Screen Recording requirement for the preview itself. Rex layout events identify the visible block, not its screen coordinates. A random marker written in the anchor's terminal, the local viewer PID, the focused accessibility window and macOS window order must agree before the helper may draw. If Rex does not expose the required accessibility information, the view stays hidden. There is no fallback to a guessed rectangle. Moving between displays requires fresh proof; windows straddling or on mirrored displays are intentionally unsupported.

The cached executable is `$(getconf DARWIN_USER_CACHE_DIR)/net.usemilo.preview/Milo Preview`. If access is denied, Milo returns before changing the layout. macOS can attribute a child process’s access to the responsible launcher rather than the helper itself; a grant to Rex or its server may cover **other programs launched through it**. Milo cannot promise a helper-only grant. Leave access off if that scope is inappropriate. The anchor needs at least 32 columns and two rows for its marker.

**What Accessibility reads:** to find the marker, the helper compares the first 32 visible characters of candidate terminal text areas in the focused Rex window. It does not request the whole terminal value, persist those prefixes or send them to the dev page, agent or clipboard.

**Overlay limits:** Rex and the panel are separate processes, so changes are not atomic. A tab, palette or moving divider can change between two observations. The helper hides when it detects uncertainty and waits for stable bounds before showing again. Rex UI drawn inside the pane without a corresponding accessibility element cannot be reliably detected with this method. Palette safety, native full-screen Spaces and focus while the panel is key remain GUI acceptance gates, not guarantees.

If the client does not expose the visible terminal text range or cannot distinguish a palette from the terminal, the robust upstream interface would be a client event keyed by block ID with the owning window, pane-body rectangle, focus and occlusion state. Server-side split ratios alone cannot provide that proof.

## Selecting an element for an agent

Use **Select** in the preview and click the element. Review the structural path in Milo’s controls, then choose **Copy** or **Cancel**. Copying does not paste into a terminal, contact an agent or run a command. Paste it yourself if you want to share it. The picker returns a bounded path derived from tag kinds and sibling positions, not DOM content, attributes, input values, cookies, source files or screenshots. Navigation invalidates an outstanding selection.

Milo’s picker is first-party code under the repository's MIT license. [React Grab](https://github.com/aidenybai/react-grab/tree/23bce0e56f2808902f1126ad581f6d8c3b5f639e/packages/react-grab) was evaluated ([MIT license](https://github.com/aidenybai/react-grab/blob/23bce0e56f2808902f1126ad581f6d8c3b5f639e/LICENSE)); it is **not** bundled or downloaded. The picker uses WebKit’s isolated content world in the explicitly opened loopback origin's main frame. It does not run arbitrary-origin scripts. The preview uses a nonpersistent WebKit data store, restricts navigation to the initial origin and does not open external windows. This is for your own dev server, **not a network sandbox for hostile applications**.

Serve the page, assets and hot-reload WebSocket from the same origin (scheme, host and port). If a framework serves its assets from a second port, proxy them through the dev server; Milo will not discover or allowlist that port automatically.

## What has been verified

The installed Apple Silicon Rex `0.1.0-preview.21` (Mac build 2061, commit `202b1b3fc0219883112258a8d0f9f95e2d47daa9`, control API revision 18) advertises terminal, onboarding and two tunnel flavors, not a browser block. The [official preview feed](https://blob.superlogical.com/public/rex-app/preview-arm64/appcast.xml), [server API](https://www.superlogical.com/rex/docs/automate/server-api), [Lua reference](https://www.superlogical.com/rex/docs/reference/lua), [Mac actions](https://www.superlogical.com/rex/docs/reference/mac-actions) and [Rust SDK](https://github.com/alpha-og/rexdk) distinguish server-created blocks from client UI. The detailed [beta changelog](https://www.superlogical.com/rex/docs/beta/community-mcp) requires membership. The overlay is a Milo design, **not a claim that Rex has added a browser API**.

* A disposable Rex server created and closed all five placements in the same tab with a sidebar present. The layout lab used a simulated Unix viewer and inspected Rex’s actual split trees and floating layer, changed tile ratios, zoomed, moved/resized and minimized/restored the float, switched tabs, returned server-side focus and observed watcher shutdown. This was not a test of the Mac client's pixels or keyboard focus.
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

1. With a Milo sidebar present, open each placement from the working shell and from the sidebar context. Inspect the tab, split parent/divider or floating layer. Compare Rex screenshots to the real page and confirm the sidebar is untouched.
2. Navigate, type, scroll and change source on disk. Confirm hot reload preserves the field. Select an element, cancel, then select again and copy. Return keyboard focus to the original shell and type into it.
3. Resize and zoom the tile. Move, resize and dismiss the float. Switch tabs, open the Rex palette, cover Rex with another app, move the window within and between displays and enter/leave a Space. The helper must hide rather than cover another surface when ownership is uncertain.
4. Close the anchor, window and session separately; check that both the helper and its Rex observer exit. Compare startup, WebKit children, helper and Rex CPU/RSS against the same window without the preview. Capture only the disposable project.
