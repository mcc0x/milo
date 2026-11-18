//! Shows the limits the tools report, or Claude Code's local activity.

mod draw;
mod files;
mod json;
mod status;
mod time;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::help::{self, Page};
use crate::keys::Key;
use crate::screen::Screen;
use crate::ui::Pane;
use crate::watch::Scope;
use crate::{paths, when};

use self::files::{Budget, Mark, Sessions, Stream, Tree};
use self::json::Value;
use self::time::{DAY, HOUR, MINUTE, Stamp, WEEK};

const FIVE_HOURS: i64 = 5 * HOUR;
const CLAUDE_FRESH: i64 = DAY;
const AGE_NOTE: i64 = 5 * MINUTE;
const TAIL_BYTES: u64 = 4 * 1024 * 1024;
const CODEX_BYTES: u64 = 16 * 1024 * 1024;
const CLAUDE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SAMPLES: usize = 4096;
const SAVED_LIMITS: &str = "claude-limits.json";

#[derive(Clone, Copy)]
struct Window {
    minutes: u64,
    percent: f64,
    resets: Option<i64>,
}

impl Window {
    fn percent(value: Value<'_>) -> Option<f64> {
        value.number().filter(|n| (0.0..=100.0).contains(n)).map(|n| n.abs())
    }

    fn number(self) -> String {
        // Rounding 99.9% to 100% would suggest there is nothing left.
        if self.percent > 99.0 && self.percent < 100.0 {
            "99+%".into()
        } else if self.percent > 0.0 && self.percent < 1.0 {
            "<1%".into()
        } else {
            format!("{:.0}%", self.percent)
        }
    }

    fn is_reset(self, now: i64) -> bool {
        self.resets.is_some_and(|reset| reset <= now)
    }

    fn codex(value: Option<Value<'_>>) -> Option<Self> {
        let [percent, minutes, resets] = value?.fields(["used_percent", "window_minutes", "resets_at"]);
        Some(Self {
            minutes: minutes?.unsigned().filter(|n| *n > 0)?,
            percent: Self::percent(percent?)?,
            resets: resets.and_then(time::reset),
        })
    }
}

#[derive(Clone, Copy)]
struct Snapshot {
    at: Stamp,
    windows: [Option<Window>; 2],
}

impl Snapshot {
    fn new(at: Stamp, windows: [Option<Window>; 2]) -> Option<Self> {
        windows.iter().any(Option::is_some).then_some(Self { at, windows })
    }

    fn codex(line: &[u8], now: i64) -> Option<Self> {
        if !files::has_marker(line, b"token_count") {
            return None;
        }
        let [kind, at, payload] = Value::parse(line)?.fields(["type", "timestamp", "payload"]);
        kind?.is("event_msg").then_some(())?;
        let at = Stamp::iso(at?)?;
        (at.seconds <= now).then_some(())?;
        let [kind, limits] = payload?.fields(["type", "rate_limits"]);
        kind?.is("token_count").then_some(())?;
        let [primary, secondary] = limits?.fields(["primary", "secondary"]);
        Self::new(at, [Window::codex(primary), Window::codex(secondary)])
    }

    fn fresh(self, now: i64) -> bool {
        (0..CLAUDE_FRESH).contains(&now.saturating_sub(self.at.seconds))
    }
}

struct CodexFile {
    mark: Mark,
    snapshot: Option<Snapshot>,
    limited: bool,
}

#[derive(Default)]
struct Codex {
    files: HashMap<PathBuf, CodexFile>,
    limited: bool,
}

impl Codex {
    fn check(&mut self, root: Option<&Path>, now: i64, budget: &mut Budget) -> bool {
        let Some(sessions) = root.and_then(|p| Sessions::scan(p, Tree::Codex, now.saturating_sub(WEEK))) else {
            *self = Self::default();
            return true;
        };
        let mut previous = std::mem::take(&mut self.files);
        self.limited = sessions.limited;
        let mut settled = true;
        for candidate in &sessions.files {
            let cached = previous.remove(&candidate.path);
            let (file, done) = Self::check_file(&sessions, candidate, cached, now, budget);
            settled &= done;
            if let Some(file) = file {
                self.limited |= file.limited;
                self.files.insert(candidate.path.clone(), file);
            }
        }
        self.limited |= !settled;
        settled
    }

