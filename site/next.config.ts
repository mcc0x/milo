import type { NextConfig } from 'next';
import { PHASE_PRODUCTION_BUILD } from 'next/constants';

// Railpack recognises a static Next.js site by the text `output: 'export'`
// and takes the directory for the Caddyfile from the text `distDir: 'dist'`.
export default (phase: string): NextConfig => ({
  output: 'export',
  // Only a build writes to dist/. `next dev` keeps its own files in .next/.
  ...(phase === PHASE_PRODUCTION_BUILD && { distDir: 'dist' }),
});
