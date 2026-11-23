use super::{Lab, Tile, records};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

const N: usize = 24;
const INITIAL: u64 = 400_000;
const STEP: u64 = 5_000;
const MS: u64 = 1_000_000;
const NATIVE: &str = "native-perf.jsonl";
const WATCH: &str = "observer-counts";
const WIRE: &str = "native-watch.log";
const PTY: &str = "bench-pty.log";

pub(super) struct Config {
    page: &'static str,
    source: PathBuf,
    artifacts: Option<PathBuf>,
    count: bool,
}

impl Config {
    pub(super) fn from_env() -> Self {
        let get = |name| std::env::var_os(name);
        let page = match get("MILO_REX_TICK_PAGE").as_deref().map(|s| s.to_str().unwrap()) {
            None | Some("light") => "light",
            Some("busy") => "busy",
            Some(other) => panic!("MILO_REX_TICK_PAGE must be light or busy, not {other:?}"),
        };
        let source = get("MILO_REX_TICK_SOURCE").map_or_else(|| env!("CARGO_MANIFEST_DIR").into(), PathBuf::from);
        let source = source.canonicalize().unwrap_or_else(|error| panic!("{}: {error}", source.display()));
        let artifacts = get("MILO_REX_TICK_ARTIFACTS").map(PathBuf::from);
        let count = match get("MILO_REX_TICK_COUNT").as_deref().map(|s| s.to_str().unwrap()) {
            None | Some("1") => true,
            Some("0") => false,
            Some(other) => panic!("MILO_REX_TICK_COUNT must be 0 or 1, not {other:?}"),
        };
        Self { page, source, artifacts, count }
    }

