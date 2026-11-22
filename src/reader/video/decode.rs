//! Owns the fixed tools, their limits, and the bounded exchange with the Reader thread.

use std::fs::File;
use std::io::{self, BufReader, Read as _, Seek as _};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::VideoFrame;

const MAX_SECONDS: u32 = 3600;
const FPS: u32 = 12;
const MAX_FRAMES: u32 = MAX_SECONDS * FPS;
const MAX_PIXELS: u32 = 9_500_000;
const MAX_DIMENSION: u32 = 8192;
const MAX_WIDTH: usize = 320;
const MAX_HEIGHT: usize = 240;
const START: &str = "Could not start the system FFmpeg tools for MP4 preview.";
const PROBE: &str = "Could not inspect the standalone MP4 with FFprobe (8.0 or later).";
const DECODE: &str = "Could not decode local MP4 video with FFmpeg (8.0 or later).";

pub(super) fn frame_time(index: u32) -> Duration {
    Duration::from_nanos(u64::from(index) * 1_000_000_000 / u64::from(FPS))
}

pub(super) struct Tools {
    ffprobe: &'static str,
    ffmpeg: &'static str,
}

impl Tools {
    pub(super) fn installed() -> Option<&'static Self> {
        const LOCATIONS: &[Tools] = &[
            #[cfg(target_os = "macos")]
            Tools { ffprobe: "/opt/homebrew/bin/ffprobe", ffmpeg: "/opt/homebrew/bin/ffmpeg" },
            #[cfg(target_os = "macos")]
            Tools { ffprobe: "/usr/local/bin/ffprobe", ffmpeg: "/usr/local/bin/ffmpeg" },
            #[cfg(target_os = "linux")]
            Tools { ffprobe: "/usr/bin/ffprobe", ffmpeg: "/usr/bin/ffmpeg" },
        ];
        LOCATIONS.iter().find(|tools| Path::new(tools.ffprobe).is_file() && Path::new(tools.ffmpeg).is_file())
    }
}

#[derive(Clone, Copy)]
pub(super) enum Task {
    Probe,
    Frames { from: Duration, left: Duration, playing: bool },
}

