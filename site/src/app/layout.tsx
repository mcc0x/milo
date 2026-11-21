import type { Metadata } from 'next';
import type { ReactNode } from 'react';

// Set here so that every page has them: the policies and the not-found page too.
export const metadata: Metadata = {
  icons: {
    icon: { url: '/favicon.png', type: 'image/png', sizes: '48x48' },
    apple: '/apple-touch-icon.png',
  },
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
