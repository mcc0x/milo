//! Optional Discord Rich Presence for Rex, off unless the user turns it on.
//!
//! One file holds the choice, and `milo presence`, Settings in the terminal
//! and the browser editor all read and write it through `Control`. While the
//! choice is off, nothing here starts a process or opens a socket.
//!
//! While it is on, one *helper* process shows a fixed line on the user's
//! Discord profile whenever a Rex app has a session open, and takes it down
//! when none has. Discord shows it under the name and icon of a Discord
//! application, which the user makes and whose public ID they give Milo.
//! Milo has no application of its own, and never needs a token or a sign-in.

mod discord;
mod helper;

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{self, BufRead as _, IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::setup::lock;
use crate::{paths, preferences, rex};

const FILE: &str = "discord.conf";
const PHASE_FILE: &str = "presence-phase";
const LIMIT: u64 = 1 << 12;
/// The internal command that runs the helper. Not for people to run.
pub const HELPER_COMMAND: &str = "presence-helper";

/// What turning presence on makes public, said before anyone opts in.
pub(crate) const PUBLIC: &str = "Discord will show the name and icon of your Discord application with the line “In a Rex session” while a Rex app has a session open on this computer. Anyone who can see your Discord activity sees it, and so can tell when you are using Rex. Milo sends nothing else: no session, tab, project or file names, no paths and no commands.";
/// Where an application ID comes from.
pub(crate) const APPLICATION_HELP: &str = "Create an application at https://discord.com/developers/applications and copy its Application ID. Its name and icon are what Discord shows.";
const NEEDS_APPLICATION: &str =
    "Discord presence needs the ID of a Discord application you created. Run milo presence enable APPLICATION_ID.";
const BAD_APPLICATION: &str = "An application ID is the 17 to 19 digit number Discord calls Application ID.";
const UNREADABLE: &str = "Could not read Milo's Discord presence setting, so presence will not start and the file was left alone. milo doctor names the file.";
const UNSAVED: &str = "Could not save Milo's Discord presence setting.";
pub const HELP: &str = "\
Usage: milo presence [status]
       milo presence enable [APPLICATION_ID] [--yes]
       milo presence disable

Show a fixed Discord Rich Presence line while Rex is in use. It is off until you turn it on.

enable asks before turning it on; --yes answers for you. APPLICATION_ID is remembered, so later runs can leave it out.
disable takes the activity down at once and stops Milo's presence helper.";

/// The saved choice. `enabled` is only ever saved with an application.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Setting {
    pub enabled: bool,
    /// The public ID of the user's own Discord application.
    pub application: Option<u64>,
}

impl Setting {
    fn parse(text: &str) -> io::Result<Self> {
        let invalid = || io::Error::from(io::ErrorKind::InvalidData);
        let mut setting = Self::default();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let (key, value) = line.split_once('=').ok_or_else(invalid)?;
            match (key.trim(), value.trim()) {
                ("presence", value) => setting.enabled = value.parse().map_err(|_| invalid())?,
                ("application_id", value) => setting.application = Some(application_id(value).ok_or_else(invalid)?),
                // A later release may save more here.
                _ => {}
            }
        }
        setting.enabled &= setting.application.is_some();
        Ok(setting)
    }

    fn text(self) -> String {
        let mut text = format!("presence = {}\n", self.enabled);
        if let Some(application) = self.application {
            let _ = writeln!(text, "application_id = {application}");
        }
        text
    }
}

/// A Discord application ID as people copy it: a number of 17 to 19 digits.
pub(crate) fn application_id(text: &str) -> Option<u64> {
    let text = text.trim();
    // A leading zero would not be saved as it was typed.
    let plain = (17..=19).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit()) && !text.starts_with('0');
    plain.then(|| text.parse().ok())?
}

/// What the helper last did, kept in a file for `status` to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// The activity is up.
    Showing,
    /// No Rex app has a session open.
    Idle,
    /// Discord is not running, or stopped answering.
    Waiting,
    /// Discord does not know the application ID.
    Refused,
    /// Discord answered the activity with an error.
    Rejected,
}

impl Phase {
    const ALL: [Self; 5] = [Self::Showing, Self::Idle, Self::Waiting, Self::Refused, Self::Rejected];

    const fn word(self) -> &'static str {
        match self {
            Self::Showing => "showing",
            Self::Idle => "idle",
            Self::Waiting => "waiting",
            Self::Refused => "refused",
            Self::Rejected => "rejected",
        }
    }
}

/// What a person is told about presence right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Off,
    /// On, with no helper running to show anything.
    Stopped,
    Running(Phase),
}

