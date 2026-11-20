import type { Metadata, Viewport } from 'next';
import claude from '../images/claude.png';
import codex from '../images/codex.png';
import companions from '../images/companions.webp';
import lockup from '../images/milo-lockup.webp';
import milo from '../images/milo.webp';
import omp from '../images/omp.svg';
import rex from '../images/rex.png';
import { demoLabel } from '../scene';
import '../style.css';
import '../guide.css';
import '../agent.css';
import '../usage.css';
import { Enhance } from './enhance';
import './fonts';

const title = 'milo — The little things that make Rex feel like home';
const description = 'A file tree, commit history, diffs, and agent usage, right where you want them. Milo remembers how you like your panes.';
const preview = {
  url: 'https://usemilo.net/milo-preview.jpg',
  alt: 'Milo’s blue cat mascot and wordmark above the tagline: The little things that make Rex feel like home.',
};

export const metadata: Metadata = {
  title,
  description,
  alternates: {
    canonical: 'https://usemilo.net/',
    types: { 'text/plain': [{ url: '/llms.txt', title: 'Milo documentation index' }] },
  },
  icons: {
    icon: { url: '/favicon.png', type: 'image/png', sizes: '48x48' },
    apple: '/apple-touch-icon.png',
  },
  openGraph: {
    type: 'website',
    url: 'https://usemilo.net/',
    title,
    description,
    images: { ...preview, type: 'image/jpeg', width: 1200, height: 630 },
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
    <>
      <a className="skip-link" href="#main">Skip to content</a>
      <main id="main" tabIndex={-1}>
        <section className="hero" aria-labelledby="title">
          <header className="masthead">
            <div className="lockup">
              <img className="brand-lockup" src={lockup.src} width="510" height="192" alt="" fetchPriority="high" />
              <h1 id="title" className="visually-hidden">milo</h1>
            </div>
            <nav className="social-links" aria-label="Social links">
              <a className="social-link" href="https://github.com/mcc0x/milo" aria-label="milo on GitHub">
                <svg className="source-mark" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12" /></svg>
              </a>
              <a className="social-link" href="https://x.com/0xmcc" aria-label="Follow 0xmcc on X">
                <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M18.9 2H22l-6.8 7.8L23.2 22h-6.3L12 14.6 5.5 22H2.3l7.4-8.5L.8 2h6.5l4.5 6.8L18.9 2Zm-1.1 18h1.7L6.3 3.9H4.5L17.8 20Z" /></svg>
              </a>
            </nav>
          </header>
          <div className="intro">
            <div className="intro-copy">
              <p className="description">The little things that make <a className="rex" href="https://www.superlogical.com/rex"><img src={rex.src} width="72" height="72" alt="" />Rex</a> feel like home.</p>
              <p className="subtext">A file tree, commit history, diffs, and agent usage, right where you want them. Milo remembers how you like your panes.</p>
            </div>
            <div className="install">
              <span className="install-prompt" aria-hidden="true">$</span>
              <code id="install-command" translate="no">brew install mcc0x/milo/milo</code>
              <button id="copy-command" data-copy="install-command" type="button" aria-describedby="install-command" hidden>Copy</button>
            </div>
            <p id="copy-feedback" className="visually-hidden" role="status"></p>
            <ul className="bubbles" role="list" aria-label="Integrations">
              <li><span><img src={claude.src} width="192" height="192" alt="Claude Code" /></span></li>
              <li><span><img src={omp.src} width="120" height="90" alt="OMP" /></span></li>
              <li><span><img src={codex.src} width="192" height="192" alt="Codex" /></span></li>
              <li><span><svg viewBox="0 0 24 30" role="img" aria-label="OpenCode">{openCodePaths}</svg></span></li>
            </ul>
          </div>

          <div className="demo-panes" id="demo-panes" role="group" aria-label="Demo panes" hidden>
            <button type="button" value="agent" aria-controls="demo" aria-pressed="true">Agent</button>
            <button type="button" value="outline" aria-controls="demo" aria-pressed="false">Outline</button>
            <button type="button" value="reader" aria-controls="demo" aria-pressed="false">Reader</button>
            <button type="button" value="history" aria-controls="demo" aria-pressed="false">History</button>
            <button type="button" value="usage" aria-controls="demo" aria-pressed="false">Usage</button>
          </div>
          <div className="terminal" id="demo" data-view="whole" data-still="" translate="no" role="img" aria-label={demoLabel} aria-describedby="demo-fallback demo-still-caption">
            <div className="rex-titlebar">
              <span className="window-lights"><i></i><i></i><i></i></span>
              <span className="rex-tab">tidebook</span><span className="rex-tab rex-tab-new" hidden>zsh</span><span>Rex</span>
            </div>
            <div className="terminal-panes">
              <p className="demo-fallback" id="demo-fallback">The interactive preview needs JavaScript.</p>
              <section className="term-pane outline">
                <div className="pane-body" id="outline-pane"></div>
              </section>
              <section className="term-pane history">
                <div className="pane-body" id="history-pane"></div>
              </section>
              <section className="term-pane usage">
                <div className="usage-strip">
                  <span><img src={codex.src} width="20" height="20" alt="Codex" /><strong id="codex-usage"></strong><small className="usage-reset term-muted">Resets in 38m</small></span>
                  <span><img src={claude.src} width="20" height="20" alt="Claude" /><strong id="claude-usage"></strong><small className="usage-reset term-muted">Resets in 1h 24m</small></span>
                </div>
              </section>
              <section className="term-pane agent" id="agent-pane">
                <div className="agent-session" id="agent-session" tabIndex={-1}>
                  <header className="agent-banner" id="agent-banner"></header>
                  <div id="agent-transcript"></div>
                </div>
                <div className="agent-chrome">
                  <div className="agent-activity" id="agent-activity" hidden></div>
                  <div className="agent-composer">
                    <div className="composer-model" id="composer-model"></div>
                    <div className="composer-line">
                      <span className="composer-prompt" id="composer-prompt"></span>
                      <span><span className="composer-text" id="composer-text"></span><i className="composer-caret"></i><span className="composer-placeholder" id="composer-placeholder"></span></span>
                    </div>
                  </div>
                  <div className="composer-hint" id="composer-hint"></div>
                </div>
              </section>
              <div className="term-pane shell-pane">
                <div className="term-line shell-prompt">tidebook <span className="term-blue">main</span> › <span className="composer-caret"></span></div>
              </div>
              <section className="reader" id="reader-pane" hidden>
                <header className="pane-head"><span>›_ milo view weather.ts</span><i className="pane-tools"></i></header>
                <div className="reader-content" id="reader-content"></div>
              </section>
            </div>
          </div>
          <div className="demo-footer">
            <button className="caption-toggle" id="demo-playback" type="button" aria-controls="demo" aria-label="Pause demo" title="Pause demo" hidden><span className="caption" id="caption" aria-hidden="true"></span></button>
            <p className="caption" id="demo-still-caption" role="status">All of it stays in view, <strong>beside your work</strong>.</p>
          </div>
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
                <div><dt><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 3v14h5M5 7h5" /><rect x="11" y="4" width="8" height="6" rx="1.5" /><rect className="pane-leaf" x="11" y="14" width="8" height="6" rx="1.5" /></svg>Outline</dt><dd>A Git-aware file tree that points out what changed.</dd></div>
                <div><dt><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="7" cy="5" r="2.3" /><circle cx="7" cy="19" r="2.3" /><circle className="pane-commit" cx="17" cy="6" r="2.3" /><path d="M7 8v9m0-3c6 0 10-1 10-6" /></svg>History</dt><dd>Browse commits and see what’s waiting to be pushed.</dd></div>
                <div><dt><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 6C9 4 6 4 3 5v13c3-1 6-1 9 1 3-2 6-2 9-1V5c-3-1-6-1-9 1zm0 0v13" /><path className="pane-ink" d="m15 10 3-.6" pathLength="1" /></svg>Reader</dt><dd>Open a file or read its diff without leaving your tab.</dd></div>
                <div><dt><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 18a9 9 0 1 1 16 0" /><path className="pane-needle" d="m12 14 4-5" /><circle cx="12" cy="15" r="1.5" /></svg>Usage</dt><dd>Check your agent limits and when they reset.</dd></div>
              </dl>
            </article>
            <article className="feature feature-memory">
              <svg viewBox="0 0 24 24" aria-hidden="true"><g className="layout-window"><rect x="2" y="4" width="20" height="16" rx="2.5" /><path d="M2 8h20" /><path className="layout-divider" d="M10 8v12" /></g></svg>
              <h3>Layouts</h3>
              <p>Save a default or a layout per worktree. Turn on automatic arrangement to reuse your pane sizes in new shell tabs.</p>
            </article>
            <article className="feature feature-settings">
              <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 7h5m5 0h8M3 17h10m5 0h3" /><circle cx="10.5" cy="7" r="2.5" /><circle cx="15.5" cy="17" r="2.5" /></svg>
              <h3>Settings</h3>
              <p>Choose your panes with <code>milo setup</code> or in your browser with <code>milo web</code>. Both come with the CLI.</p>
            </article>
            <article className="feature feature-menu">
              <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="2" y="4" width="20" height="16" rx="3" /><path d="M2 10h20m-6-3h3" /></svg>
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
          <div className="brew-pill"><code id="setup-command" translate="no">brew install mcc0x/milo/milo</code><button type="button" data-copy="setup-command" aria-describedby="setup-command" hidden>Copy</button></div>
          <p>Then run <code translate="no">milo</code> in Rex.</p>
        </section>
      </main>
      <footer className="site-footer" id="open-source">
        <div className="footer-inner">
          <div className="footer-signoff">
            <img src={milo.src} width="44" height="44" alt="" loading="lazy" />
            <div><h2>Milo is open source.</h2><p>Contributions are welcome.</p></div>
          </div>
          <nav aria-label="Project links">
            <a className="source" href="https://github.com/mcc0x/milo">GitHub</a>
            <a className="source" href="https://github.com/mcc0x/milo/releases">Releases</a>
            <a className="source" href="https://github.com/mcc0x/milo/issues">Issues</a>
            <a className="source" href="https://github.com/mcc0x/milo/blob/main/CONTRIBUTING.md">Contribute</a>
          </nav>
        </div>
      </footer>
      <Enhance />
    </>
  );
}
