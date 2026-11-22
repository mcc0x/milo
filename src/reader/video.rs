//! Muted, on-demand MP4 playback from an anonymous snapshot of an already-open file.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::fs_ops::Temp;

mod decode;
use decode::{Job, Output, Task, Tools, last_seek_time};

const MAX_BYTES: u64 = 128 * 1024 * 1024;
const PREPARE_LIMIT: Duration = Duration::from_secs(5);
const STALL: Duration = Duration::from_secs(5);

/// A privately owned, bounded copy of a verified standalone MP4.
pub(super) struct Snapshot(File);

#[derive(Default)]
pub(super) struct VideoFrame {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) rgb: Vec<u8>,
}

#[derive(Clone, Copy)]
pub(super) struct Bookmark {
    position: Duration,
    // The final sample can be on a different grid from a seek starting at zero.
    sample: Duration,
}

impl Bookmark {
    fn at(position: Duration) -> Self {
        Self { position, sample: position }
    }
}

pub(super) struct Video {
    job: Option<Job>,
    source: Option<File>,
    tools: &'static Tools,
    frame: Option<VideoFrame>,
    bookmark: Bookmark,
    duration: Duration,
    progress: Instant,
    pending: bool,
    playing: bool,
    error: Option<Cow<'static, str>>,
}

impl Video {
    pub(super) fn from_snapshot(snapshot: Snapshot) -> Result<Self, &'static str> {
        let tools = Tools::installed().ok_or("MP4 preview requires system FFmpeg and FFprobe (8.0 or later).")?;
        Ok(Self {
            source: Some(snapshot.0),
            tools,
            job: None,
            frame: None,
            bookmark: Bookmark::at(Duration::ZERO),
            duration: Duration::ZERO,
            progress: Instant::now(),
            pending: true,
            playing: false,
            error: None,
        })
    }

    pub(super) fn frame(&self) -> Option<&VideoFrame> {
        self.frame.as_ref()
    }
    pub(super) fn position(&self) -> Duration {
        self.bookmark.position
    }
    pub(super) fn bookmark(&self) -> Bookmark {
        self.bookmark
    }
    /// Restore a parked tab's bookmark even while the stream duration is still unknown.
    pub(super) fn resume_at(&mut self, bookmark: Bookmark) {
        self.pause();
        self.bookmark = bookmark;
        self.pending = true;
    }
    pub(super) fn duration(&self) -> Duration {
        self.duration
    }
    pub(super) fn is_playing(&self) -> bool {
        self.playing
    }
    pub(super) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub(super) fn needs_tick(&self) -> bool {
        self.error.is_none() && (self.playing || self.pending)
    }

    /// Also kills an unfinished still-frame read. The next active tick can retry it without autoplay.
    pub(super) fn pause(&mut self) {
        self.playing = false;
        self.job = None;
    }

    pub(super) fn toggle(&mut self) {
        if self.error.is_some() {
            return;
        }
        if self.playing {
            self.pause();
        } else {
            if !self.duration.is_zero() {
                self.job = None;
                self.restart_at_eof();
            }
            self.playing = true;
        }
    }

    pub(super) fn seek(&mut self, delta_seconds: i64) {
        if self.error.is_some() || self.duration.is_zero() {
            return;
        }
        let delta = Duration::from_secs(delta_seconds.unsigned_abs());
        let position = self.position();
        let target = if delta_seconds < 0 { position.saturating_sub(delta) } else { position.saturating_add(delta) };
        let target = target.min(last_seek_time(self.duration));
        if target != position {
            self.job = None;
            self.bookmark = Bookmark::at(target);
            self.pending = true;
        }
    }

    fn restart_at_eof(&mut self) {
        if self.position() == self.duration {
            self.bookmark = Bookmark::at(Duration::ZERO);
            self.pending = true;
        }
    }

    fn start_job(&mut self, now: Instant) -> bool {
        let task = if self.duration.is_zero() {
            Task::Probe
        } else {
            // A parked EOF bookmark keeps the EOF clock but seeks to its last sample.
            // The command rounds seeks up to microseconds; never round one into EOF.
            let from = self.bookmark.sample.min(self.duration.saturating_sub(Duration::from_micros(1)));
            Task::Frames { from, left: self.duration - from, playing: self.playing }
        };
        let source = self.source.as_mut().expect("a player without errors holds its snapshot");
        match Job::start(self.tools, source, task) {
            Ok(job) => {
                self.job = Some(job);
                self.progress = now;
                false
            }
            Err(error) => self.fail(error),
        }
    }

    /// Only tick the visible player. Returns whether its presentation changed.
    pub(super) fn tick(&mut self) -> bool {
        if !self.needs_tick() {
            return false;
        }
        let now = Instant::now();
        let Some(job) = self.job.as_mut() else {
            return self.start_job(now);
        };
        // Poll completion before draining frames so a finished job cannot leave a final frame unread.
        let finished = match job.finish() {
            Ok(finished) => finished,
            Err(error) => return self.fail(error),
        };
        let changed = if let Some(sample) = job.take_frame(&mut self.frame) {
            if self.playing {
                self.bookmark = Bookmark { position: sample.min(self.duration), sample };
            }
            self.progress = now;
            self.pending = false;
            true
        } else {
            false
        };
        if changed && !self.playing {
            self.job = None;
            return true;
        }
        match finished {
            Some(Output::Duration(duration)) => {
                self.job = None;
                self.duration = duration;
                self.bookmark.position = self.bookmark.position.min(duration);
                if self.playing {
                    self.restart_at_eof();
                }
                self.start_job(Instant::now());
                true
            }
            Some(Output::Still) => {
                self.pause();
                true
            }
            Some(Output::Playback { early }) => {
                if early {
                    let at = self.position().as_secs();
                    return self.fail(format!("MP4 playback ended early at {}:{:02}.", at / 60, at % 60));
                }
                self.bookmark.position = self.duration;
                self.pause();
                true
            }
            None if now.duration_since(self.progress) >= STALL => {
                self.fail("The local MP4 decoder stopped responding.")
            }
            None => changed,
        }
    }

    fn fail(&mut self, message: impl Into<Cow<'static, str>>) -> bool {
        self.pause();
        self.source = None;
        self.error = Some(message.into());
        true
    }
}