    pub(super) fn page(&self) -> &'static str {
        self.page
    }

    pub(super) fn body(&self) -> &'static str {
        if self.page == "light" {
            include_str!("preview_tick_light.html")
        } else {
            include_str!("preview_tick_busy.html")
        }
    }

    pub(super) fn setup(&self, lab: &Lab) -> Setup {
        // Make the fixture's relative imports resolve in the selected production
        // source snapshot without writing anything into that snapshot.
        let staged = lab.root.join("bench-src");
        fs::create_dir_all(staged.join("tests")).unwrap();
        fs::create_dir(staged.join("macos")).unwrap();
        symlink(self.source.join("macos/Preview"), staged.join("macos/Preview")).unwrap();
        let fixture = staged.join("tests/preview_rex_tick.m");
        fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/preview_rex_tick.m"), &fixture).unwrap();
        let browser = staged.join("macos/Preview/Browser.m");
        let source = self.source.join("src/rex/preview.lua");
        let watcher = if self.count {
            let path = lab.root.join("counted-watch.lua");
            let log = lab.root.join(WATCH);
            let prefix = include_str!("preview_tick_watch.lua").replace("@@WATCH_LOG@@", &format!("{:?}", log));
            fs::write(&path, prefix + &fs::read_to_string(source).unwrap()).unwrap();
            path
        } else {
            source
        };
        let control_path = lab.root.join("perf-control");
        let c_path = std::ffi::CString::new(control_path.as_os_str().as_bytes()).unwrap();
        // SAFETY: The C string is terminated and this path is inside our lab.
        if unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) } != 0 {
            panic!("{}: {}", control_path.display(), io::Error::last_os_error());
        }
        let control = File::options().read(true).write(true).open(control_path).unwrap();
        Setup { fixture, browser, watcher, control }
    }

    pub(super) fn measure(
        &self,
        tile: &Tile<'_>,
        mut setup: Setup,
        startup: Startup,
        initial: Layout,
        mut resize: impl FnMut(&[(u64, u64)]) -> Vec<Layout>,
    ) {
        let lab = tile.lab;
        eprintln!(
            "BENCH page={} source={} counted={} clock=darwin-clock-uptime-raw layout_n={N} step_millionths={STEP}",
            self.page,
            self.source.display(),
            self.count,
        );
        eprintln!(
            "Endpoints: viewer control send → fixture panel exposed after placement proof (or continuously exposed on resize) and CSS ResizeObserver delivery; not scanout/pixel presentation."
        );
        eprintln!(
            "PTY latency includes CLI startup/socket transport; acknowledgment is taken by the actual PTY reader, not Rex capture polling."
        );
        eprintln!(
            "CPU/RSS: fixture helper, Rex watcher and its reaped children, Rex server, simulated viewer; excludes WebContent and unrelated desktop processes."
        );
        if self.count {
            eprintln!(
                "Watcher counters: scoped Lua callback and Rex API invocations, completed successful local execs, lease lines; counted mode incurs local log writes."
            );
        }
        let ready = loop_until(tile, "initial WebKit DOM layout and proven preview frame", |events| {
            Some((
                event(events, "start")?,
                event(events, "page_ready")?,
                by_right(events, "visible", INITIAL, startup.split_ns)?,
            ))
        });
        let (helper, dom, visible) = ready;
        let watcher = lab.read("native-watch.pid").trim().parse::<u32>().unwrap();
        let processes = ProcessIds { watcher: Some(watcher), ..startup.ids };
        let a = snapshot(tile, &mut setup.control, b'A', processes);
        eprintln!(
            "STARTUP revision={} right={} split_ns={} helper_ms={:.3} appkit_quartz_ms={:.3} css_ms={:.3} both_ms={:.3}",
            initial.revision,
            INITIAL,
            startup.split_ns,
            elapsed(startup.split_ns, time(&helper)),
            elapsed(startup.split_ns, time(&visible)),
            elapsed(startup.split_ns, time(&dom)),
            elapsed(startup.split_ns, time(&dom).max(time(&visible))),
        );
        startup.usage.report("startup", a.usage);
        a.report(lab, "startup", None, startup.split_ns, self.count);

        start_pty(tile);
        let b = snapshot(tile, &mut setup.control, b'B', processes);
        wait_until(b.stamp() + 6_000 * MS);
        let c = snapshot(tile, &mut setup.control, b'C', processes);
        let settled = schedule(tile, "settled", INITIAL, initial.revision, &mut resize, false);
        let d = snapshot(tile, &mut setup.control, b'D', processes);
        let settled_events = lab.read(NATIVE); // The return drag revisits the same widths.
        let burst = schedule(
            tile,
            "burst",
            INITIAL + u64::try_from(N).unwrap() * STEP,
            settled.last_revision(),
            &mut resize,
            true,
        );
        let e = snapshot(tile, &mut setup.control, b'E', processes);
        c.report(lab, "idle", Some(&b), b.stamp(), self.count);
        d.report(lab, "settled", Some(&c), c.stamp(), self.count);
        e.report(lab, "burst", Some(&d), d.stamp(), self.count);
        e.report(lab, "drag-total", Some(&c), c.stamp(), self.count);

        let last = burst.rows.last().unwrap();
        // Read only after the passive observation window. The explicit DOM
        // check is a correctness gate, not a substitute reflow timestamp.
        wait_until(last.layout.sent_ns.unwrap() + 2_000 * MS);
        let samples = loop_until(tile, "final 60 Hz frame was never exposed", |events| {
            last.endpoints(events).visible.map(|_| events.to_owned())
        });
        setup.control.write_all(b"V").unwrap();
        loop_until(tile, "final DOM viewport did not match the exposed tile", |events| {
            by_right(events, "final_viewport", last.right, last.layout.sent_ns.unwrap())
        });
        let mut acks = BTreeMap::new();
        tile.wait("benchmark PTY did not acknowledge all sends", || {
            let journal = lab.read(PTY);
            acks = records(&journal)
                .map(|line| {
                    let (key, stamp) = line.split_once(' ').unwrap();
                    (key.to_owned(), stamp.parse::<u64>().unwrap())
                })
                .collect();
            settled.pty.iter().chain(&burst.pty).all(|sample| acks.contains_key(&sample.serial))
        });
        let last = format!("rex-bench-ack-{}", burst.pty.last().unwrap().serial);
        tile.wait("benchmark PTY did not print an acknowledgment", || tile.capture(tile.shell).contains(&last));
        tile.shell_is_focused();
        settled.report(&settled_events, &acks);
        burst.report(&samples, &acks);
        if let Some(path) = &self.artifacts {
            fs::create_dir_all(path).unwrap();
            for file in [NATIVE, WATCH, WIRE, PTY, "native-watch.errors"] {
                let source = lab.root.join(file);
                if source.exists() {
                    fs::copy(source, path.join(format!("{}-{file}", self.page))).unwrap();
                }
            }
        }
    }
}

