#!/usr/bin/env python3
"""Drive the menu app's Needs you section against a disposable Rex server and save pictures.

Needs a Mac with a menu bar, Rex installed and `sh macos/build-menu.sh` already run. It builds a
copy of the menu app with its own bundle identifier in a throwaway folder under /tmp, gives it a
home of its own, and starts a Rex server on a socket in that folder. The installed Milo, your
settings and the Rex you work in are not read, stopped or changed. Usage in the pictures is made up.
"""

import argparse
import os
from pathlib import Path
import select
import shutil
import subprocess
import time

REPO = Path(__file__).resolve().parents[3]
HERE = Path(__file__).resolve().parent
BUILT = REPO / "target/local-menu/Milo.app"
REX_HOME = "Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS"


def require(condition, message):
    if not condition:
        raise AssertionError(message)


class Lab:
    def __init__(self, rex):
        # A socket's path must be short, so the server and the home are under /tmp. The app is
        # under target: macOS ends an app that is started from /tmp.
        self.root = Path(f"/tmp/milo-menu-lab-{os.getpid()}")
        self.build = REPO / f"target/menu-attention/run-{os.getpid()}"
        for folder in (self.root, self.build):
            shutil.rmtree(folder, ignore_errors=True)
        self.home = self.root / "home"
        self.socket = f"unix://{self.root}/rex.sock"
        self.real_rex = rex
        self.server = None
        (self.home / REX_HOME).mkdir(parents=True)
        # The menu app's Milo finds Rex's command where Rex installs it. Under this home that
        # is a wrapper that can only reach the lab's server and never starts one.
        self.rex = self.home / REX_HOME / "rex"
        self.rex.write_text(
            f"#!/bin/sh\n[ -S '{self.root}/rex.sock' ] || exit 1\n"
            f"exec '{rex}' --autostart=false -S '{self.socket}' \"$@\"\n"
        )
        self.rex.chmod(0o700)
        (self.root / "init.lua").write_text("")
        self.env = {
            "HOME": str(self.home),
            "CFFIXED_USER_HOME": str(self.home),
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "TMPDIR": str(self.root),
            "XDG_RUNTIME_DIR": str(self.root),
            "ZDOTDIR": str(self.home),
        }

    def made_up_usage(self):
        now = time.time()
        stamp = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(now))
        day = self.home / ".codex/sessions" / time.strftime("%Y/%m/%d", time.localtime(now))
        day.mkdir(parents=True)
        window = '{"used_percent":%d,"window_minutes":%d,"resets_at":%d}'
        limits = '{"primary":%s,"secondary":%s}' % (
            window % (36, 300, now + 3 * 3600),
            window % (12, 10080, now + 4 * 86400),
        )
        (day / "rollout-a.jsonl").write_text(
            '{"timestamp":"%sZ","type":"event_msg","payload":{"type":"token_count","rate_limits":%s}}\n'
            % (stamp, limits)
        )

    def start_server(self):
        self.server = subprocess.Popen(
            [self.real_rex, "server", "run", "--restore=false", "--listen", self.socket,
             "--state-dir", str(self.root / "rex-state"), "--config", str(self.root / "init.lua")],
            env=self.env, cwd=self.home, stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        deadline = time.time() + 10
        while time.time() < deadline:
            if subprocess.run([self.rex, "whoami"], env=self.env, capture_output=True).returncode == 0:
                return
            time.sleep(0.1)
        raise AssertionError("the disposable Rex server did not start")

    def stop_server(self):
        if self.server:
            self.server.terminate()
            self.server.wait(timeout=10)
            self.server = None

    def ask(self, *args):
        done = subprocess.run([self.rex, *args], env=self.env, capture_output=True, text=True)
        require(done.returncode == 0, f"rex {args}: {done.stderr}")
        return done.stdout

    def pane(self, session, feed, tab=None):
        """A session of one terminal whose program reports each status appended to `feed`."""
        path = self.root / feed
        path.write_text("")
        script = (
            f"tail -n +1 -f '{path}' | while IFS= read -r status; do "
            "printf '\\033]7501;%s\\033\\\\' \"$status\"; done"
        )
        label = ["--window", tab] if tab else []
        out = self.ask("new", session, *label, "--shell", "none", "--json", "--", "/bin/sh", "-c", script)
        return "block:" + out.split('"block:')[1].split('"')[0]

    def reports(self, feed, status):
        with open(self.root / feed, "a") as file:
            file.write(status + "\n")

    def script_clients(self):
        """How many clients besides this question are connected to the lab's server.
        Rex goes on listing one that has gone as lost for a while; that one is asking nothing."""
        return self.ask("client", "ls", "--json").count('"connection_state": "connected"') - 1

    def build_app(self):
        require(BUILT.is_dir(), "run sh macos/build-menu.sh first")
        app = self.build / "MiloShots.app"
        (app / "Contents/MacOS").mkdir(parents=True)
        shutil.copytree(BUILT / "Contents/Resources", app / "Contents/Resources")
        sources = sorted((REPO / "macos/MenuBar").glob("*.swift"))
        joined = self.build / "App.swift"
        main = (REPO / "macos/MenuBar/App.swift").read_text().replace("\n@main\n", "\n")
        joined.write_text(main + (HERE / "shots.swift").read_text())
        others = [str(source) for source in sources if source.name != "App.swift"]
        machine = os.uname().machine
        subprocess.run(
            ["swiftc", "-O", "-swift-version", "5", "-parse-as-library", "-target", f"{machine}-apple-macos13.0",
             str(joined), *others, "-o", str(app / "Contents/MacOS/MiloShots")],
            check=True,
        )
        plist = (REPO / "macos/Info.plist").read_text()
        plist = plist.replace("net.usemilo.usage.local", f"net.usemilo.usage.shots.p{os.getpid()}")
        plist = plist.replace("<string>Milo</string>", "<string>MiloShots</string>")
        (app / "Contents/Info.plist").write_text(plist)
        version = subprocess.run([app / "Contents/Resources/Tools/milo", "--version"], capture_output=True, text=True)
        subprocess.run(["/usr/bin/plutil", "-insert", "CFBundleShortVersionString", "-string",
                        version.stdout.strip().removeprefix("milo "), str(app / "Contents/Info.plist")], check=True)
        subprocess.run(["codesign", "--force", "--sign", "-", str(app)], check=True, capture_output=True)
        return app / "Contents/MacOS/MiloShots"


class App:
    def __init__(self, lab, program, log):
        self.errors = lab.root / "app.err"
        self.child = subprocess.Popen(
            [program], env=lab.env, cwd=lab.home, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=open(self.errors, "w"), text=True, bufsize=1,
        )
        self.log = log

    def ask(self, command):
        self.child.stdin.write(command + "\n")
        self.child.stdin.flush()
        ready, _, _ = select.select([self.child.stdout], [], [], 15)
        require(ready, f"the app did not answer {command}")
        return self.child.stdout.readline().rstrip("\n")

    def do(self, command):
        answer = self.ask(command)
        require(answer == "ok", f"{command}: {answer}")

    def wait(self, what, *parts, timeout=30):
        """Waits until the app's state line holds every part, and records it."""
        deadline = time.time() + timeout
        state = ""
        while time.time() < deadline:
            state = self.ask("state")
            if all(part in state for part in parts):
                self.log.append(f"{what}: {state}")
                # Let SwiftUI lay the panel out before a picture is taken.
                time.sleep(0.6)
                return state
            time.sleep(0.2)
        raise AssertionError(f"{what}: wanted {parts}, got {state}")

    def quit(self):
        """Ends the app, and says what it wrote if it ended by itself."""
        if self.child.poll() is None:
            self.child.stdin.close()
            self.child.wait(timeout=10)
        else:
            print(f"the app ended with status {self.child.returncode}:\n{self.errors.read_text()}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--rex", default=shutil.which("rex"), help="the rex executable (default: the one on PATH)")
    parser.add_argument("--out", type=Path, default=HERE, help="where to save the pictures")
    args = parser.parse_args()
    require(args.rex, "rex was not found; pass --rex")
    lab = Lab(os.path.realpath(args.rex))
    log = []
    app = None
    out = args.out.resolve()

    def shot(kind, name):
        app.do(f"{kind} {out / name}")

    # The items of More options > Needs you, counted down from the first.
    OFF, MENU, DOT, WAITING, ERRORS, FINISHED = range(6)

    def choose(item, picture=None):
        """Chooses an item of Needs you in the real menu, saving a picture of the open menu if asked."""
        app.do(f"options choose {item} {out / picture if picture else lab.root / 'menu.png'}")

    try:
        lab.made_up_usage()
        lab.start_server()
        app = App(lab, lab.build_app(), log)

        # Off, as installed: the menu is what it was, and nothing is connected to Rex.
        app.wait("off", "show=off", "watching=false", "title= 64%")
        require(lab.script_clients() == 0, "something asked Rex while the section was off")
        app.do("show")
        app.wait("off, open", "show=off")
        shot("panel", "01-off.png")
        shot("status", "02-status-off.png")
        app.do(f"options look {OFF} {out / '03-options-off.png'}")

        # Turned on for the menu only. Waiting and errors are listed; finished work is not.
        choose(MENU)
        app.wait("on, nothing to list", "show=menu", "watching=true", "rex=connected", "rows= ")
        require(lab.script_clients() == 1, "the watch is one Rex script")
        shot("panel", "04-on-empty.png")
        api = lab.pane("api-refactor", "feed-api", tab="Tests")
        lab.pane("docs-site", "feed-docs")
        lab.pane("release-notes", "feed-notes")
        lab.reports("feed-api", "state=blocked:kind=permission:app=claude:msg=c2VjcmV0IHByb21wdA==")
        lab.reports("feed-docs", "state=error:app=codex")
        lab.reports("feed-notes", "state=done:app=codex")
        app.wait("waiting and error", "rows=api-refactor:waiting,docs-site:error ", "dot=false")
        shot("panel", "05-waiting-and-error.png")
        shot("status", "06-status-menu-only.png")

        # The dot is a separate choice, beside the quota badge that was already there.
        choose(DOT)
        app.wait("dot", "show=dot", "dot=true", "title= 64%", "2 Rex panes need you.")
        shot("status", "07-status-dot.png")

        # Each state is included on its own.
        choose(FINISHED)
        app.wait("with finished work", "rows=api-refactor:waiting,docs-site:error,release-notes:done ")
        # Until each row says how long its pane has been in its state.
        app.wait("a minute on", "ages=1m,1m,1m ", timeout=120)
        shot("panel", "08-with-finished.png")
        choose(WAITING)
        app.wait("without waits", "rows=docs-site:error,release-notes:done ")
        shot("panel", "09-errors-and-finished-only.png")
        app.do(f"options look {WAITING} {out / '10-options-on.png'}")
        choose(WAITING)
        app.wait("all three", "rows=api-refactor:waiting,docs-site:error,release-notes:done ")

        # Work resumes: the row leaves by itself. Then the agent asks again.
        lab.reports("feed-api", "state=working:app=claude")
        app.wait("resumed", "rows=docs-site:error,release-notes:done ")
        shot("panel", "11-resumed.png")
        lab.reports("feed-api", "state=blocked:kind=question:app=claude")
        app.wait("asks again", "rows=api-refactor:waiting,docs-site:error,release-notes:done ")

        # The row jumps to its own pane. No Rex app is connected to this lab server, so Rex
        # focuses the pane and the menu says why it could not show it.
        session = lab.ask("block", "inspect", api, "--json").split('"session_id": "')[1].split('"')[0]
        tab = lab.ask("block", "inspect", api, "--json").split('"window_id": "')[1].split('"')[0]
        lab.ask("window", "new", "Other", "-s", session, "--json")
        require(f'"active_window_id": "{tab}"' not in lab.ask("session", "inspect", session, "--json"), "tab not moved")
        app.do("jump api-refactor")
        app.wait("jumped", "note=The pane is focused in its session, but no Rex app on this Mac")
        view = lab.ask("session", "inspect", session, "--json")
        require(f'"active_window_id": "{tab}"' in view and f'"focused_block_id": "{api}"' in view, "wrong pane")
        log.append("jump: Rex made the pane's tab active and the pane its focused block")
        app.wait("nothing else was cleared", "rows=api-refactor:waiting,docs-site:error,release-notes:done ")
        shot("panel", "12-jump-explained.png")

        # Marked seen: that row only.
        app.do("seen release-notes")
        app.wait("seen", "rows=api-refactor:waiting,docs-site:error ")
        shot("panel", "13-marked-seen.png")

        # The other indicators still work beside it.
        app.do("indicator cpu")
        app.wait("cpu", "% tip=CPU busy across all cores", "dot=true")
        shot("status", "14-status-cpu-dot.png")
        shot("panel", "15-cpu-indicator.png")
        app.do("indicator quota")
        app.wait("quota again", "title= 64%", "dot=true")

        # Rex stops: its panes are not counted. It comes back: only what it reports now is listed.
        lab.stop_server()
        app.wait("Rex stopped", "rex=waiting", "rows= ", "dot=false", "tip=Codex")
        shot("panel", "16-rex-stopped.png")
        shot("status", "17-status-rex-stopped.png")
        lab.start_server()
        app.wait("Rex back", "rex=connected", "rows= ", "dot=false", timeout=60)
        lab.pane("migration", "feed-migration")
        lab.reports("feed-migration", "state=blocked:kind=auth:app=codex")
        app.wait("after reconnect", "rows=migration:waiting ", "dot=true")
        shot("panel", "18-reconnected.png")

        # Off again: the watch is gone.
        choose(OFF)
        app.wait("off again", "show=off", "watching=false", "dot=false")
        deadline = time.time() + 10
        while lab.script_clients() and time.time() < deadline:
            time.sleep(0.2)
        require(lab.script_clients() == 0, "the watch outlived the choice: " + lab.ask("client", "ls", "--json"))
        log.append("off: no client is connected to Rex")
        shot("panel", "19-off-again.png")
        app.do("close")
    finally:
        # The server is the lab's own child. It is stopped whatever became of the app.
        try:
            if app:
                app.quit()
        finally:
            lab.stop_server()
            (out / "walkthrough.log").write_text("\n".join(log) + "\n")
            shutil.rmtree(lab.root, ignore_errors=True)
            shutil.rmtree(lab.build, ignore_errors=True)
    print(f"Saved pictures and walkthrough.log in {out}")


if __name__ == "__main__":
    main()