    fn check_file(
        sessions: &Sessions,
        candidate: &files::Candidate,
        cached: Option<CodexFile>,
        now: i64,
        budget: &mut Budget,
    ) -> (Option<CodexFile>, bool) {
        if cached.as_ref().is_some_and(|file| file.mark == candidate.mark) {
            return (cached, true);
        }
        if candidate.mark.len.min(TAIL_BYTES) > budget.remaining {
            return (cached, false);
        }
        let Some((file, mark)) = sessions.open(&candidate.path) else { return (None, true) };
        let Ok((mut snapshot, limited)) =
            files::tail(&file, mark.len, TAIL_BYTES, budget, |line| Snapshot::codex(line, now))
        else {
            return (None, true);
        };
        if limited && let Some(old) = cached.filter(|file| mark.same_file(file.mark) && mark.len > file.mark.len) {
            snapshot = snapshot.or(old.snapshot);
        }
        (Some(CodexFile { mark, snapshot, limited }), true)
    }

    fn snapshot(&self) -> Option<Snapshot> {
        self.files.values().filter_map(|file| file.snapshot).max_by_key(|snapshot| snapshot.at)
    }
}

#[derive(Clone, Copy)]
struct Sample {
    at: i64,
    tokens: u64,
}

impl Sample {
    fn parse(line: &[u8]) -> Option<(Self, Option<std::borrow::Cow<'_, str>>)> {
        if !files::has_marker(line, b"usage") {
            return None;
        }
        let [kind, at, message] = Value::parse(line)?.fields(["type", "timestamp", "message"]);
        kind?.is("assistant").then_some(())?;
        let at = Stamp::iso(at?)?.seconds;
        let [usage, id] = message?.fields(["usage", "id"]);
        let counts = usage?.fields(["input_tokens", "output_tokens", "cache_creation_input_tokens"]);
        let mut counts = counts.into_iter().flatten().filter_map(Value::unsigned);
        let first = counts.next()?;
        let tokens = counts.fold(first, u64::saturating_add);
        Some((Self { at, tokens }, id.and_then(|v| v.string(128))))
    }
}

#[derive(Default)]
struct Samples {
    identified: HashMap<Box<str>, Sample>,
    anonymous: Vec<Sample>,
    incomplete_since: Option<i64>,
}

impl Samples {
    fn add(&mut self, sample: Sample, id: Option<&str>, cutoff: i64) {
        if let Some(previous) = id.and_then(|id| self.identified.get_mut(id)) {
            previous.at = previous.at.min(sample.at);
            previous.tokens = sample.tokens;
        } else if sample.at >= cutoff {
            if self.identified.len() + self.anonymous.len() >= MAX_SAMPLES {
                self.incomplete_since = Some(self.incomplete_since.map_or(sample.at, |d| d.max(sample.at)));
            } else if let Some(id) = id {
                self.identified.insert(id.into(), sample);
            } else {
                self.anonymous.push(sample);
            }
        }
    }

    fn expire(&mut self, cutoff: i64) {
        self.identified.retain(|_, s| s.at >= cutoff);
        self.anonymous.retain(|s| s.at >= cutoff);
        self.incomplete_since = self.incomplete_since.filter(|&at| at >= cutoff);
    }
}

struct ClaudeFile {
    mark: Mark,
    stream: Stream,
    samples: Samples,
    pending: Option<Sample>,
}

