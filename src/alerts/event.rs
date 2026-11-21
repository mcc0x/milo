//! Reads Rex's event stream and decides which events are alerts worth sending.
//!
//! `rex events --all --json` prints one JSON object per line. A terminal's
//! events arrive wrapped twice, in a `session_event` and a `block_event`.
//! Events carry no number of their own, and Rex repeats a status each time a
//! program reports it, so `Seen` remembers what each terminal last said.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::json::Value;

const TERMINAL: &str = "com.superlogical.terminal";
/// A bell or a repeated notification from one terminal counts once in this long.
const QUIET: Duration = Duration::from_secs(30);
/// More remembered terminals than any server holds. Past it, start again.
const REMEMBERED: usize = 4096;
/// Rex's own limits are 2048 bytes for a message and 192 for a title.
const TEXT_LIMIT: usize = 4096;

/// A kind of alert the user can choose to mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A program is waiting for the user.
    Blocked,
    /// A program reported that it failed.
    Error,
    /// A program reported that it finished.
    Done,
    /// A program asked for a desktop notification.
    Notification,
    /// A program rang the terminal bell.
    Bell,
}

impl Kind {
    pub(crate) const ALL: [Self; 5] = [Self::Blocked, Self::Error, Self::Done, Self::Notification, Self::Bell];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Error => "error",
            Self::Done => "done",
            Self::Notification => "notification",
            Self::Bell => "bell",
        }
    }

    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// What the kind means, for the settings pages.
    pub(crate) fn meaning(self) -> &'static str {
        match self {
            Self::Blocked => "a program is waiting for you",
            Self::Error => "a program reported an error",
            Self::Done => "a program finished",
            Self::Notification => "a program asked for a desktop notification",
            Self::Bell => "a terminal rang its bell",
        }
    }
}

/// What a blocked program is waiting for, when it said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Waiting {
    Permission,
    Question,
    Auth,
}

/// What a program says it is doing. Only the last three are alerts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Quiet,
    Blocked(Option<Waiting>),
    Error,
    Done,
}

/// One event of the kinds Milo acts on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Status { block: String, record: String, state: State, app: Option<String>, text: String },
    StatusRemoved { block: String, records: Vec<String> },
    Notification { block: String, text: String },
    Bell { block: String },
    BlockClosed { block: String },
}

/// An alert ready to be worded. `text` is what the program gave Rex.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Alert {
    pub(crate) kind: Kind,
    pub(crate) waiting: Option<Waiting>,
    pub(crate) app: Option<String>,
    pub(crate) text: String,
}

/// Reads one line of the stream. None for everything Milo has no use for.
pub(crate) fn parse(line: &[u8]) -> Option<Event> {
    let [kind, payload] = Value::parse(line)?.fields(["type", "payload"]);
    if !kind?.is("session_event") {
        return None;
    }
    let [event] = payload?.fields(["event"]);
    let [kind, payload] = event?.fields(["type", "payload"]);
    let (kind, payload) = (kind?, payload?);
    let [block, creator, name, inner] = payload.fields(["block_id", "creator_name", "name", "payload"]);
    let block = block?.string(64)?.into_owned();
    if kind.is("block_closed") {
        return Some(Event::BlockClosed { block });
    }
    if !kind.is("block_event") || !creator?.is(TERMINAL) {
        return None;
    }
    let inner = inner?;
    match name?.string(32)?.as_ref() {
        "bell" => Some(Event::Bell { block }),
        "desktop_notification" => {
            let [title, body] = inner.fields(["title", "body"]).map(text);
            Some(Event::Notification { block, text: join(&title, &body) })
        }
        "program_status_changed" => status(block, inner),
        "program_status_removed" => {
            let [ids] = inner.fields(["ids"]);
            let records = ids?.elements()?.filter_map(|id| Some(id.string(128)?.into_owned())).collect();
            Some(Event::StatusRemoved { block, records })
        }
        _ => None,
    }
}

