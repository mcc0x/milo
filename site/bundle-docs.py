"""Run from a checkout to refresh the plain-text docs shipped by the standalone site."""
import argparse
from pathlib import Path
from urllib.parse import urljoin
import re

site = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--check', action='store_true', help='fail if the published bundle is missing or stale')
args = parser.parse_args()
repo = site.parent
base = 'https://github.com/mcc0x/milo/blob/main/'
files = ['README.md', 'docs/setup.md', 'docs/panes.md', 'docs/keys.md', 'docs/usage.md',
         'skills/milo/SKILL.md', 'CONTRIBUTING.md', 'docs/internals.md']
parts = ['# Milo — full documentation\n\nDocumentation from the main branch. The pinned release may not include every feature described here.']
for name in files:
    source = base + name
    text = (repo / name).read_text(encoding='utf-8').strip()
    # Relative Markdown links in the combined document still refer to their source file.
    text = re.sub(r'(\]\()([^\s)]+)(\))', lambda m: m[1] + urljoin(source, m[2]) + m[3], text)
    text = re.sub(r'(\b(?:href|src|srcset)=")([^"]+)(")', lambda m: m[1] + urljoin(source, m[2]) + m[3], text)
    parts.append(f'Source: {source}\n\n{text}')
bundle = site / 'public/llms-full.txt'
content = ('\n\n---\n\n'.join(parts) + '\n').encode('utf-8')
if args.check:
    if not bundle.is_file() or bundle.read_bytes() != content:
        raise SystemExit('llms-full.txt is stale; run python3 site/bundle-docs.py and commit the result.')
else:
    bundle.write_bytes(content)
