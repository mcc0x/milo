"""Select checks for the exact merge tree GitHub Actions tests on a pull request."""

import re
import subprocess
import sys


APP = frozenset(("portable", "linux", "macos", "windows", "counts", "rex"))
PUBLIC = frozenset(("website",))
DASHBOARD = frozenset(("dashboard",))
ALL = APP | PUBLIC | DASHBOARD


def path_jobs(path):
    if path == b"hosted/milo-alert-texts.json" or path.startswith(b"dashboard/"):
        return APP | DASHBOARD
    if path == b"README.md":
        return PUBLIC | {"portable"}
    if path == b"site/public/install.sh":
        return PUBLIC | {"portable", "macos"}
    if path in (b"docs/img/milo-icon.png", b"site/src/images/milo-lockup.webp"):
        return PUBLIC | {"macos"}
    if path.startswith((b"macos/", b"tests/distribution-counts-macos/")) or path in (
        b"tests/menubar.sh", b"tests/menu-archive.py", b"tests/menu-bench.sh", b"tests/web-smoke.cjs",
    ):
        return {"macos"}
    if path.startswith(b"tests/menu-") and path.endswith(b".swift"):
        return {"macos"}
    if path.startswith((b"windows/", b"tests/distribution-counts-windows/", b"tests/windows-counts/")) or path in (
        b"tests/windows-install.ps1", b"tests/web-smoke-windows.cjs",
    ):
        return {"windows"}
    if path.startswith(b"linux/") or path in (
        b"tests/linux-candidate.sh", b"tests/linux-install.sh", b"tests/linux-standalone.py",
    ):
        return {"linux"}
    if path == b"tests/relay-container.sh":
        return DASHBOARD
    if path.startswith(b"hosted/counts/") or path == b"tests/anonymous-counts.py":
        return DASHBOARD | {"counts"}
    if path.startswith(b"hosted/") or path == b".dockerignore":
        return DASHBOARD
    if path.startswith((b"src/", b"tests/", b".cargo/")) or path in (
        b"Cargo.toml", b"Cargo.lock", b"build.rs", b"install.sh", b"rustfmt.toml", b"LICENSE",
    ):
        return APP
    if path.startswith((b"docs/", b"site/", b"skills/")) or path in (
        b"CONTRIBUTING.md", b"AGENTS.md", b"llms.txt", b"CODE_OF_CONDUCT.md", b"SECURITY.md",
    ):
        return PUBLIC
    return ALL


def pr_jobs(base, head, merge, *, cwd=None):
    if not all(re.fullmatch(r"[0-9a-f]{40}", sha) for sha in (base, head, merge)):
        return ALL
    try:
        parents = subprocess.check_output(["git", "show", "-s", "--format=%P", merge], cwd=cwd).split()
        if parents != [base.encode(), head.encode()]:
            return ALL
        diff = subprocess.check_output([
            "git", "diff", "--raw", "--no-renames", "--ignore-submodules=none", "-z", base, merge, "--",
        ], cwd=cwd)
    except (OSError, subprocess.CalledProcessError):
        return ALL
    if not diff or not diff.endswith(b"\0"):
        return ALL
    fields = diff[:-1].split(b"\0")
    if len(fields) % 2:
        return ALL
    jobs = set()
    for entry, path in zip(fields[::2], fields[1::2], strict=True):
        meta = entry.split()
        if len(meta) != 5 or not path or meta[-1] not in (b"A", b"D", b"M"):
            return ALL
        modes = (b"000000", b"100644", b"100755")
        if not meta[0].startswith(b":") or meta[0][1:] not in modes or meta[1] not in modes:
            return ALL
        jobs.update(path_jobs(path))
    return jobs


if __name__ == "__main__":
    base_ref, head_ref, base, head, merge = sys.argv[1:]
    ordinary = base_ref.lower() == "main" and not head_ref.lower().startswith(("release/", "qa/", "qualify/"))
    jobs = pr_jobs(base, head, merge) if ordinary else ALL
    for job in sorted(ALL):
        print(f"{job}={'true' if job in jobs else 'false'}")
