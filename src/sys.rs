//! The operating system facilities whose Unix and Windows forms differ in shape, behind one
//! interface. Code elsewhere waits on, wakes and identifies files through this module so that
//! the rest of Milo keeps one event loop on both.
//!
//! On Unix this is a thin layer over `poll`, descriptors and `stat`. On Windows a descriptor is
//! a handle value: pipes, console input and files are checked in turn for readiness, since
//! anonymous pipes cannot be waited on with the kernel's wait functions.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
pub(crate) use libc::{POLLHUP, POLLIN, POLLOUT, pollfd};
#[cfg(unix)]
pub(crate) use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{AsFd, AsRawFd, BorrowedFd, Job, POLLIN, POLLOUT, RawFd, pollfd};

/// Waits up to `timeout_ms` for any of `fds` to be ready, or forever when it is negative.
/// Returns how many are ready, zero on timeout, or a negative result on failure or interruption.
pub(crate) fn poll(fds: &mut [pollfd], timeout_ms: i32) -> i32 {
    #[cfg(unix)]
    {
        let Ok(len) = libc::nfds_t::try_from(fds.len()) else { return -1 };
        // SAFETY: The slice is writable and holds exactly `len` descriptors.
        unsafe { libc::poll(fds.as_mut_ptr(), len, timeout_ms) }
    }
    #[cfg(windows)]
    windows::poll(fds, timeout_ms)
}

/// Makes writes to a pipe end return at once rather than wait for room. Read such an end
/// through [`read_nonblocking`].
pub(crate) fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: F_SETFL takes an integer flag, and an invalid descriptor only fails the call.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    windows::set_nonblocking(fd)
}

/// Reads what a pipe holds now, whether or not it is set non-blocking: `WouldBlock` when it is
/// empty and open, zero once it is closed.
pub(crate) fn read_nonblocking<R: AsRawFd>(reader: &R, buf: &mut [u8]) -> io::Result<usize>
where
    for<'a> &'a R: io::Read,
{
    #[cfg(windows)]
    let buf = match windows::available(reader.as_raw_fd()) {
        windows::Available::Closed => return Ok(0),
        windows::Available::Bytes(0) => return Err(io::ErrorKind::WouldBlock.into()),
        windows::Available::Bytes(n) => {
            let n = usize::try_from(n).unwrap_or(usize::MAX).min(buf.len());
            &mut buf[..n]
        }
        windows::Available::Unknown => buf,
    };
    #[cfg(unix)]
    {
        let mut ready = [pollfd { fd: reader.as_raw_fd(), events: POLLIN, revents: 0 }];
        if poll(&mut ready, 0) == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
    }
    io::Read::read(&mut &*reader, buf)
}

/// Writes one byte to wake a waiting loop. A full pipe already holds a wakeup, so failure is fine.
/// Safe to call from a signal handler on Unix and a console control handler on Windows.
pub(crate) fn wake(fd: RawFd) {
    if fd < 0 {
        return;
    }
    #[cfg(unix)]
    // SAFETY: The byte lives for the call, and write is async-signal-safe.
    unsafe {
        libc::write(fd, [1u8].as_ptr().cast(), 1)
    };
    #[cfg(windows)]
    windows::write_byte(fd);
}

/// Bytes from Git, a file or a terminal as a native string, or nothing where Windows cannot
/// represent them exactly: names there are UTF-16, and a lossy conversion could name another file.
pub(crate) fn os_str(bytes: &[u8]) -> Option<&OsStr> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Some(OsStr::from_bytes(bytes))
    }
    #[cfg(windows)]
    std::str::from_utf8(bytes).ok().map(OsStr::new)
}

/// The owned form of [`os_str`].
pub(crate) fn os_string(bytes: Vec<u8>) -> Option<OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        Some(OsString::from_vec(bytes))
    }
    #[cfg(windows)]
    String::from_utf8(bytes).ok().map(OsString::from)
}

/// The real path of a file, written the way Git and the user write it. Windows adds a \\?\ prefix
/// when it canonicalizes, so a path Git reports would never match; the prefix stays only where
/// the plain form could name something else.
pub(crate) fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    let real = path.canonicalize()?;
    #[cfg(windows)]
    return Ok(windows::plain_path(real));
    #[cfg(unix)]
    Ok(real)
}