pub(super) struct Setup {
    fixture: PathBuf,
    browser: PathBuf,
    watcher: PathBuf,
    control: File,
}

impl Setup {
    pub(super) fn sources(&self) -> [&str; 2] {
        [self.fixture.to_str().unwrap(), self.browser.to_str().unwrap()]
    }

    pub(super) fn watcher(&self) -> &str {
        self.watcher.to_str().unwrap()
    }
}

#[derive(Clone, Copy)]
pub(super) struct Layout {
    pub(super) revision: u64,
    sent_ns: Option<u64>,
    reply_ns: Option<u64>,
}

impl Layout {
    pub(super) fn parse(reply: &str) -> Self {
        Self {
            revision: field(reply, "revision").unwrap_or_else(|| panic!("no Rex layout revision: {reply}")),
            sent_ns: field(reply, "bench_sent_ns"),
            reply_ns: field(reply, "bench_reply_ns"),
        }
    }

    pub(super) fn parse_batch(reply: &str) -> Vec<Self> {
        let (_, layouts) = reply.split_once(r#""layouts":["#).unwrap_or_else(|| panic!("no layout batch: {reply}"));
        layouts.split("},{").map(Self::parse).collect()
    }
}

fn now() -> u64 {
    let mut stamp = MaybeUninit::uninit();
    // SAFETY: A successful clock_gettime initializes the supplied timespec.
    assert_eq!(unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, stamp.as_mut_ptr()) }, 0);
    // SAFETY: The call above succeeded.
    let stamp = unsafe { stamp.assume_init() };
    u64::try_from(stamp.tv_sec).unwrap() * 1_000_000_000 + u64::try_from(stamp.tv_nsec).unwrap()
}

fn wait_until(target: u64) {
    loop {
        let left = target.saturating_sub(now());
        if left == 0 {
            return;
        }
        thread::sleep(Duration::from_nanos(left));
    }
}

fn elapsed(start: u64, end: u64) -> f64 {
    (end.checked_sub(start).unwrap_or_else(|| panic!("non-monotonic endpoints: {start} → {end}"))) as f64 / MS as f64
}

// The fixture emits flat JSON objects and the layout RPC adds only integer
// timing fields. No free text or nested object parsing is needed here.
fn field<T: std::str::FromStr>(line: &str, name: &str) -> Option<T> {
    let (_, value) = line.split_once(&format!("\"{name}\":"))?;
    value.trim_start().split([',', '}', ' ']).next()?.parse().ok()
}

fn has_event(line: &str, name: &str) -> bool {
    line.split_once("\"event\":").is_some_and(|(_, value)| value.trim_start().starts_with(&format!("\"{name}\"")))
}

fn time(line: &str) -> u64 {
    field(line, "t_ns").unwrap_or_else(|| panic!("no monotonic timestamp: {line}"))
}

fn event(log: &str, name: &str) -> Option<String> {
    records(log).find(|line| has_event(line, name)).map(str::to_owned)
}

fn by_right(log: &str, name: &str, right: u64, after: u64) -> Option<String> {
    records(log)
        .find(|line| {
            has_event(line, name)
                && time(line) >= after
                && field::<u64>(line, "right").is_some_and(|width| width.abs_diff(right) <= 2)
        })
        .map(str::to_owned)
}

