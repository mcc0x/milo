//! Shows the last reported usage limits and today's local Claude Code activity.

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
use self::time::{DAY, MINUTE, Stamp};

const AGE_NOTE: i64 = 5 * MINUTE;
const CLOCK_SKEW: i64 = 5 * MINUTE;
const TAIL_BYTES: u64 = 4 * 1024 * 1024;
const CODEX_BYTES: u64 = files::CODEX_FILES as u64 * TAIL_BYTES;
const CLAUDE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SAMPLES: usize = 4096;
const SAVED_LIMITS: &str = "claude-limits.json";

/// An ordinary figure has at most three digits. Keep them ready for each frame.
static PERCENT_TEXT: [[u8; 4]; 101] = {
    let mut text = [[0; 4]; 101];
    let mut n = 0_u8;
    while n <= 100 {
        text[n as usize] = [b'0' + n / 100, b'0' + n / 10 % 10, b'0' + n % 10, b'%'];
        n += 1;
    }
    text
};

#[derive(Clone, Copy, PartialEq)]
struct Window {
    minutes: u64,
    percent: f64,
    resets: Option<i64>,
    observed: i64,
}

impl Window {
    fn percent(value: Value<'_>) -> Option<f64> {
        value.number().filter(|n| (0.0..=100.0).contains(n)).map(|n| n.abs())
    }

    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "window percent is in 0..=100")]
    fn number(self) -> &'static str {
        // Rounding 99.9% to 100% would suggest there is nothing left.
        if self.percent > 99.0 && self.percent < 100.0 {
            return "99+%";
        }
        if self.percent > 0.0 && self.percent < 1.0 {
            return "<1%";
        }
        let rounded = self.percent.round_ties_even() as usize;
        let skip = usize::from(rounded < 100) + usize::from(rounded < 10);
        std::str::from_utf8(&PERCENT_TEXT[rounded][skip..]).expect("the figures are ASCII")
    }

    fn is_reset(self, now: i64) -> bool {
        self.resets.is_some_and(|reset| reset <= now)
    }

    fn codex(value: Option<Value<'_>>, observed: i64) -> Option<Self> {
        let [percent, minutes, resets] = value?.fields(["used_percent", "window_minutes", "resets_at"]);
        Some(Self {
            minutes: Self::minutes(minutes?)?,
            percent: percent?.number().filter(|n| *n >= 0.0)?.abs().min(100.0),
            resets: resets.and_then(|value| time::reset(value, observed)),
            observed,
        })
    }

    /// Fractional minutes are not a window, but some reports use 300.0 for 300.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the float is positive, whole and in range"
    )]
    fn minutes(value: Value<'_>) -> Option<u64> {
        value.unsigned().filter(|&n| n > 0).or_else(|| {
            let n = value.number()?;
            (n > 0.0 && n < u64::MAX as f64 && n.fract() == 0.0).then_some(n as u64)
        })
    }
}

#[derive(Clone, Copy)]
struct CodexReading {
    at: Stamp,
    // A report with no readable windows still supersedes older reports.
    windows: Limits,
}

type Limits = [Option<Window>; 2];

impl CodexReading {
    fn parse(line: &[u8], now: i64) -> Option<Self> {
        if !files::has_marker(line, b"token_count") {
            return None;
        }
        let [kind, at, payload] = Value::parse(line)?.fields(["type", "timestamp", "payload"]);
        kind?.is("event_msg").then_some(())?;
        let at = Stamp::iso(at?)?;
        (at.seconds <= now.saturating_add(CLOCK_SKEW)).then_some(())?;
        let [kind, limits] = payload?.fields(["type", "rate_limits"]);
        kind?.is("token_count").then_some(())?;
        let [primary, secondary] = limits?.fields(["primary", "secondary"]);
        let windows = [primary, secondary].map(|window| Window::codex(window, at.seconds));
        Some(Self { at, windows })
    }
}

struct CodexFile {
    mark: Mark,
    reading: Option<CodexReading>,
}

#[derive(Default)]
struct Codex {
    files: HashMap<PathBuf, CodexFile>,
    no_sessions: bool,
}

impl Codex {
    fn check(&mut self, root: Option<&Path>, now: i64, budget: &mut Budget) {
        let Some(root) = root else {
            *self = Self { no_sessions: true, ..Self::default() };
            return;
        };
        let sessions = match Sessions::scan(root, Tree::Codex) {
            Ok(sessions) => sessions,
            Err(error) => {
                *self = Self { no_sessions: error.kind() == io::ErrorKind::NotFound, ..Self::default() };
                return;
            }
        };
        self.no_sessions = sessions.files.is_empty() && !sessions.limited;
        if sessions.files.len() == self.files.len()
            && sessions
                .files
                .iter()
                .all(|candidate| self.files.get(&candidate.path).is_some_and(|file| file.mark == candidate.mark))
        {
            return;
        }
        let mut previous = std::mem::take(&mut self.files);
        for candidate in &sessions.files {
            let cached = previous.remove(&candidate.path);
            if let Some(file) = Self::check_file(&sessions, candidate, cached, now, budget) {
                self.files.insert(candidate.path.clone(), file);
            }
        }
    }