/// Whether a process with this ID is running. A process this user may not signal still counts.
/// An ID can be reused once its process has ended, so this is a hint, not proof.
pub(crate) fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
        // SAFETY: Signal 0 only asks whether the process exists; nothing is sent.
        unsafe { libc::kill(pid, 0) == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
    }
    #[cfg(windows)]
    windows::process_alive(pid)
}

/// Reads at `offset` without depending on, or (on Unix) moving, the file position.
pub(crate) fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    return std::os::unix::fs::FileExt::read_at(file, buf, offset);
    #[cfg(windows)]
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

/// Writes all of `bytes` at `offset`, like [`read_at`].
pub(crate) fn write_all_at(file: &File, bytes: &[u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::FileExt::write_all_at(file, bytes, offset);
    #[cfg(windows)]
    {
        let (mut bytes, mut offset) = (bytes, offset);
        while !bytes.is_empty() {
            match std::os::windows::fs::FileExt::seek_write(file, bytes, offset) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    bytes = &bytes[n..];
                    offset += n as u64;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// Which file an open handle refers to, stable while the file exists, across renames and
/// whatever path reached it. Two equal identities name the same file.
pub(crate) fn identity(file: &File) -> io::Result<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = file.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    windows::identity(file.as_raw_fd())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn a_pipe_is_ready_once_written_and_after_its_writer_closes() {
        let (reader, mut writer) = io::pipe().unwrap();
        let mut fds = [pollfd { fd: reader.as_raw_fd(), events: POLLIN, revents: 0 }];
        assert_eq!(poll(&mut fds, 0), 0);
        let mut buf = [0; 8];
        assert_eq!(read_nonblocking(&reader, &mut buf).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        writer.write_all(b"ab").unwrap();
        assert_eq!(poll(&mut fds, 1000), 1);
        assert_ne!(fds[0].revents & POLLIN, 0);
        assert_eq!(read_nonblocking(&reader, &mut buf).unwrap(), 2);
        drop(writer);
        assert_eq!(poll(&mut fds, 1000), 1);
        assert_eq!(read_nonblocking(&reader, &mut buf).unwrap(), 0);
    }

    #[test]
    fn a_negative_descriptor_is_ignored_and_the_wait_times_out() {
        let mut fds = [pollfd { fd: -1, events: POLLIN, revents: 0 }];
        let started = std::time::Instant::now();
        assert_eq!(poll(&mut fds, 30), 0);
        assert!(started.elapsed() >= std::time::Duration::from_millis(25));
    }

    #[test]
    fn a_wakeup_on_a_full_nonblocking_pipe_returns_at_once() {
        let (reader, writer) = io::pipe().unwrap();
        set_nonblocking(writer.as_raw_fd()).unwrap();
        for _ in 0..(1 << 17) {
            wake(writer.as_raw_fd());
        }
        let mut fds = [pollfd { fd: reader.as_raw_fd(), events: POLLIN, revents: 0 }];
        assert_eq!(poll(&mut fds, 0), 1);
    }

    #[test]
    fn identity_follows_the_file_not_its_name() {
        let fixture = crate::fixture::Fixture::folder();
        let (first, second) = (fixture.root.join("first.txt"), fixture.root.join("second.txt"));
        std::fs::write(&first, "one").unwrap();
        std::fs::write(&second, "two").unwrap();
        let before = identity(&File::open(&first).unwrap()).unwrap();
        assert_eq!(before, identity(&File::open(&first).unwrap()).unwrap());
        assert_ne!(before, identity(&File::open(&second).unwrap()).unwrap());
        let moved = fixture.root.join("moved.txt");
        std::fs::rename(&first, &moved).unwrap();
        assert_eq!(before, identity(&File::open(&moved).unwrap()).unwrap());
    }

    #[test]
    fn names_that_are_not_utf8_are_refused_only_where_they_cannot_be_represented() {
        assert_eq!(os_str("café.txt".as_bytes()), Some(OsStr::new("café.txt")));
        assert_eq!(os_string(b"\xff.txt".to_vec()).is_some(), cfg!(unix));
    }
}