impl ClaudeFile {
    fn check(&mut self, sessions: &Sessions, candidate: &files::Candidate, cutoff: i64, budget: &mut Budget) -> bool {
        if self.mark == candidate.mark && self.stream.offset == self.mark.len {
            return true;
        }
        if budget.remaining == 0 {
            return false;
        }
        let Some((file, mark)) = sessions.open(&candidate.path) else {
            *self = Self::new(candidate.mark);
            return true;
        };
        if !mark.same_file(self.mark) || mark.len < self.mark.len {
            *self = Self::new(mark);
        }
        self.mark = mark;
        self.samples.expire(cutoff);
        let loaded = self.stream.read(&file, mark.len, budget, |line| {
            if let Some((sample, id)) = Sample::parse(line) {
                self.samples.add(sample, id.as_deref(), cutoff);
            }
        });
        self.pending = if self.stream.offset == mark.len {
            Sample::parse(self.stream.tail()).and_then(|(sample, id)| {
                let repeated = id.is_some_and(|id| self.samples.identified.contains_key(id.as_ref()));
                (!repeated).then_some(sample)
            })
        } else {
            None
        };
        loaded.is_err() || self.stream.offset == mark.len
    }

    fn new(mark: Mark) -> Self {
        Self { mark, stream: Stream::default(), samples: Samples::default(), pending: None }
    }

    fn all(&self) -> impl Iterator<Item = &Sample> {
        self.samples.identified.values().chain(self.samples.anonymous.iter()).chain(self.pending.iter())
    }
}

#[derive(Default)]
struct Claude {
    files: HashMap<PathBuf, ClaudeFile>,
    limited: bool,
    saved_mark: Option<Mark>,
    saved: Option<Snapshot>,
}

#[derive(Default)]
struct Activity {
    five: u64,
    today: u64,
    present: bool,
    partial: bool,
}

impl Claude {
    fn check(&mut self, root: Option<&Path>, cutoff: i64, budget: &mut Budget) -> bool {
        let Some(sessions) = root.and_then(|p| Sessions::scan(p, Tree::Claude, cutoff)) else {
            self.files.clear();
            self.limited = false;
            return true;
        };
        let mut previous = std::mem::take(&mut self.files);
        self.limited = sessions.limited;
        let mut settled = true;
        for candidate in &sessions.files {
            let mut file = previous.remove(&candidate.path).unwrap_or_else(|| ClaudeFile::new(candidate.mark));
            settled &= file.check(&sessions, candidate, cutoff, budget);
            self.limited |= file.stream.offset < file.mark.len;
            self.files.insert(candidate.path.clone(), file);
        }
        settled
    }

    fn activity(&self, today: i64, five: i64, now: i64) -> Activity {
        let mut activity = Activity { partial: self.limited, ..Activity::default() };
        for file in self.files.values() {
            activity.partial |= file.samples.incomplete_since.is_some_and(|at| at >= today.min(five));
            for sample in file.all().filter(|s| s.at >= today.min(five) && s.at <= now) {
                activity.present = true;
                if sample.at >= five {
                    activity.five = activity.five.saturating_add(sample.tokens);
                }
                if sample.at >= today {
                    activity.today = activity.today.saturating_add(sample.tokens);
                }
            }
        }
        activity
    }
}

struct Today {
    day: i64,
    since: i64,
}

