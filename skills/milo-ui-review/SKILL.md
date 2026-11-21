---
name: milo-ui-review
description: Review changes to Milo’s public website, local settings dashboard, Rex panes or menu bar. Use for layout, interaction, accessibility and before/after evidence in a visual PR.
---

# Review a Milo surface

1. **Name the user action first.** For example, save a worktree override, reopen a pane, or copy an install command. Reproduce the old behavior with an isolated home and a disposable repository. Don’t use someone’s real bot, channel or Discord account.
2. **Run what ships.** Use the site’s static preview for the marketing page; for settings, build the binary and follow the browser smoke invocation in the [CI workflow](../../.github/workflows/release-tests.yml). For a pane, launch the CLI in a real PTY and resize it. For the macOS menu bar, open the packaged app on a Mac; report explicitly if you couldn’t exercise that surface.
3. **Follow the action through its states.** Try the primary action, cancel/back, repeat and reopen. Confirm that state persists only at its intended scope. If an integration is absent, don’t show an enabled state. Verify destructive actions keep their confirmation and don’t take unrelated settings with them.
4. **Inspect, don’t infer.** On web, try a narrow viewport (e.g. 390px) and desktop (e.g. 1440px), reduced motion, keyboard-only navigation and touch-sized controls. Watch for focus loss after a state change, moving action targets, double focus rings, clipped form labels, overlays behind dialogs and horizontal page scroll. On terminal surfaces, check a narrow split and the normal wide view.
5. **Attach evidence.** Use meaningful before/after screenshots, alt text and the viewport size in the [PR template](../../.github/pull_request_template.md). Capture page content, not address bars containing session keys. Record which commands and interactions passed, what was simulated, and which OS or provider wasn’t tested. Don’t label a local fixture a live integration test.
