use super::{Lab, PATIENCE, id};
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const COORDINATOR: &str = "preview-rex-tick";
const DIVIDER: &str = "preview-divider-drag";

// Both helpers are test-owned. In particular, never send keys, drag, or use
// Remote Control in an installed Rex.app or in the user's session.
struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn compile(lab: &Lab, binary: &str, sources: &[&str], frameworks: &[&str]) {
    let result = lab
        .command("/usr/bin/xcrun")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["clang", "-fobjc-arc", "-mmacosx-version-min=13.0"])
        .args(sources)
        .args(frameworks.iter().flat_map(|framework| ["-framework", *framework]))
        .arg("-o")
        .arg(lab.root.join(binary))
        .output()
        .unwrap();
    assert!(result.status.success(), "{binary}: {}", String::from_utf8_lossy(&result.stderr));
}

fn records(log: &str) -> impl Iterator<Item = &str> {
    log.split_inclusive('\n').map_while(|line| line.strip_suffix('\n'))
}

fn watcher_records(log: &str) -> impl Iterator<Item = &str> {
    records(log).map(|line| {
        if matches!(line, "hide" | "unclaimed") {
            return line;
        }
        let (state, stamp) = line.rsplit_once(" @").unwrap_or_else(|| panic!("missing watcher timestamp: {line:?}"));
        assert!(state == "renew" || state.starts_with("show "), "unknown watcher record: {line:?}");
        assert!(stamp.parse::<std::num::NonZeroU64>().is_ok(), "invalid watcher timestamp: {line:?}");
        state
    })
}

fn publish(root: &Path, name: &str, value: &str) {
    let pending = root.join(format!(".{name}"));
    fs::write(&pending, value).unwrap();
    fs::rename(pending, root.join(name)).unwrap();
}

fn window_command(lab: &Lab, command: &str) {
    publish(&lab.root, "window-command", command);
    let until = Instant::now() + PATIENCE;
    while lab.read("window-applied") != command {
        assert!(
            Instant::now() < until,
            "the disposable window host did not apply {command:?}\n{}{}",
            lab.read("window-host.errors"),
            lab.read("cover-host.errors"),
        );
        thread::sleep(Duration::from_millis(50));
    }
}

struct Tile<'a> {
    lab: &'a Lab,
    session: &'a str,
    block: &'a str,
    shell: &'a str,
    tab: &'a str,
}

#[derive(Debug, PartialEq)]
struct Presentation {
    web: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug)]
struct Probe {
    web: String,
    width: u32,
    height: u32,
    cpu: f64,
    rss: u64,
}

