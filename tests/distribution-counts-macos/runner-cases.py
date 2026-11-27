from datetime import date, datetime, timedelta, timezone
from pathlib import Path
import fcntl, json, os, pty, re, select, signal, struct, subprocess, sys, termios, time

root,binary=map(Path,sys.argv[1:3])
day=sys.argv[3]
ledger=root/"ledger.jsonl"

def events():
    return [json.loads(line) for line in ledger.read_text().splitlines()]

def assert_day():
    assert datetime.now(timezone.utc).date().isoformat()==day, "UTC date crossed; repeat with clean profiles"

def wait(predicate,seconds=7):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        if predicate(): return
        time.sleep(.05)
    raise AssertionError("expected fixture state not witnessed before timeout")

def quiet(expected):
    end=time.monotonic()+3.4
    while time.monotonic()<end:
        assert events()==expected, "unexpected request during bounded silence window"
        time.sleep(.05)

def frame(pty_fd):
    end=time.monotonic()+8
    rendered=b""
    while time.monotonic()<end:
        if select.select([pty_fd],[],[],.1)[0]:
            rendered+=os.read(pty_fd,65536)
            rendered=re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]",b"",rendered)
            if b"q quit" in rendered:
                return
    raise AssertionError("Usage did not enter its interactive pane")

def profile(name):
    home=root/name
    home.mkdir()
    env={"HOME":str(home),"PATH":"/usr/bin:/bin","SHELL":"/bin/sh",
         "TERM":"xterm-256color","LC_ALL":"en_US.UTF-8",
         "CFFIXED_USER_HOME":str(home)}
    for key,dirname in [("XDG_CONFIG_HOME","config"),("XDG_STATE_HOME","state"),
                        ("XDG_DATA_HOME","data"),("XDG_CACHE_HOME","cache"),
                        ("XDG_RUNTIME_DIR","runtime"),("TMPDIR","tmp")]:
        path=home/dirname
        path.mkdir(mode=0o700)
        env[key]=str(path)
    return home,env

def counts_files(home):
    return sorted(p for p in home.rglob("*") if "anonymous-counts" in p.name)

def preference(home, value):
    path=home/"config/milo/anonymous-counts.conf"
    path.parent.mkdir()
    path.write_bytes(value)
    return path

def seed(home, value):
    path=home/"state/milo/anonymous-counts.state"
    path.parent.mkdir()
    path.write_bytes(value)
    return path

def day_index(d):
    return str((d-date(1970,1,1)).days).encode()+b"\n"

def assert_silent_profile(home, allowed, original):
    assert counts_files(home)==sorted(allowed)
    assert all(p.read_bytes()==allowed[p] for p in allowed)
    assert events()==original

def order(home, env, *argv):
    return subprocess.run([binary,*argv],cwd=home,env=env,capture_output=True,
                          stdin=subprocess.DEVNULL,start_new_session=True,timeout=15)

def launch(home,env,observe=None):
    assert_day()
    pid,fd=pty.fork()
    if not pid:
        os.chdir(home)
        fcntl.ioctl(0,termios.TIOCSWINSZ,struct.pack("HHHH",30,100,0,0))
        os.execve(binary,[str(binary),"usage"],env)
    ended=False
    try:
        frame(fd)
        if observe: observe()
        os.write(fd,b"q")
        def reaped():
            nonlocal ended
            done,status=os.waitpid(pid,os.WNOHANG)
            if done:
                ended=True
                assert status==0
            return ended
        wait(reaped,8)
    finally:
        os.close(fd)
        if not ended:
            try: os.kill(pid,signal.SIGKILL)
            except ProcessLookupError: pass
            os.waitpid(pid,0)
    assert_day()

control=events()
assert len(control)==1
assert control[0]["event"]=="accepted" and control[0]["tls"] in ("TLSv1.2","TLSv1.3")
assert control[0]["day"]==day and not control[0]["new_install"] and not control[0]["monthly_active"]

home,env=profile("opt-out")
settings=preference(home,b"on\n")
launch(home,{**env,"MILO_TELEMETRY":"0"})
launch(home,{**env,"MILO_TELEMETRY":"0"})
quiet(control)
assert_silent_profile(home,{settings:b"on\n"},control)
print("PASS packaged opt-out overrides saved On: no request or counts state")

home,env=profile("saved-off")
settings=preference(home,b"off\n")
state=seed(home,b"first-seen\n")
launch(home,env)
quiet(control)
assert_silent_profile(home,{settings:b"off\n",state:b"first-seen\n"},control)
print("PASS saved Off preserves eligible state and suppresses the request")

for key in ("CI","GITHUB_ACTIONS"):
    home,env=profile("explicit-"+key)
    state=seed(home,b"first-seen\n")
    launch(home,{**env,key:"true"})
    quiet(control)
    assert_silent_profile(home,{state:b"first-seen\n"},control)
print("PASS CI and GITHUB_ACTIONS each suppress an otherwise eligible launch")

home,env=profile("noninteractive")
state=seed(home,b"first-seen\n")
v=order(home,env,"--version")
assert v.returncode == 0 and re.fullmatch(rb"milo [0-9]+\.[0-9]+\.[0-9]+\n", v.stdout)
subprocess.run(["/usr/bin/git","init","-q","--template=",str(home/"fixture")],env=env,
               cwd=home,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True,timeout=10)
o=order(home,env,"outline",str(home/"fixture"))
assert o.returncode!=0 and b"needs an interactive terminal" in o.stderr
quiet(control)
assert_silent_profile(home,{state:b"first-seen\n"},control)
print("PASS --version and noninteractive Outline did not claim or send counts")

home,env=profile("reporting")
state=home/"state/milo/anonymous-counts.state"
launch(home,env)
quiet(control)
assert state.read_bytes()==b"first-seen\n"
print("PASS first packaged interactive visit persists marker but does not POST")
launch(home,env,lambda:wait(lambda:len(events())>len(control)))
positive=events()
assert len(positive)==len(control)+1 and positive[:-1]==control
witness=positive[-1]
assert witness["event"]=="accepted" and witness["tls"] in ("TLSv1.2","TLSv1.3")
assert witness["day"]==day and witness["new_install"] is True and witness["monthly_active"] is True
assert state.read_bytes()==day_index(date.fromisoformat(day))
print("PASS unchanged candidate system curl sent the exact anonymous POST over privately trusted TLS")
launch(home,env)
quiet(positive)
print("PASS same-UTC-day rerun sends nothing")


today=date.fromisoformat(day)
previous_month_end=today.replace(day=1)-timedelta(days=1)
home,env=profile("previous-month")
state=seed(home,day_index(previous_month_end))
launch(home,env,lambda:wait(lambda:len(events())>len(positive)))
rollover=events()
assert len(rollover)==len(positive)+1 and rollover[:-1]==positive
witness=rollover[-1]
assert witness["event"]=="accepted" and witness["tls"] in ("TLSv1.2","TLSv1.3")
assert witness["day"]==day and witness["new_install"] is False and witness["monthly_active"] is True
assert state.read_bytes()==day_index(today)
launch(home,env)
quiet(rollover)
print("PASS seeded previous month reports monthly active but not new install, once today")
