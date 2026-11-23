use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{Lab, id, soon};

const PNG: &str = "01-river-sunrise.png";
const MP4: &str = "02-river-motion.mp4";
const MARKDOWN: &str = "03-field-notes.md";
const JPEG: &str = "04-river-dusk.jpg";
const WEBP: &str = "05-river-night.webp";
const NOTES: &str = include_str!("field-notes.md");

struct Terminal<'a> {
    lab: &'a Lab,
    session: &'a str,
    block: &'a str,
}

impl Terminal<'_> {
    fn keys(&self, keys: &[&str]) {
        self.lab.rex(&[["send-key", "-s", self.session, "-b", self.block].as_slice(), keys].concat());
    }

    fn capture(&self, format: &str) -> String {
        self.lab.rex(&["capture", "-s", self.session, "-b", self.block, "--trim", "--format", format])
    }

    fn wait(&self, format: &str, reason: &str, ready: impl Fn(&str) -> bool) -> String {
        let mut capture = String::new();
        let caught = soon(|| {
            capture = self.capture(format);
            ready(&capture)
        });
        assert!(caught, "{reason} not shown:\n{}", self.capture("text"));
        capture
    }

    fn shows(&self, text: &str) -> String {
        self.wait("text", text, |screen| screen.contains(text))
    }

    /// Rex may report its `login` wrapper rather than Milo as the terminal child.
    fn pid(&self) -> u32 {
        let json = self.lab.rex(&["block", "call", "-s", self.session, "-b", self.block, "process"]);
        let child = json.split_once("\"child\":").unwrap().1;
        let start = child.split_once("\"pid\":").unwrap().1.trim_start();
        start.split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap()
    }

    fn dismiss_exited(&self, tab: &str, pid: u32) {
        reaped(self.lab, pid);
        let remains = || self.lab.view().tab(tab).panes.iter().any(|pane| pane.block == self.block);
        if remains() {
            // Without a GUI owner, Rex retains the exited float on both baseline and release.
            // Dismiss that terminal using the Rex command, not by assuming GUI focus changed.
            let _ = self.lab.rex_output(&["block", "close", "-s", self.session, self.block]);
        }
        assert!(soon(|| !remains()), "could not dismiss exited Reader {}", self.block);
    }
}

fn picture(html: &str) -> impl Iterator<Item = &str> {
    html.lines().filter(|line| line.contains("&#9600;") || line.contains('▀'))
}

fn evidence(name: &str, html: &str) {
    if let Some(dir) = std::env::var_os("MILO_TEST_READER_EVIDENCE").map(PathBuf::from) {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{name}.html")), html).unwrap();
    }
}

fn processes(lab: &Lab) -> Vec<(u32, u32, String)> {
    let ps = lab.command("/bin/ps").args(["-axww", "-o", "pid=,ppid=,comm="]).output().unwrap();
    assert!(ps.status.success());
    String::from_utf8_lossy(&ps.stdout)
        .lines()
        .map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next().unwrap().parse().unwrap();
            let parent = fields.next().unwrap().parse().unwrap();
            (pid, parent, fields.collect::<Vec<_>>().join(" "))
        })
        .collect()
}

fn active_decoder(lab: &Lab, reader: u32) -> (u32, u32) {
    let mut child = None;
    let running = soon(|| {
        let processes = processes(lab);
        child = processes.iter().find_map(|(pid, parent, command)| {
            if !command.ends_with("/ffmpeg") {
                return None;
            }
            let mut ancestor = *parent;
            while ancestor > 1 && ancestor != reader {
                ancestor = processes.iter().find(|process| process.0 == ancestor)?.1;
            }
            (ancestor == reader).then_some((*pid, *parent))
        });
        child.is_some()
    });
    assert!(running, "the playing Rex terminal {reader} has no FFmpeg descendant");
    child.unwrap()
}

fn reaped(lab: &Lab, pid: u32) {
    assert!(soon(|| processes(lab).iter().all(|process| process.0 != pid)), "process {pid} outlived its owner");
}

