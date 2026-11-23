use super::{Lab, PATIENCE, id};
use std::cell::Cell;
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

const READY: &str = "Milo Preview: ready to confirm.";
const ACCEPTED: &str = "milo preview: confirmation accepted; checking the tile";
const VISIBLE: &str = "milo preview: preview visible";
const ROUTE_READY: &str = "milo preview: divider released";
const PAUSED: &str = "milo preview: preview paused for the Rex divider";
const BAD_STACK: &str = "milo preview: not shown: post-order: helper panel is behind the pinned Rex window";

fn records(log: &str) -> impl Iterator<Item = &str> {
    log.split_inclusive('\n').map_while(|line| line.strip_suffix('\n'))
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PaneFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    divider_x: f64,
}

impl PaneFrame {
    fn parse(line: &str) -> Self {
        let dimensions = line.split_whitespace().map(|n| n.parse::<f64>().unwrap()).collect::<Vec<_>>();
        let [x, y, width, height, divider_x] = dimensions.try_into().unwrap();
        Self { x, y, width, height, divider_x }
    }

    fn approach(self) -> (f64, f64) {
        (self.x - 1.0, self.y + self.height / 2.0)
    }

    fn divider(self) -> (f64, f64) {
        (self.divider_x, self.y + self.height / 2.0)
    }

