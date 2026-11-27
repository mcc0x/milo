from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from pr_impact import ALL, APP, DASHBOARD, PUBLIC, path_jobs, pr_jobs


class PathJobs(unittest.TestCase):
    def test_repository_inputs(self):
        cases = {
            b"README.md": PUBLIC,
            b"docs/usage.md": PUBLIC,
            b"skills/milo/SKILL.md": PUBLIC,
            b"site/bun.lock": PUBLIC,
            b"site/public/llms-full.txt": PUBLIC,
            b"site/public/install.sh": PUBLIC | {"portable", "macos"},
            b"docs/img/milo-icon.png": PUBLIC | {"macos"},
            b"site/src/images/milo-lockup.webp": PUBLIC | {"macos"},
            b"macos/MenuBar/App.swift": {"macos"},
            b"tests/menu-handoff.swift": {"macos"},
            b"tests/menu-bench.sh": {"macos"},
            b"tests/web-smoke.cjs": {"macos"},
            b"tests/distribution-counts-macos/runner.sh": {"macos"},
            b"windows/install.ps1.in": {"windows"},
            b"tests/distribution-counts-windows/qualify.ps1": {"windows"},
            b"linux/install.sh.in": {"linux"},
            b"tests/linux-candidate.sh": {"linux"},
            b"src/web/launch_windows.rs": APP,
            b"tests/windows_console.rs": APP,
            b"Cargo.lock": APP,
            b".cargo/config.toml": APP,
            b"build.rs": APP,
            b"dashboard/bundle/manifest": APP | DASHBOARD,
            b"dashboard/bun.lock": APP | DASHBOARD,
            b"hosted/milo-alert-texts.json": APP | DASHBOARD,
            b".dockerignore": DASHBOARD,
            b"hosted/Dockerfile": DASHBOARD,
            b"hosted/counts/schema.sql": DASHBOARD | {"counts"},
            b".github/workflows/windows.yml": ALL,
            b".github/ci/stage-windows.ps1": ALL,
            b".github/ci/pr_impact.py": ALL,
            b"site-build.sh": ALL,
            b"new/input.cfg": ALL,
        }
        for path, expected in cases.items():
            with self.subTest(path=path):
                self.assertEqual(path_jobs(path), expected)


class MergeJobs(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name) / "repo"
        self.root.mkdir()
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Routing Fixture")
        self.git("config", "user.email", "routing-fixture@example.invalid")
        self.write("README.md")
        self.write("Cargo.lock")
        self.common = self.commit()
        self.git("switch", "-q", "-c", "feature")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root).decode().strip()

    def write(self, name, text="example\n"):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-q", "--no-gpg-sign", "--allow-empty", "-m", "fixture")
        return self.git("rev-parse", "HEAD")

    def merged(self):
        head = self.commit()
        self.git("switch", "-q", "main")
        base = self.git("rev-parse", "HEAD")
        self.git("merge", "-q", "--no-gpg-sign", "--no-ff", "-m", "merge fixture", head)
        return base, head, self.git("rev-parse", "HEAD")

    def test_exact_merge_with_base_only_change_at_checkout_depth(self):
        self.git("switch", "-q", "main")
        self.write("Cargo.lock", "base changed the lock\n")
        self.commit()
        self.git("switch", "-q", "feature")
        self.write("site/src/docs\nchange.txt")
        shas = self.merged()
        shallow = self.root.with_name("shallow")
        self.git("clone", "-q", "--depth=2", self.root.as_uri(), str(shallow))
        self.assertEqual(pr_jobs(*shas, cwd=shallow), PUBLIC)
        self.assertEqual(pr_jobs(self.common, *shas[1:], cwd=shallow), ALL)

    def test_release_and_non_main_prs_use_full_matrix(self):
        self.write("docs/new.md")
        shas = self.merged()
        for base_ref, head_ref, jobs in (
            ("main", "docs/update", PUBLIC),
            ("main", "release/prepare-xyz", ALL),
            ("main", "Qa/qualification", ALL),
            ("main", "qualify/package", ALL),
            ("release/maintenance", "docs/update", ALL),
        ):
            with self.subTest(base_ref=base_ref, head_ref=head_ref):
                output = subprocess.check_output([
                    sys.executable, str(Path(__file__).resolve().with_name("pr_impact.py")),
                    base_ref, head_ref, *shas,
                ], cwd=self.root, text=True)
                self.assertEqual(dict(line.split("=") for line in output.splitlines()),
                                 {job: "true" if job in jobs else "false" for job in ALL})

    def test_mixed_paths_and_delete(self):
        self.write("windows/install.ps1.in")
        self.write("macos/MenuBar/App.swift")
        self.write("site/next.config.ts")
        (self.root / "Cargo.lock").unlink()
        self.assertEqual(pr_jobs(*self.merged(), cwd=self.root), APP | PUBLIC)

    def test_rename_keeps_the_old_path(self):
        (self.root / "docs").mkdir()
        self.git("mv", "Cargo.lock", "docs/lockfile.txt")
        self.assertEqual(pr_jobs(*self.merged(), cwd=self.root), APP | PUBLIC)

    def test_large_diff_includes_a_late_unknown_path(self):
        for i in range(310):
            self.write(f"docs/{i}.md")
        self.write("z-unknown")
        self.assertEqual(pr_jobs(*self.merged(), cwd=self.root), ALL)

    def test_missing_or_empty_diff_defaults_to_all(self):
        shas = self.merged()
        self.assertEqual(pr_jobs(*shas, cwd=self.root), ALL)
        self.assertEqual(pr_jobs(*shas[:2], "f" * 40, cwd=self.root), ALL)
        self.assertEqual(pr_jobs("", *shas[1:], cwd=self.root), ALL)

    def test_nonregular_files_default_to_all(self):
        (self.root / "docs").mkdir()
        (self.root / "docs" / "link.md").symlink_to("../README.md")
        self.assertEqual(pr_jobs(*self.merged(), cwd=self.root), ALL)


if __name__ == "__main__":
    unittest.main()