fn loop_until<T>(tile: &Tile<'_>, what: &str, mut predicate: impl FnMut(&str) -> Option<T>) -> T {
    let mut result = None;
    tile.wait(what, || {
        result = predicate(&tile.lab.read(NATIVE));
        result.is_some()
    });
    result.unwrap()
}

fn start_pty(tile: &Tile<'_>) {
    let script = tile.lab.root.join("bench-pty.py");
    fs::write(
        &script,
        r#"import os, sys, time
assert os.isatty(0) and os.isatty(1)
with open(sys.argv[1], 'w', buffering=1) as ack:
    print('rex-bench-pty-ready', flush=True)
    for line in sys.stdin:
        stamp = time.clock_gettime_ns(8)  # Darwin CLOCK_UPTIME_RAW.
        serial = line.strip()
        if serial:
            print(f'rex-bench-ack-{serial}', flush=True)
            print(serial, stamp, file=ack)
"#,
    )
    .unwrap();
    tile.shell_is_focused();
    tile.lab.rex(&[
        "send",
        "-s",
        tile.session,
        "-b",
        tile.shell,
        &format!("python3 -u '{}' '{}'", script.display(), tile.lab.root.join(PTY).display()),
    ]);
    tile.lab.rex(&["send-key", "-s", tile.session, "-b", tile.shell, "enter"]);
    tile.wait("real benchmark PTY listener did not start", || {
        tile.capture(tile.shell).lines().any(|line| line.trim() == "rex-bench-pty-ready")
    });
}

struct Input {
    serial: String,
    planned: u64,
    command: Command,
}

struct Ack {
    serial: String,
    planned: u64,
    sent: u64,
    returned: u64,
}

impl Input {
    fn send(mut self) -> Ack {
        wait_until(self.planned);
        let sent = now();
        let result = self.command.output().unwrap();
        let returned = now();
        assert!(result.status.success(), "bench rex send: {}", String::from_utf8_lossy(&result.stderr));
        Ack { serial: self.serial, planned: self.planned, sent, returned }
    }
}

struct Resize {
    right: u64,
    planned: u64,
    layout: Layout,
}

#[derive(Default)]
struct Endpoints {
    received: Option<u64>,
    frame: Option<u64>,
    web_frame: Option<u64>,
    visible: Option<u64>,
    continued: bool,
    css: Option<u64>,
    element: Option<u64>,
    points: Option<f64>,
    css_width: Option<u64>,
    css_layout_width: Option<f64>,
}

impl Endpoints {
    fn both(&self) -> Option<u64> {
        Some(self.visible?.max(self.css?))
    }
}

impl Resize {
    fn endpoints(&self, events: &str) -> Endpoints {
        let origin = self.layout.sent_ns.unwrap();
        let right = self.right;
        let received = by_right(events, "received", right, origin).map(|line| time(&line));
        let frame = by_right(events, "frame", right, origin);
        let web_frame = by_right(events, "web_frame", right, origin);
        let proof = by_right(events, "visible", right, origin);
        let continued = frame.as_deref().and_then(|line| field::<bool>(line, "visible_continuously")) == Some(true);
        let visible = if continued { frame.as_deref() } else { proof.as_deref() };
        let points = frame.as_deref().or(visible).and_then(|line| field::<f64>(line, "width_pt"));
        let layout = |kind: &str| web_frame.as_deref().and_then(|web| {
            let expected = field::<f64>(web, "width_pt")?;
            let expected_height = field::<f64>(web, "height_pt")?;
            records(events).filter(|line| has_event(line, kind) && time(line) >= time(web)).find_map(|line| {
                let width = field::<u64>(line, "width_css")?;
                let height = field::<u64>(line, "height_css")?;
                let layout_width = field::<f64>(line, "layout_width_css")?;
                (layout_width > 0.0
                    && (width as f64 - expected).abs() <= 1.0
                    && (height as f64 - expected_height).abs() <= 1.0)
                    .then(|| (time(line), width, layout_width))
            })
        });
        let css = layout("reflow");
        Endpoints {
            received,
            frame: frame.as_deref().map(time),
            web_frame: web_frame.as_deref().map(time),
            visible: visible.map(time),
            element: layout("element_layout").map(|record| record.0),
            continued,
            css: css.map(|(stamp, _, _)| stamp),
            points,
            css_width: css.map(|(_, width, _)| width),
            css_layout_width: css.map(|(_, _, width)| width),
        }
    }
}