fn status(block: String, payload: Value<'_>) -> Option<Event> {
    let [id, record] = payload.fields(["id", "record"]);
    let [state, kind, app, effective, title, msg] =
        record?.fields(["state", "kind", "app", "effective_app", "title", "msg"]);
    let waiting = kind.and_then(|kind| match kind.string(16)?.as_ref() {
        "permission" => Some(Waiting::Permission),
        "question" => Some(Waiting::Question),
        "auth" => Some(Waiting::Auth),
        _ => None,
    });
    // Rex asks that a state nobody knows be treated as idle.
    let state = match state?.string(16)?.as_ref() {
        "blocked" => State::Blocked(waiting),
        "error" => State::Error,
        "done" => State::Done,
        _ => State::Quiet,
    };
    let app = effective.or(app).and_then(|app| Some(app.string(32)?.into_owned())).filter(|app| short_name(app));
    Some(Event::Status {
        block,
        record: id?.string(128)?.into_owned(),
        state,
        app,
        text: join(&text(title), &text(msg)),
    })
}

/// The characters Rex's status protocol allows in a program's short name.
fn short_name(app: &str) -> bool {
    !app.is_empty() && app.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'+' | b'-'))
}

fn text(value: Option<Value<'_>>) -> String {
    value.and_then(|value| value.string(TEXT_LIMIT)).map(std::borrow::Cow::into_owned).unwrap_or_default()
}

fn join(title: &str, body: &str) -> String {
    match (title.is_empty(), body.is_empty()) {
        (false, false) => format!("{title}: {body}"),
        _ => format!("{title}{body}"),
    }
}

/// What each terminal last said, so that one alert is sent once.
#[derive(Default)]
pub(crate) struct Seen {
    /// By terminal and by the record's ID within it.
    states: HashMap<(String, String), State>,
    rang: HashMap<String, Instant>,
    /// The last notification's text, hashed, and when it came.
    notified: HashMap<String, (u64, Instant)>,
}

impl Seen {
    /// The alert this event amounts to, if it is one and is not a repeat.
    ///
    /// A status is an alert when a record enters a blocked, failed or
    /// finished state, or changes what it is waiting for. Rex sends the event
    /// again whenever the program repeats itself, which is not a new alert.
    pub(crate) fn alert(&mut self, event: Event, now: Instant) -> Option<Alert> {
        if self.states.len() + self.rang.len() + self.notified.len() > REMEMBERED {
            self.clear();
        }
        match event {
            Event::Status { block, record, state, app, text } => {
                let before = self.states.insert((block, record), state);
                let (kind, waiting) = match state {
                    State::Quiet => return None,
                    State::Blocked(waiting) => (Kind::Blocked, waiting),
                    State::Error => (Kind::Error, None),
                    State::Done => (Kind::Done, None),
                };
                (before != Some(state)).then_some(Alert { kind, waiting, app, text })
            }
            Event::StatusRemoved { block, records } => {
                let mut key = (block, String::new());
                for record in records {
                    key.1 = record;
                    self.states.remove(&key);
                }
                None
            }
            Event::Notification { block, text } => {
                let hash = fnv(text.as_bytes());
                let repeat =
                    self.notified.get(&block).is_some_and(|&(old, at)| old == hash && now.duration_since(at) < QUIET);
                if repeat {
                    return None;
                }
                self.notified.insert(block, (hash, now));
                Some(Alert { kind: Kind::Notification, waiting: None, app: None, text })
            }
            Event::Bell { block } => {
                if self.rang.get(&block).is_some_and(|&at| now.duration_since(at) < QUIET) {
                    return None;
                }
                self.rang.insert(block, now);
                Some(Alert { kind: Kind::Bell, waiting: None, app: None, text: String::new() })
            }
            Event::BlockClosed { block } => {
                self.states.retain(|(owner, _), _| *owner != block);
                self.rang.remove(&block);
                self.notified.remove(&block);
                None
            }
        }
    }

    fn clear(&mut self) {
        self.states.clear();
        self.rang.clear();
        self.notified.clear();
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}
