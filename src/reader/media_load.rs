//! Separates externally blocked source I/O from cancellable, owned preview preparation.

use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};

use super::Body;
use super::content::{LIMIT, WorkingStamp};
use super::video::{self, Bookmark, Video};
use super::working_path::ReadTarget;
use crate::picture::{self, Picture};
use crate::read::Content;

pub(super) const PENDING: &str = "Preparing preview…";
pub(super) const FAILED: &str = "The preview could not be prepared.";
const UNREADABLE: &str = "This file cannot be read.";
const LARGE_PICTURE: &str = "Image preview is limited to 64 MiB.";

/// Checkout I/O can be stuck in the kernel indefinitely. The pane never joins
/// those reads; a canceled job keeps its permit until it actually returns.
const SOURCE_LIMIT: usize = 2;
static SOURCES: AtomicUsize = AtomicUsize::new(0);

struct Permit(&'static AtomicUsize);

impl Permit {
    fn take(slots: &'static AtomicUsize) -> Option<Self> {
        let mut count = slots.load(Ordering::Relaxed);
        loop {
            if count >= SOURCE_LIMIT {
                return None;
            }
            match slots.compare_exchange_weak(count, count + 1, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(current) => count = current,
            }
        }
        Some(Self(slots))
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

type Work<T> = Box<dyn FnOnce(&AtomicBool) -> T + Send>;

enum SourceStage<T> {
    Waiting(Work<T>),
    Reading(JoinHandle<T>),
}

pub(super) struct SourceJob<T: Send + 'static> {
    slots: &'static AtomicUsize,
    stop: Arc<AtomicBool>,
    stage: Option<SourceStage<T>>,
}

impl<T: Send + 'static> SourceJob<T> {
    pub(super) fn start(work: impl FnOnce(&AtomicBool) -> T + Send + 'static) -> Self {
        Self::start_in(&SOURCES, work)
    }

    fn start_in(slots: &'static AtomicUsize, work: impl FnOnce(&AtomicBool) -> T + Send + 'static) -> Self {
        let mut job =
            Self { slots, stop: Arc::new(AtomicBool::new(false)), stage: Some(SourceStage::Waiting(Box::new(work))) };
        job.admit();
        job
    }

    fn admit(&mut self) {
        if matches!(self.stage, Some(SourceStage::Waiting(_)))
            && let Some(permit) = Permit::take(self.slots)
            && let Some(SourceStage::Waiting(work)) = self.stage.take()
        {
            let stop = Arc::clone(&self.stop);
            self.stage = thread::Builder::new()
                .name("milo-file-source".into())
                .spawn(move || {
                    let _permit = permit;
                    work(&stop)
                })
                .ok()
                .map(SourceStage::Reading);
        }
    }

    /// Only joins a thread that has already exited. A failed spawn or panic
    /// is a completed job with no result, not a job that should be retried.
    pub(super) fn poll(&mut self) -> Option<Option<T>> {
        self.admit();
        match &self.stage {
            None => return Some(None),
            Some(SourceStage::Reading(worker)) if worker.is_finished() => {}
            _ => return None,
        }
        let Some(SourceStage::Reading(worker)) = self.stage.take() else { unreachable!() };
        Some(worker.join().ok())
    }
}

impl<T: Send + 'static> Drop for SourceJob<T> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Dropping the JoinHandle detaches the source read, not a decoder.
    }
}

pub(super) enum State {
    Loading(Load),
    Text { content: Content, shown: bool },
    Ready { status_reads: bool },
    Parked(Option<Bookmark>),
}

#[derive(Clone, Copy)]
pub(super) enum Format {
    Picture,
    Video,
}

impl Format {
    pub(super) fn at(path: &str, relative: &Path) -> Option<Self> {
        if picture::is_picture(path) {
            Some(Self::Picture)
        } else if relative.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("mp4")) {
            Some(Self::Video)
        } else {
            None
        }
    }
}

enum Owned {
    Picture(Vec<u8>),
    Video(video::Snapshot),
}

enum Acquired {
    Text(Content),
    Media(Owned),
    Unavailable(&'static str),
}

pub(super) enum Outcome {
    Text(Content),
    Media { body: Body, status_reads: bool },
    Stale,
}

fn acquire(
    format: Format,
    source: ReadTarget,
    stop: &AtomicBool,
    #[cfg(test)] read: Option<TestRead>,
) -> Result<Acquired, &'static str> {
    canceled(stop)?;
    let mut file = source.open()?;
    let size = source.size;
    let mut prefix = [0; 512];
    #[cfg(test)]
    let mut read = read;
    let mut used = 0;
    while used < prefix.len() {
        canceled(stop)?;
        let buf = &mut prefix[used..];
        #[cfg(not(test))]
        let result = crate::sys::read_at(&file, buf, used as u64);
        #[cfg(test)]
        let result = match read.take() {
            Some(read) => read(&file, buf, used as u64),
            None => crate::sys::read_at(&file, buf, used as u64),
        };
        match result {
            Ok(0) => break,
            Ok(n) => used += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(UNREADABLE),
        }
    }
    canceled(stop)?;
    let prefix = &prefix[..used];
    // An incomplete UTF-8 character at the 512-byte sampling boundary is not binary.
    let binary = prefix.contains(&0)
        || std::str::from_utf8(prefix)
            .err()
            .is_some_and(|error| used != 512 || used as u64 == size || error.error_len().is_some());
    let acquired = match (binary, format) {
        (false, _) if size > LIMIT => Acquired::Text(Content::TooLarge),
        (false, _) => Acquired::Text(Content::Bytes(read_bytes(&mut file, size, LIMIT, prefix, stop)?)),
        (true, Format::Picture) => {
            Acquired::Media(Owned::Picture(read_bytes(&mut file, size, picture::FILE_LIMIT, prefix, stop)?))
        }
        (true, Format::Video) => {
            // Snapshot owns only the private copy; no checked-out descriptor
            // can reach the local decoder or the player.
            Acquired::Media(Owned::Video(video::snapshot(&mut file, stop)?))
        }
    };
    if !source.matches(&file) {
        return Err(UNREADABLE);
    }
    Ok(acquired)
}

