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

* The [maintainer’s physical acceptance baseline](https://github.com/mcc0x/milo/pull/69#issuecomment-7096963561) is the persistent **right split in Rex.app with multiple sessions attached** (`9f37252`; tested executable SHA-256 `484a560f90d19669800b7e3d8c2b5ea6c91a3d39d88d5a66c0075acde6e104fa`). Divider responsiveness remained a performance target. That acceptance does not extend to every placement, display or same-window session switch. An earlier build also passed physical click, typing and navigation.
* The browser component has been exercised in a disposable AppKit panel against a real loopback dev server: keyboard input, navigation, source hot reload retaining a draft, and select/review/cancel/copy. The packaged settings UI was checked at desktop and narrow widths; the saved placement survived restart. Linux refused the preview without opening a browser.
* The layout/observer lab opened each of the four placements beside the working terminal with a sidebar present. Separately, the **visible WebKit test used a right split** and one Unix app peer attached to four real Rex sessions. The sibling PTY executed commands before, during and after a drag. An AppKit divider responder streamed measured positions into `session.resize_pane`; the drag started outside the old page and lasted over 13 seconds. The same WebKit view retained the form, navigation history and URL, with a wider CSS viewport.
* With the test pointer parked in the sibling, the page remained visible throughout a separate 13-second idle interval. The original Rex tab disappearing, an overlapping fixture window and loss of foreground suspended the panel; it restored without input. A replacement window did not acquire the pin, and closing the original working block ended the helper and observer.
* In one four-session run, a separate nonactivating AppKit window covered three parts of a borderless host fixture: the sibling, the modeled titlebar region and the intended right tile. Preflight returned the same host window ID in all three positions. The split started with the tile covered: no panel was ordered and the input gate refused interaction. Moving the cover to the sibling revealed the page; overlap in the modeled titlebar region left it visible. Covering the loaded tile again and moving the cover away restored the same view, URL, history and draft before the divider and idle checks. Occlusion is sampled, not a synchronous notification of every window movement.
* This test used real nonactivating AppKit windows, Quartz order and a loopback `WKWebView`. It modeled the Mac viewer identity, foreground, chrome and parked pointer and filtered the stack to its own windows. It did **not** move a physical pointer, drive the Rex.app divider, or capture a user's desktop.
* Fixture timings are not physical divider or keyboard latency. Native focus return, palettes, Spaces and mixed-scale display movement still need manual verification.

### Divider performance: accepted baseline vs candidate

Apple M5 Pro, macOS 27.2.1 (26B131), Rex `0.1.0-preview.21` / API 18. Accepted `9f37252` → candidate `334f136`. One counted run per cell: baseline light, candidate light, candidate busy, baseline busy. Both used the same four-session fixture, compiler flags and pages; the busy page contains 420 responsive cards with wrapped text and inline SVGs. Each cell is **before → after**. These are not physical Rex.app measurements.

| Right split | Light page | Busy page |
|---|---:|---:|
| Request → proven, visible native frame, p50 / p95 (24 per build) | 633 / 751 → 59 / 72 ms | 613 / 734 → 57 / 70 ms |
| Native interarrival jitter, p50 / p95 (23 intervals) | 67 / 192 → 10 / 18 ms | 76 / 216 → 7 / 24 ms |
| Last request in 60 Hz burst → visible (one burst per build) | 703 → 187 ms | 736 → 182 ms |
| Split → both visible and first DOM layout received (one launch per build) | 1,370 → 344 ms | 1,367 → 363 ms |
| Sibling CLI send → PTY receipt during burst, p50 / p95 (8 lines) | 22 / 51 → 23 / 49 ms | 23 / 44 → 23 / 44 ms |
| Request → CSS resize notification, p50 / p95 | 507 / 944 → 44 / 948 ms (24 → 24) | 438 / 940 → 64 / 944 ms (24 → 23) |

The baseline waited for successive 250 ms safety ticks to identify, order and reveal a changed tile. The candidate explicitly schedules those proof steps; the periodic safety checks and input gates remain. During a fast drag, the first geometry change revokes the old frame. Repeated changes within 150 ms reuse one timer; after a pause the helper verifies the current frame and window order anew. Both builds exposed **one of 24** requested widths in the 60 Hz burst, the last one. This improves catch-up, not continuous 60 fps dragging.

In the candidate burst, neither page delivered a matching CSS notification inside the two-second passive observation window. Both later reported the final size around the explicit DOM check; that check matched the exposed WebKit viewport but is **not** a passive latency sample. The native WebKit frame change → CSS callback p95 for individual steps **increased** from 627 to 915 ms (light) and 587 to 906 ms (busy). These callbacks include WebKit and main-queue delivery; they do not measure paint or the execution time of WebKit layout.

Rex and the viewer were already running at startup. The layout origin is a synthetic viewer's socket send, not a physical divider event. The fixture matches each unique width within its phase, not an end-to-end revision ID: CSS observations may arrive after the next request or match an equivalent size. Missing notifications are not counts of missed display refreshes. The idle/drag resource windows do not include late CSS work after the fixed tail, and the counted observer synchronously logs each API invocation. Raw helper and observer costs should not be added to reaped-child CPU or treated as total system energy.

| Helper cost | Light: before → after | Busy: before → after |
|---|---:|---:|
| Idle: CPU ms / interrupt wakeups in 6 s | 22 / 97 → 19 / 130 | 21 / 100 → 25 / 139 |
| Individual changes: CPU ms / interrupt wakeups in 23.1 s | 501 / 3,396 → 481 / 2,607 | 536 / 3,181 → 510 / 3,845 |
| Burst including fixed tail: CPU ms / interrupt wakeups in ~1 s | 16 / 57 → 14 / 78 | 11 / 52 → 14 / 77 |

Idle remained at 24 background window samples per six seconds. Individual changes used more samples (93 → 160 light, 93 → 161 busy); no wakeup reduction is claimed. Helper costs include fixture instrumentation, not the WebContent worker or WindowServer. The observer did not change: per layout workload it saw 48 layout events, made 92 view and 92 owner-list calls, wrote 48 geometry and 24 renewal records, and started no subprocess during resize or idle. It ran one shell and one `ps` identity check at startup. The sibling input probe itself starts a Rex CLI for each line.

### Second comparison: keep the physically tested build

The maintainer [reported that resizing feels much better](https://github.com/mcc0x/milo/pull/69#issuecomment-7098003467) on `334f136` (executable SHA-256 `50c497ac5da5fd2378e4cfb292980648134971205efe9c9bf69347e8fec90085`). That executable remains the baseline. We tried laying out the retained, hidden WebKit panel on the first valid native geometry sample, before the usual proof for revealing it. **The trial is not shipped.**

Same host and four-session fixture as above. Each row is one pair of runs, with 24 separate changes followed by 24 requests at 60 Hz. Each cell is **accepted → trial**, in ms. Blank time is the interval with native content concealed during the burst, not a measurement of display scanout. Settle starts at the last request.

| Page / pair | Burst blank | Burst settle | Individual request → native exposure, p50 / p95 | Request → responsive element callback, p50 / p95 |
|---|---:|---:|---:|---:|
| Light 1 | 570 → 578 | 188 → 203 | 59 / 68 → 65 / 72 | 34 / 41 → 32 / 35 |
| Light 2 | 574 → 582 | 195 → 205 | 55 / 60 → 65 / 75 | 34 / 39 → 32 / 38 |
| Busy 1 | 565 → 576 | 189 → 197 | 57 / 66 → 66 / 75 | 48 / 56 → 46 / 51 |
| Busy 2 | 579 → 566 | 195 → 187 | 54 / 68 → 63 / 71 | 48 / 60 → 46 / 50 |

Only the last burst width was exposed in all eight runs; each had one uninterrupted blank. The trial delivered responsive element sizes slightly earlier, but native exposure p50 and p95 were later in every pair. Busy burst results reversed between repetitions. Neither proves a physical frame-rate gain. The light input stopped growing at its CSS width cap (10 matching element notifications per 24 individual widths); the busy card’s box changed at all 24 widths, including when the grid added a column. These are delivered box-size observations, not paint or the duration of layout. One additional busy run aborted on the fixture’s `performance.timeOrigin` equality assertion; it is excluded. Both sides of the final busy pair used the same per-document identity check instead.

All eight runs received the sibling’s eight CLI-to-PTY probes during the burst and finished with the sibling block focused. Burst input p50 ranged from 22–24 ms accepted and 21–23 ms trial; this is not physical keyboard latency. Helper CPU over six seconds idle was 17–21 ms accepted and 16–21 ms trial, with 68–74 and 69–73 interrupt wakeups respectively. Burst plus its fixed tail used 13–15 ms and 15–16 ms of helper CPU. These include the measurement hooks, not WebContent or WindowServer. There is no consistent idle win or continuity improvement to justify a new binary or relaxed exposure checks.

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

To compare revisions, use the **same benchmark checkout** for both. Point `MILO_REX_TICK_SOURCE` at a checkout or archive containing the selected revision’s `macos/Preview` and `src/rex/preview.lua`. With a macOS GUI login and Rex installed:

```sh
for page in light busy; do
  MILO_REX_TICK_SOURCE=/absolute/path/to/revision MILO_REX_TICK_PAGE=$page \
    cargo test --locked --test rex_lab preview_tick::benchmark_persistent_preview -- --exact --ignored --nocapture
done
```

The driver sends 24 individual layout changes 937 ms apart, then returns through 24 widths at 60 requests/second. It times the viewer's socket send to the native panel becoming eligible and visible, and separately to delivery of WebKit’s DOM resize notification. Neither endpoint measures physical pixels. Jitter is the absolute difference between the interval of two completed updates and the interval of their requests; skipped widths are counted separately. The sibling PTY timestamps receipt of a line, including CLI launch and transport but not physical keystrokes. Counter collection is identical on both runs; `MILO_REX_TICK_COUNT=0` disables observer instrumentation. Run serially, report skipped widths, and compare both percentiles and sample counts rather than only the fastest frames.

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
