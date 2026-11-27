"""Run from a checkout to refresh the plain-text docs shipped by the standalone site."""
import argparse
from pathlib import Path
from urllib.parse import urljoin
import re

site = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--check', action='store_true', help='fail if a published file is missing or stale')
args = parser.parse_args()
repo = site.parent
base = 'https://github.com/mcc0x/milo/blob/main/'
files = ['README.md', 'docs/setup.md', 'docs/panes.md', 'docs/keys.md', 'docs/usage.md', 'docs/presence.md',
         'docs/integrations.md',
         'skills/milo/SKILL.md', 'CONTRIBUTING.md', 'docs/internals.md']


def published(name):
    """Returns a checkout file's address on GitHub and its text, with relative links resolved against that address."""
    source = base + name
    text = (repo / name).read_text(encoding='utf-8').strip()
    # Away from the checkout, a relative Markdown link still refers to its source file.
    text = re.sub(r'(\]\()([^\s)]+)(\))', lambda m: m[1] + urljoin(source, m[2]) + m[3], text)
    text = re.sub(r'(\b(?:href|src|srcset)=")([^"]+)(")', lambda m: m[1] + urljoin(source, m[2]) + m[3], text)
    return source, text


parts = ['# Milo — full documentation\n\nDocumentation from the main branch. The pinned release may not include every feature described here.']
for name in files:
    source, text = published(name)
    parts.append(f'Source: {source}\n\n{text}')
# The site's index is the checkout's llms.txt, so the two cannot describe different programs.
outputs = {'public/llms-full.txt': '\n\n---\n\n'.join(parts), 'public/llms.txt': published('llms.txt')[1]}
# The production build sees only site/, so the install page reads this generated copy of the guide's Install section.
setup = (repo / 'docs/setup.md').read_text(encoding='utf-8').replace('\r\n', '\n')
install = re.search(r'^## Install\n.*?(?=^## |\Z)', setup, re.S | re.M)
if not install:
    raise SystemExit('docs/setup.md has no "## Install" section.')
outputs['src/generated/install.md'] = install[0].strip()
for name, text in outputs.items():
    path = site / name
    path.parent.mkdir(parents=True, exist_ok=True)
    content = (text + '\n').encode('utf-8')
    if not args.check:
        path.write_bytes(content)
    elif not path.is_file() or path.read_bytes() != content:
        raise SystemExit(f'{name} is stale; run python3 site/bundle-docs.py and commit the result.')
