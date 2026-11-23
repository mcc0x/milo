use super::{Lab, PATIENCE, id};
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

const READY: &str = "Milo Preview: ready to confirm.";
const ACCEPTED: &str = "milo preview: confirmation accepted; checking the tile";
const VISIBLE: &str = "milo preview: preview visible";
const BAD_STACK: &str = "milo preview: not shown: the Rex window stack could not be verified; uncover the tile and use one visible Rex window";

fn records(log: &str) -> impl Iterator<Item = &str> {
    log.split_inclusive('\n').map_while(|line| line.strip_suffix('\n'))
}

struct Tile<'a> {
    lab: &'a Lab,
    session: &'a str,
    block: &'a str,
}

impl Tile<'_> {
    fn capture(&self) -> String {
        self.lab.rex(&["capture", "-s", self.session, "-b", self.block, "--trim", "--unwrap"])
    }

    fn count(&self, message: &str) -> usize {
        self.capture().matches(message).count()
    }

    fn event(&self, event: &str) -> usize {
        records(&self.lab.read("native.log")).filter(|line| *line == event).count()
    }

    fn geometry(&self) -> Vec<String> {
        records(&self.lab.read("native.log"))
            .filter_map(|line| line.strip_prefix("stage ").map(str::to_owned))
            .collect()
    }

    fn state(&self, prefix: &str) -> usize {
        records(&self.lab.read("native-watch.log")).filter(|line| line.starts_with(prefix)).count()
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
                self.lab.read("native.log"),
                self.lab.read("native-watch.log"),
                self.lab.read("native-watch.errors"),
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
}

fn serve_fixture(listener: TcpListener) {
    const PAGE: &str = "<!doctype html><meta charset=utf-8><link rel=icon href=data:,>\
        <style>html,body{background:#08ba49}</style><p id=rex-lab-page>Rex preview fixture</p>";
    listener.set_nonblocking(true).unwrap();
    let until = Instant::now() + 3 * PATIENCE;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut lines = BufReader::new(&mut stream).lines();
                let request = lines.next().and_then(Result::ok).unwrap_or_default();
                while lines.next().and_then(Result::ok).is_some_and(|line| !line.is_empty()) {}
                if !request.starts_with("GET /rex-lab-page ") {
                    continue;
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PAGE}",
                    PAGE.len(),
                )
                .unwrap();
                return;
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
        .map(|name| id(&new_session(name), "session_id"));

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
    let mut viewer = |method: &str, target: &str| {
        writeln!(input, r#"["{method}",{{"session_id":"{target}"}}]"#).unwrap();
        let mut reply = String::new();
        output.read_line(&mut reply).unwrap();
        assert!(reply.contains(r#""type":"response""#), "{method}: {reply}");
    };
    viewer("session.attach", &session);
    for attached in &attached {
        viewer("session.attach", attached);
    }
    let check_attachments = format!(
        "for _,c in ipairs(assert(rex.call('client.list',{{kinds={{'app'}}}})).clients) do \
         if c.client_id=='{client}' then assert(#c.session_ids==4 and c.principal.pid=={pid}); return end end; \
         error('missing disposable viewer')"
    );
    lab.rex(&["do", "-s", &session, "-e", &check_attachments]);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/rex-lab-page", listener.local_addr().unwrap());
    let page = thread::spawn(move || serve_fixture(listener));
    let started = lab.rex(&[
        "split", "-s", &session, "-b", &main, "--split=right", "--ratio", "40", "--focus=false", "--shell=none",
        "--label", "milo:preview:confirm", "--keep-open", "--json", "--", "/bin/bash",
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rex_lab/preview_tick.sh"),
        root, concat!(env!("CARGO_MANIFEST_DIR"), "/src/rex/preview.lua"), client, &pid.to_string(), &main, &tab, &url,
    ]);
    let block = id(&started, "block_ids");
    let tile = Tile { lab: &lab, session: &session, block: &block };
    viewer("session.own_block", &session);
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
    page.join().unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while tile.event("webkit-offscreen-paint") + tile.event("webkit-snapshot-unavailable") == 0 && Instant::now() < until {
        thread::sleep(Duration::from_millis(50));
    }
    if tile.event("webkit-offscreen-paint") == 1 {
        eprintln!("real WebKit offscreen snapshot painted the fixture page; stacking modeled");
    } else {
        eprintln!("real WebKit loaded the fixture DOM; offscreen paint not confirmed; stacking modeled");
    }

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
    tile.samples(4);
    // A dismissal after more than one callback's input budget still wins
    // over reveal. Send one burst to the actual terminal line discipline.
    tile.type_text("show\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\nhide\n");
    tile.wait("queued dismissal was not consumed", || tile.count("not shown: the confirmation was canceled") == 1);
    assert_eq!(tile.event("revealed"), 4);
    tile.samples(4);
    tile.show();
    tile.wait("fresh confirmation did not recover after clearing the obstruction", || tile.event("revealed") == 5);
    // A canceled attempt may also order out a transparent staging window.
    let hidden_before_close = tile.event("hidden");
    lab.rex(&["block", "close", "-s", &session, &main]);
    tile.wait("closing the working block left the panel revealed", || tile.event("hidden") > hidden_before_close);
    tile.wait("anchor did not end on watcher EOF", || lab.read("native-exit") == "0\n");
    drop((input, output));
    lab.stop_server();
}
