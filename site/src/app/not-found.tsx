import type { Metadata } from 'next';
import { SiteLink } from './site-link';

export const metadata: Metadata = { title: 'Page not found — milo' };

// Every route carries this page in case it is needed. Its styles are inline
// so that no other page fetches them.
const styles = `
body { margin: 0; }
.not-found {
  display: grid;
  place-items: center;
  min-height: 100vh;
  background: #eceafb;
  color: #1b2466;
  font: 400 1rem/1.5 system-ui, sans-serif;
  text-align: center;
}
.not-found img { width: 7rem; height: auto; }
.not-found h1 { margin: .5rem 0 0; font-size: 1.75rem; letter-spacing: -.02em; }
.not-found p { margin: .4rem 0 0; color: #4d5280; }
.not-found a { color: inherit; font-weight: 600; text-underline-offset: .2em; }
`;

export default function NotFound() {
  return (
    <div className="not-found">
      <style>{styles}</style>
      <main>
        <img src="/apple-touch-icon.png" width="180" height="180" alt="" fetchPriority="low" />
        <h1>Nothing here.</h1>
        <p>That page does not exist. <SiteLink href="/">Go to milo</SiteLink>.</p>
      </main>
    </div>
  );
}
