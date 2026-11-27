import { describe, expect, test } from 'bun:test';
import { installGuide, slug } from '../src/install-guide';

describe('install guide', () => {
  test('renders every platform and command from docs/setup.md', () => {
    const blocks = installGuide();
    const headings = blocks.flatMap(block => (block.type === 'heading' ? [block.id] : []));
    expect(headings).toEqual(expect.arrayContaining(['macos', 'linux', 'windows']));
    const code = blocks.flatMap(block => (block.type === 'code' ? [block.text] : [])).join('\n');
    expect(code).toContain('brew install mcc0x/milo/milo');
    expect(code).toContain('sh install-linux.sh');
    expect(code).toContain('install-windows.ps1');
  });

  test('every link in the install section is absolute https', () => {
    for (const block of installGuide()) {
      if (block.type !== 'paragraph') continue;
      for (const part of block.inline) if (part.href) expect(part.href).toStartWith('https://');
    }
  });

  test('refuses Markdown it cannot render', () => {
    const doc = (body: string) => `## Install\n\n### macOS\n\n${body}\n\n\`\`\`sh\nx\n\`\`\`\n\n### Linux\n\n\`\`\`sh\nx\n\`\`\`\n\n### Windows\n\n\`\`\`sh\nx\n\`\`\`\n`;
    expect(() => installGuide(doc('- a list item'))).toThrow();
    expect(() => installGuide(doc('some **bold** text'))).toThrow();
    expect(() => installGuide(doc('plain text'))).not.toThrow();
    expect(() => installGuide('## Install\n\n### macOS\n')).toThrow();
  });

  test('anchors match GitHub heading ids', () => {
    expect(slug('When Homebrew and Milo disagree')).toBe('when-homebrew-and-milo-disagree');
  });
});
