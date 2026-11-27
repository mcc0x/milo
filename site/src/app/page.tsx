import type { Metadata, Viewport } from 'next';
import claude from '../images/claude.png';
import codex from '../images/codex.png';
import companions from '../images/companions.webp';
import lockup from '../images/milo-lockup.webp';
import mcc from '../images/mcc.webp';
import milo from '../images/milo.webp';
import omp from '../images/omp.svg';
import rex from '../images/rex.png';
import '../style.css';
import '../guide.css';
import '../agent.css';
import '../usage.css';
import { Bubble } from './bubble';
import { CopyAnnouncer, CopyButton, CopyStatus } from './copy';
import { Demo } from './demo/demo';
import { SiteLink } from './site-link';
import './fonts';

function FeatureIcon({ name }: { name: string }) {
  return <img className="milo-feature-icon" src={`/icons/milo/${name}.webp`} width={48} height={48} loading="lazy" decoding="async" alt="" />;
}

const title = 'milo — The little things that make Rex feel like home';
const description = 'Review changes, edit files, and check agent usage in your terminal. In Rex, Milo can also remember your layout and send alerts to Discord or Telegram.';
const setup = 'https://github.com/mcc0x/milo/blob/main/docs/setup.md';
const assets = 'https://github.com/mcc0x/milo/releases/download/v0.1.9';
const preview = {
  url: 'https://usemilo.net/milo-preview.jpg',
  alt: 'Milo — Your Git workspace, inside Rex. A file tree, commit history and agent session sit above the Milo and Rex mascots.',
};

export const metadata: Metadata = {
  title,
  description,
  alternates: {
    canonical: 'https://usemilo.net/',
    types: { 'text/plain': [{ url: '/llms.txt', title: 'Milo documentation index' }] },
  },
  openGraph: {
    type: 'website',
    url: 'https://usemilo.net/',
    title,
    description,
    images: { ...preview, type: 'image/jpeg', width: 1280, height: 640 },
  },
  twitter: { card: 'summary_large_image', title, description, images: preview },
};

export const viewport: Viewport = { themeColor: '#d8e7fe' };

const openCodePaths = (
  <>
    <path fill="#292828" fillRule="evenodd" d="M0 0h24v30H0zm6 6v18h12V6z" />
    <path fill="#a49e9e" d="M6 12h12v12H6z" />
  </>
);

