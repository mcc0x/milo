"""Count Git launches during local settings transfers; --check also enforces Git-free global operations."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def sample(binary, home, check):
    repo = home / "checkout"
    cwd = repo / "src"
    cwd.mkdir(parents=True)
    env = {"HOME": str(home), "PATH": os.defpath, "LC_ALL": "C", "GIT_CONFIG_GLOBAL": os.devnull,
           "XDG_CONFIG_HOME": str(home / "config"), "XDG_STATE_HOME": str(home / "state"),
           "GIT_CONFIG_SYSTEM": str(home / "gitconfig")}
    trace = home / "trace.jsonl"

    def git(*args):
        return subprocess.check_output(["git", *args], cwd=repo, env=env, text=True).strip()

    git("config", "--file", env["GIT_CONFIG_SYSTEM"], "trace2.eventTarget", str(trace))
    git("init", "-q", "--template=")
    git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "first", "--allow-empty")
    commit = git("rev-parse", "HEAD")
    original = "# keep this comment\nicons = plain\nusage_codex = hide\n"
    prefs = home / "config/milo/preferences.conf"
    prefs.parent.mkdir(parents=True)
    prefs.write_text(original)
    bundle = home / "bundle"

    counts = []

    def settings(label, *args):
        trace.write_text("")
        result = subprocess.check_output([binary, "settings", *args], cwd=cwd, env=env, text=True)
        starts = sum(json.loads(line)["event"] == "start" for line in trace.read_text().splitlines())
        print(f"{label}\t{starts}")
        counts.append(starts)
        return result

    assert "icons = plain" in settings("global export", "export", "all")
    bundle.write_text("milo-settings 1\n[global]\nicons = off\n")
    assert "Icons: plain → off" in settings("global preview", "import", bundle)
    assert prefs.read_text() == original
    settings("global apply", "import", bundle, "--apply")
    assert sorted(prefs.read_text().splitlines()) == sorted(original.replace("plain", "off").splitlines())
    settings("undo", "undo")
    assert prefs.read_text() == original
    if check:
        assert not any(counts), "global transfers must not launch Git"

    # A nested invocation still has to resolve the checkout, not adopt its folder name.
    bundle.write_text(f"milo-settings 1\n[project]\nname = moved\nrepository = {commit}\nicons = off\n")
    assert f"{repo} (same repository)" in settings("project preview", "import", bundle)
    assert counts[-1] > 0, "positive control: Git tracing must be active"
    if check:
        events = map(json.loads, trace.read_text().splitlines())
        assert sum("--show-toplevel" in event["argv"] for event in events if event["event"] == "start") == 1

    # With no match in the current checkout, still try the bundle's home-relative location.
    other = home / "other"
    git("init", "-q", "--template=", str(other))
    cwd = other
    bundle.write_text(f"milo-settings 1\n[project]\nname = moved\nhome = checkout\nrepository = {commit}\nicons = off\n")
    assert f"{repo} (same repository)" in settings("project preview via home", "import", bundle)

    # An explicit one-shot mapping needs a worktree, not the browser's cross-request review token.
    for action in [(), ("--apply",)]:
        label = "explicit CLI apply" if action else "explicit CLI preview"
        assert f"{other} (chosen by you)" in settings(label, "import", bundle, "--map", "moved=.", *action)
        if check:
            events = map(json.loads, trace.read_text().splitlines())
            assert not any("rev-list" in event["argv"] or "--absolute-git-dir" in event["argv"]
                           for event in events if event["event"] == "start")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="milo-transfer-") as directory:
        sample(args.binary.resolve(strict=True), Path(directory).resolve(), args.check)