impl State {
    /// One or two sentences on what Discord shows now and why.
    pub(crate) const fn sentence(self) -> &'static str {
        match self {
            Self::Off => "Milo is not connected to Discord and shows nothing there.",
            Self::Stopped => {
                "Nothing is shown: Milo's presence helper is not running. It starts when Rex loads Milo's saved layout, or when you turn presence on while Rex is running."
            }
            Self::Running(Phase::Showing) => "Discord is showing “In a Rex session”.",
            Self::Running(Phase::Idle) => "Nothing is shown until a Rex app opens a session.",
            Self::Running(Phase::Waiting) => {
                "Nothing is shown: the Discord app is not running here. Milo looks for it every 10 seconds while Rex is in use."
            }
            Self::Running(Phase::Refused) => {
                "Nothing is shown: Discord does not know this application ID. Check it, then turn presence on with the right one."
            }
            Self::Running(Phase::Rejected) => "Nothing is shown: Discord did not accept the activity.",
        }
    }
}

/// The saved choice and the folders around it. Every surface goes through this.
#[derive(Clone)]
pub(crate) struct Control {
    config: PathBuf,
    state: PathBuf,
    /// Whether turning presence on may start the helper. Tests leave it off.
    starts_helper: bool,
}

impl Control {
    pub(crate) fn real() -> Option<Self> {
        Some(Self::at(paths::config_dir()?, paths::state_dir()?).starting_helper())
    }

    pub(crate) fn at(config: PathBuf, state: PathBuf) -> Self {
        Self { config, state, starts_helper: false }
    }

    pub(crate) fn starting_helper(mut self) -> Self {
        self.starts_helper = true;
        self
    }

    /// The file that holds the choice.
    pub(crate) fn file(&self) -> PathBuf {
        self.config.join(FILE)
    }

    pub(crate) fn setting(&self) -> io::Result<Setting> {
        preferences::read(&self.file(), LIMIT)?.map_or(Ok(Setting::default()), |text| Setting::parse(&text))
    }

    fn save(&self, setting: Setting) -> io::Result<()> {
        let path = self.file();
        preferences::locked(&path, |dir| preferences::replace(dir, &path, &setting.text()))
    }

    /// The saved choice and what is happening now.
    pub(crate) fn status(&self) -> io::Result<(Setting, State)> {
        let setting = self.setting()?;
        let state = if !setting.enabled {
            State::Off
        } else if lock::held(&self.state, lock::PRESENCE_LOCK).unwrap_or(false) {
            self.phase().map_or(State::Stopped, State::Running)
        } else {
            State::Stopped
        };
        Ok((setting, state))
    }

