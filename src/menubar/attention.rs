//! The menu's optional **Needs you** section: its saved choice, and the commands the menu app runs for it.
//!
//! *Attention* is a Rex terminal whose program reports that it waits for the
//! person, failed, or finished. The choice is off until someone turns it on,
//! and off means no process, no Rex call and no timer. It is separate from
//! alert mirroring and from low-quota notifications, and needs neither.

mod board;
mod watch;

use std::io;
use std::path::Path;

use super::{FILE_LIMIT, Indicator, save_value};
use crate::{paths, preferences, rex};

pub(crate) use board::Need;

/// Where the panes that need the person are shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Show {
    /// Nothing is watched.
    #[default]
    Off,
    /// Listed in the menu when it is opened.
    Menu,
    /// Listed in the menu, with a dot on the menu bar item while any is listed.
    Dot,
}

impl Show {
    pub(crate) const fn value(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Menu => "menu",
            Self::Dot => "dot",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Off, Self::Menu, Self::Dot].into_iter().find(|show| show.value() == value)
    }
}

/// Which reported states are listed. Each is chosen on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct States {
    pub(crate) waiting: bool,
    pub(crate) errors: bool,
    pub(crate) done: bool,
}

impl Default for States {
    /// Finished work is the common case and asks for nothing, so it starts left out.
    fn default() -> Self {
        Self { waiting: true, errors: true, done: false }
    }
}

impl States {
    const NAMES: [&'static str; 3] = ["waiting", "errors", "done"];

    fn flags(self) -> [bool; 3] {
        [self.waiting, self.errors, self.done]
    }

    pub(crate) fn names(self) -> impl Iterator<Item = &'static str> {
        Self::NAMES.into_iter().zip(self.flags()).filter_map(|(name, chosen)| chosen.then_some(name))
    }

    pub(crate) fn value(self) -> String {
        self.names().collect::<Vec<_>>().join(",")
    }

    /// A list such as `waiting,errors`. At least one state, each named once.
    pub(crate) fn parse(list: &str) -> Option<Self> {
        let mut chosen = [false; 3];
        for name in list.split(',') {
            let index = Self::NAMES.iter().position(|known| *known == name.trim())?;
            if std::mem::replace(&mut chosen[index], true) {
                return None;
            }
        }
        let [waiting, errors, done] = chosen;
        Some(Self { waiting, errors, done })
    }

    pub(crate) fn include(self, need: Need) -> bool {
        match need {
            Need::Waiting => self.waiting,
            Need::Error => self.errors,
            Need::Done => self.done,
        }
    }
}

/// The saved choice for the section.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Attention {
    pub(crate) show: Show,
    pub(crate) states: States,
}

impl Attention {
    /// What is saved, or the default when nothing is. A value this build does not know is an error,
    /// which callers treat as off: an unreadable choice must never start a watch.
    pub(crate) fn load(file: &Path) -> io::Result<Self> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid menu attention choice");
        let Some(text) = preferences::read(file, FILE_LIMIT)? else { return Ok(Self::default()) };
        let mut choice = Self::default();
        if let Some(value) = preferences::layer_value(&text, "attention") {
            choice.show = Show::parse(value).ok_or_else(invalid)?;
        }
        if let Some(value) = preferences::layer_value(&text, "attention_states") {
            choice.states = States::parse(value).ok_or_else(invalid)?;
        }
        Ok(choice)
    }

    fn describe(self) -> String {
        let states = self.states.value().replace(',', ", ");
        match self.show {
            Show::Off => format!("Needs you is off. Milo is not watching Rex for it.\nWhen on, it lists: {states}."),
            Show::Menu => format!("Needs you is on, in the menu only.\nIt lists: {states}."),
            Show::Dot => format!("Needs you is on, with a dot in the menu bar.\nIt lists: {states}."),
        }
    }
}

/// What `milo menubar attention` was asked to do.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Status,
    Show(Show),
    States(States),
    /// Streams the section's rows to the menu app until its input closes.
    Watch,
    /// Shows one pane in Rex, after checking it is still the pane that was listed.
    Jump {
        host: String,
        block: String,
    },
}

pub(crate) const USAGE: &str = "expected milo menubar attention [off | menu | dot | states waiting,errors,done]";

