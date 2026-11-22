//! Optional Discord Rich Presence for Rex, off unless the user turns it on.
//!
//! One file holds the choice, and `milo presence`, Settings in the terminal
//! and the browser editor all read and write it through `Control`. While the
//! choice is off, nothing here starts a process or opens a socket.
//!
//! While it is on, one *helper* process shows a fixed line on the user's
//! Discord profile whenever a Rex app has a session open, and takes it down
//! when none has. Discord shows it under the name and icon of Milo's public
//! Discord application, or of one the user made and named instead. Either
//! way only the application's public ID is used: never a bot, a token or a
//! sign-in.
//!
//! *Details* are the names a person may choose to add under the fixed line:
//! the session in front, the Git project in its focused pane, or both. They
//! are off unless chosen, and Rex is asked about sessions only for the names
//! that were.

mod discord;
mod helper;
mod label;

use std::borrow::Cow;
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
/// Milo's own Discord application. The ID is public and is all Milo holds of it.
pub(crate) const OFFICIAL_APPLICATION: u64 = 1_573_574_411_418_599_586;

/// What is public for each choice, said before anyone opts in to it.
macro_rules! public {
    ($application:literal, $names:literal) => {
        concat!(
            "Discord will show ",
            $application,
            " with the line “In a Rex session” while a Rex app has a session open on this computer. Anyone who can see your Discord activity sees it, and so can tell when you are using Rex. ",
            $names
        )
    };
}
macro_rules! public_for {
    ($application:literal) => {
        [
            public!(
                $application,
                "Milo sends nothing else: no session, tab, project or file names, no paths and no commands."
            ),
            public!(
                $application,
                "They will also see the name of the Rex session in front on this computer, as in “Session: release-notes”. A session's name is whatever you or a coding agent called it, and often describes the work. Milo sends no tab titles, project or file names, paths or commands. If Milo cannot tell which session is in front, no name is shown."
            ),
            public!(
                $application,
                "They will also see the folder name of the Git project in the focused pane of the Rex session in front on this computer, as in “Project: milo”. It is the name only: never the path, the remote address or a branch. Milo sends no session or tab names, file names or commands. Outside a Git project, or if Milo cannot tell which session is in front, no name is shown."
            ),
            public!(
                $application,
                "They will also see two names from the Rex session in front on this computer: the session's own name, which is whatever you or a coding agent called it, and the folder name of the Git project in its focused pane, as in “Project: milo · Session: release-notes”. Milo sends no tab titles, file names, paths, remote addresses or commands. A name Milo cannot be sure of is left out."
            ),
        ]
    };
}
const PUBLIC_MILO: [&str; 4] = public_for!("Milo's name and icon");
const PUBLIC_OWN: [&str; 4] = public_for!("the name and icon of your own Discord application");

/// What becomes public with these `details`, under Milo's application or the person's own.
pub(crate) const fn public(details: Details, own_application: bool) -> &'static str {
    if own_application { PUBLIC_OWN[details as usize] } else { PUBLIC_MILO[details as usize] }
}

/// How to show the activity under another name and icon.
pub(crate) const APPLICATION_HELP: &str = "Milo's own Discord application is used unless you give another. To show a name and icon of your choosing, create an application at https://discord.com/developers/applications and copy its Application ID. No bot or token is needed.";
const BAD_APPLICATION: &str = "An application ID is the 17 to 19 digit number Discord calls Application ID.";
const BAD_DETAILS: &str = "Details are one of: generic, session, project, both.";
const UNREADABLE: &str = "Could not read Milo's Discord presence setting, so presence will not start and the file was left alone. milo doctor names the file.";
const UNSAVED: &str = "Could not save Milo's Discord presence setting.";
const REVIEW: &str = "Discord presence was changed elsewhere, so nothing was changed here. Look at what it shows now.";
const SYNTAX: &str = "expected milo presence [status] | enable [APPLICATION_ID | --official] [--details DETAILS] [--yes] | details DETAILS [--yes] | application official|APPLICATION_ID | disable";
pub const HELP: &str = "\
Usage: milo presence [status]
       milo presence enable [APPLICATION_ID | --official] [--details DETAILS] [--yes]
       milo presence details generic | session | project | both [--yes]
       milo presence application official | APPLICATION_ID
       milo presence disable

