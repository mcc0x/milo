"""Print true unless Git proves that every changed path is independent of the Mac build."""
import subprocess
import sys


def macos_needed(event, base, head):
    if event not in ('push', 'pull_request') or not base or not head:
        return True
    # Compare the whole push, or only the PR's changes since its merge base.
    revisions = f'{base}...{head}' if event == 'pull_request' else f'{base}..{head}'
    diff = subprocess.run(['git', 'diff', '--name-only', '--no-renames', '-z', revisions, '--'], stdout=subprocess.PIPE)
    if diff.returncode or not diff.stdout:
        return True
    for path in diff.stdout.rstrip(b'\0').split(b'\0'):
        if path in (b'docs/img/milo-icon.png', b'site/src/images/milo-lockup.webp',
                    b'site/public/install.sh', b'hosted/milo-alert-texts.json'):
            return True  # Menu packaging, installer tests, or Rust include_str!.
        if path.startswith((b'site/', b'docs/', b'hosted/', b'.railway/')):
            continue
        if path not in (b'README.md', b'CONTRIBUTING.md', b'AGENTS.md', b'llms.txt', b'.dockerignore'):
            return True
    return False


if __name__ == '__main__':
    print(str(macos_needed(*sys.argv[1:])).lower())
