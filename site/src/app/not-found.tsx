import type { Metadata } from 'next';

export const metadata: Metadata = {
  title: 'Page not found — milo',
  icons: { icon: { url: '/favicon.png', type: 'image/png', sizes: '48x48' } },
};

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
        {/* A full page load: the home page's styles and demo start from a clean document. */}
        <p>That page does not exist. <a href="/">Go to milo</a>.</p>
      </main>
    </div>
  );
}