impl Tile<'_> {
    fn capture(&self, block: &str) -> String {
        let output = self.lab.rex_output(&["capture", "-s", self.session, "-b", block, "--trim", "--unwrap"]).unwrap();
        String::from_utf8(if output.status.success() { output.stdout } else { output.stderr }).unwrap()
    }

    fn wait(&self, description: &str, mut done: impl FnMut() -> bool) {
        let until = Instant::now() + PATIENCE;
        while !done() {
            assert!(
                Instant::now() < until,
                "{description}\npreview terminal:\n{}\nshell:\n{}\nnative:\n{}\nwatcher:\n{}{}",
                self.capture(self.block),
                self.capture(self.shell),
                self.lab.read("native.log"),
                self.lab.read("native-watch.log"),
                self.lab.read("native-watch.errors"),
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn focus(&self, target: &str) {
        self.lab.rex(&["focus", "-s", self.session, target]);
    }

    fn shell_is_focused(&self) {
        let check = format!(
            "local v=assert(rex.session.view{{}}); assert(v.active_window_id=='{}'); \
             for _,w in ipairs(v.windows) do if w.window_id=='{}' then \
               assert(w.focused_block_id=='{}',tostring(w.focused_block_id)); return \
             end end; error('missing working tab')",
            self.tab, self.tab, self.shell,
        );
        self.lab.rex(&["do", "-s", self.session, "-e", &check]);
    }

    fn event_count(&self, event: &str) -> usize {
        records(&self.lab.read("native.log")).filter(|line| *line == event).count()
    }

    fn state_count(&self, event: &str) -> usize {
        watcher_records(&self.lab.read("native-watch.log")).filter(|line| *line == event).count()
    }

    fn shown(&self) -> Option<Presentation> {
        let log = self.lab.read("native.log");
        let mut fields = records(&log)
            .filter(|line| line.starts_with("shown ") || *line == "concealed")
            .last()?
            .strip_prefix("shown ")?
            .split_whitespace();
        Some(Presentation {
            web: fields.next().unwrap().into(),
            x: fields.next().unwrap().parse().unwrap(),
            y: fields.next().unwrap().parse().unwrap(),
            width: fields.next().unwrap().parse().unwrap(),
            height: fields.next().unwrap().parse().unwrap(),
        })
    }

    fn probe(&self, sequence: u32) -> Probe {
        publish(&self.lab.root, "probe", &sequence.to_string());
        let prefix = format!("probe {sequence} ");
        let mut result = None;
        self.wait("WebKit state and real panel placement were not proved", || {
            let log = self.lab.read("native.log");
            result = records(&log).find_map(|line| line.strip_prefix(&prefix)).map(str::to_owned);
            result.is_some()
        });
        let line = result.unwrap();
        let mut fields = line.split_whitespace();
        Probe {
            web: fields.next().unwrap().into(),
            width: fields.next().unwrap().parse().unwrap(),
            height: fields.next().unwrap().parse().unwrap(),
            cpu: fields.next().unwrap().parse().unwrap(),
            rss: fields.next().unwrap().parse().unwrap(),
        }
    }

    fn shell_command(&self, serial: &str) {
        self.shell_is_focused();
        // Send actual terminal input. The exact output is not present in
        // the echoed command, and both PTY tests must succeed to print it.
        let text = format!("test -t 0 && test -t 1 && printf '%s%s\\n' rex-live- {serial}");
        self.lab.rex(&["send", "-s", self.session, "-b", self.shell, &text]);
        self.lab.rex(&["send-key", "-s", self.session, "-b", self.shell, "enter"]);
        let expected = format!("rex-live-{serial}");
        self.wait("the neighboring interactive shell did not execute the input", || {
            self.capture(self.shell).lines().any(|line| line.trim() == expected)
        });
        self.shell_is_focused();
    }

    fn blocked(&self, sequence: u32) {
        publish(&self.lab.root, "blocked-probe", &sequence.to_string());
        let event = format!("blocked {sequence}");
        self.wait("the overlapping foreign window did not leave the preview hidden and noninteractive", || {
            self.event_count(&event) == 1
                && self.shown().is_none()
                && self.capture(self.block).contains("another window overlaps the tile; waiting for clear placement")
        });
    }

    fn wait_hidden(&self, previous_outs: usize) {
        self.wait("the actual AppKit panel did not order out", || {
            self.shown().is_none() && self.event_count("out") > previous_outs
        });
    }
}

fn serve_fixture(listener: TcpListener, finished: mpsc::Receiver<()>) {
    const PAGE: &str = "<!doctype html><meta charset=utf-8><link rel=icon href=data:,>\
        <p id=rex-lab-page>Rex preview fixture</p><a href=/rex-lab-form id=divider-link>Open the form</a>";
    const FORM: &str = "<!doctype html><meta charset=utf-8><link rel=icon href=data:,>\
        <form autocomplete=off><input id=divider-form></form>";
    fn reply(mut stream: TcpStream, loads: &mut [usize; 2]) {
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut lines = BufReader::new(&mut stream).lines();
        let request = lines.next().and_then(Result::ok).unwrap_or_default();
        while lines.next().and_then(Result::ok).is_some_and(|line| !line.is_empty()) {}
        let (status, body) = if request.starts_with("GET /rex-lab-page ") {
            loads[0] += 1;
            ("200 OK", PAGE)
        } else if request.starts_with("GET /rex-lab-form ") {
            loads[1] += 1;
            ("200 OK", FORM)
        } else {
            ("404 Not Found", "")
        };
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        )
        .unwrap();
    }
    listener.set_nonblocking(true).unwrap();
    let mut loads = [0, 0];
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                reply(stream, &mut loads);
                assert!(loads.iter().all(|&count| count <= 1), "WebKit unexpectedly reloaded: {loads:?}");
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                match finished.recv_timeout(Duration::from_millis(20)) {
                    Ok(()) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            Err(error) => panic!("loopback fixture: {error}"),
        }
    }
    assert_eq!(loads, [1, 1], "the two WebKit documents must load exactly once");
}

