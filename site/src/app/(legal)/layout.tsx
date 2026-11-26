import type { ReactNode } from 'react';
import '../fonts';
import { SiteLink } from '../site-link';
import './policy.css';

export default function LegalLayout({ children }: { children: ReactNode }) {
  return <main className="policy"><SiteLink className="policy-home" href="/">← Milo</SiteLink>{children}
    <footer><SiteLink href="/terms/">Terms of Service</SiteLink><SiteLink href="/privacy/">Privacy Policy</SiteLink></footer>
  </main>;
}
