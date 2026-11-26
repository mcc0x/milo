"""Exercise an installed Linux Milo: python3 tests/linux-standalone.py /absolute/path/to/milo"""

from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import tempfile
import time


def git_fixture(repo, env):
    repo.mkdir()

    def git(*args, data=None):
        subprocess.run(["git", *args], cwd=repo, env=env, input=data, check=True, timeout=10)

    git("init", "-q", "--template=", "-b", "main")
    git("fast-import", "--quiet", "--done", data=b"""commit refs/heads/main
committer Milo Fixture <fixture@example.invalid> 1000000000 +0000
data <<END
Fixture commit
END
M 100644 inline notes.txt
data <<END
Fixture text
END
done
""")
    git("restore", "--source=HEAD", "--staged", "--worktree", ".")


def wait_for(predicate, description):
    deadline = time.monotonic() + 10
    while not predicate():
        if time.monotonic() >= deadline:
            raise RuntimeError(f"Timed out waiting for {description}")
        time.sleep(0.05)


def terminal(binary, home, repo, env):
    def tmux(*args, check=True):
        return subprocess.run(["tmux", "-S", home / "socket", "-f", "/dev/null", *args],
                              cwd=repo, env=env, check=check, capture_output=True, timeout=10)

    def see(text):
        wait_for(lambda: text.encode() in tmux("capture-pane", "-p", "-t", "lab").stdout, text)

    def send(*keys):
        tmux("send-keys", "-t", "lab", *keys)

    shell = '''
set -e
before=$(stty -g)
"$1"
[ "$(stty -g)" = "$before" ]
"$1" outline
[ "$(stty -g)" = "$before" ]
: > "$HOME/restored"
'''
    notes = repo / "notes.txt"
    try:
        tmux("new-session", "-d", "-s", "lab", "-x", "100", "-y", "30", "-c", repo,
             "/bin/sh", "-c", shell, "lab", binary)
        see("Open browser settings")
        send("Enter")
        see("notes.txt")
        send("Enter")
        see("Fixture text")
        send("e")
        see("^S save")
        send("-l", "saved: ")
        send("C-s")
        see("Saved")
        assert notes.read_bytes() == b"saved: Fixture text\n"

        send("-l", "draft: ")
        see("draft: Fixture text")
        outside = b"External edit\r\n"
        notes.write_bytes(outside)
        send("C-s")
        see("Changed on disk")
        assert notes.read_bytes() == outside
        send("Escape")
        see("^S save")
        send("Escape")
        see("d discard")
        send("d")
        see("e edit")
        assert notes.read_bytes() == outside
        send("q")
        see("c changed only")
        send("q")
        see("Open browser settings")

        send("Down", "Enter")
        see("Fixture commit")
        send("Enter")
        see("notes.txt")
        send("Down", "Enter")
        see("Fixture text")
        send("q")
        see("Fixture commit")
        send("q")
        see("Open browser settings")
        send("q")
        see("c changed only")
        send("C-c")
        wait_for((home / "restored").exists, "clean exits with restored terminal modes")
    finally:
        tmux("kill-server", check=False)
    print("PASS: Home, Outline, Reader save and conflict, History; tty restored after quit and Ctrl-C")


def settings(binary, home, repo, env):
    editor = runpy.run_path(str(Path(__file__).with_name("settings-bench.py")))["editor"]
    choices = {"usage_claude": "always", "usage_opencode": "hide"}
    prefs = home / "config/milo/preferences.conf"
    with editor(binary, home) as request:
        state = request()
        assert not state["context"]["inside_rex"]
        for field, value in choices.items():
            state = request({"action": "set", "version": state["version"],
                             "field": f"appearance.{field}", "value": value})
        assert state["dirty"] and not prefs.exists()
        saved = request({"action": "save", "version": state["version"], "group": "preferences", "scope": "global"})
        assert not saved["dirty"]
    assert prefs.read_text().splitlines() == [f"{field} = {value}" for field, value in choices.items()]
    with editor(binary, home) as reopened:
        appearance = reopened()["appearance"]
        assert all(appearance[field] == value for field, value in choices.items())
    doctor = subprocess.check_output([binary, "doctor"], cwd=repo, env=env, text=True, timeout=15)
    assert "Claude Code limits: no local report;" in doctor
    assert "OpenCode hidden" in doctor and "OpenCode activity:" not in doctor
    print("PASS: headless settings draft, save and restart; Doctor reports missing Claude and hidden OpenCode")


if __name__ == "__main__":
    if len(sys.argv) != 2 or not Path(sys.argv[1]).is_absolute():
        sys.exit(__doc__)
    binary = Path(sys.argv[1]).resolve(strict=True)
    path = "/usr/bin:/bin"
    if shutil.which("rex", path=path):
        sys.exit("This standalone test requires a system PATH without Rex")
    with tempfile.TemporaryDirectory(prefix="milo-standalone-", dir="/tmp") as root:
        home = Path(root)
        (home / "run").mkdir(mode=0o700)
        repo = home / "checkout"
        env = {"PATH": path, "HOME": str(home), "SHELL": "/bin/sh", "TERM": "xterm-256color",
               "LC_ALL": "C.UTF-8", "XDG_CONFIG_HOME": str(home / "config"), "XDG_STATE_HOME": str(home / "state"),
               "XDG_RUNTIME_DIR": str(home / "run"), "TMPDIR": str(home), "TMUX_TMPDIR": str(home / "run"),
               "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null"}
        git_fixture(repo, env)
        terminal(binary, home, repo, env)
        settings(binary, home, repo, env)