// cargo test --test rex_lab preview_tick::persistent_preview_coexists_with_shell -- --ignored --nocapture
#[test]
#[ignore = "needs Rex, Python 3, Xcode tools and a macOS GUI login; only disposable nonactivating windows"]
fn persistent_preview_coexists_with_shell() {
    let mut lab = Lab::start();
    compile(
        &lab,
        COORDINATOR,
        &["tests/preview_rex_tick.m", "macos/Preview/Browser.m"],
        &["AppKit", "CoreGraphics", "WebKit"],
    );
    compile(&lab, DIVIDER, &["tests/preview_divider_drag.m"], &["AppKit"]);

    let root = lab.root.to_str().unwrap();
    let host = lab
        .command(lab.root.join(COORDINATOR))
        .args(["--window-host", root])
        .stderr(lab.log("window-host.errors"))
        .spawn()
        .unwrap();
    let mut host = OwnedChild(host);
    let deadline = Instant::now() + PATIENCE;
    let (target, window_width) = loop {
        let ids = lab.read("window-ids");
        let mut fields = ids.split_whitespace();
        if let Some(id) = fields.next() {
            break (id.to_owned(), fields.nth(1).unwrap().parse::<f64>().unwrap());
        }
        assert!(host.0.try_wait().unwrap().is_none() && Instant::now() < deadline, "{}", lab.read("window-host.errors"));
        thread::sleep(Duration::from_millis(50));
    };
    let _cover = OwnedChild(
        lab.command(lab.root.join(COORDINATOR))
            .args(["--cover-host", root])
            .stderr(lab.log("cover-host.errors"))
            .spawn()
            .unwrap(),
    );

    let drag = lab.command(lab.root.join(DIVIDER)).output().unwrap();
    assert!(drag.status.success(), "{}", String::from_utf8_lossy(&drag.stderr));
    let ratios = std::str::from_utf8(&drag.stdout).unwrap().split_whitespace()
        .map(|ratio| ratio.parse::<f64>().unwrap()).collect::<Vec<_>>();
    let [right_start, right_end] = ratios[..] else { panic!("disposable split produced {ratios:?}") };
    assert!((0.25..0.75).contains(&right_start) && (0.25..0.75).contains(&right_end) && right_end - right_start > 0.08);

    let new_session = |name: &str| {
        lab.rex(&["new", name, "-c", root, "--shell=none", "--json", "--", "/bin/bash", "--noprofile", "--norc", "-i"])
    };
    let created = new_session("native-persistent");
    let session = id(&created, "session_id");
    let tab = id(&created, "window_id");
    let shell = id(&created, "block_ids");
    let another_tab = lab.new_tab(&session, &lab.root);
    let attached = ["native-attached-a", "native-attached-b", "native-attached-c"]
        .map(|name| id(&new_session(name), "session_id"));
    lab.rex(&["focus", "-s", &session, &shell]);

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
        reply
    };
    let scope = format!(r#"{{"session_id":"{session}"}}"#);
    viewer("session.attach", &scope);
    for attached in &attached {
        viewer("session.attach", &format!(r#"{{"session_id":"{attached}"}}"#));
    }
    let check_attachments = format!(
        "for _,c in ipairs(assert(rex.call('client.list',{{kinds={{'app'}}}})).clients) do \
         if c.client_id=='{client}' then assert(#c.session_ids==4 and c.principal.pid=={pid}); return end end; \
         error('missing disposable multi-attached viewer')"
    );
    lab.rex(&["do", "-s", &session, "-e", &check_attachments]);

    let inventory = || {
        lab.rex(&[
            "do", "-s", &session, "-e",
            "local ids={}; for _,w in ipairs(assert(rex.session.view{}).windows) do \
             table.insert(ids,w.window_id); for _,l in ipairs(w.layers) do \
             for _,b in ipairs(l.blocks) do table.insert(ids,b.block_id .. '=' .. (b.label or '')) end end end; \
             table.sort(ids); print(table.concat(ids,' '))",
        ])
    };
    let owner_pid = pid.to_string();
    let right_fraction = format!("{right_start:.6}");
    let preflight = |scenario: &str| {
        let result = lab.command(lab.root.join(COORDINATOR))
            .args(["--preflight", &owner_pid, root, scenario, &right_fraction]).output().unwrap();
        if result.status.success() {
            let pin = String::from_utf8(result.stdout).unwrap();
            assert_eq!(pin.trim(), target, "{scenario}: returned a different pin");
            Ok(())
        } else {
            let reason = String::from_utf8_lossy(&result.stderr);
            assert_eq!(result.status.code(), Some(1), "{scenario}: preflight crashed: {reason}");
            assert!(result.stdout.is_empty() && !reason.trim().is_empty(), "{scenario}: {reason}");
            Err(reason.into_owned())
        }
    };
    let before = inventory();
    // The same foreign AppKit window is above and intersecting the real
    // fixture window in each case; only its relation to the tile changes.
    for position in ["sibling", "titlebar", "tile"] {
        window_command(&lab, position);
        preflight(position).expect("an overlapping foreign window prevented pinning");
        assert_eq!(inventory(), before, "{position} changed Rex tabs or panes before the split");
    }
    for (scenario, refusal) in [
        ("background", "was not foreground"),
        ("second-rex", "additional normal Rex window"),
        ("fullscreen", "occupies the entire display"),
    ] {
        let reason = preflight(scenario).expect_err("unexpected successful pin");
        assert!(reason.contains(refusal), "{scenario}: {reason}");
        assert_eq!(inventory(), before, "{scenario} changed Rex tabs or panes before the split");
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/rex-lab-page", listener.local_addr().unwrap());
    let (finish_http, finished) = mpsc::channel();
    let page = thread::spawn(move || serve_fixture(listener, finished));
    let start = Instant::now();
    let started = lab.rex(&[
        "split", "-s", &session, "-b", &shell, "--split=right", "--ratio", &(100.0 * right_start).round().to_string(),
        "--focus=false", "--shell=none", "--label", "milo:preview", "--keep-open", "--json", "--", "/bin/bash",
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rex_lab/preview_tick.sh"), root,
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/rex/preview.lua"), client, &owner_pid, &shell, &tab, &url, &target,
    ]);
    let block = id(&started, "block_ids");
    let tile = Tile { lab: &lab, session: &session, block: &block, shell: &shell, tab: &tab };
    let owned = format!(r#"{{"session_id":"{session}","block_ids":["{shell}","{block}"]}}"#);
    let resize_pane = |viewer: &mut dyn FnMut(&str, &str) -> String, right: f64| {
        let reply = viewer("session.resize_pane", &format!(
            r#"{{"session_id":"{session}","direction":"horizontal","before_block_id":"{shell}","after_block_id":"{block}","ratio":{:.6}}}"#,
            1.0 - right,
        ));
        let revision = reply.split_once(r#""revision":"#).unwrap_or_else(|| panic!("no Rex layout revision: {reply}")).1;
        revision.split(|c: char| !c.is_ascii_digit()).next().unwrap().parse::<u64>().unwrap()
    };
    let mut last_revision = resize_pane(&mut viewer, right_start);
    viewer("session.own_block", &owned);

    let layout_is = |right: f64| {
        let latest = lab.read("native-watch.log");
        // Watcher dimensions are integer millionths of the Rex workspace.
        let mut fields = match watcher_records(&latest).filter(|line| line.starts_with("show ")).last() {
            Some(line) => line.split_whitespace(),
            None => return false,
        };
        assert_eq!(fields.next(), Some("show"));
        assert_eq!(fields.next(), Some(owner_pid.as_str()));
        assert_eq!(fields.next(), Some("tile"));
        let [x, y, width, height] = std::array::from_fn(|_| fields.next().unwrap().parse::<f64>().unwrap());
        assert!(fields.next().is_none());
        y == 0.0 && height == 1_000_000.0
            && (x - (1.0 - right) * 1_000_000.0).abs() <= 2.0
            && (width - right * 1_000_000.0).abs() <= 2.0
    };
    tile.wait("the claimed right tile did not receive its real watcher lease", || layout_is(right_start));
    tile.blocked(1);
    assert_eq!(tile.event_count("ordered"), 0, "the initially covered preview tried to order");
    window_command(&lab, "sibling");
    tile.wait("moving the cover to the sibling did not reveal the preview", || tile.shown().is_some());
    let visible_at = start.elapsed();
    tile.shell_is_focused();
    publish(&lab.root, "seed-form", "");
    tile.wait("WebKit did not navigate and seed the real form", || tile.event_count("form-seeded") == 1);
    let original = tile.shown().unwrap();
    let initial = tile.probe(1);
    eprintln!("Fixture split to visible: {visible_at:.2?}; to WK form ready: {:.2?}", start.elapsed());

    let concealed = tile.event_count("concealed");
    tile.shell_command("before-resize");
    assert_eq!(tile.event_count("concealed"), concealed, "shell input concealed the actual panel");

    // Bring the same foreign window above the visible panel, but only over
    // the titlebar. Neither the projection nor the proven order may change.
    window_command(&lab, "titlebar");
    let titlebar = tile.probe(2);
    assert_eq!((&titlebar.web, titlebar.width, titlebar.height), (&initial.web, initial.width, initial.height));
    assert_eq!(tile.shown().unwrap(), original);
    assert_eq!(tile.event_count("concealed"), concealed, "titlebar overlap hid the page");

    // Only the foreign window moves. The watcher keeps the lease and the
    // shell stays focused while the seeded page hides and comes back.
    let outs = tile.event_count("out");
    let hides = tile.state_count("hide");
    window_command(&lab, "tile");
    tile.wait_hidden(outs);
    tile.blocked(2);
    window_command(&lab, "sibling");
    let uncovered = tile.probe(3);
    assert_eq!((&uncovered.web, uncovered.width, uncovered.height), (&initial.web, initial.width, initial.height));
    assert_eq!(tile.shown().unwrap(), original);
    assert_eq!(tile.state_count("hide"), hides, "cover movement revoked the Rex watcher lease");
    tile.shell_is_focused();

    // The separate disposable NSSplitView verifies the drag starts outside
    // the old page and sends measurements while the local mouse is held.
    // Forward its real resize notifications to Rex as they arrive.
    let drag_start = Instant::now();
    let mut live_drag = OwnedChild(
        lab.command(lab.root.join(DIVIDER)).arg("--live").stdout(Stdio::piped())
            .stderr(lab.log("divider-live.errors")).spawn().unwrap()
    );
    let mut right = right_start;
    let mut updates = 0;
    let mut checked_while_held = false;
    for line in BufReader::new(live_drag.0.stdout.take().unwrap()).lines() {
        let next = line.unwrap().trim().parse::<f64>().unwrap();
        assert!(next >= right && next <= right_end + 0.000002, "unexpected live split ratio {right} → {next}");
        if next > right { updates += 1; }
        right = next;
        let revision = resize_pane(&mut viewer, right);
        assert!(revision > last_revision, "Rex did not apply a fresh divider revision");
        last_revision = revision;
        if updates == 3 && !checked_while_held {
            // The responder pauses after the rapid first three updates,
            // with its local drag still held. Require the browser to catch
            // up and let the neighboring shell execute before mouse-up.
            let gain = window_width * (right - right_start);
            tile.wait("preview did not follow the Rex divider while held", || {
                layout_is(right) && tile.shown().is_some_and(|frame| {
                    (original.x - frame.x - gain).abs() <= 3.0
                })
            });
            let held = tile.probe(4);
            assert_eq!(held.web, initial.web);
            assert!(held.width > initial.width + 20);
            let before_input = tile.event_count("concealed");
            tile.shell_command("divider-held");
            assert_eq!(tile.event_count("concealed"), before_input);
            assert!(live_drag.0.try_wait().unwrap().is_none(), "local mouse-up preceded the held-drag check");
            checked_while_held = true;
        }
    }
    assert!(live_drag.0.wait().unwrap().success(), "{}", lab.read("divider-live.errors"));
    assert!(checked_while_held && updates >= 12 && (right - right_end).abs() <= 0.000002,
        "incomplete live AppKit drag: {updates} updates; final {right}");
    assert!(drag_start.elapsed() > Duration::from_secs(12), "AppKit released the local divider early");
    // The production chrome projection applies at most ten points of
    // horizontal surround here. This tolerance includes that contribution
    // and inward pixel rounding, but cannot admit the previous drag step.
    let expected_gain = window_width * (right_end - right_start);
    tile.wait("Rex's resize burst did not move the native pane to the final tile", || {
        layout_is(right_end) && tile.shown().is_some_and(|frame| {
            (original.x - frame.x - expected_gain).abs() <= 3.0
        })
    });
    let expanded = tile.shown().unwrap();
    assert_eq!(expanded.web, original.web);
    assert!(expanded.width > original.width + 20.0);
    assert!((expanded.x + expanded.width - original.x - original.width).abs() <= 1.0);
    assert_eq!((expanded.y, expanded.height), (original.y, original.height));
    let resized = tile.probe(5);
    assert_eq!(resized.web, initial.web);
    assert!(resized.width > initial.width + 20, "the real CSS viewport did not resize: {initial:?} → {resized:?}");
    assert!(resized.height > 0);

    let concealed = tile.event_count("concealed");
    tile.shell_command("after-resize");
    let idle_time = Instant::now();
    let idle_start = tile.probe(6);
    let renewals = tile.state_count("renew");
    // No input or pointer movement during this interval. This exceeds the
    // removed consent lifetime, not any shortened test clock.
    thread::sleep(Duration::from_secs(13));
    let idle_end = tile.probe(7);
    assert!(tile.state_count("renew") > renewals, "the real observer was not alive while idle");
    assert_eq!(tile.event_count("concealed"), concealed, "typing or the idle interval concealed the panel");
    assert_eq!((idle_end.web, idle_end.width, idle_end.height), (idle_start.web, idle_start.width, idle_start.height));
    eprintln!(
        "Native fixture only, idle {:.2?}: CPU {:.3}s → {:.3}s; resident {:.1} → {:.1} MiB",
        idle_time.elapsed(), idle_start.cpu, idle_end.cpu,
        idle_start.rss as f64 / 1_048_576.0, idle_end.rss as f64 / 1_048_576.0,
    );
    tile.shell_command("after-idle");
    assert_eq!(tile.event_count("concealed"), concealed, "typing after idle hid the panel");

    // Hold a real Quartz result across tab departure so an earlier proof is
    // in flight when the real watcher revokes its lease. No replacement order
    // is allowed while the original tab is away.
    let outs = tile.event_count("out");
    let hides = tile.state_count("hide");
    publish(&lab.root, "hold-sample", "");
    tile.wait("fixture did not hold an in-flight Quartz sample", || tile.event_count("sample-held") == 1);
    tile.focus(&another_tab);
    tile.wait("the observer did not revoke on real tab departure", || tile.state_count("hide") > hides);
    tile.wait_hidden(outs);
    let orders = tile.event_count("ordered");
    fs::remove_file(lab.root.join("hold-sample")).unwrap();
    tile.wait("the stale Quartz result did not reach the coordinator", || tile.event_count("sample-rejected") == 1);
    assert_eq!(tile.event_count("ordered"), orders);
    tile.focus(&shell);
    let restored = tile.probe(8);
    assert_eq!(restored.web, resized.web);
    assert_eq!(tile.shown().unwrap(), expanded);
    tile.shell_is_focused();

    let outs = tile.event_count("out");
    publish(&lab.root, "foreground-away", "");
    tile.wait_hidden(outs);
    fs::remove_file(lab.root.join("foreground-away")).unwrap();
    assert_eq!(tile.probe(9).web, resized.web);
    tile.shell_is_focused();

    // The old ID still exists but no longer presents onscreen. A fresh
    // same-owner replacement appears while the original Rex lease renews.
    let outs = tile.event_count("out");
    window_command(&lab, "replace");
    tile.wait_hidden(outs);
    let orders = tile.event_count("ordered");
    let renewals = tile.state_count("renew");
    tile.wait("no fresh observer lease while replacement was visible", || tile.state_count("renew") > renewals);
    assert_eq!(tile.event_count("ordered"), orders, "the anchor followed an unrelated same-owner window");

    let hides = tile.state_count("hide");
    lab.rex(&["block", "close", "-s", &session, &shell]);
    tile.wait("working-shell closure did not end the real native watcher", || {
        tile.state_count("hide") > hides && lab.read("native-exit") == "0\n" && tile.event_count("stopped") == 1
    });
    assert!(tile.shown().is_none());
    assert_eq!(tile.event_count("ordered"), orders);
    lab.rex(&["block", "close", "-s", &session, &block]);
    assert!(!inventory().contains("milo:preview"), "teardown created a replacement pane");
    finish_http.send(()).unwrap();
    page.join().unwrap();

    let direct = tile.event_count("appkit-hit");
    let isolated = tile.event_count("filtered-hit");
    eprintln!(
        "Observed: disposable AppKit divider responder drag; real Rex layout/focus and shell PTYs; \
         WK identity, form, history and CSS viewport across foreign occlusion; \
         AppKit order, Space, non-key/nonactivation and Quartz stack of separate disposable processes \
         ({direct} direct AppKit hits, {isolated} isolated fixture hits). \
         Modeled: Rex.app PID/foreground and chrome, parked pointer; no global input or same-window host/session detection."
    );
    drop((input, output));
    lab.stop_server();
}