pub(super) fn verify() {
    // Rex captures text cells, not Kitty layers. Exercise the cell preview
    // from startup; a late synthetic capability reply is not a terminal.
    let mut lab = Lab::start_with_graphics(Some("off"));
    lab.start_app();
    let dir = lab.root.join("rich-reader");
    fs::create_dir(&dir).unwrap();
    for (name, bytes) in [
        (PNG, include_bytes!("river-sunrise.png").as_slice()),
        (MP4, include_bytes!("river-motion.mp4").as_slice()),
        (MARKDOWN, NOTES.as_bytes()),
        (JPEG, include_bytes!("river-dusk.jpg").as_slice()),
        (WEBP, include_bytes!("river-night.webp").as_slice()),
    ] {
        fs::write(dir.join(name), bytes).unwrap();
    }

    let [session, tab] = lab.new_session("rich-reader", "files", &dir);
    let opened = lab.rex(&[
        "run",
        "-s",
        &session,
        "-w",
        &tab,
        "-c",
        dir.to_str().unwrap(),
        "--split=left",
        "--shell=none",
        "--label",
        "outline",
        "--",
        lab.milo.to_str().unwrap(),
        "outline",
    ]);
    let pane = |label: &str| lab.view().tab(&tab).panes.iter().find(|p| p.label == label).map(|p| p.block.clone());
    let find = |label: &str| {
        assert!(soon(|| pane(label).is_some()), "no {label} in {:?}: {opened}", lab.view());
        pane(label).unwrap()
    };
    let focused = || id(&lab.rex(&["session", "inspect", &session, "--json"]), "focused_block_id");
    let outline_id = find("outline");
    let outline = Terminal { lab: &lab, session: &session, block: &outline_id };
    outline.shows(WEBP);
    let tiled = lab.view().tab(&tab).panes.len();

    outline.keys(&["enter"]);
    let reader_id = find(PNG);
    let reader = Terminal { lab: &lab, session: &session, block: &reader_id };
    assert_eq!(focused(), reader_id);
    reader.shows("480×270");
    let reader_pid = reader.pid();
    let image = reader.wait("html", "a fitted 480×270 image", |html| picture(html).count() >= 8);
    evidence("after-image-non-kitty", &image);

    let began = Instant::now();
    outline.keys(&["down", "enter"]);
    reader.shows("Paused 0:00 / 0:09");
    let first = reader.wait("html", "the first MP4 frame", |html| {
        picture(html).next().is_some() && !picture(html).eq(picture(&image))
    });
    eprintln!("first 480×270 MP4 frame observed through Rex: {:.0?}", began.elapsed());
    assert_eq!(find(MP4), reader_id);
    assert_eq!(focused(), reader_id);
    assert_eq!(lab.view().tab(&tab).panes.len(), tiled + 1);
    evidence("after-video-paused", &first);

    // Observe pixel changes, not just a clock or a header saying “Playing.”
    reader.keys(&["space"]);
    reader.shows("Playing");
    let (decoder, player_pid) = active_decoder(&lab, reader_pid);
    let began = Instant::now();
    let mut frames = BTreeSet::new();
    let mut latest = String::new();
    for _ in 0..8 {
        let frame = reader.capture("html");
        let pixels: String = picture(&frame).collect();
        assert!(!pixels.is_empty(), "playing lost its frame");
        if frames.insert(pixels) {
            latest = frame;
        }
        std::thread::sleep(Duration::from_millis(70));
    }
    eprintln!("{} distinct frames in 8 Rex captures over {:.0?}", frames.len(), began.elapsed());
    assert!(frames.len() >= 3, "playback did not advance visually");
    evidence("after-video-playing", &latest);
    reader.keys(&["space"]);
    reader.shows("Paused");
    reaped(&lab, decoder);

    reader.keys(&["left"]);
    reader.wait("html", "a return to the first MP4 frame", |html| picture(html).eq(picture(&first)));
    reader.keys(&["right"]);
    reader.shows("Paused 0:05 / 0:09");
    let seek = reader.wait("html", "the MP4 frame at five seconds", |html| {
        picture(html).next().is_some() && !picture(html).eq(picture(&first))
    });
    evidence("after-video-seek", &seek);

    // A background reader tab stops the decoder and stays paused on return.
    reader.keys(&["space"]);
    let (decoder, _) = active_decoder(&lab, reader_pid);
    reader.keys(&["ctrl+p"]);
    reader.shows("480×270");
    reaped(&lab, decoder);
    reader.keys(&["ctrl+n"]);
    reader.shows("Paused 0:05 / 0:09");

    outline.keys(&["down", "enter"]);
    let rendered = reader.shows("m source");
    assert!(rendered.contains("Riverside field notes") && !rendered.contains("# Riverside"), "{rendered}");
    assert!(rendered.contains("e edit"), "{rendered}");
    assert_eq!(find(MARKDOWN), reader_id);
    assert_eq!(lab.view().tab(&tab).panes.len(), tiled + 1);
    let rendered_html = reader.capture("html");
    assert!(picture(&rendered_html).next().is_none(), "video cells carried into Markdown");
    evidence("after-markdown-rendered", &rendered_html);
    reader.keys(&["m"]);
    let source = reader.shows("m preview");
    assert!(source.contains("# Riverside field notes") && source.contains("**field guide**"), "{source}");
    evidence("after-markdown-source", &reader.capture("html"));
    reader.keys(&["m"]);
    reader.shows("m source");

    // Edit from the rendered view; switching tabs must preserve the unsaved source.
    reader.keys(&["e"]);
    reader.shows("^S save");
    lab.rex(&["send", "-s", &session, "-b", &reader_id, "Lab: "]);
    reader.shows("Lab: # Riverside field notes");
    assert_eq!(fs::read_to_string(dir.join(MARKDOWN)).unwrap(), NOTES);
    reader.keys(&["ctrl+p"]);
    reader.shows("Paused");
    reader.keys(&["ctrl+n"]);
    reader.shows("Lab: # Riverside field notes");
    reader.keys(&["ctrl+s"]);
    assert!(soon(|| fs::read_to_string(dir.join(MARKDOWN)).unwrap() == format!("Lab: {NOTES}")));

    reader.keys(&["ctrl+w"]);
    reader.shows("Paused");
    reader.keys(&["space"]);
    let (decoder, _) = active_decoder(&lab, reader_pid);
    reader.keys(&["ctrl+w"]);
    reader.shows("480×270");
    reaped(&lab, decoder);
    reader.keys(&["q"]);
    reader.dismiss_exited(&tab, reader_pid);
    reaped(&lab, player_pid);

    // The other supported image formats use the same fallback and each paints its own pixels.
    let mut previous = image;
    for label in [JPEG, WEBP] {
        outline.keys(&["down", "enter"]);
        let block = find(label);
        let still = Terminal { lab: &lab, session: &session, block: &block };
        still.shows("480×270");
        let pid = still.pid();
        previous =
            still.wait("html", label, |html| picture(html).count() >= 8 && !picture(html).eq(picture(&previous)));
        still.keys(&["esc"]);
        still.dismiss_exited(&tab, pid);
    }

    let outline_pid = outline.pid();
    lab.stop_server();
    reaped(&lab, outline_pid);
}
