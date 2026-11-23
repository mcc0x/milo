//! Require positive evidence that a native action belongs on this Mac.

pub(super) fn check() -> Result<(), &'static str> {
    #[cfg(target_os = "macos")]
    {
        mac::check()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Native opening is only available on macOS")
    }
}

#[cfg(all(test, not(target_os = "macos")))]
#[test]
fn a_non_mac_has_no_native_opening() {
    assert_eq!(check(), Err("Native opening is only available on macOS"));
}

#[cfg(target_os = "macos")]
mod mac {
    use std::env;
    use std::ffi::OsStr;
    use std::io::{self, IsTerminal as _};
    use std::os::unix::ffi::OsStrExt as _;

    const LOCAL_TERMINAL: &str = "Native opening needs a local graphical terminal";
    const GRAPHICAL: u32 = 0x0010;
    const REMOTE: u32 = 0x1000;

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SessionGetInfo(session: u32, id: *mut u32, attributes: *mut u32) -> i32;
    }

    pub(super) fn check() -> Result<(), &'static str> {
        let terminal = env::var_os("TERM");
        let program = env::var_os("TERM_PROGRAM");
        check_markers(|name| env::var_os(name).is_some(), terminal.as_deref(), program.as_deref())?;
        if !foreground_terminal() {
            return Err(LOCAL_TERMINAL);
        }
        match (env::var_os("REX_SESSION"), env::var_os("REX_BLOCK")) {
            (Some(session), Some(block)) => crate::rex::native_app(&session, &block),
            (None, None) => standalone(),
            _ => Err("Could not confirm a local Rex app for this pane"),
        }
    }

    fn check_markers(
        present: impl Fn(&str) -> bool,
        terminal: Option<&OsStr>,
        program: Option<&OsStr>,
    ) -> Result<(), &'static str> {
        // Agent sockets and askpass helpers are also common in local terminals.
        if ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "MOSH_CONNECTION", "MOSH_IP"].into_iter().any(&present) {
            return Err("Native opening is unavailable from a remote terminal");
        }
        let mux = ["TMUX", "TMUX_PANE", "STY", "ZELLIJ", "ZELLIJ_SESSION_NAME", "BYOBU_BACKEND"];
        let mux_terminal = terminal.is_some_and(|term| {
            let name = term.as_bytes().split(|&b| b == b'-').next().unwrap_or_default();
            matches!(name, b"tmux" | b"screen" | b"dvtm" | b"zellij")
        });
        let mux_program = program.is_some_and(|program| matches!(program.as_bytes(), b"tmux" | b"screen" | b"zellij"));
        if mux.into_iter().any(present) || mux_terminal || mux_program {
            return Err("Native opening is unavailable inside a terminal multiplexer");
        }
        Ok(())
    }

    fn foreground_terminal() -> bool {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return false;
        }
        // SAFETY: Neither call takes a pointer. stdin was just checked as a terminal.
        let foreground = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        // SAFETY: Getting the current process group touches no memory.
        let group = unsafe { libc::getpgrp() };
        foreground == group
    }

    fn graphical_session() -> bool {
        // AuthSession.h defines -1 (as a UInt32) to mean the caller's own session.
        let mut attributes = 0;
        // SAFETY: The optional ID is not requested, and the attribute output is a writable UInt32.
        let status = unsafe { SessionGetInfo(u32::MAX, std::ptr::null_mut(), &raw mut attributes) };
        graphical(status, attributes)
    }

    fn graphical(status: i32, attributes: u32) -> bool {
        status == 0 && attributes & (GRAPHICAL | REMOTE) == GRAPHICAL
    }

    fn standalone() -> Result<(), &'static str> {
        if !graphical_session() {
            return Err(LOCAL_TERMINAL);
        }
        // SAFETY: Getting the parent process ID touches no memory.
        let parent = unsafe { libc::getppid() };
        // SAFETY: Getting the current effective user ID touches no memory.
        let uid = unsafe { libc::geteuid() };
        direct_terminal(parent, uid, inspect).then_some(()).ok_or(LOCAL_TERMINAL)
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Program {
        Shell,
        Terminal,
    }

    fn program(exe: &[u8]) -> Option<Program> {
        // Only these actual executables count, not TERM_PROGRAM or a process name.
        match exe {
            b"/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal"
            | b"/Applications/Ghostty.app/Contents/MacOS/ghostty"
            | b"/Applications/iTerm.app/Contents/MacOS/iTerm2"
            | b"/Applications/iTerm2.app/Contents/MacOS/iTerm2" => Some(Program::Terminal),
            b"/bin/zsh" | b"/bin/bash" | b"/bin/sh" | b"/bin/csh" | b"/bin/tcsh" | b"/bin/ksh" | b"/usr/bin/login" => {
                Some(Program::Shell)
            }
            _ => None,
        }
    }

    struct Ancestor {
        program: Program,
        parent: libc::pid_t,
        uid: libc::uid_t,
        started: (u64, u64),
    }

    fn inspect(pid: libc::pid_t) -> Option<Ancestor> {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: info is writable storage for the full PROC_PIDTBSDINFO record.
        let read = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
        if read != size {
            return None;
        }
        // SAFETY: proc_pidinfo said it wrote the full record above.
        let info = unsafe { info.assume_init() };
        let mut path = [0; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let size = u32::try_from(path.len()).ok()?;
        // SAFETY: path is writable for size bytes; the call looks up the executable in the kernel.
        let len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), size) };
        let exe = path.get(..usize::try_from(len).ok()?)?;
        Some(Ancestor {
            program: program(exe)?,
            parent: libc::pid_t::try_from(info.pbi_ppid).ok()?,
            uid: info.pbi_uid,
            started: (info.pbi_start_tvsec, info.pbi_start_tvusec),
        })
    }

    fn direct_terminal(
        mut pid: libc::pid_t,
        uid: libc::uid_t,
        mut inspect: impl FnMut(libc::pid_t) -> Option<Ancestor>,
    ) -> bool {
        let mut child_started = None;
        for _ in 0..16 {
            if pid <= 1 {
                break;
            }
            let Some(ancestor) = inspect(pid) else { break };
            // A newer process cannot be the parent we meant to inspect.
            if child_started.is_some_and(|started| ancestor.started > started) {
                break;
            }
            if ancestor.program == Program::Terminal {
                return ancestor.uid == uid;
            }
            child_started = Some(ancestor.started);
            pid = ancestor.parent;
        }
        false
    }

    #[cfg(test)]
    mod tests {
        use super::{Ancestor, GRAPHICAL, Program, REMOTE, check_markers, direct_terminal, graphical, program};
        use std::ffi::OsStr;

        #[test]
        fn ssh_and_multiplexer_evidence_veto_even_a_native_terminal_label() {
            let terminal = Some(OsStr::new("xterm-256color"));
            let apple = Some(OsStr::new("Apple_Terminal"));
            for marker in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "TMUX", "STY", "ZELLIJ"] {
                assert!(check_markers(|name| name == marker, terminal, apple).is_err(), "{marker}");
            }
            assert!(check_markers(|name| name == "SSH_AUTH_SOCK", terminal, apple).is_ok());
            assert!(check_markers(|_| false, Some(OsStr::new("screen-256color")), apple).is_err());
            assert!(check_markers(|_| false, terminal, Some(OsStr::new("tmux"))).is_err());
        }

        #[test]
        fn the_security_session_must_positively_report_graphics_without_remote_access() {
            assert!(graphical(0, GRAPHICAL));
            assert!(!graphical(0, GRAPHICAL | REMOTE));
            assert!(!graphical(0, 0));
            assert!(!graphical(-1, GRAPHICAL));
        }

        #[test]
        fn standalone_only_trusts_a_direct_chain_from_a_known_app() {
            assert_eq!(program(b"/Applications/Ghostty.app/Contents/MacOS/ghostty"), Some(Program::Terminal));
            assert_eq!(program(b"/tmp/Ghostty.app/Contents/MacOS/ghostty"), None);
            assert_eq!(program(b"/opt/homebrew/bin/tmux"), None);
            let inspect = |pid| match pid {
                11 => Some(Ancestor { program: Program::Shell, parent: 10, uid: 501, started: (1, 2) }),
                10 => Some(Ancestor { program: Program::Terminal, parent: 1, uid: 501, started: (1, 1) }),
                _ => None,
            };
            assert!(direct_terminal(11, 501, inspect));
            assert!(!direct_terminal(11, 502, inspect));
            assert!(!direct_terminal(12, 501, inspect));
            assert!(!direct_terminal(11, 501, |pid| {
                let mut ancestor = inspect(pid)?;
                if pid == 10 {
                    ancestor.started = (2, 0);
                }
                Some(ancestor)
            }));
        }
    }
}