    fn center(self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

struct Tile<'a> {
    lab: &'a Lab,
    root: &'a Path,
    session: &'a str,
    block: &'a str,
    window: &'a str,
    pointer_serial: Cell<u32>,
}

impl Tile<'_> {
    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root.join(name)).unwrap_or_default()
    }

    fn capture(&self) -> String {
        self.lab.rex(&["capture", "-s", self.session, "-b", self.block, "--trim", "--unwrap"])
    }

    fn count(&self, message: &str) -> usize {
        self.capture().matches(message).count()
    }

    fn event(&self, event: &str) -> usize {
        records(&self.read("native.log")).filter(|line| *line == event).count()
    }

    fn geometry(&self) -> Vec<String> {
        records(&self.read("native.log"))
            .filter_map(|line| line.strip_prefix("stage ").map(str::to_owned))
            .collect()
    }

    fn frame(&self) -> PaneFrame {
        PaneFrame::parse(
            records(&self.read("native.log"))
                .filter_map(|line| line.strip_prefix("projected "))
                .last()
                .expect("no geometry from the real Rex layout"),
        )
    }

    fn pointer(&self, (x, y): (f64, f64), down: bool) {
        let serial = self.pointer_serial.get() + 1;
        self.pointer_serial.set(serial);
        let next = self.root.join("pointer-next");
        fs::write(&next, format!("{serial} {x} {y} {}\n", u8::from(down))).unwrap();
        fs::rename(next, self.root.join("pointer-state")).unwrap();
        self.wait("the fixture did not sample the modeled pointer", || self.event(&format!("pointer {serial}")) == 1);
    }

    fn scope(&self, script: &str) -> String {
        self.lab.rex(&["do", "-s", self.session, "-e", script])
    }

    fn resize(&self, viewer: &mut impl FnMut(&str, &str), main: &str, ratio: f64) -> PaneFrame {
        let old = self.frame();
        let guards = self.state("guard ");
        viewer("session.resize_pane", &format!(
            r#"{{"session_id":"{}","direction":"horizontal","before_block_id":"{main}","after_block_id":"{}","ratio":{ratio}}}"#,
            self.session, self.block,
        ));
        self.wait("real Rex resize did not move the preview tile", || self.state("guard ") > guards && self.frame() != old);
        self.frame()
    }

    fn check_focus(&self, expected: &str) {
        self.scope(&format!(
            "local view=assert(rex.session.view{{}}); for _,w in ipairs(view.windows) do \
             if w.window_id=='{}' then assert(w.focused_block_id=='{expected}',tostring(w.focused_block_id)); return end \
             end; error('missing working tab')",
            self.window,
        ));
    }

    fn state(&self, prefix: &str) -> usize {
        records(&self.read("native-watch.log")).filter(|line| line.starts_with(prefix)).count()
    }

    fn focus(&self, target: &str) {
        self.lab.rex(&["focus", "-s", self.session, target]);
    }

    fn type_text(&self, text: &str) {
        self.lab.rex(&["send", "-s", self.session, "-b", self.block, text]);
    }

    fn enter(&self) {
        self.lab.rex(&["send-key", "-s", self.session, "-b", self.block, "enter"]);
    }

    fn show(&self) {
        self.type_text("show");
        self.enter();
    }

    fn wait(&self, description: &str, mut done: impl FnMut() -> bool) {
        let until = Instant::now() + PATIENCE;
        while !done() {
            assert!(
                Instant::now() < until,
                "{description}\nterminal:\n{}\nnative:\n{}\nwatcher:\n{}{}",
                self.capture(),
                self.read("native.log"),
                self.read("native-watch.log"),
                self.read("native-watch.errors"),
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn samples(&self, count: usize) {
        let start = self.event("sample");
        self.wait("missing fresh sampling ticks", || self.event("sample") >= start + count);
    }

    fn reveal(&self, count: usize) {
        self.wait("guarded confirmation did not reveal the panel", || {
            self.count(VISIBLE) >= count && self.event("revealed") >= count
        });
        assert_eq!(self.count(ACCEPTED), count, "{}", self.capture());
    }

    fn presentation(&self) -> (usize, usize, usize) {
        (self.geometry().len(), self.event("revealed"), self.count(ACCEPTED))
    }

    // The Lua fixtures cover when Rex snapshots emit the marker. Deliver
    // that same pipe record at a deterministic native gesture boundary.
    fn model_revision_gap(&self) {
        let mut states = fs::OpenOptions::new().write(true).open(self.root.join("native-input")).unwrap();
        states.write_all(b"resize-gap\n").unwrap();
    }

    fn start_drag(&self) -> (usize, usize, usize) {
        self.check_focus(self.block);
        let counts = self.presentation();
        let frame = self.frame();
        let hidden = self.event("hidden");
        let paused = self.count(PAUSED);
        self.pointer(frame.approach(), false);
        self.wait("crossing the right tile's left edge did not conceal before the press", || {
            self.event("hidden") == hidden + 1 && self.count(PAUSED) == paused + 1
        });
        self.pointer(frame.divider(), true);
        assert_eq!(self.presentation(), counts, "pressing the divider staged or revealed the page");
        counts
    }

    fn route_ready(&self, previous: usize) {
        self.wait("Rex divider return did not arm on the terminal", || self.count(ROUTE_READY) > previous);
    }

    fn motion(&self, viewer: &mut impl FnMut(&str, &str)) {
        let log = self.read("native.log");
        let center = records(&log).filter_map(|line| line.strip_prefix("cell-center ")).last().unwrap();
        let (x, y) = center.split_once(' ').unwrap();
        viewer("block.event", &format!(
            r#"{{"session_id":"{}","block_id":"{}","event":{{"type":"mouse","payload":{{"action":"motion","x":{x},"y":{y}}}}}}}"#,
            self.session, self.block,
        ));
    }

    fn confirm_local_receipt(&self) {
        self.wait("no local-only menu receipt after release", || self.read("test-menu-item").len() == 6);
        self.type_receipt(&self.read("test-menu-item"));
    }

    fn type_receipt(&self, digits: &str) {
        assert!(digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_digit()));
        assert!(!self.capture().contains(digits), "native receipt appeared in the shared terminal");
        let mut args = vec!["send-key", "-s", self.session, "-b", self.block];
        args.extend((0..6).map(|i| &digits[i..i + 1]));
        args.push("enter");
        self.lab.rex(&args);
    }

    fn active_route_modes(&self) -> usize {
        self.scope(&format!(
            "local modes=assert(rex.call('com.superlogical.terminal.modes',{{session_id='{}',block_id='{}'}})).modes; \
             local count=0; for _,m in ipairs(modes) do \
             if m.set and (m.number==1003 or m.number==1006 or m.number==2004) then count=count+1 end end; print(count)",
            self.session, self.block,
        )).trim().parse().unwrap()
    }

    fn restored(&self, (stages, reveals, confirmations): (usize, usize, usize), new: PaneFrame) {
        self.wait("fresh routed input did not reprove and reveal the same WebKit form", || {
            self.event("revealed") == reveals + 1
                && self.count(VISIBLE) == reveals + 1
                && self.event("webkit-state-preserved") == reveals
        });
        assert_eq!(self.presentation(), (stages + 1, reveals + 1, confirmations));
        let stage = format!("{:.1} {:.1} {:.1} {:.1}", new.x, new.y, new.width, new.height);
        assert_eq!(self.geometry().last(), Some(&stage), "reveal did not follow the new Rex divider");
        self.check_focus(self.block);
    }
}

fn serve_fixture(listener: TcpListener, expect_form: bool) {
    const PAGE: &str = "<!doctype html><meta charset=utf-8><link rel=icon href=data:,>\
        <style>html,body{background:#08ba49}</style><p id=rex-lab-page>Rex preview fixture</p>\
        <a href=/rex-lab-form id=divider-link>Open the form</a>";
    const FORM: &str = "<!doctype html><meta charset=utf-8><link rel=icon href=data:,>\
        <form autocomplete=off><input id=divider-form></form>";
    listener.set_nonblocking(true).unwrap();
    let until = Instant::now() + 3 * PATIENCE;
    let (mut page_served, mut form_served) = (false, false);
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut lines = BufReader::new(&mut stream).lines();
                let request = lines.next().and_then(Result::ok).unwrap_or_default();
                while lines.next().and_then(Result::ok).is_some_and(|line| !line.is_empty()) {}
                let body = if request.starts_with("GET /rex-lab-page ") {
                    assert!(!page_served, "WebKit unexpectedly reloaded the initial page");
                    page_served = true;
                    PAGE
                } else if request.starts_with("GET /rex-lab-form ") {
                    assert!(!form_served, "WebKit unexpectedly reloaded the form");
                    form_served = true;
                    FORM
                } else {
                    continue;
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                )
                .unwrap();
                if page_served && (!expect_form || form_served) {
                    return;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < until, "the isolated WKWebView never requested its page");
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("loopback fixture: {error}"),
        }
    }
}