impl Task {
    fn prepare(self, tools: &Tools, input: &mut File) -> Result<Command, &'static str> {
        input.rewind().map_err(|_| START)?;
        let probe = matches!(self, Self::Probe);
        let mut command = Command::new(if probe { tools.ffprobe } else { tools.ffmpeg });
        command
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::from(input.try_clone().map_err(|_| START)?))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .args(["-hide_banner", "-v", "error", "-max_alloc", "67108864"]);
        if !probe {
            command.args(["-nostdin", "-filter_threads", "1"]);
        }
        // Never autodetect containers, load external tracks, or permit another input protocol.
        // fd: uses the anonymous, seekable file on descriptor 0; no filename or URL comes from the MP4.
        command.args([
            "-f", "mov", "-protocol_whitelist", "fd", "-format_whitelist", "mov", "-enable_drefs", "0",
            "-use_absolute_path", "0", "-ignore_chapters", "1", "-max_streams", "16",
        ]);
        // FFprobe initializes decoders across streams even when only queried for video metadata.
        // Limit that initialization to common MP4 codecs; the playback process only allows video.
        let codecs = if probe { "h264,hevc,mpeg4,aac,mp3,ac3,eac3,alac,opus,flac" } else { "h264,hevc,mpeg4" };
        command.args(["-codec_whitelist", codecs, "-threads", "1", "-max_pixels"]).arg(MAX_PIXELS.to_string());
        match self {
            Self::Probe => {
                command.args([
                    // MPEG-4 Part 2 dimensions can live in the first packets, not in the MP4 header.
                    "-probesize",
                    "512000",
                    "-analyzeduration",
                    "500000",
                    "-select_streams",
                    "V:0",
                    "-show_entries",
                    "stream=codec_name,width,height,duration:format=duration",
                    "-of",
                    "default",
                    "fd:",
                ]);
            }
            Self::Frames { from, left, playing } => {
                // Keep keyframe preroll, but let the FPS filter drop it against the requested
                // position, not the preceding keyframe. Round frame timestamps up so a
                // frame after the seek point cannot replace the actual picture at that point.
                command.args(["-nofind_stream_info", "-hwaccel", "none", "-noaccurate_seek", "-ss"]).arg(timestamp(from))
                    .args(["-an", "-sn", "-dn", "-i", "fd:", "-map", "0:V:0"])
                    .args(["-an", "-sn", "-dn", "-vf"])
                    .arg(format!("fps=fps={FPS}:start_time=0:round=up,scale={MAX_WIDTH}:{MAX_HEIGHT}:force_original_aspect_ratio=decrease:reset_sar=1"))
                    .args(["-fps_mode", "passthrough", "-frames:v"]).arg(if playing { playback_frames(left) } else { 1 }.to_string());
                // PPM gives us the small, square-pixel image dimensions, including autorotation.
                command.args(["-threads", "1", "-c:v", "ppm", "-pix_fmt", "rgb24", "-f", "image2pipe", "pipe:1"]);
            }
        }
        let cpu = self.cpu_seconds();
        // SAFETY: The hook only invokes async-signal-safe setrlimit calls in the child before exec.
        unsafe { command.pre_exec(move || limit_child(cpu)) };
        Ok(command)
    }

    fn cpu_seconds(self) -> libc::rlim_t {
        const STARTUP: libc::rlim_t = 5;
        match self {
            Self::Frames { left, playing: true, .. } => {
                let seconds = (left.as_secs() + u64::from(left.subsec_nanos() != 0)).min(u64::from(MAX_SECONDS));
                // CPU time excludes pipe backpressure. Allow headroom for playback slower
                // than real time; the Reader separately stops jobs that stop making progress.
                STARTUP + 2 * seconds as libc::rlim_t
            }
            _ => STARTUP,
        }
    }

    fn error(self) -> &'static str {
        match self {
            Self::Probe => PROBE,
            Self::Frames { .. } => DECODE,
        }
    }
}