Show on Discord that you are in a Rex session. It is off until you turn it on.

enable shows the line “In a Rex session” under Milo's Discord application. It says what becomes public and asks first; --yes answers for you.
details chooses what else is shown: generic (nothing, the default), session (the name of the session in front), project (the folder name of the Git project in its focused pane) or both. Showing more asks first.
application switches between Milo's application (official) and one of your own, by its public Application ID. No bot or token is needed.
disable takes the activity down at once and stops Milo's presence helper.";

/// The names a person chose to show under the fixed line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Details {
    /// The fixed line and nothing else.
    #[default]
    Generic,
    Session,
    Project,
    Both,
}

impl Details {
    pub(crate) const ALL: [Self; 4] = [Self::Generic, Self::Session, Self::Project, Self::Both];

    /// The choice's name in the settings file, the command line and the browser editor.
    pub(crate) const fn value(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Session => "session",
            Self::Project => "project",
            Self::Both => "both",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|details| details.value() == value)
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Generic => "Generic",
            Self::Session => "Session name",
            Self::Project => "Project name",
            Self::Both => "Both names",
        }
    }

    /// What the choice shows, to finish “Discord presence shows …”.
    const fn shows(self) -> &'static str {
        match self {
            Self::Generic => "the fixed line and no names",
            Self::Session => "the name of the session in front",
            Self::Project => "the folder name of the Git project in front",
            Self::Both => "the session name and the project's folder name",
        }
    }

    pub(crate) const fn session(self) -> bool {
        matches!(self, Self::Session | Self::Both)
    }

    pub(crate) const fn project(self) -> bool {
        matches!(self, Self::Project | Self::Both)
    }

    /// Whether this choice shows a name that `other` does not, which a person must be asked about.
    pub(crate) const fn shows_more_than(self, other: Self) -> bool {
        (self.session() && !other.session()) || (self.project() && !other.project())
    }

    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Generic => Self::Session,
            Self::Session => Self::Project,
            Self::Project => Self::Both,
            Self::Both => Self::Generic,
        }
    }
}

/// The saved choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Setting {
    pub enabled: bool,
    /// The public ID of the person's own Discord application, if they gave one in place of Milo's.
    pub application: Option<u64>,
    pub details: Details,
}

impl Setting {
    /// A file saved before there were details or an application of Milo's
    /// own reads as it always did: its application kept, and no names shown.
    fn parse(text: &str) -> io::Result<Self> {
        let invalid = || io::Error::from(io::ErrorKind::InvalidData);
        let mut setting = Self::default();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let (key, value) = line.split_once('=').ok_or_else(invalid)?;
            match (key.trim(), value.trim()) {
                ("presence", value) => setting.enabled = value.parse().map_err(|_| invalid())?,
                ("application_id", value) => setting.application = Some(application_id(value).ok_or_else(invalid)?),
                ("details", value) => setting.details = Details::parse(value).ok_or_else(invalid)?,
                // A later release may save more here.
                _ => {}
            }
        }
        Ok(setting)
    }

    fn text(self) -> String {
        let mut text = format!("presence = {}\n", self.enabled);
        if let Some(application) = self.application {
            let _ = writeln!(text, "application_id = {application}");
        }
        if self.details != Details::Generic {
            let _ = writeln!(text, "details = {}", self.details.value());
        }
        text
    }

    /// The application the activity is shown under.
    pub(crate) fn application_id(self) -> u64 {
        self.application.unwrap_or(OFFICIAL_APPLICATION)
    }

    /// What is public with this choice turned on.
    pub(crate) const fn public(self) -> &'static str {
        public(self.details, self.application.is_some())
    }
}

/// A Discord application ID as people copy it: a number of 17 to 19 digits.
pub(crate) fn application_id(text: &str) -> Option<u64> {
    let text = text.trim();
    // A leading zero would not be saved as it was typed.
    let plain = (17..=19).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit()) && !text.starts_with('0');
    plain.then(|| text.parse().ok())?
}

