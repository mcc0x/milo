//! `milo alerts`: see, choose, test and turn off the mirroring of Rex alerts.

use crate::sys::AsRawFd as _;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{self, BufRead as _, IsTerminal as _, Read as _, Write as _};

use super::{Alerts, Content, Destination, Kind, Summary, describe, forward, lifecycle, message, parse_kinds};

pub(crate) const HELP: &str = "\
Usage: milo alerts
       milo alerts telegram CHAT
       milo alerts discord
       milo alerts use telegram | discord
       milo alerts events KIND[,KIND...]
       milo alerts content metadata | text
       milo alerts origin on | off
       milo alerts test [telegram | discord]
       milo alerts on|off [telegram | discord]
       milo alerts notices on|off telegram | discord
       milo alerts forget telegram | discord
       milo alerts forget telegram --local-only

Mirror Rex alerts to a Telegram chat, a Discord channel, or both. Forwarding
is opt-in, and Rex's own notifications stay as they are.

milo alerts shows what is saved, whether mirroring is running, and exactly
what would be sent.
telegram CHAT saves a bot and the chat it posts to. CHAT is the chat's number
or a public channel's @name. The bot's token is asked for, or read from
standard input; never give it as an argument.
discord saves a channel's webhook link, asked for in the same way.
use selects a saved destination for commands without a provider. It also
switches mirroring if you have not set independent forwarding choices.
events chooses the kinds mirrored: blocked, error, done, notification, bell.
The first three are chosen to begin with.
content chooses whether to send the line the program gave Rex. The default,
metadata, sends only the kind and the program's short name unless origin is on.
origin on adds a short Rex session label when Milo can verify the event's
session, for every enabled destination. It is off by default, independent
of program text, and may reveal a name you use in Rex.
test sends one message written by Milo to the named or selected destination.
on and off with a provider affect only that provider. Without a provider, off
pauses all forwarding and on resumes it (or starts with the selected destination
if none are chosen). forget removes a saved destination and its secret.
notices opts one saved destination into plain forwarding status: enabled,
explicitly paused, or recovered. Notices are off by default, carry no Rex
alert data, and a final pause notice is best effort.
forget telegram --local-only is only for a Telegram chat connected with Milo
whose connection service is gone for good, so that forget can never revoke
the connection. It removes the connection from this machine and revokes
nothing: send /stop to Milo's bot in that chat to revoke it.";

const USAGE: &str = "unexpected arguments; see milo alerts --help";

/// Runs one `milo alerts` command and prints what it did.
pub(crate) fn command(action: Option<&OsStr>, rest: &[OsString]) -> Result<(), String> {
    let alerts = Alerts::real().ok_or("Milo could not find its settings folder; HOME is not set.")?;
    let words: Vec<_> = action.into_iter().chain(rest.iter().map(OsString::as_os_str)).map(OsStr::to_str).collect();
    let report = match words.as_slice() {
        [] => status(&alerts.summary()?, alerts.secrets.place()),
        [Some("telegram"), Some(chat)] => {
            let token = secret("Telegram bot token: ")?;
            alerts.set_telegram(&token, chat)?
        }
        [Some("discord")] => {
            let link = secret("Discord webhook link: ")?;
            alerts.set_discord(&link)?
        }
        [Some("use"), Some(name)] => alerts.choose(destination(name)?)?,
        [Some("events"), Some(list)] => {
            alerts.set_kinds(parse_kinds(list).ok_or("Kinds are blocked, error, done, notification and bell.")?)?
        }
        [Some("content"), Some(name)] => {
            alerts.set_content(Content::parse(name).ok_or("Content is metadata or text.")?)?
        }
        [Some("origin"), Some("on")] => alerts.set_origin(true)?,
        [Some("origin"), Some("off")] => alerts.set_origin(false)?,
        [Some("origin"), Some(_)] => return Err("Origin is on or off.".into()),
        [Some("test")] => alerts.test()?,
        [Some("test"), Some(name)] => alerts.test_destination(destination(name)?)?,
        [Some("on")] => alerts.enable()?,
        [Some("on"), Some(name)] => alerts.set_forwarding(destination(name)?, true)?,
        [Some("off")] => alerts.disable()?,
        [Some("off"), Some(name)] => alerts.set_forwarding(destination(name)?, false)?,
        [Some("notices"), Some("on"), Some(name)] => {
            alerts.locked(|edit| edit.set_notices(destination(name)?, true))?
        }
        [Some("notices"), Some("off"), Some(name)] => {
            alerts.locked(|edit| edit.set_notices(destination(name)?, false))?
        }
        [Some("forget"), Some(name)] => alerts.forget(destination(name)?)?,
        [Some("forget"), Some("telegram"), Some("--local-only")] => alerts.forget_telegram_locally()?,
        [Some("send-notice"), Some(name), Some(id)] => {
            lifecycle::dispatch(&alerts, destination(name)?, u64::from_str_radix(id, 16).map_err(|_| USAGE)?);
            return Ok(());
        }
        [Some("supervise")] => {
            return forward::supervise(&alerts).map_err(|error| format!("could not supervise Rex alerts: {error}"));
        }
        [Some("run"), Some(parent), Some(recovery)] => {
            let parent = parent.parse().map_err(|_| USAGE)?;
            let recovery = forward::Recovery::parse(recovery).ok_or(USAGE)?;
            return forward::run(&alerts, parent, recovery)
                .map_err(|error| format!("could not mirror Rex alerts: {error}"));
        }
        _ => return Err(USAGE.into()),
    };
    println!("{report}");
    Ok(())
}

