import type { MetadataRoute } from 'next';

export const dynamic = 'force-static';

export default function sitemap(): MetadataRoute.Sitemap {
  return ['/', '/terms/', '/privacy/'].map(path => ({ url: `https://usemilo.net${path}` }));
}