#[derive(Debug, Default, PartialEq)]
struct BlankTime {
    intervals: usize,
    total: u64,
    longest: u64,
}

impl BlankTime {
    fn include(&mut self, from: u64, to: u64) {
        if to > from {
            let length = to - from;
            self.intervals += 1;
            self.total += length;
            self.longest = self.longest.max(length);
        }
    }
}

// Native content visibility, not scanout. Clip gaps crossing a boundary;
// multiple outstanding widths can share one uninterrupted blank interval.
fn blank_time(log: &str, start: u64, end: u64) -> BlankTime {
    assert!(end >= start);
    let mut hidden = Some(start); // A panel is not exposed before the first positive proof.
    let mut blank = BlankTime::default();
    for line in records(log) {
        let visible = has_event(line, "visible");
        if !visible && !has_event(line, "concealed") { continue; }
        let t = time(line);
        if t > end { break; }
        if visible {
            if let Some(begin) = hidden.take() { blank.include(begin, t); }
        } else {
            hidden.get_or_insert(start.max(t));
        }
    }
    if let Some(begin) = hidden { blank.include(begin, end); }
    blank
}

#[test]
fn blank_time_clips_both_edges_and_counts_a_sustained_gap_once() {
    let trace = "{\"event\":\"visible\",\"t_ns\":1}\n{\"event\":\"concealed\",\"t_ns\":4}\n\
                 {\"event\":\"concealed\",\"t_ns\":7}\n{\"event\":\"visible\",\"t_ns\":10}\n\
                 {\"event\":\"concealed\",\"t_ns\":12}\n{\"event\":\"visible\",\"t_ns\":20}\n";
    assert_eq!(blank_time(trace, 5, 16), BlankTime { intervals: 2, total: 9, longest: 5 });
    assert_eq!(blank_time(trace, 10, 12), BlankTime::default());
}

struct Phase {
    name: &'static str,
    rows: Vec<Resize>,
    pty: Vec<Ack>,
}

fn schedule(
    tile: &Tile<'_>,
    name: &'static str,
    mut right: u64,
    mut revision: u64,
    resize: &mut impl FnMut(&[(u64, u64)]) -> Vec<Layout>,
    burst: bool,
) -> Phase {
    // A settled step must allow the accepted build's multiple 250 ms
    // proof stages to finish. Avoid phase-locking to its sampling timer.
    let cadence = if burst { 1_000_000_000 / 60 } else { 937 * MS };
    let origin = now() + 100 * MS;
    let pulses: Vec<Input> = (0..if burst { 8 } else { 12 })
        .map(|index| {
            let serial = format!("{name}-{index:02}");
            let planned = origin + if burst { (25 + index * 45) * MS } else { 100 * MS + index * 2 * cadence };
            let mut command = tile.lab.command(tile.lab.root.join("bin/rex"));
            command.args(["send", "-s", tile.session, "-b", tile.shell, &format!("{serial}\n")]);
            Input { serial, planned, command }
        })
        .collect();
    thread::scope(|scope| {
        let inputs = scope.spawn(move || pulses.into_iter().map(Input::send).collect());
        let requests: Vec<_> = (0..N)
            .map(|index| {
                right = if burst { right - STEP } else { right + STEP };
                (origin + u64::try_from(index).unwrap() * cadence, right)
            })
            .collect();
        let layouts = resize(&requests);
        assert_eq!(layouts.len(), N, "missing Rex layout responses");
        let rows = requests
            .into_iter()
            .zip(layouts)
            .map(|((planned, right), layout)| {
                assert!(layout.revision > revision, "Rex failed to apply resize at {right} millionths");
                revision = layout.revision;
                Resize { right, planned, layout }
            })
            .collect();
        let pty = inputs.join().unwrap();
        // Identical fixed tail: include ordinary catch-up in resource deltas,
        // and show any sample arriving after the boundary in the raw timings.
        wait_until(origin + u64::try_from(N).unwrap() * cadence + 500 * MS);
        Phase { name, rows, pty }
    })
}