fn timestamp(time: Duration) -> String {
    let micros = seek_micros(time);
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

fn seek_micros(time: Duration) -> u128 {
    // Do not seek just before a displayed frame timestamp between two microseconds.
    time.as_nanos().div_ceil(1_000)
}

fn playback_frames(left: Duration) -> u32 {
    // Count sample times strictly before the end, including the initial frame at zero.
    let frames = (left.as_nanos() * u128::from(FPS)).div_ceil(1_000_000_000);
    u32::try_from(frames).unwrap_or(MAX_FRAMES).min(MAX_FRAMES)
}

pub(super) fn last_seek_time(duration: Duration) -> Duration {
    let index = playback_frames(duration).saturating_sub(1);
    let last = frame_time(index);
    // FFprobe rounds the duration to microseconds. Don't round a grid point up
    // to a seek exactly at the advertised EOF.
    if seek_micros(last) * 1_000 >= duration.as_nanos() { frame_time(index.saturating_sub(1)) } else { last }
}

fn playback_ended_early(left: Duration, count: u32) -> bool {
    // FFmpeg's FPS filter repeats low-rate pictures up to the decoded EOF timestamp.
    // Count the interval covered by the final output sample and allow two more samples
    // for differences in probe, seek and resampling precision.
    left > frame_time(count + 2)
}

fn limit_child(cpu: libc::rlim_t) -> io::Result<()> {
    let cpu = libc::rlimit { rlim_cur: cpu, rlim_max: cpu };
    // SAFETY: setrlimit reads this initialized limit and applies it to the calling child only.
    if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &cpu) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: setrlimit reads this initialized limit and applies it to the calling child only.
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &core) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // Darwin rejects exec with a finite address-space limit. FFmpeg still enforces its
    // own allocation, dimensions, stream, and thread limits there.
    #[cfg(target_os = "linux")]
    {
        let address_space = libc::rlimit { rlim_cur: 1024 * 1024 * 1024, rlim_max: 1024 * 1024 * 1024 };
        // SAFETY: setrlimit reads this initialized limit and applies it to the calling child only.
        if unsafe { libc::setrlimit(libc::RLIMIT_AS, &address_space) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub(super) enum Output {
    Duration(Duration),
    Still,
    Playback { early: bool },
}

#[derive(Default)]
struct Inbox {
    latest: Option<(u32, VideoFrame)>,
    spare: Option<VideoFrame>,
}

#[derive(Default)]
struct Exchange {
    stop: AtomicBool,
    inbox: Mutex<Inbox>,
}

/// The owner drops this before rewinding the snapshot or hiding the player.
pub(super) struct Job {
    child: Child,
    worker: Option<JoinHandle<Result<Output, &'static str>>>,
    exchange: Arc<Exchange>,
    task: Task,
}

impl Job {
    pub(super) fn start(tools: &Tools, input: &mut File, task: Task) -> Result<Self, &'static str> {
        let mut child = task.prepare(tools, input)?.spawn().map_err(|_| START)?;
        let stdout = child.stdout.take().expect("decoder stdout is piped");
        let mut job = Self { child, worker: None, exchange: Arc::new(Exchange::default()), task };
        let exchange = Arc::clone(&job.exchange);
        job.worker = Some(
            thread::Builder::new()
                .name("milo-video".into())
                .spawn(move || match task {
                    Task::Probe => probe(stdout).map(Output::Duration),
                    Task::Frames { left, playing, .. } => {
                        let count = frames(stdout, &exchange, playing)?;
                        if playing {
                            Ok(Output::Playback { early: playback_ended_early(left, count) })
                        } else {
                            Ok(Output::Still)
                        }
                    }
                })
                .map_err(|_| START)?,
        );
        Ok(job)
    }

    pub(super) fn take_frame(&self, visible: &mut Option<VideoFrame>) -> Option<u32> {
        let mut inbox = self.exchange.inbox.lock().expect("video exchange is not poisoned");
        let (index, frame) = inbox.latest.take()?;
        inbox.spare = visible.replace(frame);
        Some(index)
    }

    pub(super) fn finish(&mut self) -> Result<Option<Output>, &'static str> {
        if crate::fs_ops::over_memory_budget(&self.child) {
            return Err("The local MP4 decoder exceeded its memory budget.");
        }
        let worker = self.worker.as_ref().expect("active video job owns its worker");
        if !worker.is_finished() {
            return Ok(None);
        }
        let Some(status) = self.child.try_wait().map_err(|_| self.task.error())? else {
            return Ok(None);
        };
        let result =
            self.worker.take().expect("active video job owns its worker").join().map_err(|_| self.task.error())?;
        if status.success() { result.map(Some) } else { Err(self.task.error()) }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.exchange.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn probe(stdout: ChildStdout) -> Result<Duration, &'static str> {
    const MAX_OUTPUT: usize = 4096;
    let mut bytes = Vec::new();
    stdout.take((MAX_OUTPUT + 1) as u64).read_to_end(&mut bytes).map_err(|_| PROBE)?;
    if bytes.len() > MAX_OUTPUT {
        return Err(PROBE);
    }
    parse_probe(std::str::from_utf8(&bytes).map_err(|_| PROBE)?)
}

fn parse_probe(text: &str) -> Result<Duration, &'static str> {
    let (mut codec, mut width, mut height, mut stream_duration, mut format_duration) = (None, None, None, None, None);
    let mut section = "";
    for line in text.lines() {
        if line.starts_with('[') {
            section = line;
        } else if let Some((key, value)) = line.split_once('=') {
            match (section, key) {
                ("[STREAM]", "codec_name") => codec = Some(value),
                ("[STREAM]", "width") => width = value.parse::<u32>().ok(),
                ("[STREAM]", "height") => height = value.parse::<u32>().ok(),
                ("[STREAM]", "duration") => stream_duration = Some(value),
                ("[FORMAT]", "duration") => format_duration = Some(value),
                _ => {}
            }
        }
    }
    if !matches!(codec, Some("h264" | "hevc" | "mpeg4")) {
        return Err("MP4 preview needs a local H.264, HEVC, or MPEG-4 Part 2 video track.");
    }
    // Check both sides of the dimensions advertised in the header. If the coded
    // dimensions change later, FFmpeg still enforces the pixel-count limit.
    if !width.zip(height).is_some_and(|(w, h)| {
        (1..=MAX_DIMENSION).contains(&w) && (1..=MAX_DIMENSION).contains(&h) && w * h <= MAX_PIXELS
    }) {
        return Err("MP4 video dimensions are missing or exceed the preview limit.");
    }
    stream_duration
        .filter(|&value| value != "N/A")
        .or(format_duration)
        .and_then(duration)
        .ok_or("MP4 preview needs a known duration of at most one hour.")
}

