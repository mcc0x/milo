# Milo marketing site

This is the public Next.js App Router site, not the settings dashboard. Next generates `site/AGENTS.md` during local development; keep project-specific guidance here instead. Read the installed Next docs for any routing or export feature you change.

- Keep copy, headings, links and the install command useful on the first HTML response. Use client components only for actual interaction, such as copying or playing the demo. The production server serves static files: don’t add a route that needs a running Next server.
- Look in `style.css` for typography and color, `guide.css` for feature and footer layouts, and `terminal.css` for the illustrated Rex window. Reuse these rules before inventing another type scale, card or button. Keep the scene in `scene.ts` consistent with what the CLI actually does.
- Give the primary action a short verb. Keep secondary copy legible against the background and don’t convey state by color alone. The installation CTA must remain selectable even without clipboard access. Respect reduced motion; don’t reintroduce movement through a decorative layer.
- From `site/`, use `bun install --frozen-lockfile`, `bun test`, `bun run build`, then `bun run preview`. Check the exported page on a narrow phone and a desktop, including keyboard focus, text wrapping, the footer and direct navigation to any new route. Use the repo’s [UI review skill](../../skills/milo-ui-review/SKILL.md) for visual PR evidence.
