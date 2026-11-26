//! Names one running process by its ID and the time it started, so that an
//! ID the system has since given to another process is not mistaken for it,
//! and reads the folder it is in from the system rather than from anything it
//! printed.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: i32,
    /// When the process started, in the system's own units. Only compared.
    pub start: u64,
}

impl Process {
    pub fn current() -> Option<Self> {
        Self::of(i32::try_from(std::process::id()).ok()?)
    }

    pub fn of(pid: i32) -> Option<Self> {
        if pid <= 0 {
            return None;
        }
        Some(Self { pid, start: started(pid)? })
    }

    /// Whether this very process is still running.
    pub fn alive(&self) -> bool {
        Self::of(self.pid) == Some(*self)
    }

    /// Whether `pid` is this process or one it started, directly or not.
    pub fn leads(&self, pid: i32) -> bool {
        let mut pid = pid;
        // A chain of parents this long is not a terminal's job.
        for _ in 0..32 {
            if pid == self.pid {
                return self.alive();
            }
            match parent(pid) {
                Some(next) if next > 1 && next != pid => pid = next,
                _ => return false,
            }
        }
        false
    }

    /// The folder the process is in, if it is still this process.
    pub fn cwd(&self) -> Option<PathBuf> {
        let dir = cwd(self.pid)?;
        // The ID could have been given to another process between the two reads.
        (self.alive() && dir.is_absolute()).then_some(dir)
    }
}

#[cfg(target_os = "linux")]
fn started(pid: i32) -> Option<u64> {
    let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    // The name in parentheses can hold spaces and parentheses; the fields
    // after its last closing parenthesis cannot. The start time is field 22,
    // the 20th after the name.
    let end = stat.iter().rposition(|&b| b == b')')?;
    let fields = std::str::from_utf8(&stat[end + 1..]).ok()?;
    fields.split_ascii_whitespace().nth(19)?.parse().ok()
}

#[cfg(target_os = "linux")]
fn parent(pid: i32) -> Option<i32> {
    let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    let end = stat.iter().rposition(|&b| b == b')')?;
    std::str::from_utf8(&stat[end + 1..]).ok()?.split_ascii_whitespace().nth(1)?.parse().ok()
}

#[cfg(target_os = "linux")]
fn cwd(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(target_os = "macos")]
fn started(pid: i32) -> Option<u64> {
    // SAFETY: proc_bsdinfo holds only integers and byte arrays, which are valid when zeroed.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` is writable and `size` is its exact size in bytes.
    let written = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (written == size).then(|| info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

#[cfg(target_os = "macos")]
fn parent(pid: i32) -> Option<i32> {
    // SAFETY: proc_bsdinfo holds only integers and byte arrays, which are valid when zeroed.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` is writable and `size` is its exact size in bytes.
    let written = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (written == size).then(|| i32::try_from(info.pbi_ppid).ok()).flatten()
}

#[cfg(target_os = "macos")]
fn cwd(pid: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: proc_vnodepathinfo holds only integers and byte arrays, which are valid when zeroed.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = i32::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
    // SAFETY: `info` is writable and `size` is its exact size in bytes.
    let written = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size) };
    if written != size {
        return None;
    }
    let path = &info.pvi_cdir.vip_path;
    // SAFETY: c_char and u8 have the same size and alignment, and the slice covers only the array.
    let bytes = unsafe { std::slice::from_raw_parts(path.as_ptr().cast::<u8>(), std::mem::size_of_val(path)) };
    let end = bytes.iter().position(|&b| b == 0)?;
    (end > 0).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..end])))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn started(_pid: i32) -> Option<u64> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn parent(_pid: i32) -> Option<i32> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn cwd(_pid: i32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_is_known_by_its_folder_until_it_ends() {
        let dir = crate::fixture::Fixture::folder();
        let mut child = std::process::Command::new("/bin/sleep").arg("30").current_dir(&dir.root).spawn().unwrap();
        let process = Process::of(i32::try_from(child.id()).unwrap()).unwrap();
        assert!(process.alive());
        assert_eq!(process.cwd().unwrap().canonicalize().unwrap(), dir.root.canonicalize().unwrap());
        assert!(!Process { start: process.start + 1, ..process }.alive());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!process.alive());
        assert_eq!(process.cwd(), None);
    }

    #[test]
    fn this_process_is_alive_and_leads_what_it_started() {
        let me = Process::current().unwrap();
        assert!(me.alive());
        let mut child = std::process::Command::new("/bin/sh").args(["-c", "sleep 30; :"]).spawn().unwrap();
        let shell = i32::try_from(child.id()).unwrap();
        assert!(me.leads(me.pid) && me.leads(shell));
        assert!(!Process::of(shell).unwrap().leads(me.pid));
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