fn duration(value: &str) -> Option<Duration> {
    let value = value.parse::<f64>().ok()?;
    if value <= 0.0 || value > f64::from(MAX_SECONDS) {
        return None;
    }
    Duration::try_from_secs_f64(value).ok().filter(|time| !time.is_zero())
}

fn frames(stdout: ChildStdout, exchange: &Exchange, playing: bool) -> Result<u32, &'static str> {
    let mut reader = BufReader::new(stdout);
    let mut frame = VideoFrame::default();
    let mut start = None;
    let limit = if playing { MAX_FRAMES } else { 1 };
    for index in 0..limit {
        if exchange.stop.load(Ordering::Relaxed) {
            return Ok(index);
        }
        if !read_frame(&mut reader, &mut frame).map_err(|_| DECODE)? {
            return if index == 0 { Err("MP4 has no decodable local video frames.") } else { Ok(index) };
        }
        let deadline = *start.get_or_insert_with(Instant::now) + frame_time(index);
        while !exchange.stop.load(Ordering::Relaxed) {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else { break };
            thread::park_timeout(remaining);
        }
        if exchange.stop.load(Ordering::Relaxed) {
            return Ok(index);
        }
        let mut inbox = exchange.inbox.lock().expect("video exchange is not poisoned");
        let superseded = inbox.latest.replace((index, frame));
        frame = superseded.map(|(_, frame)| frame).or_else(|| inbox.spare.take()).unwrap_or_default();
    }
    Ok(limit)
}

