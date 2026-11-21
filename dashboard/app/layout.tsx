import type { Metadata, Viewport } from 'next';
import type { ReactNode } from 'react';
import Topbar from './topbar';
import './globals.css';

export const metadata: Metadata = {
  title: 'Milo settings',
  referrer: 'no-referrer',
  icons: { icon: { url: '/favicon.png', type: 'image/png', sizes: '48x48' } },
};
export const viewport: Viewport = { themeColor: '#d8e7fe' };

export default function RootLayout({ children }: { children: ReactNode }) {
  return <html lang="en"><body><Topbar />{children}</body></html>;
}
