import type { NextConfig } from 'next';
import { PHASE_PRODUCTION_BUILD } from 'next/constants';

// Railpack reads this file as text: `output: 'export'` marks a static Next.js
// site, and the first quoted value after the output directory key below is
// the directory the Caddyfile serves. Keep that key out of comments.
export default (phase: string): NextConfig => ({
  output: 'export',
  trailingSlash: true,
  devIndicators: false,
  // Only a build writes to dist/. `next dev` keeps its own files in .next/.
  ...(phase === PHASE_PRODUCTION_BUILD && { distDir: 'dist' }),
});
