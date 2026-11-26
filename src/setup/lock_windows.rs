//! Process-owned file locks and the processes that hold them are Unix facilities: a holder is
//! named by a kernel lock and ended by a signal. Nothing here is ported to Windows yet, so every
//! operation says so instead of reporting a lock as free or held.

use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Held by the one process that may set the user's Discord activity.
pub(crate) const PRESENCE_LOCK: &CStr = c"presence.lock";

/// Windows has no signal to send; this only keeps the name that Unix code uses.
pub(crate) const REX_STARTED: libc::c_int = 0;

fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "Process locks are not supported on Windows yet"))
}

pub(super) fn setup(_dir: &Path) -> Result<File, &'static str> {
    Err("Setup is not supported on Windows yet.")
}

pub(super) fn running(_dir: &Path) -> io::Result<bool> {
    unsupported()
}

pub(crate) fn held(_dir: &Path, _name: &CStr) -> io::Result<bool> {
    unsupported()
}

pub(crate) fn alerts_worker(_dir: &Path) -> io::Result<Option<i32>> {
    unsupported()
}

pub(crate) fn alerts_supervised(_dir: &Path) -> io::Result<bool> {
    unsupported()
}

pub(crate) fn hold_alerts(_dir: &Path) -> io::Result<Option<File>> {
    unsupported()
}

pub(crate) fn hold_alerts_supervisor(_dir: &Path) -> io::Result<Option<File>> {
    unsupported()
}

pub(crate) fn stop_alerts(_dir: &Path) -> io::Result<()> {
    unsupported()
}

pub(crate) fn stop_alerts_worker(_dir: &Path) -> io::Result<()> {
    unsupported()
}

pub(crate) fn tell_alerts_worker_rex_started(_dir: &Path) -> io::Result<()> {
    unsupported()
}

pub(crate) fn wait_alerts_supervised(_dir: &Path) -> io::Result<bool> {
    unsupported()
}

pub(crate) fn hold(_dir: &Path, _name: &CStr, _patience: Duration) -> io::Result<Option<File>> {
    unsupported()
}

pub(super) fn wait_running(_dir: &Path) -> io::Result<bool> {
    unsupported()
}

pub(super) fn listen(_dir: &Path, _script: &Path) -> io::Result<()> {
    unsupported()
}

pub(crate) fn hold_in_child(_command: &mut Command, _dir: &Path, _name: &CStr) -> io::Result<()> {
    unsupported()
}

pub(super) fn stop(_dir: &Path) -> io::Result<()> {
    unsupported()
}

pub(crate) fn stop_holder(_dir: &Path, _name: &CStr) -> io::Result<()> {
    unsupported()
}

pub(crate) fn signal_holder(_dir: &Path, _name: &CStr) -> io::Result<()> {
    unsupported()
}