impl Today {
    fn at(now: i64, day: i64) -> Self {
        let (mut low, mut high) = (now.saturating_sub(2 * DAY), now);
        while low < high {
            let middle = low + (high - low) / 2;
            if when::local_day(middle) < day {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Self { day, since: high }
    }

    fn since(&mut self, now: i64) -> i64 {
        let day = when::local_day(now);
        if self.day != day {
            *self = Self::at(now, day);
        }
        self.since
    }
}

/// Uses the installed providers' directories; tests supply their own directories and time.
pub struct Usage {
    codex_dir: Option<PathBuf>,
    claude_dir: Option<PathBuf>,
    limits_file: Option<PathBuf>,
    codex: Codex,
    claude: Claude,
    today: Today,
    fixed_now: Option<i64>,
    #[cfg(test)]
    bytes_read: u64,
}

impl Usage {
    pub fn open() -> Self {
        let home = paths::home();
        let codex = home.as_ref().map(|p| p.join(".codex/sessions"));
        let claude = home.as_ref().map(|p| p.join(".claude/projects"));
        let state = paths::state_dir();
        let mut usage = Self::from_dirs(codex, claude, state, when::now());
        usage.fixed_now = None;
        usage
    }

    fn from_dirs(codex_dir: Option<PathBuf>, claude_dir: Option<PathBuf>, state: Option<PathBuf>, now: i64) -> Self {
        let mut usage = Self {
            codex_dir,
            claude_dir,
            limits_file: state.map(|dir| dir.join(SAVED_LIMITS)),
            codex: Codex::default(),
            claude: Claude::default(),
            today: Today::at(now, when::local_day(now)),
            fixed_now: Some(now),
            #[cfg(test)]
            bytes_read: 0,
        };
        usage.check();
        usage
    }

    fn now(&self) -> i64 {
        self.fixed_now.unwrap_or_else(when::now)
    }

    fn refresh(&mut self, now: i64) -> bool {
        let cutoff = self.today.since(now).min(now.saturating_sub(FIVE_HOURS));
        let mut codex_budget = Budget::new(CODEX_BYTES);
        let mut claude_budget = Budget::new(CLAUDE_BYTES);
        if let Some(path) = &self.limits_file {
            status::read(path, &mut self.claude.saved_mark, &mut self.claude.saved, &mut claude_budget);
        }
        let codex = self.codex.check(self.codex_dir.as_deref(), now, &mut codex_budget);
        let claude = self.claude.check(self.claude_dir.as_deref(), cutoff, &mut claude_budget);
        #[cfg(test)]
        {
            self.bytes_read = codex_budget.read + claude_budget.read;
        }
        codex && claude
    }
}

impl Pane for Usage {
    fn check(&mut self) -> bool {
        self.refresh(self.now())
    }

    fn draw(&mut self, screen: &mut Screen) {
        let now = self.now();
        let today = self.today.since(now);
        draw::usage(screen, self, today, now);
    }

    fn watch_scope(&self) -> Scope {
        let paths =
            [self.codex_dir.as_deref(), self.claude_dir.as_deref(), self.limits_file.as_deref().and_then(Path::parent)];
        let mut roots = Vec::new();
        let mut complete = true;
        for path in paths.into_iter().flatten() {
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
                roots.push(path.to_owned());
            } else {
                complete = false;
            }
        }
        Scope { roots, ignored: Vec::new(), complete }
    }

    fn key(&mut self, key: Key) -> bool {
        if key == Key::Char('r') {
            self.check();
        }
        false
    }

    fn next_frame(&self) -> Option<Duration> {
        let now = self.now();
        let earliest = self
            .codex
            .snapshot()
            .into_iter()
            .chain(self.claude.saved.filter(|s| s.fresh(now)))
            .flat_map(|s| s.windows.into_iter().flatten().filter_map(|w| w.resets))
            .chain(self.claude.saved.map(|s| s.at.seconds.saturating_add(CLAUDE_FRESH)))
            .filter(|&reset| reset > now)
            .min();
        let wait = earliest.map_or(MINUTE, |ts| ts.saturating_sub(now).min(MINUTE));
        Some(Duration::from_secs(u64::try_from(wait).unwrap_or(60)))
    }

    fn help(&self) -> Page {
        use help::{Row, heading, key, note};
        Page::new(vec![
            heading("Usage limits"),
            note("Codex reads its sessions."),
            note("Claude Code gets limits"),
            note("from its status line."),
            note("Bars show the share of"),
            note("each limit used. A reset"),
            note("means its figure expired."),
            note("As of: last limit report."),
            Row::new(),
            heading("Claude Code activity"),
            note("After a day, shows local"),
            note("transcript activity."),
            note("Input and output tokens,"),
            note("including cache writes,"),
            note("not cache reads. Today"),
            note("starts at local midnight."),
            Row::new(),
            note("From sessions on this"),
            note("machine, as fresh as the"),
            note("last message. Turn on"),
            note("limits in milo setup."),
            key("r", "reload"),
            key("q", "quit"),
        ])
    }
}

/// Claude Code must get a line and a successful exit even if it sent bad data.
pub fn claude_status() -> i32 {
    let path = paths::state_dir().map(|dir| dir.join(SAVED_LIMITS));
    status::run(io::stdin().lock(), io::stdout().lock(), path.as_deref(), when::now())
}

#[cfg(test)]
mod tests;
