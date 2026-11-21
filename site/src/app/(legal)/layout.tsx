import type { ReactNode } from 'react';
import '../fonts';
import './policy.css';

export default function LegalLayout({ children }: { children: ReactNode }) {
  return <main className="policy"><a className="policy-home" href="/">← Milo</a>{children}
    <footer><a href="/terms/">Terms of Service</a><a href="/privacy/">Privacy Policy</a></footer>
  </main>;
}