    fn check_file(
        sessions: &Sessions,
        candidate: &files::Candidate,
        cached: Option<CodexFile>,
        now: i64,
        budget: &mut Budget,
    ) -> Option<CodexFile> {
        if cached.as_ref().is_some_and(|file| file.mark == candidate.mark) {
            return cached;
        }
        let (file, mark) = sessions.open(&candidate.path)?;
        let (mut reading, limited) =
            files::tail(&file, mark.len, TAIL_BYTES, budget, |line| CodexReading::parse(line, now)).ok()?;
        if limited && let Some(old) = cached.filter(|file| mark.same_file(file.mark) && mark.len > file.mark.len) {
            reading = reading.or(old.reading);
        }
        Some(CodexFile { mark, reading })
    }

    fn latest(&self) -> Option<CodexReading> {
        self.files.values().filter_map(|file| file.reading).max_by_key(|reading| reading.at)
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
    saved: Option<Limits>,
}

#[derive(Default)]
struct Activity {
    today: u64,
    partial: bool,
}

impl Claude {
    fn check(&mut self, root: Option<&Path>, cutoff: i64, budget: &mut Budget) -> bool {
        let Some(sessions) = root.and_then(|p| Sessions::scan(p, Tree::Claude { since: cutoff }).ok()) else {
            self.files.clear();
            self.limited = false;
            return true;
        };
        self.limited = sessions.limited;
        if sessions.files.len() == self.files.len()
            && sessions.files.iter().all(|candidate| {
                self.files
                    .get(&candidate.path)
                    .is_some_and(|file| file.mark == candidate.mark && file.stream.offset == file.mark.len)
            })
        {
            return true;
        }
        let mut previous = std::mem::take(&mut self.files);
        let mut settled = true;
        for candidate in &sessions.files {
            let mut file = previous.remove(&candidate.path).unwrap_or_else(|| ClaudeFile::new(candidate.mark));
            settled &= file.check(&sessions, candidate, cutoff, budget);
            self.limited |= file.stream.offset < file.mark.len;
            self.files.insert(candidate.path.clone(), file);
        }
        settled
    }

    fn activity(&self, today: i64, now: i64) -> Activity {
        let mut activity = Activity { partial: self.limited, ..Activity::default() };
        for file in self.files.values() {
            activity.partial |= file.samples.incomplete_since.is_some_and(|at| at >= today);
            for sample in file.all().filter(|s| (today..=now).contains(&s.at)) {
                activity.today = activity.today.saturating_add(sample.tokens);
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
        let cutoff = self.today.since(now);
        let mut codex_budget = Budget::new(CODEX_BYTES);
        let mut claude_budget = Budget::new(CLAUDE_BYTES);
        if let Some(path) = &self.limits_file {
            status::read(path, &mut self.claude.saved_mark, &mut self.claude.saved, &mut claude_budget);
        }
        self.codex.check(self.codex_dir.as_deref(), now, &mut codex_budget);
        let claude = self.claude.check(self.claude_dir.as_deref(), cutoff, &mut claude_budget);
        #[cfg(test)]
        {
            self.bytes_read = codex_budget.read + claude_budget.read;
        }
        claude
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
            .latest()
            .map(|reading| reading.windows)
            .into_iter()
            .chain(self.claude.saved)
            .flat_map(|windows| windows.into_iter().flatten().filter_map(|w| w.resets))
            .filter(|&reset| reset > now)
            .min();
        let wait = earliest.map_or(MINUTE, |ts| ts.saturating_sub(now).min(MINUTE));
        Some(Duration::from_secs(u64::try_from(wait).unwrap_or(60)))
    }

    fn help(&self) -> Page {
        Page::new(help_rows())
    }
}

fn help_rows() -> Vec<help::Row> {
    use help::{Row, heading, key, note};
    vec![
        heading("Usage limits"),
        note("Figures show the percent"),
        note("used in each window."),
        note("From this machine's"),
        note("sessions, as fresh as"),
        note("the last message. Past"),
        note("its reset, a window"),
        note("starts again at 0%."),
        Row::new(),
        heading("Claude Code"),
        note("Turn on its limits in"),
        note("milo setup. Tokens today"),
        note("show local activity,"),
        note("not a limit."),
        key("r", "reload"),
        key("q", "quit"),
    ]
}

/// Claude Code must get a line and a successful exit even if it sent bad data.
pub fn claude_status() -> i32 {
    let path = paths::state_dir().map(|dir| dir.join(SAVED_LIMITS));
    status::run(io::stdin().lock(), io::stdout().lock(), path.as_deref(), when::now())
}

#[cfg(test)]
mod tests;