// cargo test --test rex_lab preview_confirmation_uses_rex_pty -- --ignored --nocapture
#[test]
#[ignore = "needs Rex, Python 3, Xcode tools and a macOS GUI login; uses only a disposable server and an offscreen panel"]
fn preview_confirmation_uses_rex_pty() {
    let mut lab = Lab::start();
    let compile = lab
        .command("/usr/bin/xcrun")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "clang",
            "-fobjc-arc",
            "-mmacosx-version-min=13.0",
            "tests/preview_rex_tick.m",
            "macos/Preview/Browser.m",
            "-framework",
            "AppKit",
            "-framework",
            "CoreGraphics",
            "-framework",
            "WebKit",
            "-o",
        ])
        .arg(lab.root.join("preview-rex-tick"))
        .output()
        .unwrap();
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));

    let root = lab.root.to_str().unwrap();
    let new_session = |name: &str| {
        lab.rex(&["new", name, "-c", root, "--shell=none", "--json", "--", "/bin/sleep", "300"])
    };
    let created = new_session("native-guard");
    let session = id(&created, "session_id");
    let tab = id(&created, "window_id");
    let main = id(&created, "block_ids");
    let another_tab = lab.new_tab(&session, &lab.root);
    let attached = ["native-attached-a", "native-attached-b", "native-attached-c"]
        .map(|name| {
            let created = new_session(name);
            (id(&created, "session_id"), id(&created, "block_ids"))
        });

    let mut peer = lab
        .command("python3")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rex_lab/preview_viewer.py"))
        .arg(lab.root.join("s.sock"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = peer.id();
    let mut input = peer.stdin.take().unwrap();
    let mut output = BufReader::new(peer.stdout.take().unwrap());
    lab.app = Some(peer);
    let mut client = String::new();
    output.read_line(&mut client).unwrap();
    assert!(client.starts_with("client:"), "{client}");
    let client = client.trim();
    let mut viewer = |method: &str, payload: &str| {
        writeln!(input, r#"["{method}",{payload}]"#).unwrap();
        let mut reply = String::new();
        output.read_line(&mut reply).unwrap();
        assert!(reply.contains(r#""type":"response""#), "{method}: {reply}");
    };
    let owner_session = format!(r#"{{"session_id":"{session}"}}"#);
    viewer("session.attach", &owner_session);
    for (attached, _) in &attached {
        viewer("session.attach", &format!(r#"{{"session_id":"{attached}"}}"#));
    }
    let check_attachments = format!(
        "for _,c in ipairs(assert(rex.call('client.list',{{kinds={{'app'}}}})).clients) do \
         if c.client_id=='{client}' then assert(#c.session_ids==4 and c.principal.pid=={pid}); return end end; \
         error('missing disposable viewer')"
    );
    lab.rex(&["do", "-s", &session, "-e", &check_attachments]);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/rex-lab-page", listener.local_addr().unwrap());
    let page = thread::spawn(move || serve_fixture(listener, true));
    let launch = |root: &Path, main: &str, tab: &str, url: &str| {
        let started = lab.rex(&[
            "split", "-s", &session, "-b", main, "--split=right", "--ratio", "40", "--focus=false", "--shell=none",
            "--label", "milo:preview:confirm", "--keep-open", "--json", "--", "/bin/bash",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rex_lab/preview_tick.sh"), root.to_str().unwrap(),
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/rex/preview.lua"), client, &pid.to_string(), main, tab, url,
        ]);
        id(&started, "block_ids")
    };
    let block = launch(&lab.root, &main, &tab, &url);
    let tile = Tile {
        lab: &lab, root: &lab.root, session: &session, block: &block, window: &tab, pointer_serial: Cell::new(0),
    };
    viewer("session.own_block", &owner_session);
    tile.focus(&main);
    tile.wait("the real watcher did not start hidden", || tile.state("hide") >= 1);
    // A line that arrived before the tile became eligible must not open it.
    tile.show();
    tile.focus(&block);
    tile.wait("focus did not arm the tile", || tile.state("guard ") >= 1 && tile.count(READY) == 1);
    tile.wait("the guarded watcher did not renew", || tile.state("renew") >= 2);
    tile.samples(4);
    assert_eq!((tile.count(READY), tile.count(ACCEPTED), tile.geometry().len()), (1, 0, 0));

    // A real focus loss is a hard boundary; a half command cannot follow the
    // user out of the tile and become valid when focus returns.
    let guards = tile.state("guard ");
    let hides = tile.state("hide");
    tile.type_text("sh");
    tile.focus(&main);
    tile.wait("scope loss was not reported", || tile.state("hide") > hides);
    tile.focus(&block);
    tile.wait("focus was not restored", || tile.state("guard ") > guards);
    tile.samples(4);
    assert_eq!(tile.count("any pending input was discarded"), 1, "a real focus reset must explain the lost partial line");
    tile.type_text("ow");
    tile.enter();
    tile.samples(2);
    assert_eq!((tile.count(READY), tile.count(ACCEPTED), tile.event("revealed")), (1, 0, 0));
    let guards = tile.state("guard ");
    let hides = tile.state("hide");
    tile.type_text("sh");
    tile.focus(&main);
    tile.wait("second scope loss was not observed", || tile.state("hide") > hides);
    tile.focus(&block);
    tile.wait("second scope restoration was not observed", || tile.state("guard ") > guards);
    tile.samples(4);
    tile.type_text("ow");
    tile.enter();
    tile.wait("truncated command after a second reset had no explanation", || tile.count("input followed a Rex focus change") == 1);

    // Real input spans multiple ticks and repeated late Quartz replies, without
    // flushing the canonical line or announcing a new readiness epoch.
    let delay = lab.root.join("delay-samples");
    fs::write(&delay, "").unwrap();
    tile.wait("missing stalled geometry", || tile.event("late-sample") >= 1);
    tile.show();
    tile.wait("complete command vanished during a sampling stall", || tile.count("not shown: Rex tile geometry is stale") == 1);
    tile.type_text("sh");
    let late = tile.event("late-sample");
    tile.wait("missing intentionally late samples", || tile.event("late-sample") >= late + 3);
    assert_eq!((tile.count(READY), tile.count(ACCEPTED), tile.geometry().len()), (1, 0, 0));
    fs::remove_file(delay).unwrap();
    tile.samples(5);
    tile.type_text("ow");
    tile.samples(2);
    assert_eq!(tile.count(ACCEPTED), 0, "native code read an incomplete line");
    tile.enter();
    tile.reveal(1);
    assert_eq!(tile.event("cold-start-delay"), 1, "cold WebKit startup must not require another confirmation");
    assert_eq!(tile.count(READY), 1, "sampling or guard renew repeatedly rearmed the PTY");
    tile.wait("real WebKit did not load the loopback fixture", || tile.event("webkit-loaded") == 1);
    let until = Instant::now() + Duration::from_secs(2);
    while tile.event("webkit-offscreen-paint") + tile.event("webkit-snapshot-unavailable") == 0 && Instant::now() < until {
        thread::sleep(Duration::from_millis(50));
    }
    if tile.event("webkit-offscreen-paint") == 1 {
        eprintln!("real WebKit offscreen snapshot painted the fixture page; stacking modeled");
    } else {
        eprintln!("real WebKit loaded the fixture DOM; offscreen paint not confirmed; stacking modeled");
    }
    fs::write(lab.root.join("seed-form"), "").unwrap();
    tile.wait("the actual WebKit view did not navigate and retain a form value", || tile.event("webkit-form-seeded") == 1);
    page.join().unwrap();

    tile.focus(&main);
    tile.wait("losing tile focus did not conceal it", || tile.state("hide") >= 2 && tile.event("hidden") == 1);
    let guards = tile.state("guard ");
    tile.focus(&block);
    tile.wait("refocusing the tile did not restore guarded eligibility", || tile.state("guard ") > guards);
    tile.samples(4);
    tile.show();
    tile.reveal(2);
    tile.focus(&another_tab);
    tile.wait("leaving the Rex tab did not conceal the panel", || tile.state("hide") >= 3 && tile.event("hidden") == 2);
    let guards = tile.state("guard ");
    tile.focus(&block);
    tile.wait("returning to the tab did not restore guarded eligibility", || tile.state("guard ") > guards);
    tile.samples(4);
    assert_eq!(tile.event("revealed"), 2, "concealed consent survived a focus/tab change");
    tile.show();
    tile.reveal(3);

    let before = tile.geometry();
    let guards = tile.state("guard ");
    let divider = format!(
        "assert(rex.session.resize_pane{{direction='horizontal',before_block_id='{main}',after_block_id='{block}',ratio=0.45}})"
    );
    lab.rex(&["do", "-s", &session, "-e", &divider]);
    tile.wait("changing the Rex divider did not invalidate placement", || {
        tile.state("guard ") > guards && tile.event("hidden") == 3
    });
    tile.samples(4);
    assert_eq!(tile.event("revealed"), 3, "a moved divider reused old consent");
    tile.show();
    tile.reveal(4);
    let resized = tile.geometry();
    assert_ne!(before.last(), resized.last(), "native placement did not follow the Rex divider");

    tile.focus(&main);
    tile.wait("placement did not conceal after focus loss", || tile.event("hidden") == 4);
    fs::write(lab.root.join("deny-placement"), "").unwrap();
    let guards = tile.state("guard ");
    tile.focus(&block);
    tile.wait("focus did not restore guarded eligibility", || tile.state("guard ") > guards);
    tile.samples(4);
    tile.show();
    tile.wait("placement failure did not report a bounded reason", || tile.count(BAD_STACK) == 1);
    assert_eq!(tile.count(ACCEPTED), 5, "placement failure was not distinguished from input receipt");
    assert_eq!(tile.geometry().len(), resized.len() + 1, "the real panel never attempted staging");
    tile.wait("the unproven staged panel did not conceal", || tile.event("hidden") == 5);
    tile.samples(4);
    assert_eq!(tile.count(READY), 1, "sample and focus transitions must not loop the prompt");
    tile.samples(6);
    assert_eq!(
        (tile.count(BAD_STACK), tile.count(READY), tile.count(VISIBLE), tile.event("revealed")),
        (1, 1, 4, 4)
    );
    assert!(!tile.capture().contains(&url), "a bounded reason must not print the page URL");

    fs::remove_file(lab.root.join("deny-placement")).unwrap();
    assert_eq!(tile.event("quartz-panel-absent"), 1, "the first reveal must survive one missing Quartz record");
    let absent = lab.root.join("absent-panel");
    fs::write(&absent, "").unwrap();
    tile.show();
    tile.wait("permanently absent panel had no post-order reason", || {
        tile.count("not shown: post-order: ordered helper panel missing from Quartz") == 1
    });
    assert_eq!(tile.event("quartz-panel-absent"), 3, "only one missing snapshot may be retried");
    let leave = lab.root.join("leave-pointer");
    fs::write(&leave, "").unwrap();
    tile.samples(4);
    tile.show();
    tile.wait("a missing Quartz row masked pointer departure", || {
        tile.count("not shown: keep the pointer inside the preview tile") == 1
    });
    assert_eq!(tile.event("quartz-panel-absent"), 4);
    fs::remove_file(leave).unwrap();
    fs::remove_file(absent).unwrap();
    tile.samples(4);
    let miss = lab.root.join("miss-center");
    fs::write(&miss, "").unwrap();
    let stages = tile.geometry();
    tile.show();
    tile.wait("AppKit center miss was conflated with post-order verification", || {
        tile.count("not shown: order: AppKit found no window at the tile center") == 1
    });
    assert_eq!(tile.geometry(), stages, "a missing center hit must not order a panel");
    assert_eq!(tile.event("revealed"), 4, "a failed placement exposed the page");
    fs::remove_file(miss).unwrap();
    let chrome = lab.root.join("rex-overlay");
    fs::write(&chrome, "").unwrap();
    tile.samples(4);
    tile.show();
    tile.wait("overlapping Rex chrome was not distinguished from a second normal Rex window", || {
        tile.count("not shown: pre-order: a Rex overlay overlaps the tile") == 1
    });
    assert_eq!(tile.geometry(), stages);
    fs::remove_file(chrome).unwrap();
    tile.samples(4);
    // Inject only when the main thread is at the final stacked-panel proof;
    // no earlier tick can have consumed part of this burst.
    let hold = lab.root.join("hold-staged-proof");
    fs::write(&hold, "").unwrap();
    tile.show();
    tile.wait("did not reach the post-stage proof", || tile.event("awaiting-dismissal-burst") == 1);
    tile.type_text("\n\n\n\n\n\n\n\nhide\n");
    fs::remove_file(&hold).unwrap();
    tile.wait("queued dismissal was not consumed", || tile.count("not shown: the confirmation was canceled") == 1);
    assert_eq!(tile.event("revealed"), 4);
    tile.samples(4);
    tile.show();
    tile.wait("fresh confirmation did not recover after clearing the obstruction", || tile.event("revealed") == 5);
    let input_gate = lab.root.join("hold-visible-input");
    fs::write(&input_gate, "").unwrap();
    tile.wait("did not reach visible input gate", || tile.event("awaiting-input-dismissal") == 1);
    tile.type_text("hide\n");
    fs::remove_file(input_gate).unwrap();
    tile.wait("queued dismissal did not deny input", || tile.event("denied-input-after-dismissal") == 1);
    tile.samples(4);
    tile.show();
    tile.wait("did not recover after terminal dismissal", || tile.event("revealed") == 6);

    // The simulated macOS pointer leaves the page before Rex receives the
    // press. It stays concealed while Rex itself actually resizes the split.
    let initial_modes = tile.active_route_modes();
    let short = tile.start_drag();
    let new = tile.resize(&mut viewer, &main, 0.43);
    tile.pointer(new.divider(), true);
    tile.samples(2);
    assert_eq!(tile.presentation(), short, "a pressed divider revealed the page");
    let route = tile.count(ROUTE_READY);
    tile.pointer(new.divider(), false);
    tile.route_ready(route);
    assert_eq!(tile.active_route_modes(), 3, "the offscreen route must use actual Rex reporting modes");
    tile.motion(&mut viewer); // Even routed motion cannot expose the page while on the divider.
    tile.samples(3);
    assert_eq!(tile.presentation(), short, "releasing on the divider revealed the page");
    tile.pointer(new.center(), false);

    // A switch to a different attached session/host may be unobservable.
    // Old saved focus, a global pointer return, guard renewals and input in a
    // different attached terminal cannot count as a return to this anchor.
    let (other_session, other_block) = &attached[0];
    viewer("block.event", &format!(
        r#"{{"session_id":"{other_session}","block_id":"{other_block}","event":{{"type":"mouse","payload":{{"action":"motion","x":3,"y":3}}}}}}"#,
    ));
    let renewals = tile.state("renew");
    tile.wait("watcher did not renew while awaiting the routed return", || tile.state("renew") > renewals);
    tile.samples(2);
    assert_eq!(tile.presentation(), short, "an unobservable switch or global pointer return reopened the preview");
    let (x, y) = new.center();
    tile.pointer((x + 4.0, y), false); // Fresh modeled movement, not the older global position.
    tile.motion(&mut viewer);
    tile.restored(short, new);
    assert_eq!(tile.active_route_modes(), initial_modes, "resume leaked terminal reporting modes");

    // Let the actual 12-second consent expire while continuously held down.
    // After return, no-button motion is not fresh explicit consent.
    let long = tile.start_drag();
    tile.scope(&format!(
        "for _,r in ipairs{{0.44,0.45,0.46,0.47}} do assert(rex.session.resize_pane{{direction='horizontal',\
         before_block_id='{main}',after_block_id='{block}',ratio=r}}) end"
    ));
    let new = tile.resize(&mut viewer, &main, 0.48);
    tile.pointer(new.divider(), true);
    let renewals = tile.state("renew");
    thread::sleep(Duration::from_secs(13));
    assert!(tile.state("renew") >= renewals + 8, "the real Rex lease did not stay alive during the drag");
    assert_eq!(tile.presentation(), long, "holding past the consent deadline staged or revealed the page");
    let route = tile.count(ROUTE_READY);
    tile.pointer(new.divider(), false);
    tile.samples(3);
    assert_eq!(tile.presentation(), long, "the expired drag revealed on mouse-up");
    tile.route_ready(route);
    let on_divider = tile.count(ROUTE_READY);
    tile.confirm_local_receipt();
    tile.route_ready(on_divider);
    assert_eq!(tile.presentation(), long, "a correct receipt revealed over the divider");
    tile.pointer(new.center(), false);
    tile.motion(&mut viewer);
    tile.samples(4);
    assert_eq!(tile.presentation(), long, "mouse motion revived expired consent");
    tile.enter();
    tile.samples(2);
    assert_eq!(tile.presentation(), long, "a bare Return from a Rex client renewed expired consent");
    let grant_requested = Instant::now();
    tile.confirm_local_receipt();
    tile.restored(long, new);

    let gap = tile.start_drag();
    tile.model_revision_gap();
    let new = tile.resize(&mut viewer, &main, 0.44);
    tile.pointer(new.divider(), true);
    let route = tile.count(ROUTE_READY);
    tile.pointer(new.divider(), false);
    tile.route_ready(route);
    tile.pointer(new.center(), false);
    tile.motion(&mut viewer);
    tile.samples(2);
    assert_eq!(tile.presentation(), gap, "a revision gap retained passive consent");
    assert!(grant_requested.elapsed() < Duration::from_secs(12), "expiry masked the gap revocation");
    tile.confirm_local_receipt();
    tile.restored(gap, new);

    // No sampled button-down or hover interval. Handle a quick release on
    // either side of the *old* boundary, but don't infer consent from a ratio.
    for ratio in [0.57, 0.48] {
        let before = tile.presentation();
        let route = tile.count(ROUTE_READY);
        fs::write(lab.root.join("fast-release"), "").unwrap();
        let new = tile.resize(&mut viewer, &main, ratio);
        tile.route_ready(route);
        tile.pointer(new.center(), false);
        tile.motion(&mut viewer);
        tile.samples(2);
        assert_eq!(tile.presentation(), before);
        if ratio == 0.48 {
            let old = tile.read("test-menu-item");
            assert_eq!(old.len(), 6);
            thread::sleep(Duration::from_secs(13));
            tile.wait("the armed return route did not rotate at expiry", || {
                let current = tile.read("test-menu-item");
                current.len() == 6 && current != old
            });
            tile.type_receipt(&old);
            tile.samples(2);
            assert_eq!(tile.presentation(), before, "the pre-expiry receipt started a new grant");
        }
        tile.confirm_local_receipt();
        tile.restored(before, new);
    }

    // Real tab loss while concealed is a hard revocation, even if the same
    // tab and block are selected again and then receive terminal input.
    let pending = tile.start_drag();
    let new = tile.resize(&mut viewer, &main, 0.43);
    tile.pointer(new.divider(), true);
    let route = tile.count(ROUTE_READY);
    tile.pointer(new.divider(), false);
    tile.route_ready(route);
    let hides = tile.state("hide");
    tile.focus(&another_tab);
    tile.wait("leaving the tab did not revoke the watcher lease", || tile.state("hide") > hides);
    tile.pointer(new.center(), false);
    let guards = tile.state("guard ");
    tile.focus(&block);
    tile.wait("returning to the tab did not reacquire eligibility", || tile.state("guard ") > guards);
    tile.samples(4);
    assert_eq!(tile.active_route_modes(), initial_modes, "tab loss leaked terminal reporting modes");
    tile.enter();
    tile.samples(4);
    assert_eq!(tile.presentation(), pending, "a returned-to tab revived revoked consent");
    let canceled = tile.count("divider pause ended");
    tile.type_text("show\nignored\nignored\nhide\n");
    tile.wait("arming the post-resize route discarded a queued hide", || tile.count("divider pause ended") > canceled);
    tile.samples(3);
    assert_eq!(tile.presentation(), pending);
    tile.show();
    tile.wait("canceling the resize downgraded local confirmation to a shared show command",
        || tile.read("test-menu-item").len() == 6);
    assert_eq!(tile.presentation(), pending);
    tile.confirm_local_receipt();
    tile.wait("a new confirmation did not recover from tab loss", || tile.event("revealed") == pending.1 + 1);

    // Close the working block while the hidden tile still owns mouse
    // reporting and noncanonical input. No staged return has restored them.
    let closing = tile.start_drag();
    let new = tile.resize(&mut viewer, &main, 0.48);
    tile.pointer(new.divider(), true);
    let route = tile.count(ROUTE_READY);
    tile.pointer(new.divider(), false);
    tile.route_ready(route);
    assert_eq!(tile.active_route_modes(), 3);
    let hides = tile.state("hide");
    lab.rex(&["block", "close", "-s", &session, &main]);
    tile.wait("the real watcher did not revoke placement on close", || tile.state("hide") > hides);
    tile.wait("anchor did not end on watcher EOF", || tile.read("native-exit") == "0\n");
    assert_eq!(tile.active_route_modes(), initial_modes, "closing the pane leaked reporting modes");
    assert_eq!(tile.event("revealed"), closing.1, "a late receipt revealed after close");

    // Use another anchor in the same four-attached viewer to exercise actual
    // owner attachment loss independently of working-terminal closure.
    let client_root = lab.root.join("client-loss");
    fs::create_dir(&client_root).unwrap();
    for asset in ["bin", "preview-rex-tick"] {
        symlink(lab.root.join(asset), client_root.join(asset)).unwrap();
    }
    let created = lab.rex(&[
        "window", "new", "-s", &session, "client-loss", "-c", root, "--shell=none",
        "--json", "--", "/bin/sleep", "300",
    ]);
    let main = id(&created, "block_ids");
    let tab = id(&created, "window_id");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/rex-lab-page", listener.local_addr().unwrap());
    let page = thread::spawn(move || serve_fixture(listener, false));
    let block = launch(&client_root, &main, &tab, &url);
    let tile = Tile {
        lab: &lab, root: &client_root, session: &session, block: &block, window: &tab, pointer_serial: Cell::new(0),
    };
    viewer("session.own_block", &owner_session);
    tile.focus(&block);
    tile.wait("fresh anchor never became eligible", || tile.count(READY) == 1);
    tile.samples(4);
    tile.show();
    tile.reveal(1);
    tile.wait("fresh anchor never loaded WebKit", || tile.event("webkit-loaded") == 1);
    page.join().unwrap();
    // No previous divider interaction: a gap arriving before hover or press
    // must still fence subsequent shared `show`, not merely hide the pixels.
    let hidden = tile.event("hidden");
    tile.model_revision_gap();
    tile.wait("first gap did not conceal", || tile.event("hidden") > hidden);
    tile.resize(&mut viewer, &main, 0.46);
    tile.samples(3);
    tile.show();
    tile.wait("first-gap show bypassed the local code", || tile.read("test-menu-item").len() == 6);
    assert_eq!(tile.event("revealed"), 1);
    tile.confirm_local_receipt();
    tile.wait("local confirmation did not recover after the first gap", || tile.event("revealed") == 2);
    let detached = tile.start_drag();
    let new = tile.resize(&mut viewer, &main, 0.45);
    tile.pointer(new.divider(), true);
    let route = tile.count(ROUTE_READY);
    tile.pointer(new.divider(), false);
    tile.route_ready(route);
    // Another app joins this same source session after release. It can send
    // input to this PTY; none of it should count as a passive local return.
    let mut second = lab.command("python3")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rex_lab/preview_viewer.py"))
        .arg(lab.root.join("s.sock"))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut second_in = second.stdin.take().unwrap();
    let mut second_out = BufReader::new(second.stdout.take().unwrap());
    let mut reply = String::new();
    second_out.read_line(&mut reply).unwrap();
    assert!(reply.starts_with("client:"));
    writeln!(second_in, r#"["session.attach",{owner_session}]"#).unwrap();
    reply.clear();
    second_out.read_line(&mut reply).unwrap();
    assert!(reply.contains(r#""type":"response""#), "{reply}");
    tile.wait("watcher did not report a shared source session", || tile.state("shared-input") == 1);
    tile.pointer(new.center(), false);
    tile.motion(&mut viewer);
    tile.enter();
    tile.samples(3);
    assert_eq!(tile.presentation(), detached, "shared PTY input alone revived a concealed page");
    let hold = client_root.join("hold-staged-proof");
    fs::write(&hold, "").unwrap();
    tile.pointer(new.center(), true);
    tile.enter();
    tile.samples(2);
    assert_eq!(tile.presentation(), detached, "an unrelated local press plus shared Return revived the page");
    tile.confirm_local_receipt();
    tile.wait("the return did not reach final placement proof", || tile.event("awaiting-dismissal-burst") == 1);
    let hidden = tile.event("hidden");
    let hides = tile.state("hide");
    viewer("session.detach", &owner_session);
    tile.wait("the real watcher did not revoke the departing owner", || tile.state("hide") > hides);
    fs::remove_file(hold).unwrap();
    tile.wait("the anchor survived owner attachment loss", || tile.read("native-exit") == "0\n");
    assert!(tile.event("hidden") > hidden, "owner loss left the staged panel ordered");
    assert_eq!(tile.event("revealed"), detached.1, "a late proof referred to the departed client");
    drop(second_in);
    assert!(second.wait().unwrap().success());
    drop((input, output));
    lab.stop_server();
}
