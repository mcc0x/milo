import type { NextConfig } from 'next';

const config: NextConfig = {
  output: 'export',
  generateBuildId: async () => 'milo-dashboard',
  devIndicators: false,
};

export default config;