fn ms(value: Option<u64>, origin: u64) -> String {
    value.map_or_else(|| "-".into(), |value| format!("{:.3}", elapsed(origin, value)))
}

fn percentiles(label: &str, metric: &str, values: impl Iterator<Item = f64>) {
    let mut values: Vec<_> = values.collect();
    values.sort_unstable_by(f64::total_cmp);
    if values.is_empty() {
        eprintln!("STATS phase={label} metric={metric} n=0 p50_ms=- p95_ms=-");
        return;
    }
    let n = values.len();
    let rank = |percent: usize| values[(n * percent).div_ceil(100) - 1];
    eprintln!("STATS phase={label} metric={metric} n={n} p50_ms={:.3} p95_ms={:.3}", rank(50), rank(95));
}

impl Phase {
    fn last_revision(&self) -> u64 {
        self.rows.last().unwrap().layout.revision
    }

    fn report(&self, samples: &str, acks: &BTreeMap<String, u64>) {
        let mut ends = Vec::with_capacity(self.rows.len());
        let name = self.name;
        for row in &self.rows {
            let ep = row.endpoints(samples);
            let sent = row.layout.sent_ns.unwrap();
            eprintln!(
                "LAYOUT phase={name} revision={} right={} width_pt={:?} width_css={:?} root_layout_width_css={:?} planned_ns={} sent_ns={} reply_ms={} observer_ms={} frame_ms={} web_frame_ms={} visible_ms={} continuously_visible={} css_ms={} both_ms={}",
                row.layout.revision,
                row.right,
                ep.points,
                ep.css_width,
                ep.css_layout_width,
                row.planned,
                sent,
                ms(row.layout.reply_ns, sent),
                ms(ep.received, sent),
                ms(ep.frame, sent),
                ms(ep.web_frame, sent),
                ms(ep.visible, sent),
                ep.continued,
                ms(ep.css, sent),
                ms(ep.both(), sent)
            );
            ends.push((row, ep));
        }
        let first = self.rows[0].layout.sent_ns.unwrap();
        if let Some(last_visible) = ends.last().and_then(|(_, ep)| ep.visible) {
            let blank = blank_time(samples, first, last_visible);
            eprintln!("CONTINUITY phase={name} duration_ms={:.3} blank_ms={:.3} longest_blank_ms={:.3} gaps={}",
                elapsed(first, last_visible), blank.total as f64 / MS as f64,
                blank.longest as f64 / MS as f64, blank.intervals);
        }
        let count = |seen: fn(&Endpoints) -> bool| ends.iter().filter(|(_, endpoint)| seen(endpoint)).count();
        let n = self.rows.len();
        eprintln!(
            "FRAMES phase={name} requested={n} observer_missing={} frame_skipped={} visible_skipped={} css_skipped={} both_missing={}",
            n - count(|ep| ep.received.is_some()),
            n - count(|ep| ep.frame.is_some()),
            n - count(|ep| ep.visible.is_some()),
            n - count(|ep| ep.css.is_some()),
            n - count(|ep| ep.both().is_some())
        );
        for (label, endpoint) in [
            ("observer-pipe", (|ep: &Endpoints| ep.received) as fn(&Endpoints) -> Option<u64>),
            ("appkit-frame", |ep| ep.frame),
            ("wk-view-frame", |ep| ep.web_frame),
            ("native-visible", |ep| ep.visible),
            ("css-delivered", |ep| ep.css),
            ("responsive-element", |ep| ep.element),
            ("both", Endpoints::both),
        ] {
            percentiles(
                name,
                label,
                ends.iter()
                    .filter_map(|(row, ep)| endpoint(ep).map(|stamp| elapsed(row.layout.sent_ns.unwrap(), stamp))),
            );
        }
        percentiles(name, "wk-frame-to-css", ends.iter().filter_map(|(_, ep)| Some(elapsed(ep.web_frame?, ep.css?))));
        percentiles(
            name,
            "request-lateness",
            self.rows.iter().map(|row| elapsed(row.planned, row.layout.sent_ns.unwrap())),
        );
        for (label, endpoint) in [
            ("visible-interarrival-jitter", (|ep: &Endpoints| ep.visible) as fn(&Endpoints) -> Option<u64>),
            ("both-interarrival-jitter", Endpoints::both),
        ] {
            let paired = ends.iter().filter_map(|(row, ep)| endpoint(ep).map(|at| (row.layout.sent_ns.unwrap(), at)));
            let jitter = paired.clone().zip(paired.skip(1)).map(|((send, done), (next, next_done))| {
                (i128::from(next_done) - i128::from(done) - i128::from(next - send)).unsigned_abs() as f64 / MS as f64
            });
            percentiles(name, label, jitter);
        }
        for pulse in &self.pty {
            let ack = acks[&pulse.serial];
            eprintln!(
                "PTY phase={name} serial={} planned_ns={} send_ns={} ack_ns={} cli_ms={:.3} ack_ms={:.3}",
                pulse.serial,
                pulse.planned,
                pulse.sent,
                ack,
                elapsed(pulse.sent, pulse.returned),
                elapsed(pulse.sent, ack)
            );
        }
        percentiles(name, "pty-cli-to-ack", self.pty.iter().map(|pulse| elapsed(pulse.sent, acks[&pulse.serial])));
    }
}

