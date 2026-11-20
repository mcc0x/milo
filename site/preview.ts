// Serves the built site the way the Caddyfile does: files as they are named,
// index.html for a directory, and 404.html with a 404 for everything else.
import { join } from 'node:path';

const dist = join(import.meta.dir, 'dist');
const port = Number(process.env.PORT ?? 4173);

Bun.serve({
  port,
  async fetch(request) {
    // Every published path is plain ASCII, and the URL parser has already resolved any `..`.
    const { pathname } = new URL(request.url);
    for (const candidate of [pathname, join(pathname, 'index.html')]) {
      const file = Bun.file(join(dist, candidate));
      if (await file.exists()) return new Response(file);
    }
    return new Response(Bun.file(join(dist, '404.html')), { status: 404 });
  },
});
console.log(`http://localhost:${port}`);