fn canceled(stop: &AtomicBool) -> Result<(), &'static str> {
    if stop.load(Ordering::Relaxed) { Err(FAILED) } else { Ok(()) }
}

fn read_bytes(
    file: &mut File,
    size: u64,
    limit: u64,
    prefix: &[u8],
    stop: &AtomicBool,
) -> Result<Vec<u8>, &'static str> {
    if size > limit {
        return Err(LARGE_PICTURE);
    }
    if prefix.len() as u64 > size {
        return Err(UNREADABLE);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size).map_err(|_| UNREADABLE)?);
    bytes.extend_from_slice(prefix);
    file.seek(SeekFrom::Start(bytes.len() as u64)).map_err(|_| UNREADABLE)?;
    while (bytes.len() as u64) < size {
        canceled(stop)?;
        let chunk = (size - bytes.len() as u64).min(64 << 10);
        if file.by_ref().take(chunk).read_to_end(&mut bytes).map_err(|_| UNREADABLE)? as u64 != chunk {
            return Err(UNREADABLE);
        }
    }
    canceled(stop)?;
    // Don't accept a file that grew after inspection, or read past the limit.
    if crate::sys::read_at(file, &mut [0], size).map_err(|_| UNREADABLE)? != 0 {
        return Err(if size == picture::FILE_LIMIT { LARGE_PICTURE } else { UNREADABLE });
    }
    Ok(bytes)
}

enum Stage {
    Source(SourceJob<Acquired>),
    Decode { stop: Arc<AtomicBool>, worker: Option<JoinHandle<Result<Picture, &'static str>>> },
}

pub(super) struct Load {
    stage: Stage,
    stamp: WorkingStamp,
    pub(super) bookmark: Option<Bookmark>,
}

impl Load {
    pub(super) fn start(format: Format, target: ReadTarget, stamp: WorkingStamp, bookmark: Option<Bookmark>) -> Self {
        #[cfg(test)]
        let (slots, read) = TEST_SOURCE.take().map_or((&SOURCES, None), |(slots, read)| (slots, Some(read)));
        #[cfg(not(test))]
        let slots = &SOURCES;
        let source = SourceJob::start_in(slots, move |stop| {
            acquire(
                format,
                target,
                stop,
                #[cfg(test)]
                read,
            )
            .unwrap_or_else(Acquired::Unavailable)
        });
        Self { stage: Stage::Source(source), stamp, bookmark }
    }

    pub(super) fn poll(&mut self, is_current: impl FnOnce(WorkingStamp) -> bool) -> Option<Outcome> {
        match &mut self.stage {
            Stage::Source(source) => {
                let acquired = source.poll()?.unwrap_or(Acquired::Unavailable(FAILED));
                if !is_current(self.stamp) {
                    return Some(Outcome::Stale);
                }
                match acquired {
                    Acquired::Text(content) => Some(Outcome::Text(content)),
                    Acquired::Unavailable(note) => Some(Outcome::Media { body: Body::Note(note), status_reads: false }),
                    Acquired::Media(Owned::Video(snapshot)) => {
                        let body = match Video::from_snapshot(snapshot) {
                            Ok(mut video) => {
                                if let Some(bookmark) = self.bookmark {
                                    video.resume_at(bookmark);
                                }
                                Body::Video(Box::new(video))
                            }
                            Err(why) => Body::Note(why),
                        };
                        Some(Outcome::Media { body, status_reads: true })
                    }
                    Acquired::Media(Owned::Picture(bytes)) => {
                        let stop = Arc::new(AtomicBool::new(false));
                        let cancel = Arc::clone(&stop);
                        let worker = thread::Builder::new()
                            .name("milo-preview".into())
                            .spawn(move || Picture::decode_cancellable(bytes, &cancel).map_err(|e| e.note));
                        match worker {
                            Ok(worker) => {
                                self.stage = Stage::Decode { stop, worker: Some(worker) };
                                None
                            }
                            Err(_) => Some(Outcome::Media { body: Body::Note(FAILED), status_reads: true }),
                        }
                    }
                }
            }
            Stage::Decode { worker, .. } => {
                if !worker.as_ref()?.is_finished() {
                    return None;
                }
                let body = worker.take()?.join().unwrap_or(Err(FAILED)).map(Body::Picture).unwrap_or_else(Body::Note);
                if is_current(self.stamp) {
                    Some(Outcome::Media { body, status_reads: true })
                } else {
                    Some(Outcome::Stale)
                }
            }
        }
    }
}

impl Drop for Load {
    fn drop(&mut self) {
        if let Stage::Decode { stop, worker } = &mut self.stage
            && let Some(worker) = worker.take()
        {
            stop.store(true, Ordering::Relaxed);
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
type TestRead = Box<dyn FnOnce(&File, &mut [u8], u64) -> io::Result<usize> + Send>;

#[cfg(test)]
thread_local! {
    static TEST_SOURCE: std::cell::Cell<Option<(&'static AtomicUsize, TestRead)>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn intercept_next_read(slots: &'static AtomicUsize, read: TestRead) {
    TEST_SOURCE.set(Some((slots, read)));
}