#[derive(Clone, Copy)]
struct ProcessIds {
    rex: u32,
    viewer: u32,
    watcher: Option<u32>,
}

#[derive(Clone, Copy)]
struct ProcessUsage {
    cpu: u64,
    child_cpu: u64,
    rss: u64,
    wakes: u64,
}

fn process_usage(pid: u32) -> Option<ProcessUsage> {
    let pid = i32::try_from(pid).unwrap();
    let mut raw = MaybeUninit::<libc::rusage_info_v2>::uninit();
    // SAFETY: The Darwin API takes the address of the buffer through its
    // opaque pointer typedef; the buffer has the size required by flavor 2.
    let result = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, raw.as_mut_ptr().cast()) };
    if result != 0 {
        return None;
    }
    // SAFETY: libproc filled the buffer for a successful request.
    let raw = unsafe { raw.assume_init() };
    Some(ProcessUsage {
        cpu: raw.ri_user_time + raw.ri_system_time,
        child_cpu: raw.ri_child_user_time + raw.ri_child_system_time,
        rss: raw.ri_resident_size,
        wakes: raw.ri_interrupt_wkups,
    })
}

#[derive(Clone, Copy)]
struct Usage([Option<ProcessUsage>; 3]);

impl ProcessIds {
    fn sample(self) -> Usage {
        Usage([process_usage(self.rex), process_usage(self.viewer), self.watcher.and_then(process_usage)])
    }
}

impl Usage {
    fn report(self, phase: &str, next: Self) {
        #[repr(C)]
        struct Timebase { numer: u32, denom: u32 }
        unsafe extern "C" {
            fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
        }
        let mut scale = Timebase { numer: 0, denom: 0 };
        // SAFETY: This writable struct matches Darwin's mach_timebase_info_data_t.
        assert_eq!(unsafe { mach_timebase_info(&mut scale) }, 0);
        let ns = |ticks: u64| u128::from(ticks) * u128::from(scale.numer) / u128::from(scale.denom);
        for (index, name) in ["rex-server", "viewer", "watcher"].into_iter().enumerate() {
            let Some(to) = next.0[index] else {
                eprintln!("RESOURCE phase={phase} process={name} unavailable");
                continue;
            };
            let from = self.0[index];
            let delta = |f: fn(ProcessUsage) -> u64| f(to).saturating_sub(from.map_or(0, f));
            eprintln!(
                "RESOURCE phase={phase} process={name} cpu_ns={} reaped_child_cpu_ns={} interrupt_wakes={} rss_start_bytes={:?} rss_end_bytes={}",
                ns(delta(|x| x.cpu)),
                ns(delta(|x| x.child_cpu)),
                delta(|x| x.wakes),
                from.map(|x| x.rss),
                to.rss
            );
        }
    }
}