/// The application a person gave as their own. Milo's own ID is still
/// Milo's application, however it was given.
fn own_application(id: Option<u64>) -> Option<u64> {
    id.filter(|id| *id != OFFICIAL_APPLICATION)
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
                "Nothing is shown: Discord does not know this application ID. Check it, or go back to Milo's application."
            }
            Self::Running(Phase::Rejected) => "Nothing is shown: Discord did not accept the activity.",
        }
    }
}

/// The saved choice and what is happening now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub setting: Setting,
    pub state: State,
    /// The second line Discord is showing, when there is one.
    names: Option<String>,
}

impl Status {
    /// What Discord shows now and why, with the names it shows if any.
    pub(crate) fn sentence(&self) -> Cow<'static, str> {
        match (self.state, &self.names) {
            (State::Running(Phase::Showing), Some(names)) => {
                format!("Discord is showing “In a Rex session” with “{names}”.").into()
            }
            (State::Running(Phase::Showing), None) if self.setting.details != Details::Generic => {
                "Discord is showing “In a Rex session” with no name: Milo has none it can be sure of for the session in front.".into()
            }
            (state, _) => state.sentence().into(),
        }
    }
}

/// One change to the saved choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Enable,
    Details(Details),
    /// A person's own application, or `None` for Milo's.
    Application(Option<u64>),
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

    pub(crate) fn status(&self) -> io::Result<Status> {
        let setting = self.setting()?;
        let noted = if !setting.enabled {
            None
        } else if lock::held(&self.state, lock::PRESENCE_LOCK).unwrap_or(false) {
            self.phase()
        } else {
            None
        };
        let state = match noted {
            _ if !setting.enabled => State::Off,
            Some((phase, _)) => State::Running(phase),
            None => State::Stopped,
        };
        Ok(Status { setting, state, names: noted.and_then(|(_, names)| names) })
    }

    /// Saves one change and, while presence is on, starts a helper that
    /// follows it in place of the one running. Returns what to tell the person.
    ///
    /// `reviewed` is the choice the surface had shown when the person asked for the change.
    /// If another surface has saved a different one since, nothing changes: what they were
    /// told would become public is no longer what would.
    pub(crate) fn apply(&self, change: Change, reviewed: Setting) -> Result<String, &'static str> {
        let path = self.file();
        let (saved, setting) = preferences::locked(&path, |dir| {
            let Ok(saved) = self.setting() else { return Ok(Err(UNREADABLE)) };
            if saved != reviewed {
                return Ok(Err(REVIEW));
            }
            let setting = match change {
                Change::Enable => Setting { enabled: true, ..saved },
                Change::Details(details) => Setting { details, ..saved },
                Change::Application(own) => Setting { application: own_application(own), ..saved },
            };
            preferences::replace(dir, &path, &setting.text())?;
            Ok(Ok((saved, setting)))
        })
        .map_err(|_| UNSAVED)??;
        let now: Cow<'static, str> = if !setting.enabled {
            "Presence is off, so nothing is shown.".into()
        } else if setting == saved && change != Change::Enable {
            self.status().map_or(State::Stopped.sentence().into(), |status| status.sentence())
        } else {
            self.restart()
        };
        Ok(match change {
            Change::Enable => format!("Discord presence is on. {now}"),
            Change::Details(details) => format!("Discord presence shows {}. {now}", details.shows()),
            Change::Application(_) => match setting.application {
                Some(own) => format!("Discord presence uses your application {own}. {now}"),
                None => format!("Discord presence uses Milo's application. {now}"),
            },
        })
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

    /// Ends the running helper, which takes its activity down, and starts
    /// one that reads the choice afresh. Waits a moment to say what it found.
    fn restart(&self) -> Cow<'static, str> {
        const STARTING: Duration = Duration::from_millis(1500);
        // A helper showing other names or another application, or one that Discord refused, makes way.
        let _ = lock::stop_holder(&self.state, lock::PRESENCE_LOCK);
        if !self.starts_helper || self.launch(&rex::program()).is_err() {
            return State::Stopped.sentence().into();
        }
        let began = Instant::now();
        loop {
            // The helper notes a phase once Rex has answered it, and ends at once if Rex is not running.
            if let Ok(status) = self.status()
                && matches!(status.state, State::Running(_))
            {
                return status.sentence();
            }
            if began.elapsed() >= STARTING {
                return State::Stopped.sentence().into();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The helper's last note: what it did, and the second line it showed.
    fn phase(&self) -> Option<(Phase, Option<String>)> {
        let note = preferences::read(&self.state.join(PHASE_FILE), LIMIT).ok()??;
        let (word, names) = note.split_once('\n').unwrap_or((&note, ""));
        let phase = Phase::ALL.into_iter().find(|phase| phase.word() == word.trim())?;
        Some((phase, Some(names.trim().to_owned()).filter(|names| !names.is_empty())))
    }

    fn note_phase(&self, phase: Phase, names: Option<&str>) {
        let _ = std::fs::write(self.state.join(PHASE_FILE), format!("{}\n{}", phase.word(), names.unwrap_or_default()));
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
    Enable {
        /// `Some(None)` goes back to Milo's application.
        application: Option<Option<u64>>,
        details: Option<Details>,
        confirmed: bool,
    },
    Details {
        details: Details,
        confirmed: bool,
    },
    Application(Option<u64>),
    Disable,
}

impl Request {
    pub fn parse(action: Option<&OsStr>, rest: &[OsString]) -> Result<Self, String> {
        let words: Option<Vec<&str>> = rest.iter().map(|word| word.to_str()).collect();
        let details = |word: &str| Details::parse(word).ok_or(BAD_DETAILS);
        match (action.map(OsStr::to_str), words.as_deref()) {
            (None | Some(Some("status")), Some([])) => Ok(Self::Status),
            (Some(Some("disable")), Some([])) => Ok(Self::Disable),
            (Some(Some("details")), Some([word])) => Ok(Self::Details { details: details(word)?, confirmed: false }),
            (Some(Some("details")), Some([word, "--yes"] | ["--yes", word])) => {
                Ok(Self::Details { details: details(word)?, confirmed: true })
            }
            (Some(Some("application")), Some(["official"])) => Ok(Self::Application(None)),
            (Some(Some("application")), Some([word])) => {
                Ok(Self::Application(Some(application_id(word).ok_or(BAD_APPLICATION)?)))
            }
            (Some(Some("enable")), Some(words)) => {
                let (mut application, mut chosen, mut confirmed) = (None, None, false);
                let mut words = words.iter();
                while let Some(&word) = words.next() {
                    match word {
                        "--yes" if !confirmed => confirmed = true,
                        "--official" if application.is_none() => application = Some(None),
                        "--details" if chosen.is_none() => chosen = Some(details(words.next().ok_or(BAD_DETAILS)?)?),
                        word if application.is_none() && !word.starts_with('-') => {
                            application = Some(Some(application_id(word).ok_or(BAD_APPLICATION)?));
                        }
                        _ => return Err(SYNTAX.into()),
                    }
                }
                Ok(Self::Enable { application, details: chosen, confirmed })
            }
            _ => Err(SYNTAX.into()),
        }
    }
}

/// Runs `milo presence` and returns its last line.
pub fn command(request: Request) -> Result<String, String> {
    let control = Control::real().ok_or("Could not find Milo's settings folders.")?;
    let saved = || control.setting().map_err(|_| UNREADABLE.to_owned());
    match request {
        Request::Status => {
            let status = control.status().map_err(|_| UNREADABLE)?;
            let setting = status.setting;
            let mut report = format!("Discord presence: {}\n", if setting.enabled { "on" } else { "off" });
            match setting.application {
                Some(own) => writeln!(report, "Application: your own, {own}"),
                None => writeln!(report, "Application: Milo's, {OFFICIAL_APPLICATION}"),
            }
            .expect("writing to a string");
            writeln!(report, "Details: {}, which shows {}", setting.details.value(), setting.details.shows())
                .expect("writing to a string");
            report.push_str(&status.sentence());
            if !setting.enabled {
                write!(report, "\nIf you turn it on: {}", setting.public()).expect("writing to a string");
            }
            Ok(report)
        }
        Request::Enable { application, details, confirmed } => {
            let saved = saved()?;
            let application = application.map(own_application);
            let wanted = Setting {
                enabled: true,
                application: application.unwrap_or(saved.application),
                details: details.unwrap_or(saved.details),
            };
            if !confirmed && (!saved.enabled || wanted.details.shows_more_than(saved.details)) {
                confirm(wanted, "Turn on Discord presence?", "Discord presence was left as it was")?;
            }
            // Nothing is on until every part of the choice is saved, so that no helper starts on half of it.
            let mut reviewed = saved;
            if let Some(application) = application {
                reviewed.application = application;
                control.save(reviewed).map_err(|_| UNSAVED)?;
            }
            if let Some(details) = details {
                reviewed.details = details;
                control.save(reviewed).map_err(|_| UNSAVED)?;
            }
            control.apply(Change::Enable, reviewed).map_err(str::to_owned)
        }
        Request::Details { details, confirmed } => {
            let saved = saved()?;
            if !confirmed && saved.enabled && details.shows_more_than(saved.details) {
                confirm(Setting { details, ..saved }, "Show this on Discord?", "The details were left as they were")?;
            }
            let said = control.apply(Change::Details(details), saved).map_err(str::to_owned)?;
            // Off, the choice is only saved. Say what turning presence on will then make public.
            Ok(if saved.enabled {
                said
            } else {
                format!("{said} If you turn it on: {}", public(details, saved.application.is_some()))
            })
        }
        Request::Application(own) => control.apply(Change::Application(own), saved()?).map_err(str::to_owned),
        Request::Disable => control.disable().map_err(str::to_owned),
    }
}

/// Says what `wanted` makes public and waits for a yes typed at a terminal.
fn confirm(wanted: Setting, question: &str, left: &str) -> Result<(), String> {
    let public = wanted.public();
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return Err(format!("{left}, because nobody could be asked. {public} To go ahead knowing that, add --yes."));
    }
    let mut out = io::stdout().lock();
    write!(out, "{public}\n{question} [y/N] ").and_then(|()| out.flush()).map_err(|e| e.to_string())?;
    let mut answer = String::new();
    stdin.lock().read_line(&mut answer).map_err(|e| e.to_string())?;
    if matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") { Ok(()) } else { Err(format!("{left}.")) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    const OWN: u64 = 1_234_567_890_123_456_789;

    fn control(f: &Fixture) -> Control {
        Control::at(f.root.join("config"), f.root.join("state"))
    }

    fn saved(f: &Fixture) -> String {
        std::fs::read_to_string(f.root.join("config").join(FILE)).unwrap()
    }

    impl Control {
        /// Applies a change as a surface that has just shown what is saved.
        fn change(&self, change: Change) -> Result<String, &'static str> {
            self.apply(change, self.setting().unwrap())
        }
    }

    #[test]
    fn presence_is_off_until_enabled_and_then_uses_milos_application_with_no_names() {
        let f = Fixture::folder();
        let control = control(&f);
        let status = control.status().unwrap();
        assert_eq!((status.setting, status.state), (Setting::default(), State::Off));
        assert_eq!(status.setting.application_id(), 1_573_574_411_418_599_586);
        assert!(!f.root.join("config").exists(), "reading the choice writes nothing");

        assert!(control.change(Change::Enable).unwrap().starts_with("Discord presence is on."));
        assert_eq!(saved(&f), "presence = true\n");
        let on = control.status().unwrap();
        assert_eq!(on.setting, Setting { enabled: true, application: None, details: Details::Generic });
        assert_eq!(on.state, State::Stopped);

        assert!(control.disable().unwrap().starts_with("Discord presence is off."));
        assert_eq!(saved(&f), "presence = false\n");
        assert_eq!(control.status().unwrap().state, State::Off);
    }

    #[test]
    fn a_persons_own_application_and_details_are_kept_until_they_change_them() {
        let f = Fixture::folder();
        let control = control(&f);
        // Choosing while presence is off turns nothing on.
        control.change(Change::Application(Some(OWN))).unwrap();
        let said = control.change(Change::Details(Details::Both)).unwrap();
        assert_eq!(
            said,
            "Discord presence shows the session name and the project's folder name. Presence is off, so nothing is shown."
        );
        assert_eq!(saved(&f), "presence = false\napplication_id = 1234567890123456789\ndetails = both\n");

        control.change(Change::Enable).unwrap();
        control.disable().unwrap();
        control.change(Change::Enable).unwrap();
        let kept = Setting { enabled: true, application: Some(OWN), details: Details::Both };
        assert_eq!(control.setting().unwrap(), kept);
        assert_eq!(kept.application_id(), OWN);

        // Going back to Milo's application and to no names removes them from the file.
        assert!(
            control.change(Change::Application(None)).unwrap().starts_with("Discord presence uses Milo's application.")
        );
        control.change(Change::Details(Details::Generic)).unwrap();
        assert_eq!(saved(&f), "presence = true\n");
        // Milo's own ID is Milo's application however it was given.
        control.change(Change::Application(Some(OFFICIAL_APPLICATION))).unwrap();
        assert_eq!(control.setting().unwrap().application, None);
    }

    #[test]
    fn a_file_from_before_details_keeps_its_application_and_shows_no_names() {
        let f = Fixture::folder();
        let control = control(&f);
        std::fs::create_dir_all(f.root.join("config")).unwrap();
        let file = f.root.join("config").join(FILE);
        for (text, enabled) in [
            ("presence = true\napplication_id = 1234567890123456789\n", true),
            ("presence = false\napplication_id = 1234567890123456789\n", false),
        ] {
            std::fs::write(&file, text).unwrap();
            let setting = control.setting().unwrap();
            assert_eq!(setting, Setting { enabled, application: Some(OWN), details: Details::Generic });
            assert_eq!(setting.public(), public(Details::Generic, true));
            // Saving it again without a change writes the same file.
            control.change(Change::Details(Details::Generic)).unwrap();
            assert_eq!(saved(&f), text);
        }
    }

    #[test]
    fn a_setting_file_nobody_could_have_saved_is_an_error_and_never_turns_presence_on() {
        let f = Fixture::folder();
        let control = control(&f);
        std::fs::create_dir_all(f.root.join("config")).unwrap();
        let file = f.root.join("config").join(FILE);
        for text in [
            "presence = yes\n",
            "presence = true\napplication_id = rex\n",
            "presence = true\ndetails = everything\n",
            "presence\n",
        ] {
            std::fs::write(&file, text).unwrap();
            assert!(control.setting().is_err(), "{text:?}");
            for change in [Change::Enable, Change::Details(Details::Both), Change::Application(None)] {
                assert_eq!(control.apply(change, Setting::default()), Err(UNREADABLE));
            }
            // Turning it off still stops a helper, and says the choice itself could not be saved.
            assert_eq!(control.disable(), Err(UNREADABLE));
            assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
        }
    }

    #[test]
    fn what_becomes_public_names_exactly_what_each_choice_adds() {
        for own in [false, true] {
            let who = if own { "the name and icon of your own Discord application" } else { "Milo's name and icon" };
            for details in Details::ALL {
                let text = public(details, own);
                assert!(text.contains(who) && text.contains(discord::DETAILS), "{text}");
                assert_eq!(
                    text.contains("the name of the Rex session in front") || text.contains("the session's own name"),
                    details.session(),
                    "{text}"
                );
                assert_eq!(text.contains("the folder name of the Git project"), details.project(), "{text}");
            }
            assert!(public(Details::Generic, own).contains("Milo sends nothing else"));
        }
        // Only a choice that adds a name needs asking about again.
        assert!(Details::Session.shows_more_than(Details::Generic));
        assert!(Details::Project.shows_more_than(Details::Session));
        assert!(Details::Both.shows_more_than(Details::Project));
        assert!(!Details::Session.shows_more_than(Details::Both));
        assert!(!Details::Generic.shows_more_than(Details::Both));
        assert!(!Details::Both.shows_more_than(Details::Both));
    }

    #[test]
    fn status_repeats_the_names_only_while_they_are_shown() {
        let f = Fixture::folder();
        let control = control(&f);
        std::fs::create_dir_all(f.root.join("state")).unwrap();
        control.note_phase(Phase::Showing, Some("Project: milo"));
        assert_eq!(control.phase(), Some((Phase::Showing, Some("Project: milo".into()))));
        control.note_phase(Phase::Idle, None);
        assert_eq!(control.phase(), Some((Phase::Idle, None)));
        let showing = |names: Option<&str>, details| Status {
            setting: Setting { enabled: true, application: None, details },
            state: State::Running(Phase::Showing),
            names: names.map(str::to_owned),
        };
        assert_eq!(showing(None, Details::Generic).sentence(), "Discord is showing “In a Rex session”.");
        assert_eq!(
            showing(Some("Project: milo"), Details::Project).sentence(),
            "Discord is showing “In a Rex session” with “Project: milo”."
        );
        assert!(showing(None, Details::Both).sentence().contains("with no name"));
    }

    #[test]
    fn application_ids_are_whole_numbers_of_the_length_discord_gives() {
        assert_eq!(application_id(" 12345678901234567 "), Some(12_345_678_901_234_567));
        assert_eq!(application_id("1234567890123456789"), Some(OWN));
        assert_eq!(application_id("1573574411418599586"), Some(OFFICIAL_APPLICATION));
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
    fn the_presence_command_reads_each_choice_once() {
        let parse = |action: Option<&str>, rest: &[&str]| {
            Request::parse(action.map(OsStr::new), &rest.iter().map(OsString::from).collect::<Vec<_>>())
        };
        assert_eq!(parse(None, &[]), Ok(Request::Status));
        assert_eq!(parse(Some("status"), &[]), Ok(Request::Status));
        assert_eq!(parse(Some("disable"), &[]), Ok(Request::Disable));
        assert_eq!(
            parse(Some("enable"), &[]),
            Ok(Request::Enable { application: None, details: None, confirmed: false })
        );
        assert_eq!(
            parse(Some("enable"), &["--yes", "12345678901234567", "--details", "project"]),
            Ok(Request::Enable {
                application: Some(Some(12_345_678_901_234_567)),
                details: Some(Details::Project),
                confirmed: true
            })
        );
        assert_eq!(
            parse(Some("enable"), &["--official"]),
            Ok(Request::Enable { application: Some(None), details: None, confirmed: false })
        );
        assert_eq!(
            parse(Some("details"), &["both"]),
            Ok(Request::Details { details: Details::Both, confirmed: false })
        );
        assert_eq!(
            parse(Some("details"), &["generic", "--yes"]),
            Ok(Request::Details { details: Details::Generic, confirmed: true })
        );
        assert_eq!(parse(Some("application"), &["official"]), Ok(Request::Application(None)));
        assert_eq!(parse(Some("application"), &["1234567890123456789"]), Ok(Request::Application(Some(OWN))));
        assert_eq!(parse(Some("enable"), &["rex"]), Err(BAD_APPLICATION.to_owned()));
        assert_eq!(parse(Some("application"), &["milo"]), Err(BAD_APPLICATION.to_owned()));
        assert_eq!(parse(Some("details"), &["titles"]), Err(BAD_DETAILS.to_owned()));
        assert_eq!(parse(Some("enable"), &["--details"]), Err(BAD_DETAILS.to_owned()));
        for (action, rest) in [
            (Some("enable"), &["12345678901234567", "12345678901234568"][..]),
            (Some("enable"), &["12345678901234567", "--official"]),
            (Some("enable"), &["--yes", "--yes"]),
            (Some("enable"), &["--details", "both", "--details", "session"]),
            (Some("enable"), &["--token"]),
            (Some("details"), &[]),
            (Some("application"), &[]),
            (Some("disable"), &["now"]),
            (Some("toggle"), &[]),
        ] {
            assert!(parse(action, rest).is_err(), "{action:?} {rest:?}");
        }
    }
}