fn read_frame(reader: &mut impl io::Read, frame: &mut VideoFrame) -> io::Result<bool> {
    let invalid = || io::Error::from(io::ErrorKind::InvalidData);
    let mut magic = [0; 3];
    match reader.read_exact(&mut magic[..1]) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
        result => result?,
    }
    reader.read_exact(&mut magic[1..])?;
    if &magic != b"P6\n" {
        return Err(invalid());
    }
    let mut dimensions = [0; 16];
    for byte in &mut dimensions {
        reader.read_exact(std::slice::from_mut(byte))?;
        if *byte == b'\n' {
            break;
        }
    }
    let end = dimensions.iter().position(|&byte| byte == b'\n').ok_or_else(invalid)?;
    let header = std::str::from_utf8(&dimensions[..end]).map_err(|_| invalid())?;
    let (w, h) = header.split_once(' ').ok_or_else(invalid)?;
    let width = w.parse::<usize>().ok().filter(|n| (1..=MAX_WIDTH).contains(n)).ok_or_else(invalid)?;
    let height = h.parse::<usize>().ok().filter(|n| (1..=MAX_HEIGHT).contains(n)).ok_or_else(invalid)?;
    let mut depth = [0; 4];
    reader.read_exact(&mut depth)?;
    if &depth != b"255\n" {
        return Err(invalid());
    }
    frame.rgb.resize(width * height * 3, 0);
    reader.read_exact(&mut frame.rgb)?;
    frame.width = width;
    frame.height = height;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_limits_distinguish_stills_and_probes_from_long_playback() {
        let playback = |left, playing| Task::Frames { from: Duration::ZERO, left, playing }.cpu_seconds();
        let hour = Duration::from_secs(3600);
        assert_eq!(Task::Probe.cpu_seconds(), 5);
        assert_eq!(playback(hour, false), 5);
        assert_eq!(playback(Duration::from_millis(1500), true), 9);
        assert_eq!(playback(hour, true), 7205);
    }

    #[test]
    fn seek_timestamp_and_frame_count_handle_subframe_boundaries() {
        assert_eq!(timestamp(frame_time(1)), "0.083334");
        assert_eq!(timestamp(Duration::from_nanos(999_999_999)), "1.000000");
        assert_eq!(playback_frames(Duration::from_nanos(1)), 1);
        assert_eq!(playback_frames(frame_time(1)), 1);
        assert_eq!(playback_frames(frame_time(1) + Duration::from_nanos(1)), 2);
        assert_eq!(playback_frames(Duration::from_secs(3600)), MAX_FRAMES);
    }

    #[test]
    fn last_seek_uses_the_final_sample_on_the_playback_grid() {
        assert_eq!(last_seek_time(Duration::from_millis(13_020)), Duration::from_secs(13));
        assert_eq!(last_seek_time(Duration::from_secs(13)), frame_time(155));
        assert_eq!(last_seek_time(Duration::from_micros(166_667)), frame_time(1));
        assert_eq!(last_seek_time(Duration::from_millis(20)), Duration::ZERO);
    }

    #[test]
    fn playback_eof_distinguishes_partial_streams_from_near_end_rounding() {
        let eight_seconds = Duration::from_secs(8);
        assert!(playback_ended_early(eight_seconds, 49));
        assert!(!playback_ended_early(eight_seconds, 94));
        assert!(!playback_ended_early(eight_seconds, 96));

        // A job started by a seek only expects playback from the new position.
        assert!(!playback_ended_early(Duration::from_secs(3), 36));
        assert!(!playback_ended_early(frame_time(1), 1));
    }

    #[test]
    fn accepts_bounded_video_probe_even_without_a_format_duration() {
        let valid = "[STREAM]\ncodec_name=h264\nwidth=3840\nheight=2160\nduration=12.250000\n[/STREAM]\n[FORMAT]\nduration=N/A\n[/FORMAT]";
        assert_eq!(parse_probe(valid), Ok(Duration::from_millis(12_250)));
        assert!(parse_probe(&valid.replace("h264", "vp9")).is_err());
        assert!(parse_probe(&valid.replace("3840", "8192")).is_err());
        assert!(parse_probe(&valid.replace("12.250000", "3600.1")).is_err());
        assert!(parse_probe(&valid.replace("12.250000", "NaN")).is_err());
    }

    #[test]
    fn only_missing_stream_duration_falls_back_to_the_container() {
        let fields = "[STREAM]\ncodec_name=h264\nwidth=320\nheight=240\n";
        let format = "[/STREAM]\n[FORMAT]\nduration=2.500000\n[/FORMAT]";
        assert_eq!(parse_probe(&format!("{fields}{format}")), Ok(Duration::from_millis(2500)));
        assert_eq!(parse_probe(&format!("{fields}duration=N/A\n{format}")), Ok(Duration::from_millis(2500)));
        assert!(parse_probe(&format!("{fields}duration=3600.1\n{format}")).is_err());
        assert!(parse_probe(&format!("{fields}duration=0.000000\n{format}")).is_err());
    }

    #[test]
    fn reads_packed_rgb_frames_and_rejects_excess_dimensions() {
        let mut data = b"P6\n2 1\n255\n\0\x0a\xff\x01\x02\x03P6\n1 1\n255\n\xff\xff\xff".as_slice();
        let mut frame = VideoFrame::default();
        assert!(read_frame(&mut data, &mut frame).unwrap());
        assert_eq!((frame.width, frame.height, frame.rgb.as_slice()), (2, 1, [0, 10, 255, 1, 2, 3].as_slice()));
        assert!(read_frame(&mut data, &mut frame).unwrap());
        assert_eq!((frame.width, frame.height, frame.rgb.as_slice()), (1, 1, [255, 255, 255].as_slice()));
        assert!(!read_frame(&mut data, &mut frame).unwrap());
        assert!(read_frame(&mut b"P6\n321 1\n255\n".as_slice(), &mut frame).is_err());
        assert!(read_frame(&mut b"P6\n1 1\n255\n\xff\xff".as_slice(), &mut frame).is_err());
    }
}