pub(super) struct Startup {
    ids: ProcessIds,
    usage: Usage,
    split_ns: u64,
}

impl Startup {
    pub(super) fn begin(lab: &Lab) -> Self {
        let ids = ProcessIds {
            rex: lab.server.as_ref().unwrap().id(),
            viewer: lab.app.as_ref().unwrap().id(),
            watcher: None,
        };
        let usage = ids.sample();
        Self { ids, usage, split_ns: now() }
    }
}

struct Snapshot {
    native: String,
    obs_bytes: usize,
    wire_bytes: usize,
    usage: Usage,
}

fn snapshot(tile: &Tile<'_>, control: &mut File, marker: u8, ids: ProcessIds) -> Snapshot {
    control.write_all(&[marker]).unwrap();
    let native = loop_until(tile, "native resource snapshot", |log| {
        records(log)
            .find(|line| has_event(line, "counters") && field::<u8>(line, "marker") == Some(marker))
            .map(str::to_owned)
    });
    let length =
        |path| -> usize { fs::metadata(tile.lab.root.join(path)).map_or(0, |data| data.len().try_into().unwrap()) };
    Snapshot { native, obs_bytes: length(WATCH), wire_bytes: length(WIRE), usage: ids.sample() }
}

impl Snapshot {
    fn stamp(&self) -> u64 {
        time(&self.native)
    }

    fn report(&self, lab: &Lab, phase: &str, from: Option<&Self>, start_ns: u64, counted: bool) {
        let val = |field_name| field::<u64>(&self.native, field_name);
        let delta = |field_name| {
            val(field_name).and_then(|end| match from {
                Some(from) => field::<u64>(&from.native, field_name).map(|start| end.checked_sub(start).unwrap()),
                None => Some(end),
            })
        };
        eprintln!(
            "RESOURCE phase={phase} process=helper window_ms={:.3} cpu_user_ns={:?} cpu_system_ns={:?} interrupt_wakes={:?} platform_idle_wakes={:?} rss_start_bytes={:?} rss_end_bytes={:?} ticks={:?} quartz_windows={:?} quartz_background={:?} quartz_pin={:?}",
            elapsed(start_ns, self.stamp()),
            delta("cpu_user_ns"),
            delta("cpu_system_ns"),
            delta("interrupt_wakeups"),
            delta("platform_idle_wakeups"),
            from.and_then(|from| field::<u64>(&from.native, "resident_bytes")),
            val("resident_bytes"),
            delta("tick_calls"),
            delta("quartz_window_samples"),
            delta("quartz_background_samples"),
            delta("quartz_pin_samples")
        );
        if let Some(from) = from {
            from.usage.report(phase, self.usage);
        }
        for (file, start, end) in [
            (WATCH, from.map_or(0, |s| s.obs_bytes), self.obs_bytes),
            (WIRE, from.map_or(0, |s| s.wire_bytes), self.wire_bytes),
        ] {
            if file == WATCH && !counted {
                continue;
            }
            let log = lab.read(file);
            let mut totals = BTreeMap::new();
            // These are append-only, newline-flushed records. Count by the
            // sampled log offsets; operations crossing a boundary are visible
            // as an attempt in one window and a completion in the next.
            let boundary = |offset| log[..offset].rfind('\n').map_or(0, |i| i + 1);
            for row in records(&log[boundary(start)..boundary(end)]) {
                let key = if file == WIRE { row.split_whitespace().next().unwrap() } else { row };
                *totals.entry(key).or_insert(0usize) += 1;
            }
            eprintln!(
                "OBSERVER phase={phase} stream={file} total={} by_kind={totals:?}",
                totals.values().sum::<usize>()
            );
        }
    }
}