fn destination(name: &str) -> Result<Destination, String> {
    Destination::parse(name).ok_or_else(|| "Destinations are telegram and discord.".into())
}

fn status(summary: &Summary, place: &str) -> String {
    let settings = &summary.settings;
    let health = summary.health.label();
    let mut report = format!("Rex alert mirroring: {health}\nDestination: {}\n", describe(settings));
    for destination in Destination::ALL {
        let saved = if summary.saved(destination) { "saved" } else { "not saved" };
        let forwarding = if settings.active(destination) { "on" } else { "off" };
        let notices = if settings.notices[destination as usize] { "on" } else { "off" };
        let _ = writeln!(
            report,
            "  {}: {saved}; forwarding {forwarding}; lifecycle notices {notices}",
            destination.title()
        );
    }
    report.push_str(&format!("Secrets are kept in {place}.\n{}", message::disclosure(settings)));
    let unchosen: Vec<_> = Kind::ALL.into_iter().filter(|&kind| !settings.mirrors(kind)).map(Kind::name).collect();
    if !unchosen.is_empty() {
        report.push_str(&format!("\nNot mirrored: {}.", unchosen.join(", ")));
    }
    if !summary.note.is_empty() {
        report.push_str(&format!("\n{}", summary.note));
    }
    report
}

/// One line from standard input, asked for without showing it when a person types it.
fn secret(prompt: &str) -> Result<String, String> {
    const LIMIT: u64 = 512;
    let stdin = io::stdin();
    let hidden = stdin.is_terminal().then(|| {
        eprint!("{prompt}");
        let _ = io::stderr().flush();
        Echo::off(stdin.as_raw_fd())
    });
    let mut line = String::new();
    let read = stdin.lock().take(LIMIT).read_line(&mut line);
    if hidden.is_some() {
        eprintln!();
    }
    drop(hidden);
    match read {
        Ok(_) if !line.trim().is_empty() => Ok(line.trim().to_owned()),
        _ => Err("Nothing was read from standard input, so nothing was saved.".into()),
    }
}

/// The keyboard's modes as they were before the prompt, for `interrupted`.
static SHOWN: std::sync::OnceLock<libc::termios> = std::sync::OnceLock::new();

/// Puts typed characters back on the screen before Ctrl-C ends the prompt.
extern "C" fn interrupted(_: libc::c_int) {
    if let Some(modes) = SHOWN.get() {
        // SAFETY: Standard input is open for the life of the process, the modes
        // are those read from it, and tcsetattr may be called from a handler.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, modes) };
    }
    // SAFETY: Ending the process without running destructors is safe from a signal handler.
    unsafe { libc::_exit(130) };
}

/// Keeps typed characters off the screen until dropped.
struct Echo(libc::c_int, libc::termios);

impl Echo {
    fn off(fd: libc::c_int) -> Option<Self> {
        // SAFETY: A zeroed termios is plain numbers, and tcgetattr fills it in.
        let mut modes: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: The descriptor is open and `modes` is writable storage for one termios.
        if unsafe { libc::tcgetattr(fd, &raw mut modes) } != 0 {
            return None;
        }
        let saved = Self(fd, modes);
        if SHOWN.set(modes).is_ok() {
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                // SAFETY: The handler only restores the modes saved just above and ends the process.
                unsafe { libc::signal(signal, interrupted as *const () as libc::sighandler_t) };
            }
        }
        modes.c_lflag &= !libc::ECHO;
        // SAFETY: The descriptor is open and `modes` is a whole termios read from it.
        unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw const modes) };
        Some(saved)
    }
}

impl Drop for Echo {
    fn drop(&mut self) {
        // SAFETY: The descriptor is still open and the modes are those read from it.
        unsafe { libc::tcsetattr(self.0, libc::TCSAFLUSH, &raw const self.1) };
    }
}