impl Command {
    pub(crate) fn parse(args: &[impl AsRef<std::ffi::OsStr>]) -> Option<Self> {
        let words: Vec<&str> = args.iter().map(|arg| arg.as_ref().to_str()).collect::<Option<_>>()?;
        Some(match words[..] {
            [] => Self::Status,
            ["watch"] => Self::Watch,
            ["states", list] => Self::States(States::parse(list)?),
            ["jump", host, block] => Self::Jump { host: host.to_owned(), block: block.to_owned() },
            [show] => Self::Show(Show::parse(show)?),
            _ => return None,
        })
    }
}

pub(crate) fn run(command: Command) -> Result<(), String> {
    let config = paths::config_dir().ok_or("Milo's configuration directory is unavailable")?;
    let file = Indicator::file(&config);
    let saved = |result: io::Result<()>| result.map_err(|error| format!("Could not save the menu's choice: {error}"));
    let report = |text: &str| crate::print_line(text);
    match command {
        Command::Status => {
            let choice =
                Attention::load(&file).map_err(|error| format!("Could not read the menu's choice: {error}"))?;
            report(&choice.describe())
        }
        Command::Show(show) => {
            saved(save_value(&config, "attention", |_| Ok(show.value().to_owned())))?;
            report(&Attention { show, states: Attention::load(&file).unwrap_or_default().states }.describe())
        }
        Command::States(states) => {
            saved(save_value(&config, "attention_states", |_| Ok(states.value())))?;
            report(&Attention { show: Attention::load(&file).unwrap_or_default().show, states }.describe())
        }
        Command::Watch => watch::run(&file).map_err(|error| format!("The watch on Rex ended: {error}")),
        Command::Jump { host, block } => report(&rex::jump(&host, &block).report()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn the_choice_starts_off_and_keeps_the_indicator_beside_it() {
        let folder = Fixture::folder();
        let config = folder.root.clone();
        let file = Indicator::file(&config);
        assert_eq!(Attention::load(&file).unwrap(), Attention::default());
        assert_eq!(Attention::default().show, Show::Off);
        assert_eq!(Attention::default().states.value(), "waiting,errors");

        Indicator::Cpu.save(&config).unwrap();
        save_value(&config, "attention", |_| Ok("dot".into())).unwrap();
        save_value(&config, "attention_states", |_| Ok("done,waiting".into())).unwrap();
        Indicator::Memory.save(&config).unwrap();
        save_value(&config, "attention", |_| Ok("menu".into())).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "attention_states = done,waiting\nindicator = memory\nattention = menu\n"
        );
        assert_eq!(Indicator::load(&file).unwrap(), Indicator::Memory);
        let choice = Attention::load(&file).unwrap();
        assert_eq!(choice.show, Show::Menu);
        assert_eq!(choice.states, States { waiting: true, errors: false, done: true });
    }

    #[test]
    fn a_choice_this_build_does_not_know_is_an_error_and_never_a_watch() {
        let folder = Fixture::folder();
        let file = Indicator::file(&folder.root);
        for text in
            ["attention = always\n", "attention = dot\nattention_states = \n", "attention_states = waiting,waiting\n"]
        {
            std::fs::write(&file, text).unwrap();
            assert_eq!(Attention::load(&file).unwrap_err().kind(), io::ErrorKind::InvalidData, "{text}");
            // The indicator beside it is still read.
            assert_eq!(Indicator::load(&file).unwrap(), Indicator::Quota);
        }
        assert_eq!(States::parse("errors"), Some(States { waiting: false, errors: true, done: false }));
        assert_eq!(States::parse("waiting, done").map(States::value).as_deref(), Some("waiting,done"));
        assert_eq!(States::parse("bell"), None);
    }

    #[test]
    fn commands_take_whole_words_only() {
        let parse = |words: &[&str]| Command::parse(&words.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>());
        assert_eq!(parse(&[]), Some(Command::Status));
        assert_eq!(parse(&["dot"]), Some(Command::Show(Show::Dot)));
        assert_eq!(
            parse(&["states", "done"]),
            Some(Command::States(States { waiting: false, errors: false, done: true }))
        );
        assert_eq!(parse(&["watch"]), Some(Command::Watch));
        for refused in [&["on"][..], &["states"], &["states", ""], &["dot", "menu"], &["jump", "host"]] {
            assert_eq!(parse(refused), None, "{refused:?}");
        }
    }
}
