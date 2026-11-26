//! Watches the folder of a shell on this computer through the system, for a
//! pane that follows a shell `milo ssh` started here.

use std::io::{self, PipeReader, PipeWriter, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, RawFd};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use super::process::Process;
use crate::rex::{ShellChange, ShellNews};

/// How often the shell's folder is read. The system offers no notice of it.
const LOOK: Duration = Duration::from_millis(300);

pub struct ShellWatch {
    wake: PipeReader,
    folders: Receiver<PathBuf>,
}

impl ShellWatch {
    pub fn start(shell: Process) -> io::Result<Self> {
        let (wake, mut ring) = io::pipe()?;
        // SAFETY: The descriptor is this pipe's open read end.
        if unsafe { libc::fcntl(wake.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let (send, folders) = mpsc::channel();
        std::thread::spawn(move || look(shell, &send, &mut ring));
        Ok(Self { wake, folders })
    }

    pub fn fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    pub fn read(&mut self) -> ShellNews {
        let mut bytes = [0; 64];
        while self.wake.read(&mut bytes).is_ok_and(|n| n > 0) {}
        let mut change = ShellChange::Unchanged;
        loop {
            match self.folders.try_recv() {
                Ok(dir) => change = ShellChange::Directory(dir),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if matches!(change, ShellChange::Unchanged) {
                        change = ShellChange::Closed;
                    }
                    break;
                }
            }
        }
        ShellNews { change, foreground: None }
    }
}

/// Sends each new folder until the shell ends or nobody is watching.
fn look(shell: Process, send: &mpsc::Sender<PathBuf>, ring: &mut PipeWriter) {
    let mut last = None;
    while let Some(dir) = shell.cwd() {
        if last.as_ref() != Some(&dir) {
            if send.send(dir.clone()).is_err() || ring.write_all(b"x").is_err() {
                return;
            }
            last = Some(dir);
        }
        std::thread::sleep(LOOK);
    }
    let _ = ring.write_all(b"x");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::time::Instant;

    #[test]
    fn a_shell_s_new_folder_arrives_and_its_end_closes_the_watch() {
        let first = Fixture::folder();
        let second = Fixture::folder();
        let mut shell = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 1; cd \"$0\" && exec sleep 30", second.root.to_str().unwrap()])
            .current_dir(&first.root)
            .spawn()
            .unwrap();
        let mut watch = ShellWatch::start(Process::of(i32::try_from(shell.id()).unwrap()).unwrap()).unwrap();
        let until = Instant::now() + Duration::from_secs(30);
        let mut seen = Vec::new();
        while seen.len() < 2 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
            if let ShellChange::Directory(dir) = watch.read().change {
                seen.push(dir.canonicalize().unwrap());
            }
        }
        assert_eq!(seen, [first.root.canonicalize().unwrap(), second.root.canonicalize().unwrap()]);
        shell.kill().unwrap();
        shell.wait().unwrap();
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            std::thread::sleep(Duration::from_millis(50));
            if matches!(watch.read().change, ShellChange::Closed) {
                break;
            }
            assert!(Instant::now() < until, "the watch did not close");
        }
    }
}