fn check_preparation(cancel: &AtomicBool, deadline: Instant) -> Result<(), &'static str> {
    if cancel.load(Ordering::Relaxed) {
        Err("MP4 preview preparation was canceled.")
    } else if Instant::now() >= deadline {
        Err("MP4 preview preparation timed out.")
    } else {
        Ok(())
    }
}

pub(super) fn snapshot(source: &mut File, cancel: &AtomicBool) -> Result<Snapshot, &'static str> {
    const READ: &str = "Could not read the MP4 file.";
    const COPY: &str = "Could not create a private MP4 preview snapshot.";
    const LARGE: &str = "MP4 preview is limited to 128 MiB.";
    const FORMAT: &str = "MP4 preview requires a standalone MP4 file.";
    const PREFIX: usize = 4096;
    let deadline = Instant::now() + PREPARE_LIMIT;

    check_preparation(cancel, deadline)?;
    let meta = source.metadata().map_err(|_| READ)?;
    if !meta.is_file() {
        return Err(FORMAT);
    }
    if meta.len() > MAX_BYTES {
        return Err(LARGE);
    }
    source.rewind().map_err(|_| READ)?;
    let mut chunk = [0; 64 * 1024];
    let mut used = 0;
    while used < PREFIX {
        check_preparation(cancel, deadline)?;
        match source.read(&mut chunk[used..PREFIX]) {
            Ok(0) => break,
            Ok(n) => used += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(READ),
        }
    }
    if !is_mp4(&chunk[..used]) {
        return Err(FORMAT);
    }
    check_preparation(cancel, deadline)?;
    let mut copy = anonymous_file().map_err(|_| COPY)?;
    copy.write_all(&chunk[..used]).map_err(|_| COPY)?;
    let mut remaining = MAX_BYTES - used as u64;
    loop {
        check_preparation(cancel, deadline)?;
        // Read one extra byte at the limit to catch growth since the metadata check.
        let len = usize::try_from(remaining + 1).unwrap_or(usize::MAX).min(chunk.len());
        match source.read(&mut chunk[..len]) {
            Ok(0) => {
                check_preparation(cancel, deadline)?;
                return Ok(Snapshot(copy));
            }
            Ok(n) if n as u64 > remaining => return Err(LARGE),
            Ok(n) => {
                check_preparation(cancel, deadline)?;
                copy.write_all(&chunk[..n]).map_err(|_| COPY)?;
                remaining -= n as u64;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(READ),
        }
    }
}

