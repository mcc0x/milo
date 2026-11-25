"""Runs a program in a terminal of its own, 100 by 30, under a throwaway home, and prints the screen it left.

usage: PROBE_HOME=DIR [PROBE_CWD=DIR] [PROBE_NAME=value]... probe.py SECONDS [--type TEXT@SECOND]... -- PROGRAM ARGS...

PROBE_HOME becomes HOME, with the XDG folders and TMPDIR below it. Any other PROBE_NAME=value reaches the program
as NAME=value, PATH included, and nothing else of the caller's environment does. Each --type writes TEXT to the
terminal SECOND seconds in, with escapes such as \\r decoded. After SECONDS the program is ended. Needs pyte,
in a virtual environment of its own. It was used to see what each agent's release draws; see README.md.
"""
import os, pty, sys, time, select, fcntl, termios, struct, signal, pyte
args = sys.argv[1:]
secs = float(args.pop(0)); typed = []
while args[0] != "--":
    assert args.pop(0) == "--type"; text, at = args.pop(0).rsplit("@", 1); typed.append((float(at), text.encode().decode("unicode_escape").encode()))
args.pop(0)
home = os.environ["PROBE_HOME"]; os.makedirs(home, exist_ok=True)
env = {"HOME": home, "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "TERM": "xterm-256color", "LANG": "en_US.UTF-8",
       "XDG_CONFIG_HOME": home + "/.config", "XDG_DATA_HOME": home + "/.local/share", "XDG_STATE_HOME": home + "/.local/state",
       "XDG_CACHE_HOME": home + "/.cache", "TMPDIR": home + "/tmp", "NO_OPEN_BROWSER": "1", "BROWSER": "/usr/bin/true"}
env.update({k[6:]: v for k, v in os.environ.items() if k.startswith("PROBE_") and k != "PROBE_HOME" and k != "PROBE_CWD"})
os.makedirs(env["TMPDIR"], exist_ok=True)
pid, fd = pty.fork()
if pid == 0:
    os.chdir(os.environ.get("PROBE_CWD", home)); os.execvpe(args[0], args, env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
class S(pyte.Screen):
    def report_device_status(self, *a, **k): pass
screen = S(100, 30); stream = pyte.ByteStream(screen)
start = time.time(); status = None
while time.time() - start < secs:
    for t in [t for t in typed if t[0] <= time.time() - start]:
        typed.remove(t); os.write(fd, t[1])
    r, _, _ = select.select([fd], [], [], 0.1)
    if r:
        try: data = os.read(fd, 65536)
        except OSError: data = b""
        if not data: break
        stream.feed(data)
        # answer terminal queries a TUI may wait on
        if b"\x1b[6n" in data: os.write(fd, b"\x1b[1;1R")
        if b"\x1b[c" in data: os.write(fd, b"\x1b[?62;c")
done, st = os.waitpid(pid, os.WNOHANG)
if done: status = os.waitstatus_to_exitcode(st)
else:
    os.kill(pid, signal.SIGTERM); time.sleep(0.5)
    try: os.kill(pid, signal.SIGKILL)
    except OSError: pass
print("\n".join(l.rstrip() for l in screen.display if l.strip()))
print(f"--- exit: {status if status is not None else 'still running (killed)'}")
