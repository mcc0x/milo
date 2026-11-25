//! Require positive evidence that a native action belongs on this Mac.

use std::path::Path;

pub(super) fn check(_root: &Path) -> Result<(), &'static str> {
    #[cfg(target_os = "macos")]
    {
        mac::check(_root)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Native opening is only available on macOS")
    }
}

#[cfg(all(test, not(target_os = "macos")))]
#[test]
fn a_non_mac_has_no_native_opening() {
    assert_eq!(check(Path::new(".")), Err("Native opening is only available on macOS"));
}

#[cfg(target_os = "macos")]
mod mac {
    use std::env;
    use std::ffi::OsStr;
    use std::io::{self, IsTerminal as _};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    use crate::term::{self, Host};

    const LOCAL_TERMINAL: &str = "Native opening needs a local graphical terminal";
    const REX_UNCONFIRMED: &str = "Could not confirm a local Rex app for this pane";
    const MULTIPLEXER: &str = "Native opening is unavailable inside a terminal multiplexer";
    const GRAPHICAL: u32 = 0x0010;
    const REMOTE: u32 = 0x1000;

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SessionGetInfo(session: u32, id: *mut u32, attributes: *mut u32) -> i32;
    }

    pub(super) fn check(root: &Path) -> Result<(), &'static str> {
        let terminal = env::var_os("TERM");
        let program = env::var_os("TERM_PROGRAM");
        check_markers(|name| env::var_os(name).is_some(), terminal.as_deref(), program.as_deref())?;
        if !foreground_terminal() {
            return Err(LOCAL_TERMINAL);
        }
        let session = env::var_os("REX_SESSION");
        let block = env::var_os("REX_BLOCK");
        let rex_server = env::var_os("REX_SERVER").is_some();
        match route(term::host(), program.as_deref(), session.as_deref(), block.as_deref(), rex_server)? {
            Route::Rex { session, block } => crate::rex::native_app(root, session, block),
            Route::Standalone => standalone(),
        }
    }

    enum Route<'a> {
        /// An older Rex names the session too. A newer one leaves it for Rex to report.
        Rex {
            session: Option<&'a OsStr>,
            block: &'a OsStr,
        },
        Standalone,
    }

    fn route<'a>(
        host: Host,
        program: Option<&OsStr>,
        session: Option<&'a OsStr>,
        block: Option<&'a OsStr>,
        rex_server: bool,
    ) -> Result<Route<'a>, &'static str> {
        match host {
            Host::Rex => match block {
                Some(block) => Ok(Route::Rex { session, block }),
                None => Err(REX_UNCONFIRMED),
            },
            Host::Multiplexer => Err(MULTIPLEXER),
            // Launch Services may pass the launching process's Rex environment to a different
            // terminal. Take the standalone path, which still verifies the actual executable.
            Host::Graphics => Ok(Route::Standalone),
            Host::Other if program == Some(OsStr::new("Apple_Terminal")) => Ok(Route::Standalone),
            Host::Other | Host::Unknown if session.is_none() && block.is_none() && !rex_server => Ok(Route::Standalone),
            Host::Other | Host::Unknown => Err(REX_UNCONFIRMED),
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
            return Err(MULTIPLEXER);
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
    }

    fn inspect(pid: libc::pid_t) -> Option<Ancestor> {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::uninit();
        let size = i32::try_from(std::mem::size_of::<libc::proc_bsdshortinfo>()).ok()?;
        // The short record is available even for the root-owned login process.
        // SAFETY: info is writable storage for the full PROC_PIDT_SHORTBSDINFO record.
        let read = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDT_SHORTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
        if read != size {
            return None;
        }
        // SAFETY: proc_pidinfo said it wrote the full record above.
        let info = unsafe { info.assume_init() };
        if libc::pid_t::try_from(info.pbsi_pid).ok()? != pid {
            return None;
        }
        let mut path = [0; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let size = u32::try_from(path.len()).ok()?;
        // SAFETY: path is writable for size bytes; the call looks up the executable in the kernel.
        let len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), size) };
        let exe = path.get(..usize::try_from(len).ok()?)?;
        Some(Ancestor {
            program: program(exe)?,
            parent: libc::pid_t::try_from(info.pbsi_ppid).ok()?,
            uid: info.pbsi_uid,
        })
    }

    fn direct_terminal(
        mut pid: libc::pid_t,
        uid: libc::uid_t,
        mut inspect: impl FnMut(libc::pid_t) -> Option<Ancestor>,
    ) -> bool {
        for _ in 0..16 {
            if pid <= 1 {
                break;
            }
            let Some(ancestor) = inspect(pid) else { break };
            if ancestor.program == Program::Terminal {
                return ancestor.uid == uid;
            }
            pid = ancestor.parent;
        }
        false
    }

    #[cfg(test)]
    mod tests {
        use super::{
            Ancestor, GRAPHICAL, Host, Program, REMOTE, REX_UNCONFIRMED, Route, check_markers, direct_terminal,
            graphical, program, route,
        };
        use std::ffi::OsStr;

        const SESSION: &str = "session:01a1fe07-6593-7bcb-a091-bfac0d945bd5";
        const BLOCK: &str = "block:01a1fe07-6593-7d94-b45e-a15c13d6737c";

        #[test]
        fn recognized_standalone_terminals_do_not_query_rex_with_inherited_ids() {
            for (host, program) in [(Host::Graphics, "ghostty"), (Host::Other, "Apple_Terminal")] {
                let route =
                    route(host, Some(OsStr::new(program)), Some(OsStr::new(SESSION)), Some(OsStr::new(BLOCK)), true);
                assert!(matches!(route, Ok(Route::Standalone)));
            }
        }

        #[test]
        fn rex_branding_requires_the_callers_block_and_never_falls_back() {
            let ids = (Some(OsStr::new(SESSION)), Some(OsStr::new(BLOCK)));
            for session in [None, ids.0] {
                assert!(matches!(route(Host::Rex, None, session, None, false), Err(REX_UNCONFIRMED)));
            }
            assert!(matches!(
                route(Host::Rex, None, ids.0, ids.1, true),
                Ok(Route::Rex { session: Some(session), block }) if session == SESSION && block == BLOCK
            ));
            // Rex preview.23 and later name the block alone.
            assert!(matches!(
                route(Host::Rex, None, None, ids.1, true),
                Ok(Route::Rex { session: None, block }) if block == BLOCK
            ));
        }

        #[test]
        fn an_ambiguous_terminal_cannot_use_inherited_rex_signals() {
            for host in [Host::Other, Host::Unknown] {
                assert!(matches!(
                    route(host, None, Some(OsStr::new(SESSION)), Some(OsStr::new(BLOCK)), false),
                    Err(REX_UNCONFIRMED)
                ));
                assert!(matches!(route(host, None, None, Some(OsStr::new(BLOCK)), false), Err(REX_UNCONFIRMED)));
                assert!(matches!(route(host, None, None, None, true), Err(REX_UNCONFIRMED)));
            }
        }

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
                12 => Some(Ancestor { program: program(b"/bin/bash")?, parent: 11, uid: 501 }),
                11 => Some(Ancestor { program: program(b"/usr/bin/login")?, parent: 10, uid: 0 }),
                10 => Some(Ancestor {
                    program: program(b"/Applications/Ghostty.app/Contents/MacOS/ghostty")?,
                    parent: 1,
                    uid: 501,
                }),
                _ => None,
            };
            assert!(direct_terminal(12, 501, inspect));
            assert!(!direct_terminal(12, 502, inspect));
            assert!(!direct_terminal(13, 501, inspect));
            assert!(!direct_terminal(12, 501, |pid| if pid == 11 { None } else { inspect(pid) }));
        }
    }
}