fn is_mp4(bytes: &[u8]) -> bool {
    let Some((size, rest)) = bytes.split_first_chunk::<4>() else { return false };
    let size = u32::from_be_bytes(*size) as usize;
    if size < 16 || !size.is_multiple_of(4) || size > bytes.len() || !rest.starts_with(b"ftyp") {
        return false;
    }
    fn brand(bytes: &[u8]) -> bool {
        matches!(
            bytes,
            b"isom"
                | b"iso2"
                | b"iso3"
                | b"iso4"
                | b"iso5"
                | b"iso6"
                | b"iso7"
                | b"iso8"
                | b"iso9"
                | b"mp41"
                | b"mp42"
                | b"avc1"
                | b"M4V "
                | b"dash"
                | b"MSNV"
        )
    }
    brand(&bytes[8..12]) || bytes[16..size].as_chunks::<4>().0.iter().any(|bytes| brand(bytes.as_slice()))
}

fn anonymous_file() -> io::Result<File> {
    let mut template = if cfg!(target_os = "macos") {
        b"/private/tmp/milo-video.XXXXXX\0".to_vec()
    } else {
        b"/tmp/milo-video.XXXXXX\0".to_vec()
    };
    // SAFETY: A writable NUL-terminated template with six trailing Xs; mkdtemp does not retain it.
    if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
        return Err(io::Error::last_os_error());
    }
    let directory = Path::new(OsStr::from_bytes(&template[..template.len() - 1]));
    let file = Temp::create_read_write(directory.join("input")).and_then(|(temp, file)| {
        temp.unlink()?;
        Ok(file)
    });
    fs::remove_dir(directory)?;
    file
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{Body, Reader};
    use crate::fixture::Fixture;
    use std::os::unix::fs::MetadataExt as _;
    use std::process::Command;
    use std::thread;

    #[test]
    fn copies_the_open_file_without_reopening_its_path() {
        let fixture = Fixture::folder();
        let path = fixture.root.join("clip.mp4");
        let mut original = b"\0\0\0\x10ftypisom\0\0\0\0original".to_vec();
        original.extend((0..=255).cycle().take(150_000));
        fs::write(&path, &original).unwrap();
        let mut opened = File::open(&path).unwrap();
        fs::rename(&path, fixture.root.join("moved.mp4")).unwrap();
        fs::write(&path, b"replacement").unwrap();

        let Snapshot(mut copy) = snapshot(&mut opened, &AtomicBool::new(false)).unwrap();
        assert_eq!(copy.metadata().unwrap().nlink(), 0);
        copy.rewind().unwrap();
        let mut bytes = Vec::new();
        copy.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.as_slice(), original.as_slice());
    }

    #[test]
    fn rejects_a_file_larger_than_the_snapshot_limit() {
        let fixture = Fixture::folder();
        let mut file = File::create(fixture.root.join("oversized.mp4")).unwrap();
        file.set_len(MAX_BYTES + 1).unwrap();
        assert_eq!(snapshot(&mut file, &AtomicBool::new(false)).err(), Some("MP4 preview is limited to 128 MiB."));
    }

    #[test]
    fn aborts_a_canceled_or_expired_snapshot() {
        let fixture = Fixture::folder();
        let path = fixture.root.join("clip.mp4");
        fs::write(&path, b"\0\0\0\x10ftypisom\0\0\0\0").unwrap();
        assert_eq!(
            snapshot(&mut File::open(&path).unwrap(), &AtomicBool::new(true)).err(),
            Some("MP4 preview preparation was canceled.")
        );
        assert_eq!(
            check_preparation(&AtomicBool::new(false), Instant::now()).unwrap_err(),
            "MP4 preview preparation timed out."
        );
    }

    #[test]
    fn requires_a_supported_mp4_file_type_box() {
        assert!(is_mp4(b"\0\0\0\x18ftypisom\0\0\0\0iso2mp41"));
        assert!(is_mp4(b"\0\0\0\x14ftypav01\0\0\0\0mp42"));
        assert!(!is_mp4(b"\0\0\0\x10ftypqt  \0\0\0\0"));
        assert!(!is_mp4(b"\0\0\0\x18ftypisom\0\0\0\0"));
        assert!(!is_mp4(b"\0\0\0\x10moovisom\0\0\0\0"));
    }

    #[test]
    #[ignore = "needs system FFmpeg and FFprobe (8.0 or later)"]
    fn non_grid_eof_bookmark_restores_the_last_sample_across_tabs() {
        fn poll_until(mut done: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done() {
                assert!(Instant::now() < deadline, "timed out waiting for the MP4 preview");
                thread::sleep(Duration::from_millis(5));
            }
        }

        fn loaded(reader: &mut Reader) -> &mut Video {
            poll_until(|| {
                reader.tick_media();
                matches!(&reader.body, Body::Video(_))
            });
            let Body::Video(video) = &mut reader.body else { unreachable!() };
            video
        }

        fn settle(video: &mut Video) {
            poll_until(|| {
                video.tick();
                !video.needs_tick()
            });
            assert_eq!(video.error(), None);
        }

        let fixture = Fixture::new();
        let file = fixture.root.join("clip.mp4");
        let generator = "color=c=red:size=160x120:rate=25:d=13.04,drawbox=color=blue:t=fill:enable='gte(t,12.96)'";
        assert!(
            Command::new("ffmpeg")
                .args(["-nostdin", "-loglevel", "error", "-f", "lavfi", "-i", generator, "-c:v", "libx264"])
                .arg(&file)
                .status()
                .unwrap()
                .success()
        );

        let mut reader = Reader::open(&file).unwrap();
        let video = loaded(&mut reader);
        settle(video);
        let eof = Duration::from_millis(13_040);
        assert_eq!(video.duration(), eof);
        for _ in 0..3 {
            video.seek(5);
        }
        assert_eq!(video.position(), Duration::from_secs(13));
        video.toggle();
        settle(video);
        assert_eq!(video.position(), eof);
        let blue = &video.frame().unwrap().rgb;
        assert!(blue[2] > blue[0], "playback on the clip-wide grid should end on blue");

        video.seek(-5);
        assert_eq!(video.position(), Duration::from_millis(8_040));
        video.toggle();
        settle(video);
        assert_eq!(video.position(), eof);
        let final_rgb = video.frame().unwrap().rgb.clone();
        assert!(final_rgb[0] > final_rgb[2], "playback from 8.04 s should end on red");

        let reopen = |reader: &mut Reader| {
            reader.park_media();
            reader.refresh(false);
        };
        for _ in 0..3 {
            reopen(&mut reader);
        }
        for _ in 0..3 {
            let video = loaded(&mut reader);
            assert_eq!(video.duration(), Duration::ZERO);
            assert_eq!(video.position(), eof);
            reopen(&mut reader);
        }
        let video = loaded(&mut reader);
        settle(video);
        assert_eq!(video.position(), eof);
        assert!(!video.is_playing());
        assert_eq!(video.frame().unwrap().rgb, final_rgb);

        video.toggle();
        assert!(video.is_playing());
        assert_eq!(video.position(), Duration::ZERO);
        assert_eq!(video.bookmark().sample, Duration::ZERO);
    }
}
