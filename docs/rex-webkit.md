# Can Milo open a local site in a Rex pane?

**Not on the tested Mac preview.** Rex can place a block beside the project's terminal, but neither the server nor the Mac client advertises a way for Milo to put a `WKWebView` in that block. [Issue #68](https://github.com/mcc0x/milo/issues/68) remains open. There is no `milo preview` command, external-browser fallback or element picker.

## What was checked

On 2026-11-22, the installed Apple Silicon client identified itself as Rex 0.1.0 (2061). Its [official Preview update feed](https://blob.superlogical.com/public/rex-app/preview-arm64/appcast.xml) pairs that build with Rex `0.1.0-preview.21`, commit `202b1b3fc0219883112258a8d0f9f95e2d47daa9`. `rex version` and the disposable server's `server.version` agreed: control API revision **18**, `darwin_arm64`. The feed identifies the builds; the detailed [beta changelog](https://www.superlogical.com/rex/docs/beta/community-mcp) requires Discord membership and was not used as evidence of missing capabilities. Other builds and platforms are unverified.

The [server API](https://www.superlogical.com/rex/docs/automate/server-api) reports what the connected server can create. `rex block types --json` and Lua's `rex.call("block_creator.list")` returned the same four flavors:

| Flavor | What it provides |
| --- | --- |
| `com.superlogical.terminal.shell` | A terminal running a shell or command |
| `com.superlogical.onboarding.onboarding` | Rex's own walkthrough; exposes `state` and `set_step`, not URL navigation |
| `com.superlogical.tunnel.dial` | A connection to a host and port |
| `com.superlogical.tunnel.listen` | A listening socket for forwarded connections |

To repeat the split probe with an installed Rex:

```sh
cargo test --locked --test rex_lab preview_block_capabilities -- --exact --ignored --nocapture
```

It uses a private server, socket, home and session. `session.new_split` placed onboarding beside the terminal without moving focus. The same call with a deliberately unregistered flavor returned `block flavor not found` (`404`, `not_found`); the two blocks and terminal focus were unchanged. The inventory matters more than that error: a made-up name would fail even on a server with a browser under a different name.

The Mac client's own `rex -C CLIENT actions --json` offered Quick Look and Forward Port, but no action to mount a URL in a pane. [Quick Look](https://www.superlogical.com/rex/docs/reference/mac-actions) previews links on screen; [`rex forward open`](https://www.superlogical.com/rex/docs/reference/cli) opens the system browser unless passed `--no-open`. Neither gives Milo a browser block. `--block-dir` discovers **server** executables, not Mac view extensions. The [Lua API](https://www.superlogical.com/rex/docs/reference/lua) composes registered blocks; the third-party [Rust SDK](https://github.com/alpha-og/rexdk) wraps the server protocol. No public Rex client renderer SDK was found.

## What would unblock it

Rex needs to advertise a browser flavor rendered by the **target client**, or a supported client extension point to mount a WebKit view. It must be possible to open a specified URL beside a specified block, return focus to the terminal and close only the new pane. A server version or a successful split does not prove the attached client can render it. Reject unsupported clients; for a remote project, require an explicit forward to its server or decline. Never guess a port or navigate an existing tab.

Element handoff is a separate opt-in. [React Grab 0.2.0](https://github.com/aidenybai/react-grab/tree/23bce0e56f2808902f1126ad581f6d8c3b5f639e/packages/react-grab) runs **in the page**; its [MIT license](https://github.com/aidenybai/react-grab/blob/23bce0e56f2808902f1126ad581f6d8c3b5f639e/LICENSE) doesn't supply a Rex renderer. Nothing is bundled. If added, pin and audit the shipped bundle and notices, disable its default version check, and load it locally only for the explicitly approved dev origin and main frame. Revoke access on navigation. Show the exact bounded selection as untrusted text in Milo-owned UI; copy or send only after the user chooses a destination and confirms. Page content must not select the recipient, invoke an agent, or type commands into a shell.

Before claiming support, show the disposable project's live page **inside Rex** beside its terminal. Exercise links, form input, scrolling, file-edit hot reload, focus in both directions, picker cancellation and teardown. Measure time to an interactive page and idle CPU/RSS including WebKit child processes against the same layout without the preview. The spike produced no browser measurements or screenshots: it created a walkthrough, not a browser.
