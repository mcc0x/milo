//! Resolves reader paths from a held project directory, never through an unguarded open.
//!
//! The Unix form follows directory links that stay inside the project; the Windows form
//! refuses every reparse point on the way, so `links` only counts on Unix.

use std::io;

use crate::read::Content;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(super) use unix::{Entry, ReadTarget, Root, Stamp};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(super) use windows::{Entry, ReadTarget, Root, Stamp};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum LookupError {
    Outside,
    Gone,
    Unreadable,
}

impl LookupError {
    fn of(error: io::Error) -> Self {
        #[cfg(windows)]
        if error.raw_os_error() == Some(crate::fs_ops::ERROR_REPARSE_POINT_ENCOUNTERED) {
            return Self::Outside;
        }
        match error.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => Self::Gone,
            _ => Self::Unreadable,
        }
    }

    pub(super) fn content(self) -> Content {
        match self {
            Self::Gone => Content::Gone,
            Self::Outside | Self::Unreadable => Content::Unreadable,
        }
    }
}
