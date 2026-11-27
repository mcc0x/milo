import { readFileSync } from 'node:fs';
import { join } from 'node:path';

export type Inline = { text: string; code?: boolean; href?: string };
export type Block =
  | { type: 'paragraph'; inline: Inline[] }
  | { type: 'code'; lang: string; text: string }
  | { type: 'heading'; level: 2 | 3; id: string; text: string };

const source = 'https://github.com/mcc0x/milo/blob/main/docs/setup.md';

function inline(text: string, where: string): Inline[] {
  const parts: Inline[] = [];
  const token = /`([^`]+)`|\[([^\]]+)\]\(([^)\s]+)\)/g;
  let last = 0;
  for (const match of text.matchAll(token)) {
    if (match.index > last) parts.push({ text: text.slice(last, match.index) });
    if (match[1] !== undefined) {
      parts.push({ text: match[1], code: true });
    } else {
      const href = new URL(match[3], source).href;
      if (!href.startsWith('https://')) throw new Error(`docs/setup.md ${where}: unsupported link ${match[3]}`);
      parts.push({ text: match[2], href });
    }
    last = match.index + match[0].length;
  }
  parts.push({ text: text.slice(last) });
  const plain = parts.filter(part => !part.code && !part.href).map(part => part.text).join('');
  if (/[`*_<>[\]]|!\(/.test(plain.replaceAll(/\b\w+_\w+\b/g, ''))) {
    throw new Error(`docs/setup.md ${where}: Markdown the install page does not render: ${text}`);
  }
  return parts.filter(part => part.text !== '');
}

export function slug(text: string) {
  return text.toLowerCase().replaceAll(/[^a-z0-9 -]/g, '').trim().replaceAll(' ', '-');
}

/** Reads the install section of docs/setup.md and refuses any construct it cannot render, so a step is never dropped silently. */
export function installGuide(markdown = readFileSync(join(process.cwd(), '..', 'docs', 'setup.md'), 'utf8')): Block[] {
  const lines = markdown.replaceAll('\r\n', '\n').split('\n');
  const start = lines.indexOf('## Install');
  if (start < 0) throw new Error('docs/setup.md has no "## Install" section');
  const end = lines.findIndex((line, index) => index > start && /^## /.test(line));
  const section = lines.slice(start + 1, end < 0 ? undefined : end);
  const blocks: Block[] = [];
  const ids = new Set<string>();
  let paragraph: string[] = [];
  const flush = () => {
    if (paragraph.length) blocks.push({ type: 'paragraph', inline: inline(paragraph.join(' '), paragraph[0]) });
    paragraph = [];
  };
  for (let i = 0; i < section.length; i++) {
    const line = section[i];
    const fence = /^```(\w*)$/.exec(line);
    if (fence) {
      flush();
      const close = section.indexOf('```', i + 1);
      if (close < 0) throw new Error('docs/setup.md: unclosed code fence in the install section');
      blocks.push({ type: 'code', lang: fence[1], text: section.slice(i + 1, close).join('\n') });
      i = close;
      continue;
    }
    const heading = /^(#{3,4}) (.+)$/.exec(line);
    if (heading) {
      flush();
      const id = slug(heading[2]);
      if (!id || ids.has(id)) throw new Error(`docs/setup.md: duplicate or empty heading anchor for "${heading[2]}"`);
      ids.add(id);
      blocks.push({ type: 'heading', level: heading[1].length === 3 ? 2 : 3, id, text: heading[2] });
      continue;
    }
    if (line.trim() === '') {
      flush();
      continue;
    }
    if (/^(#|\s*[-*+] |\s*\d+\. |\||>|<|!\[)/.test(line)) throw new Error(`docs/setup.md: install section line the page does not render: ${line}`);
    paragraph.push(line);
  }
  flush();
  for (const os of ['macOS', 'Linux', 'Windows']) {
    const at = blocks.findIndex(block => block.type === 'heading' && block.text === os);
    const next = blocks.findIndex((block, index) => index > at && block.type === 'heading' && block.level === 2);
    if (at < 0 || !blocks.slice(at, next < 0 ? undefined : next).some(block => block.type === 'code')) {
      throw new Error(`docs/setup.md: the install section needs a ${os} heading with a command`);
    }
  }
  return blocks;
}