export default function Home() {
  return (
    <CopyAnnouncer>
      <a className="skip-link" href="#main">Skip to content</a>
      <main id="main" tabIndex={-1}>
        <section className="hero" aria-labelledby="title">
          <header className="masthead">
            <div className="lockup">
              <img className="brand-lockup" src={lockup.src} width="510" height="192" alt="" fetchPriority="high" />
              <h1 id="title" className="visually-hidden">milo</h1>
            </div>
            <nav className="social-links" aria-label="Social links">
              <SiteLink className="social-link" href="https://github.com/mcc0x/milo" aria-label="milo on GitHub">
                <svg className="source-mark" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12" /></svg>
              </SiteLink>
              <SiteLink className="social-link" href="https://x.com/0xmcc" aria-label="Follow 0xmcc on X">
                <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M18.9 2H22l-6.8 7.8L23.2 22h-6.3L12 14.6 5.5 22H2.3l7.4-8.5L.8 2h6.5l4.5 6.8L18.9 2Zm-1.1 18h1.7L6.3 3.9H4.5L17.8 20Z" /></svg>
              </SiteLink>
            </nav>
          </header>
          <div className="intro">
            <div className="intro-copy">
              <p className="description">The little things that make <SiteLink className="rex" href="https://www.superlogical.com/rex"><img src={rex.src} width="72" height="72" alt="" />Rex</SiteLink> feel like home.</p>
              <p className="subtext">{description}</p>
            </div>
            <div className="hero-actions">
              <a className="hero-action hero-action-primary" href="#install">
                <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M10 3v9m0 0 3.5-3.5M10 12 6.5 8.5M4 13.5V15a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2v-1.5" /></svg>
                Install Milo
              </a>
              <SiteLink className="hero-action" href="https://github.com/mcc0x/milo">
                <svg viewBox="0 0 20 20" aria-hidden="true"><path d="m7 6-4 4 4 4m6-8 4 4-4 4m-1.5-10.5-3 13" /></svg>
                Source Code
              </SiteLink>
            </div>
            <ul className="bubbles" role="list" aria-label="Integrations">
              <Bubble><img src={claude.src} width="192" height="192" alt="Claude Code" /></Bubble>
              <Bubble><img src={omp.src} width="120" height="90" alt="OMP" /></Bubble>
              <Bubble><img src={codex.src} width="192" height="192" alt="Codex" /></Bubble>
              <Bubble><svg viewBox="0 0 24 30" role="img" aria-label="OpenCode">{openCodePaths}</svg></Bubble>
            </ul>
          </div>

          <Demo />
        </section>
        <section className="downloads" id="install" aria-labelledby="install-title">
          <h2 id="install-title">Install Milo</h2>
          <p className="feature-intro">Start in your terminal. No Rex required. See the <SiteLink href={`${setup}#install`}>installation guide</SiteLink>.</p>
          <div className="feature-grid">
            <article className="feature platform" id="install-macos" tabIndex={-1}>
              <FeatureIcon name="macos" />
              <h3>macOS <small>Apple Silicon</small></h3>
              <p>macOS 13 or later, with Homebrew.</p>
              <div className="platform-get">
                <code className="platform-command" id="install-command" translate="no">brew install mcc0x/milo/milo</code>
                <CopyButton target="install-command" />
              </div>
            </article>
            <article className="feature platform" id="install-linux" tabIndex={-1}>
              <FeatureIcon name="linux" />
              <h3>Linux <small>x86-64</small></h3>
              <p>glibc 2.35 or later.</p>
              <div className="platform-get">
                <code translate="no">milo-x86_64-unknown-linux-gnu</code>
                <a className="download" href={`${assets}/milo-x86_64-unknown-linux-gnu`}>Download<span className="visually-hidden"> the Linux binary</span></a>
              </div>
            </article>
            <article className="feature platform" id="install-windows" tabIndex={-1}>
              <FeatureIcon name="windows" />
              <h3>Windows <small>x64</small></h3>
              <p>Native Windows, not WSL.</p>
              <div className="platform-get">
                <code translate="no">milo-x86_64-pc-windows-msvc.exe</code>
                <a className="download" href={`${assets}/milo-x86_64-pc-windows-msvc.exe`}>Download<span className="visually-hidden"> the Windows executable</span></a>
              </div>
            </article>
          </div>
          <CopyStatus />
        </section>
        <section className="features" aria-labelledby="features-title">
          <h2 id="features-title">What’s in Milo?</h2>
          <p className="feature-intro">
            {/* Each separator is one string: split text nodes are not kerned together. */}
            {'Four panes, local settings, and layouts that remember your worktree. Bring your own agent: '}
            <span><img src={claude.src} width="192" height="192" alt="" />Claude Code</span>{', '}
            <span><img className="omp-mark" src={omp.src} width="120" height="90" alt="" />OMP</span>{', '}
            <span><img src={codex.src} width="192" height="192" alt="" />Codex</span>{', or '}
            <span><svg viewBox="0 0 24 30" aria-hidden="true">{openCodePaths}</svg>OpenCode</span>.
          </p>
          <div className="feature-grid">
            <article className="feature feature-panes">
              <h3 className="visually-hidden">Workspace panes</h3>
              <dl>
                <div><dt><FeatureIcon name="outline" />Outline</dt><dd>A Git-aware file tree that points out what changed.</dd></div>
                <div><dt><FeatureIcon name="history" />History</dt><dd>Browse commits and see what’s waiting to be pushed.</dd></div>
                <div><dt><FeatureIcon name="reader" />Reader</dt><dd>Open a file or read its diff without leaving your tab.</dd></div>
                <div><dt><FeatureIcon name="usage" />Usage</dt><dd>Check your agent limits and when they reset.</dd></div>
              </dl>
            </article>
            <article className="feature feature-memory">
              <FeatureIcon name="layouts" />
              <h3>Layouts</h3>
              <p>Save a default or a layout per worktree. Turn on automatic arrangement to reuse your pane sizes in new shell tabs.</p>
            </article>
            <article className="feature feature-settings">
              <FeatureIcon name="settings" />
              <h3>Settings</h3>
              <p>Choose your panes with <code>milo setup</code> or in your browser with <code>milo web</code>. Both come with the CLI.</p>
            </article>
            <article className="feature feature-menu">
              <FeatureIcon name="menu-bar" />
              <h3>Menu bar <small>macOS</small></h3>
              <p>See remaining usage and open Settings without Rex. Add it with <code>milo menubar install</code>.</p>
            </article>
          </div>
        </section>
        <section className="field-notes" id="companion" aria-labelledby="companion-title">
          <header className="start-intro" id="field-notes">
            <img className="companions" src={companions.src} width="544" height="356" alt="Rex with a magnifying glass beside Milo, pointing ahead." loading="lazy" />
            <h2 id="companion-title">Your Rex deserves a companion.</h2>
          </header>
          <p>Run <code translate="no">milo setup</code> in a supported Rex session to arrange Milo beside your shell.</p>
        </section>
      </main>
      <footer className="site-footer" id="open-source">
        <div className="footer-inner">
          <div className="footer-project">
            <img src={milo.src} width="44" height="44" alt="" loading="lazy" />
            <h2>Milo is <SiteLink href="https://github.com/mcc0x/milo">open source</SiteLink></h2>
          </div>
          <div className="footer-maker">
            <SiteLink href="https://bymcc.net/">
              <span>Made with <span className="visually-hidden">love </span><svg className="maker-heart" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 21 4.3 13.4C-1.8 7.6 5.7 1.4 10 5.7l2 2 2-2c4.3-4.3 11.8 1.9 5.7 7.7Z" /></svg> by <span className="maker-name">mcc</span></span>
              <img src={mcc.src} width="44" height="44" alt="" loading="lazy" decoding="async" />
            </SiteLink>
          </div>
        </div>
        <nav className="footer-legal" aria-label="Legal">
          <SiteLink className="source" href="/terms/">Terms</SiteLink>
          <SiteLink className="source" href="/privacy/">Privacy</SiteLink>
        </nav>
      </footer>
    </CopyAnnouncer>
  );
}
