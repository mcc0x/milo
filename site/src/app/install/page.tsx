import type { Metadata } from 'next';
import { Fragment } from 'react';
import '../fonts';
import '../../install.css';
import { installGuide } from '../../install-guide';
import type { Inline } from '../../install-guide';
import { SiteLink } from '../site-link';

const title = 'Install Milo — macOS, Linux and Windows';
const description = 'Install Milo 0.1.9 on Apple Silicon macOS, Linux x86-64 (glibc) or native Windows x64: the commands, release files, checksums and prerequisites for each.';
const image = {
  url: 'https://usemilo.net/milo-social.jpg',
  alt: 'Milo: review agent edits in your terminal. A file tree, commit history and agent session beside the Milo and Rex mascots.',
};

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: 'https://usemilo.net/install/' },
  openGraph: { type: 'website', url: 'https://usemilo.net/install/', title, description, images: { ...image, type: 'image/jpeg', width: 1280, height: 640 } },
  twitter: { card: 'summary_large_image', title, description, images: image },
};

function Text({ parts }: { parts: Inline[] }) {
  return parts.map((part, index) => {
    if (part.code) return <code key={index} translate="no">{part.text}</code>;
    if (part.href) return <SiteLink key={index} href={part.href}>{part.text}</SiteLink>;
    return <Fragment key={index}>{part.text}</Fragment>;
  });
}

export default function Install() {
  const blocks = installGuide();
  const first = blocks.findIndex(block => block.type === 'heading');
  const toc = blocks.flatMap(block => (block.type === 'heading' && block.level === 2 ? [block] : []));
  return (
    <main className="install">
      <a className="install-home" href="/">← Milo</a>
      <h1>Install Milo</h1>
      <div className="install-lead">
        {blocks.slice(0, first).map((block, index) => block.type === 'paragraph' && <p key={index}><Text parts={block.inline} /></p>)}
        <p>Milo runs on its own in any Git project. You don’t need Rex, a browser or an account. These steps come from the <SiteLink href="https://github.com/mcc0x/milo/blob/main/docs/setup.md#install">setup guide</SiteLink> in the repository and describe the published v0.1.9 release.</p>
      </div>
      <nav aria-label="On this page" className="install-toc">
        <ul>{toc.map(block => <li key={block.id}><a href={`#${block.id}`}>{block.text}</a></li>)}</ul>
      </nav>
      {blocks.slice(first).map((block, index) => {
        if (block.type === 'heading') {
          const Heading = block.level === 2 ? 'h2' : 'h3';
          return <Heading key={index} id={block.id}><a href={`#${block.id}`}>{block.text}</a></Heading>;
        }
        if (block.type === 'code') return <pre key={index} tabIndex={0}><code translate="no">{block.text}</code></pre>;
        return <p key={index}><Text parts={block.inline} /></p>;
      })}
      <footer><a href="/">Home</a><a href="/terms/">Terms of Service</a><a href="/privacy/">Privacy Policy</a></footer>
    </main>
  );
}
