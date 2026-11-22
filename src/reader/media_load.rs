//! Prepares only the active reader's binary media, using the inspected file descriptor.

use std::fs::File;
use std::io::{self, Read as _};
use std::os::unix::fs::FileExt as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::Body;
use super::content::WorkingStamp;
use super::video::Video;
use super::working_path::{Entry, Stamp};
use crate::picture::{self, Picture};

pub(super) const PENDING: &str = "Preparing preview…";
pub(super) const FAILED: &str = "The preview could not be prepared.";
const UNREADABLE: &str = "This file cannot be read.";
const LARGE_PICTURE: &str = "Image preview is limited to 64 MiB.";

pub(super) enum State {
    Loading(Load),
    Ready,
    Parked(Option<Duration>),
}

enum Prepared {
    Picture(Picture),
    Video(Box<Video>),
}

pub(super) struct Input {
    file: File,
    picture_size: Option<u64>,
}

impl Input {
    pub(super) fn inspect(path: &str, relative: &Path, entry: &Entry<'_>) -> Option<Self> {
        let picture = picture::is_picture(path);
        let video = relative.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"));
        if !(picture || video) || !entry.is_regular() {
            return None;
        }
        let file = entry.open_read().ok()?;
        let size = entry.size();
        if Stamp::of_file(&file) != Some(entry.stamp()) || !binary_prefix(&file, size).ok()? {
            return None;
        }
        Some(Self { file, picture_size: picture.then_some(size) })
    }

    fn prepare(self, position: Option<Duration>, stop: &AtomicBool) -> Result<Prepared, &'static str> {
        if stop.load(Ordering::Relaxed) {
            return Err(FAILED);
        }
        if let Some(size) = self.picture_size {
            let bytes = read_picture(self.file, size, stop)?;
            Picture::decode_cancellable(bytes, stop).map(Prepared::Picture).map_err(|error| error.note)
        } else {
            let mut video = Video::open_cancellable(self.file, stop)?;
            if let Some(position) = position {
                video.resume_at(position);
            }
            Ok(Prepared::Video(Box::new(video)))
        }
    }
}

fn binary_prefix(file: &File, size: u64) -> io::Result<bool> {
    let mut prefix = [0; 512];
    let mut used = 0;
    while used < prefix.len() {
        match file.read_at(&mut prefix[used..], used as u64) {
            Ok(0) => break,
            Ok(n) => used += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    let bytes = &prefix[..used];
    // An incomplete UTF-8 character at the sampling boundary is not binary.
    Ok(bytes.contains(&0)
        || std::str::from_utf8(bytes)
            .err()
            .is_some_and(|error| used != prefix.len() || used as u64 == size || error.error_len().is_some()))
}

fn read_picture(mut file: File, size: u64, stop: &AtomicBool) -> Result<Vec<u8>, &'static str> {
    const CHUNK: u64 = 64 << 10;
    if size > picture::FILE_LIMIT {
        return Err(LARGE_PICTURE);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size).map_err(|_| LARGE_PICTURE)?);
    while (bytes.len() as u64) < size {
        if stop.load(Ordering::Relaxed) {
            return Err(FAILED);
        }
        let chunk = (size - bytes.len() as u64).min(CHUNK);
        if file.by_ref().take(chunk).read_to_end(&mut bytes).map_err(|_| UNREADABLE)? as u64 != chunk {
            return Err(UNREADABLE);
        }
    }
    if stop.load(Ordering::Relaxed) {
        return Err(FAILED);
    }
    // Don't decode a file that grew after inspection, or read past the limit.
    if file.read_at(&mut [0], size).map_err(|_| UNREADABLE)? != 0 {
        return Err(if size == picture::FILE_LIMIT { LARGE_PICTURE } else { UNREADABLE });
    }
    Ok(bytes)
}

pub(super) struct Load {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<Prepared, &'static str>>>,
    stamp: WorkingStamp,
    pub(super) position: Option<Duration>,
}

impl Load {
    pub(super) fn start(input: Input, stamp: WorkingStamp, position: Option<Duration>) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&stop);
        let worker = thread::Builder::new().name("milo-preview".into()).spawn(move || input.prepare(position, &cancel))?;
        Ok(Self { stop, worker: Some(worker), stamp, position })
    }

    pub(super) fn ready(&self) -> bool {
        self.worker.as_ref().is_some_and(JoinHandle::is_finished)
    }

    pub(super) fn finish(mut self) -> Option<(WorkingStamp, Body)> {
        let body = match self.worker.take()?.join().ok()? {
            Ok(Prepared::Picture(picture)) => Body::Picture(picture),
            Ok(Prepared::Video(video)) => Body::Video(video),
            Err(note) => Body::Note(note),
        };
        Some((self.stamp, body))
    }
}

impl Drop for Load {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.stop.store(true, Ordering::Relaxed);
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}