    /// Turns presence on, with `application` or else the one saved before,
    /// and starts the helper. Returns what to tell the person.
    pub(crate) fn enable(&self, application: Option<u64>) -> Result<String, &'static str> {
        let saved = self.setting().map_err(|_| UNREADABLE)?;
        let application = application.or(saved.application).ok_or(NEEDS_APPLICATION)?;
        self.save(Setting { enabled: true, application: Some(application) }).map_err(|_| UNSAVED)?;
        // A helper showing another application, or one that Discord refused, makes way.
        let _ = lock::stop_holder(&self.state, lock::PRESENCE_LOCK);
        let state = if self.starts_helper { self.start(&rex::program()) } else { State::Stopped };
        Ok(format!("Discord presence is on. {}", state.sentence()))
    }

    /// Turns presence off and takes the activity down before returning. The
    /// helper is stopped even when the choice could not be read or saved: it
    /// will not start again from a choice Milo cannot read.
    pub(crate) fn disable(&self) -> Result<String, &'static str> {
        let saved = self
            .setting()
            .map_err(|_| UNREADABLE)
            .and_then(|saved| self.save(Setting { enabled: false, ..saved }).map_err(|_| UNSAVED));
        lock::stop_holder(&self.state, lock::PRESENCE_LOCK)
            .map_err(|_| "Milo's presence helper did not stop. Quit Discord or Rex to clear the activity.")?;
        saved?;
        Ok(format!("Discord presence is off. {}", State::Off.sentence()))
    }

    /// Starts the helper if presence is on. `milo listen` calls this, so
    /// that presence comes up with Rex. While presence is off it only reads the choice.
    pub(crate) fn start_if_enabled(&self) {
        if self.setting().is_ok_and(|setting| setting.enabled) {
            let _ = self.launch(&rex::program());
        }
    }

    fn launch(&self, rex: &OsStr) -> io::Result<()> {
        // The helper leaves a copy of itself running and returns at once.
        Command::new(std::env::current_exe()?)
            .arg(HELPER_COMMAND)
            .arg(rex)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(drop)
    }

    /// Starts the helper and waits a moment to say what it found.
    fn start(&self, rex: &OsStr) -> State {
        const STARTING: Duration = Duration::from_millis(1500);
        if self.launch(rex).is_err() {
            return State::Stopped;
        }
        let began = Instant::now();
        loop {
            // The helper notes a phase once Rex has answered it, and ends at once if Rex is not running.
            if let Ok((_, state @ State::Running(_))) = self.status() {
                return state;
            }
            if began.elapsed() >= STARTING {
                return State::Stopped;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn phase(&self) -> Option<Phase> {
        let word = preferences::read(&self.state.join(PHASE_FILE), LIMIT).ok()??;
        Phase::ALL.into_iter().find(|phase| phase.word() == word.trim())
    }

    fn note_phase(&self, phase: Phase) {
        let _ = std::fs::write(self.state.join(PHASE_FILE), phase.word());
    }

    fn forget_phase(&self) {
        let _ = std::fs::remove_file(self.state.join(PHASE_FILE));
    }
}

/// Runs the helper for the internal command of that name.
pub fn helper(args: &[OsString]) -> i32 {
    match args {
        [rex] => helper::run(rex),
        _ => 1,
    }
}

/// Turns presence off when Milo itself is removed, so that a later install starts with it off.
pub fn turn_off() {
    if let Some(control) = Control::real()
        && control.setting().is_ok_and(|setting| setting.enabled)
    {
        let _ = control.disable();
    }
}

/// Brings presence up with Rex, when it is on.
pub fn start_with_rex() {
    if let Some(control) = Control::real() {
        control.start_if_enabled();
    }
}

/// What `milo presence` was asked to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Status,
    Enable { application: Option<u64>, confirmed: bool },
    Disable,
}

impl Request {
    pub fn parse(action: Option<&OsStr>, rest: &[OsString]) -> Result<Self, String> {
        let words: Option<Vec<&str>> = rest.iter().map(|word| word.to_str()).collect();
        match (action.map(OsStr::to_str), words.as_deref()) {
            (None | Some(Some("status")), Some([])) => Ok(Self::Status),
            (Some(Some("disable")), Some([])) => Ok(Self::Disable),
            (Some(Some("enable")), Some(words)) => {
                let (mut application, mut confirmed) = (None, false);
                for &word in words {
                    if word == "--yes" && !confirmed {
                        confirmed = true;
                    } else if application.is_none() && !word.starts_with('-') {
                        application = Some(application_id(word).ok_or(BAD_APPLICATION)?);
                    } else {
                        return Err("expected milo presence enable [APPLICATION_ID] [--yes]".into());
                    }
                }
                Ok(Self::Enable { application, confirmed })
            }
            _ => Err("expected milo presence [status] | enable [APPLICATION_ID] [--yes] | disable".into()),
        }
    }
}

/// Runs `milo presence` and returns its last line.
pub fn command(request: Request) -> Result<String, String> {
    let control = Control::real().ok_or("Could not find Milo's settings folders.")?;
    match request {
        Request::Status => {
            let (setting, state) = control.status().map_err(|_| UNREADABLE)?;
            let mut report = format!("Discord presence: {}\n", if setting.enabled { "on" } else { "off" });
            match setting.application {
                Some(application) => writeln!(report, "Application ID: {application}"),
                None => writeln!(report, "Application ID: not set. {APPLICATION_HELP}"),
            }
            .expect("writing to a string");
            report.push_str(state.sentence());
            if !setting.enabled {
                write!(report, "\nIf you turn it on: {PUBLIC}").expect("writing to a string");
            }
            Ok(report)
        }
        Request::Enable { application, confirmed } => {
            let saved = control.setting().map_err(|_| UNREADABLE)?;
            if application.or(saved.application).is_none() {
                // An error is printed on one line.
                return Err(format!("{NEEDS_APPLICATION} {APPLICATION_HELP}"));
            }
            if !confirmed && !saved.enabled {
                confirm()?;
            }
            control.enable(application).map_err(str::to_owned)
        }
        Request::Disable => control.disable().map_err(str::to_owned),
    }
}

/// Says what becomes public and waits for a yes typed at a terminal.
fn confirm() -> Result<(), String> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return Err(format!(
            "Discord presence was left off, because nobody could be asked. {PUBLIC} To turn it on knowing that, add --yes."
        ));
    }
    let mut out = io::stdout().lock();
    write!(out, "{PUBLIC}\nTurn on Discord presence? [y/N] ").and_then(|()| out.flush()).map_err(|e| e.to_string())?;
    let mut answer = String::new();
    stdin.lock().read_line(&mut answer).map_err(|e| e.to_string())?;
    if matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
        Ok(())
    } else {
        Err("Discord presence was left off.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    fn control(f: &Fixture) -> Control {
        Control::at(f.root.join("config"), f.root.join("state"))
    }

    #[test]
    fn presence_is_off_until_enabled_and_keeps_the_application_when_turned_off() {
        let f = Fixture::folder();
        let control = control(&f);
        assert_eq!(control.status().unwrap(), (Setting::default(), State::Off));
        assert!(!f.root.join("config").exists(), "reading the choice writes nothing");
        assert_eq!(control.enable(None), Err(NEEDS_APPLICATION));
        assert_eq!(control.status().unwrap().1, State::Off);

        let id = 1_234_567_890_123_456_789;
        control.enable(Some(id)).unwrap();
        let on = Setting { enabled: true, application: Some(id) };
        assert_eq!(control.status().unwrap(), (on, State::Stopped));
        assert_eq!(
            std::fs::read_to_string(f.root.join("config").join(FILE)).unwrap(),
            "presence = true\napplication_id = 1234567890123456789\n"
        );

        assert!(control.disable().unwrap().starts_with("Discord presence is off."));
        assert_eq!(control.status().unwrap(), (Setting { enabled: false, ..on }, State::Off));
        // The remembered application turns it back on without being typed again.
        control.enable(None).unwrap();
        assert_eq!(control.setting().unwrap(), on);
    }

    #[test]
    fn a_setting_file_nobody_could_have_saved_is_an_error_and_never_turns_presence_on() {
        let f = Fixture::folder();
        let control = control(&f);
        std::fs::create_dir_all(f.root.join("config")).unwrap();
        let file = f.root.join("config").join(FILE);
        for text in ["presence = yes\n", "presence = true\napplication_id = rex\n", "presence\n"] {
            std::fs::write(&file, text).unwrap();
            assert!(control.setting().is_err(), "{text:?}");
            assert_eq!(control.enable(None), Err(UNREADABLE));
            // Turning it off still stops a helper, and says the choice itself could not be saved.
            assert_eq!(control.disable(), Err(UNREADABLE));
            assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
        }
        // On without an application cannot show anything, so it reads as off.
        std::fs::write(&file, "presence = true\nlater_choice = 1\n").unwrap();
        assert_eq!(control.setting().unwrap(), Setting::default());
    }

    #[test]
    fn application_ids_are_whole_numbers_of_the_length_discord_gives() {
        assert_eq!(application_id(" 12345678901234567 "), Some(12_345_678_901_234_567));
        assert_eq!(application_id("1234567890123456789"), Some(1_234_567_890_123_456_789));
        for bad in [
            "",
            "1234567890123456",
            "12345678901234567890",
            "1234567890123456x",
            "-1234567890123456",
            // Saved as a number, these would not read back as what was typed.
            "00000000000000000",
            "0123456789012345678",
            "１２３４５６７８９０１２３４５６７",
        ] {
            assert_eq!(application_id(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_presence_command_takes_one_application_and_one_yes() {
        let parse = |action: Option<&str>, rest: &[&str]| {
            Request::parse(action.map(OsStr::new), &rest.iter().map(OsString::from).collect::<Vec<_>>())
        };
        assert_eq!(parse(None, &[]), Ok(Request::Status));
        assert_eq!(parse(Some("status"), &[]), Ok(Request::Status));
        assert_eq!(parse(Some("disable"), &[]), Ok(Request::Disable));
        assert_eq!(parse(Some("enable"), &[]), Ok(Request::Enable { application: None, confirmed: false }));
        assert_eq!(
            parse(Some("enable"), &["--yes", "12345678901234567"]),
            Ok(Request::Enable { application: Some(12_345_678_901_234_567), confirmed: true })
        );
        assert_eq!(parse(Some("enable"), &["rex"]), Err(BAD_APPLICATION.to_owned()));
        for (action, rest) in [
            (Some("enable"), &["12345678901234567", "12345678901234568"][..]),
            (Some("enable"), &["--yes", "--yes"]),
            (Some("enable"), &["--token"]),
            (Some("disable"), &["now"]),
            (Some("toggle"), &[]),
        ] {
            assert!(parse(action, rest).is_err(), "{action:?} {rest:?}");
        }
    }
}
