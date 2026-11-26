//! Shows the last reported usage limits, today's local Claude Code and
//! OpenCode activity, and what providers report for accounts the user turned on.

mod accounts;
mod draw;
mod files;
mod http;
mod menu;
mod omp;
mod opencode;
mod signature;
mod sqlite;
mod status;
pub(crate) mod time;

use crate::sys::RawFd;
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::help::{self, Page};
use crate::json::Value;
use crate::keys::Key;
use crate::pane::{After, External, Pane};
use crate::preferences::{Show, UsageAccounts, UsageTool, UsageTools};
use crate::screen::Screen;
use crate::watch::Scope;
use crate::when::{DAY, MINUTE};
use crate::{paths, preferences, when};

use self::files::{Budget, FileVersion, Reverse, SessionFile, SessionRoot, Sessions, SnapshotVersion, Stream, Tree};
use self::time::Timestamp;

pub use menu::menu_stream;

const CLOCK_SKEW: i64 = 5 * MINUTE;
const CODEX_LIVE: i64 = 5 * MINUTE;
const TAIL_BYTES: u64 = 4 * 1024 * 1024;
const CODEX_BYTES: u64 = files::CODEX_FILES as u64 * TAIL_BYTES;
const CODEX_DEFERRED_LIMIT: usize = 512;
const CLAUDE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SAMPLES: usize = 4096;
const SAVED_LIMITS: &str = "claude-limits.json";
const NO_TOOLS: &str = "Usage tools: none found";

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

/// Keep a window's own observation time even if another window is newer.
#[derive(Clone, Copy, PartialEq)]
struct Window {
    minutes: u64,
    percent: f64,
    resets: Option<i64>,
    observed: i64,
}

impl Window {
    /// Reject malformed or out-of-range percentages instead of showing a misleading figure.
    fn percent(value: Value<'_>) -> Option<f64> {
        value.number().filter(|n| (0.0..=100.0).contains(n)).map(|n| n.abs())
    }

    /// How much of the window is left, as the pane and the status line show it.
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "window percent is in 0..=100")]
    fn number(self) -> &'static str {
        let left = 100.0 - self.percent;
        // Rounding 0.4% to 0% would suggest there is nothing left.
        if left > 0.0 && left < 1.0 {
            return "<1%";
        }
        if left > 99.0 && left < 100.0 {
            return "99+%";
        }
        let rounded = left.round_ties_even() as usize;
        let skip = usize::from(rounded < 100) + usize::from(rounded < 10);
        std::str::from_utf8(&PERCENT_TEXT[rounded][skip..]).expect("the figures are ASCII")
    }

    /// Whole percentages are the menu protocol's only numeric representation.
    /// Preserve the distinction between nearly and completely empty/full.
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "window percent is in 0..=100")]
    fn remaining(self) -> u8 {
        let left = 100.0 - self.percent;
        if left > 0.0 && left < 100.0 { (left.round_ties_even() as u8).clamp(1, 99) } else { left as u8 }
    }

    fn label(self) -> Cow<'static, str> {
        match self.minutes {
            10_080 => "week".into(),
            300 => "5h".into(),
            1440 => "day".into(),
            n if n.is_multiple_of(1440) => format!("{}d", n / 1440).into(),
            n if n.is_multiple_of(60) => format!("{}h", n / 60).into(),
            n => format!("{n}m").into(),
        }
    }

    fn is_stale(self, now: i64, source: Source) -> bool {
        now >= self.stale_at(source)
    }

    fn is_reset(self, now: i64) -> bool {
        self.resets.is_some_and(|reset| reset <= now)
    }

    fn stale_at(self, source: Source) -> i64 {
        self.observed.saturating_add(source.fresh_for())
    }

    /// Codex reports percentages and durations under different names from Claude.
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

/// The first two slots are the provider's all-models windows.
type BaseWindows = [Option<Window>; 2];

/// Keep the optional model week separate so it never replaces an all-models limit.
#[derive(Clone, Copy, Default, PartialEq)]
struct Limits {
    base: BaseWindows,
    model_week: Option<ModelWeek>,
}

impl Limits {
    const fn new(base: BaseWindows) -> Self {
        Self { base, model_week: None }
    }

    /// Include the model-specific week when scheduling the next reset.
    fn windows(&self) -> impl Iterator<Item = &Window> {
        self.base.iter().flatten().chain(self.model_week.as_ref().map(|model| &model.window))
    }

    /// A report can arrive without a single window Milo can show.
    fn any(&self) -> bool {
        self.windows().next().is_some()
    }
}

/// What this machine says about a tool, apart from the choice made in setup.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Presence {
    /// Nothing the tool saved was found, so nothing says it is in use here.
    Absent,
    /// The tool keeps data here, but none of it is a figure Milo can show.
    Found,
    /// A saved figure: a limit window, or tokens recorded today.
    Reading,
}

impl Presence {
    /// A tool with limits has a reading once a report holds a window.
    fn of_limits(reported: Option<&Reading>, found: bool) -> Self {
        if reported.is_some_and(|reading| reading.windows.any()) {
            Self::Reading
        } else if found || reported.is_some() {
            Self::Found
        } else {
            Self::Absent
        }
    }

    /// Auto gives a place only to a figure. Always keeps the place, to say what is unknown.
    fn placed(self, show: Show) -> bool {
        match show {
            Show::Auto => self == Self::Reading,
            Show::Always => true,
            Show::Hide => false,
        }
    }

    /// Whether the pane or doctor has anything to say about the tool.
    fn relevant(self, show: Show) -> bool {
        match show {
            Show::Auto => self != Self::Absent,
            Show::Always => true,
            Show::Hide => false,
        }
    }
}

/// Store the short provider label inline so a reading can stay cheap to copy.
#[derive(Clone, Copy, PartialEq)]
struct ModelWeek {
    window: Window,
    caption: [u8; 37],
    len: usize,
}

impl ModelWeek {
    /// Only accept labels safe to print in the strip.
    fn new(tier: &str, window: Window) -> Option<Self> {
        let len = tier.len();
        if len == 0 || !tier.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) {
            return None;
        }
        let mut caption = [0; 37];
        let (name, suffix) = caption.get_mut(..len + 5)?.split_at_mut(len);
        name.copy_from_slice(tier.as_bytes());
        suffix.copy_from_slice(b" week");
        caption[0].make_ascii_uppercase();
        Some(Self { window, caption, len })
    }

    fn label(&self) -> &str {
        &self.caption()[..self.len]
    }

    fn caption(&self) -> &str {
        std::str::from_utf8(&self.caption[..self.len + 5]).expect("model captions are ASCII")
    }
}

/// The newest reading wins even if a provider did not report a usable window.
#[derive(Clone, Copy, PartialEq)]
struct Reading {
    at: Timestamp,
    windows: Limits,
}

impl Reading {
    fn visible_at(&self) -> i64 {
        self.at.seconds.saturating_sub(CLOCK_SKEW)
    }

    /// Skip non-limit transcript lines and ignore separate Codex model buckets.
    fn codex(line: &[u8]) -> Option<Self> {
        if !files::has_marker(line, b"token_count") {
            return None;
        }
        let [kind, at, payload] = Value::parse(line)?.fields(["type", "timestamp", "payload"]);
        kind?.is("event_msg").then_some(())?;
        let at = Timestamp::iso(at?)?;
        let [kind, limits] = payload?.fields(["type", "rate_limits"]);
        kind?.is("token_count").then_some(())?;
        let [bucket, primary, secondary] =
            limits.filter(|value| value.object())?.fields(["limit_id", "primary", "secondary"]);
        if bucket.is_some_and(|id| !id.null() && !id.is("codex")) {
            return None;
        }
        let windows = Limits::new([primary, secondary].map(|window| Window::codex(window, at.seconds)));
        Some(Self { at, windows })
    }
}

#[derive(Clone, Copy)]
enum Source {
    CodexSession,
    ClaudeStatus,
    ClaudeCache,
    Omp,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::CodexSession => "Codex session log",
            Self::ClaudeStatus => "Milo Claude status line",
            Self::ClaudeCache => "Claude Code usage cache",
            Self::Omp => "OMP usage database",
        }
    }

    fn fresh_for(self) -> i64 {
        match self {
            // OMP itself can go about six minutes between provider fetches.
            Self::Omp => 10 * MINUTE,
            Self::CodexSession | Self::ClaudeStatus | Self::ClaudeCache => 5 * MINUTE,
        }
    }
}

/// Complete rollout reports in file order. Keep only the deferred reports
/// that can become the last valid line as the clock catches up.
#[derive(Default)]
struct CodexReports {
    reading: Option<Reading>,
    deferred: VecDeque<Reading>,
    recheck_at: Option<i64>,
}

impl CodexReports {
    fn ready(&self, now: i64) -> usize {
        self.deferred.partition_point(|reading| reading.visible_at() <= now)
    }

    fn advance(&mut self, now: i64) {
        let ready = self.ready(now);
        if ready != 0 {
            self.reading = Some(self.deferred[ready - 1]);
            self.deferred.drain(..ready);
        }
    }

    fn discard_superseded(&mut self, by: &Reading) {
        if self.recheck_at.is_some_and(|at| by.visible_at() <= at) {
            self.recheck_at = None;
        }
        self.deferred.truncate(self.deferred.partition_point(|older| older.visible_at() < by.visible_at()));
    }

    fn append(&mut self, reading: Reading, now: i64) {
        if reading.visible_at() <= now {
            self.reading = Some(reading);
            self.deferred.clear();
            self.recheck_at = None;
        } else {
            self.discard_superseded(&reading);
            if self.recheck_at.is_none() {
                if self.deferred.len() == CODEX_DEFERRED_LIMIT {
                    self.recheck_at = Some(reading.visible_at());
                } else {
                    self.deferred.push_back(reading);
                }
            }
        }
    }

    fn recheck_due(&self, now: i64) -> bool {
        self.recheck_at.is_some_and(|at| at <= now)
    }
}

/// Retain the most recent complete report and the state needed to read appends.
struct CodexFile {
    version: FileVersion,
    stream: Stream,
    complete: CodexReports,
    pending: Option<Reading>,
}

impl CodexFile {
    fn read(
        file: &std::fs::File,
        version: FileVersion,
        cached: Option<Self>,
        now: i64,
        budget: &mut Budget,
    ) -> io::Result<Self> {
        let floor = budget.remaining.saturating_sub(TAIL_BYTES);
        let mut retained = None;
        if let Some(mut old) = cached {
            if version == old.version && !old.complete.recheck_due(now) {
                return Ok(old);
            }
            let growing = version.same_file(old.version) && version.len > old.version.len;
            if version == old.version || growing && old.stream.can_resume(file, budget)? {
                old.complete.advance(now);
                if growing
                    && !old.complete.recheck_due(now)
                    && version.len - old.version.len <= budget.remaining - floor
                {
                    old.stream.read(file, version.len, budget, |line| {
                        if let Some(reading) = Reading::codex(line) {
                            old.complete.append(reading, now);
                        }
                    })?;
                    if old.stream.offset != version.len {
                        return Err(io::ErrorKind::UnexpectedEof.into());
                    }
                    old.pending = Reading::codex(old.stream.tail());
                    old.version = version;
                    return Ok(old);
                }
                if old.complete.recheck_due(now) {
                    old.complete.recheck_at = None;
                }
                // A large append can move the previous line out of the tail.
                // Keep a formerly unterminated report only if the append finished it.
                if growing
                    && let Some(pending) = old.pending
                    && old.stream.tail_terminated(file, version.len, budget)?
                {
                    old.complete.append(pending, now);
                }
                retained = Some(old.complete);
            }
        }

        let mut deferred = VecDeque::<Reading>::new();
        // An exceptionally long run of deferred reports gets one bounded
        // read when a report outside the in-memory frontier may be usable.
        let mut recheck_at = None;
        let (reading, stream) = files::tail(file, version.len, budget.remaining - floor, budget, |line| {
            let reading = Reading::codex(line)?;
            if reading.visible_at() <= now {
                return Some(reading);
            }
            // A later line already eligible at the same time takes precedence.
            if deferred.front().is_none_or(|later| reading.visible_at() < later.visible_at()) {
                if deferred.len() == CODEX_DEFERRED_LIMIT {
                    recheck_at = deferred.pop_back().map(|reading| reading.visible_at());
                }
                deferred.push_front(reading);
            }
            None
        })?;
        let complete = match (reading, retained) {
            (None, Some(mut previous)) => {
                for reading in deferred {
                    previous.append(reading, now);
                }
                previous.recheck_at = previous.recheck_at.into_iter().chain(recheck_at).min();
                previous
            }
            (reading, _) => CodexReports { reading, deferred, recheck_at },
        };
        let pending = Reading::codex(stream.tail());
        Ok(Self { version, stream, complete, pending })
    }

    /// A valid last line is usable before its newline, but may still change.
    fn reported(&self, now: i64) -> Option<&Reading> {
        self.pending
            .as_ref()
            .filter(|r| r.visible_at() <= now)
            .or_else(|| self.complete.ready(now).checked_sub(1).map(|i| &self.complete.deferred[i]))
            .or_else(|| self.complete.reading.as_ref().filter(|r| r.visible_at() <= now))
    }

    fn next_reading(&self, now: i64) -> Option<i64> {
        let ready = self.complete.ready(now);
        [self.pending.as_ref(), self.complete.deferred.get(ready), self.complete.reading.as_ref()]
            .into_iter()
            .flatten()
            .map(Reading::visible_at)
            .filter(|&change| change > now)
            .min()
    }
}

#[derive(Default)]
enum CodexSessions {
    #[default]
    Missing,
    Present,
    Unreadable,
}

/// Keep one snapshot per session alongside the account snapshot from OMP.
#[derive(Default)]
struct Codex {
    files: Sessions<CodexFile>,
    omp: Option<Reading>,
    sessions: CodexSessions,
}

impl Codex {
    /// Drop sessions that vanished and reuse reports from files that did not change.
    fn check(&mut self, path: Option<&Path>, now: i64, clock: Instant, force: bool, budget: &mut Budget) {
        let root = match self.files.refresh(path, Tree::Codex, clock, force) {
            Ok((root, _)) => root,
            Err(error) => {
                self.sessions = if error.kind() == io::ErrorKind::NotFound {
                    CodexSessions::Missing
                } else {
                    CodexSessions::Unreadable
                };
                return;
            }
        };
        self.sessions = if self.files.inaccessible {
            CodexSessions::Unreadable
        } else if self.files.entries.iter().all(|file| file.version.len == 0) && !self.files.partial {
            CodexSessions::Missing
        } else {
            CodexSessions::Present
        };
        for candidate in &mut self.files.entries {
            if budget.remaining != 0
                && !candidate
                    .cached
                    .as_ref()
                    .is_some_and(|file| file.version == candidate.version && !file.complete.recheck_due(now))
            {
                candidate.cached = root.open(&candidate.path).and_then(|(file, version)| {
                    CodexFile::read(&file, version, candidate.cached.take(), now, budget).ok()
                });
            }
            if candidate.version.len != 0 && candidate.cached.is_none() {
                self.sessions = CodexSessions::Unreadable;
            }
        }
    }

    fn has_recent_rollout(&self, now: i64) -> bool {
        let now = i128::from(now);
        let recent = now - i128::from(CODEX_LIVE)..=now + i128::from(CLOCK_SKEW);
        self.files.entries.iter().any(|file| recent.contains(&file.version.modified_second()))
    }

    /// Different sources can describe the same account, not separate limits to add.
    fn reported(&self, now: i64) -> Option<(&Reading, Source)> {
        self.files
            .cached()
            .filter_map(|file| file.reported(now).map(|r| (r, Source::CodexSession)))
            .chain(self.omp.iter().filter(|r| r.visible_at() <= now).map(|r| (r, Source::Omp)))
            .max_by_key(|(r, _)| r.at)
    }

    fn next_reading(&self, now: i64) -> Option<i64> {
        self.files
            .cached()
            .filter_map(|file| file.next_reading(now))
            .chain(self.omp.iter().map(Reading::visible_at).filter(|&change| change > now))
            .min()
    }

    /// A rollout with something in it, or a tree Milo was refused, says Codex
    /// runs here. An empty folder or an empty file is only a leftover.
    fn presence(&self, reported: Option<(&Reading, Source)>) -> Presence {
        let found = matches!(self.sessions, CodexSessions::Unreadable)
            || self.files.entries.iter().any(|file| file.version.len > 0);
        Presence::of_limits(reported.map(|(reading, _)| reading), found)
    }
}

/// One assistant message contributes tokens, not a percentage of the account limit.
#[derive(Clone, Copy)]
struct Sample {
    at: i64,
    tokens: u64,
}

impl Sample {
    /// Filter obvious non-usage lines before invoking the JSON reader.
    fn parse(line: &[u8]) -> Option<(Self, Option<std::borrow::Cow<'_, str>>)> {
        if !files::has_marker(line, b"usage") {
            return None;
        }
        let (at, kind, message) = Self::record(line)?;
        Self::from_record(at, kind, message)
    }

    /// Backward reads also need timestamps from records that carry no usage.
    fn record(line: &[u8]) -> Option<(i64, Option<Value<'_>>, Option<Value<'_>>)> {
        let [kind, at, message] = Value::parse(line)?.fields(["type", "timestamp", "message"]);
        Some((Timestamp::iso(at?)?.seconds, kind, message))
    }

    /// Count generated and newly cached tokens, not tokens read from the cache.
    fn from_record<'a>(
        at: i64,
        kind: Option<Value<'_>>,
        message: Option<Value<'a>>,
    ) -> Option<(Self, Option<std::borrow::Cow<'a, str>>)> {
        kind?.is("assistant").then_some(())?;
        let [usage, id] = message?.fields(["usage", "id"]);
        let counts = usage?.fields(["input_tokens", "output_tokens", "cache_creation_input_tokens"]);
        let mut counts = counts.into_iter().flatten().filter_map(Value::unsigned);
        let first = counts.next()?;
        let tokens = counts.fold(first, u64::saturating_add);
        Some((Self { at, tokens }, id.and_then(|v| v.string(128))))
    }
}

/// Identified messages can recur; anonymous ones cannot be safely merged.
#[derive(Default)]
struct Samples {
    identified: HashMap<Box<str>, Sample>,
    anonymous: Vec<Sample>,
    incomplete_since: Option<i64>,
}

impl Samples {
    /// Keep the earliest timestamp while preferring totals seen later in the transcript.
    fn add(&mut self, sample: Sample, id: Option<&str>, cutoff: i64, revisiting: bool) {
        if let Some(previous) = id.and_then(|id| self.identified.get_mut(id)) {
            previous.at = previous.at.min(sample.at);
            if !revisiting {
                previous.tokens = sample.tokens;
            }
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

/// Retains both the backward catch-up and the forward tail across read budgets.
struct ClaudeFile {
    version: FileVersion,
    /// Earliest day boundary covered by the retained samples and cursors.
    cutoff: i64,
    stream: Stream,
    reverse: Option<Reverse>,
    samples: Samples,
    pending: Option<(Sample, Option<Box<str>>)>,
}

impl ClaudeFile {
    /// A file is settled only after the old and newly appended work is done.
    fn settled(&self) -> bool {
        self.reverse.is_none() && self.stream.offset == self.version.len
    }

    /// Rewritten files start over; appends use the existing cursor.
    fn check(
        &mut self,
        root: &SessionRoot,
        path: &Path,
        candidate: FileVersion,
        cutoff: i64,
        budget: &mut Budget,
    ) -> bool {
        if self.version == candidate && self.settled() {
            return true;
        }
        if budget.remaining == 0 {
            return false;
        }
        let Some((file, version)) = root.open(path) else {
            *self = Self::new(candidate, cutoff);
            return true;
        };
        if !version.same_file(self.version) || version != self.version && version.len <= self.version.len {
            *self = Self::new(version, cutoff);
        }
        self.version = version;
        self.cutoff = cutoff;
        self.samples.expire(cutoff);
        let loaded = self.read(&file, cutoff, budget);
        self.pending = if self.settled() {
            Sample::parse(self.stream.tail()).map(|(mut sample, id)| {
                if let Some(previous) = id.as_ref().and_then(|id| self.samples.identified.get(id.as_ref())) {
                    sample.at = sample.at.min(previous.at);
                }
                (sample, id.map(|id| id.into_owned().into_boxed_str()))
            })
        } else {
            None
        };
        loaded.is_err() || self.settled()
    }

    /// Find today's records from the end, then switch to consuming appends.
    fn read(&mut self, file: &std::fs::File, cutoff: i64, budget: &mut Budget) -> io::Result<()> {
        if let Some(reverse) = &mut self.reverse {
            let mut reached_history = false;
            while reverse.offset != 0 && budget.remaining != 0 {
                let (mut old, mut current) = (false, false);
                let _ = reverse.block::<()>(file, 0, budget, |line, at_end| {
                    if at_end {
                        self.stream.remember_tail(line);
                    } else if let Some((at, kind, message)) = line.and_then(Sample::record) {
                        old |= at < cutoff;
                        current |= at >= cutoff;
                        if let Some((sample, id)) = Sample::from_record(at, kind, message) {
                            self.samples.add(sample, id.as_deref(), cutoff, true);
                        }
                    }
                    ControlFlow::Continue(())
                })?;
                // Transcripts append in time order. Allow mixed timestamps in the
                // boundary block rather than stopping at the first old record.
                reached_history = old && !current;
                if reached_history {
                    break;
                }
            }
            if !reached_history && reverse.offset != 0 {
                return Ok(());
            }
            self.reverse = None;
        }
        self.stream.read(file, self.version.len, budget, |line| {
            if let Some((sample, id)) = Sample::parse(line) {
                self.samples.add(sample, id.as_deref(), cutoff, false);
            }
        })
    }

    /// Save the original end so subsequent appends are read exactly once.
    fn new(version: FileVersion, cutoff: i64) -> Self {
        Self {
            version,
            cutoff,
            stream: Stream::resume(version.len),
            reverse: (version.len != 0).then(|| Reverse::new(version.len)),
            samples: Samples::default(),
            pending: None,
        }
    }

    /// Include a valid unterminated record provisionally until a newline commits it.
    fn identified(&self) -> impl Iterator<Item = (&str, Sample)> {
        let pending = self.pending.as_ref().and_then(|(sample, id)| Some((id.as_deref()?, *sample)));
        self.samples
            .identified
            .iter()
            .filter(move |(id, _)| pending.is_none_or(|(pending, _)| id.as_ref() != pending))
            .map(|(id, sample)| (id.as_ref(), *sample))
            .chain(pending)
    }

    fn anonymous(&self) -> impl Iterator<Item = &Sample> {
        self.samples
            .anonymous
            .iter()
            .chain(self.pending.as_ref().filter(|(_, id)| id.is_none()).map(|(sample, _)| sample))
    }
}

/// Local token activity and account limits have separate sources and meanings.
#[derive(Default)]
struct Claude {
    files: Sessions<ClaudeFile>,
    identified: Vec<Sample>,
    partial: bool,
    saved_version: Option<SnapshotVersion>,
    status: Option<status::Saved>,
    native_version: Option<SnapshotVersion>,
    native: Option<status::Native>,
    omp: Option<Reading>,
    unreadable_limits: bool,
}

/// A partial total is a lower bound, not an exact amount of today's activity.
#[derive(Default)]
struct Activity {
    today: u64,
    partial: bool,
}

impl Claude {
    fn readings(&self) -> [(Option<&Reading>, Source); 3] {
        let account = self.native.as_ref().and_then(|native| native.account);
        [
            (self.status.as_ref().and_then(|saved| saved.for_account(account)), Source::ClaudeStatus),
            (self.native.as_ref().and_then(|native| native.reading.as_ref()), Source::ClaudeCache),
            (self.omp.as_ref(), Source::Omp),
        ]
    }

    fn reported(&self, now: i64) -> Option<(&Reading, Source)> {
        self.readings()
            .into_iter()
            .filter_map(|(reading, source)| Some((reading?, source)))
            .filter(|(reading, _)| reading.visible_at() <= now)
            .max_by_key(|(reading, _)| reading.at)
    }

    fn next_reading(&self, now: i64) -> Option<i64> {
        self.readings()
            .into_iter()
            .filter_map(|(reading, _)| reading)
            .map(Reading::visible_at)
            .filter(|&change| change > now)
            .min()
    }

    /// Claude Code keeps `~/.claude.json` from its first run and writes a
    /// transcript for each session. A `~/.claude` folder alone is no sign of
    /// it: Milo's own setup can create one.
    fn presence(&self, reported: Option<(&Reading, Source)>) -> Presence {
        let found = self.unreadable_limits
            || self.native_version.is_some_and(|version| version.len() > 0)
            || self.saved_version.is_some_and(|version| version.len() > 0)
            || self.files.entries.iter().any(|file| file.version.len > 0);
        Presence::of_limits(reported.map(|(reading, _)| reading), found)
    }

    /// Merge duplicate IDs only when a contributing file changes.
    fn check(&mut self, path: Option<&Path>, cutoff: i64, clock: Instant, force: bool, budget: &mut Budget) -> bool {
        let Ok((root, mut changed)) = self.files.refresh(path, Tree::Claude { since: cutoff }, clock, force) else {
            self.identified.clear();
            self.partial = false;
            return true;
        };
        self.partial = self.files.partial;
        let mut settled = true;
        for SessionFile { path, version, cached } in &mut self.files.entries {
            // A wider day can include records the backward scan skipped or
            // the forward scan discarded. Reparse them even if the file is unchanged.
            if cached.as_ref().is_some_and(|file| cutoff < file.cutoff) {
                *cached = None;
            }
            if cached.as_ref().is_some_and(|file| file.version == *version && file.settled()) {
                continue;
            }
            let file = cached.get_or_insert_with(|| ClaudeFile::new(*version, cutoff));
            settled &= file.check(&root, path, *version, cutoff, budget);
            self.partial |= !file.settled();
            changed = true;
        }
        self.partial |= !settled;
        if !changed {
            return settled;
        }
        // Appends usually leave the number of distinct IDs nearly unchanged.
        // Don't reserve for vanished sessions or assume all transcripts have disjoint IDs.
        let ids: usize = self.files.cached().map(|file| file.samples.identified.len()).sum();
        let mut unique = HashMap::<&str, Sample>::with_capacity(self.identified.len().min(ids));
        for (id, sample) in self.files.cached().flat_map(ClaudeFile::identified) {
            unique
                .entry(id)
                .and_modify(|old| {
                    old.at = old.at.min(sample.at);
                    old.tokens = old.tokens.max(sample.tokens);
                })
                .or_insert(sample);
        }
        self.identified.clear();
        self.identified.extend(unique.into_values());
        settled
    }

    /// Use the current local day even when the underlying transcripts have not changed.
    fn activity(&self, today: i64, now: i64) -> Activity {
        let mut activity = Activity { partial: self.partial, ..Activity::default() };
        activity.partial |= self
            .files
            .cached()
            .any(|file| today < file.cutoff || file.samples.incomplete_since.is_some_and(|at| at >= today));
        for sample in self
            .identified
            .iter()
            .chain(self.files.cached().flat_map(ClaudeFile::anonymous))
            .filter(|s| (today..=now).contains(&s.at))
        {
            activity.today = activity.today.saturating_add(sample.tokens);
        }
        activity
    }
}

/// Cache the first instant of the local day to avoid searching on every frame.
struct Today {
    day: i64,
    since: i64,
}

impl Today {
    /// Find local midnight without assuming a day has exactly 24 hours.
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

    /// The kept midnight stands only while it is still where this date begins:
    /// a change of time zone can move it without changing the date.
    fn since(&mut self, now: i64) -> i64 {
        let day = when::local_day(now);
        let begins_here = when::local_day(self.since) == day && when::local_day(self.since - 1) != day;
        if self.day != day || !begins_here {
            *self = Self::at(now, day);
        }
        self.since
    }
}

/// What OpenCode recorded locally, read off the pane loop like OMP's reports.
struct OpenCode {
    poll: sqlite::Poll<opencode::Report>,
    report: opencode::Report,
}

impl OpenCode {
    fn new(path: Option<PathBuf>, wait: Duration) -> Self {
        Self { poll: opencode::poll(path, wait), report: opencode::Report::default() }
    }

    /// A background result needs a new frame when it changes the report, or
    /// ends a read the pane was announcing.
    fn collect(&mut self) -> bool {
        let checking = self.poll.checking();
        let Some(result) = self.poll.collect() else { return false };
        let Some(report) = result else { return checking };
        let changed = self.report != report;
        self.report = report;
        changed || checking
    }

    /// Totals stand only for the local day they were read for, and only when something was used.
    fn today(&self, today: i64) -> Option<&opencode::Activity> {
        match &self.report {
            opencode::Report::Today(activity) if activity.since == today && activity.tokens.fresh() > 0 => {
                Some(activity)
            }
            _ => None,
        }
    }

    /// Only a database OpenCode wrote says it is in use here.
    fn presence(&self, today: i64) -> Presence {
        if self.today(today).is_some() {
            Presence::Reading
        } else if self.report == opencode::Report::Missing {
            Presence::Absent
        } else {
            Presence::Found
        }
    }
}

/// Codex resolves an explicit home against its working directory, and never
/// falls back to the default if that override names an invalid directory.
fn codex_sessions(home: Option<&Path>) -> Option<PathBuf> {
    let root = match std::env::var("CODEX_HOME").ok().filter(|dir| !dir.is_empty()) {
        Some(dir) => std::fs::canonicalize(dir).ok().filter(|dir| dir.is_dir())?,
        None => home?.join(".codex"),
    };
    Some(root.join("sessions"))
}

fn claude_state(claude: Option<&paths::ClaudePaths>) -> Option<PathBuf> {
    claude.map_or_else(paths::state_dir, paths::ClaudePaths::state)
}

fn claude_sources() -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
    let claude = paths::claude();
    (
        claude.as_ref().map(paths::ClaudePaths::projects),
        claude.as_ref().map(paths::ClaudePaths::config),
        claude_state(claude.as_ref()),
    )
}

/// Uses the installed providers' directories; tests supply their own directories and time.
pub struct Usage {
    codex_dir: Option<PathBuf>,
    claude_dir: Option<PathBuf>,
    limits_file: Option<PathBuf>,
    claude_config: Option<PathBuf>,
    omp: sqlite::Poll<omp::Reports>,
    opencode: OpenCode,
    accounts: accounts::Accounts,
    codex: Codex,
    claude: Claude,
    today: Today,
    large_figures: bool,
    tools: UsageTools,
    preference_note: Option<&'static str>,
    #[cfg(test)]
    fixed_now: Option<i64>,
    #[cfg(test)]
    bytes_read: u64,
}

impl Usage {
    pub fn open() -> Self {
        let root = preferences::worktree(Path::new("."));
        let (prefs, note) = preferences::Preferences::for_pane(root.as_deref());
        let home = paths::home();
        let usage = Self::for_home(home.as_deref(), when::now(), prefs.usage_tools)
            .with_large_figures(prefs.usage_large_figures);
        let (omp, opencode) = (omp::database(home.as_deref()), opencode::database(home.as_deref()));
        let mut usage = usage.with_databases(omp, opencode, sqlite::QUERY_WAIT);
        usage.preference_note = note;
        usage.accounts = accounts::Accounts::open(prefs.usage_accounts);
        usage
    }

    pub fn with_large_figures(mut self, large: bool) -> Self {
        self.large_figures = large;
        self
    }

    /// A hidden tool is not looked for: its folders are neither walked nor
    /// watched, and its saved reports are not read.
    pub fn showing(mut self, tools: UsageTools) -> Self {
        self.tools = tools;
        if tools[UsageTool::Codex] == Show::Hide {
            self.codex_dir = None;
        }
        if tools[UsageTool::Claude] == Show::Hide {
            self.claude_dir = None;
            self.limits_file = None;
            self.claude_config = None;
        }
        self
    }

    /// OMP's database serves both tools with limits; OpenCode's serves only OpenCode.
    fn with_databases(mut self, omp: Option<PathBuf>, opencode: Option<PathBuf>, wait: Duration) -> Self {
        self.omp = omp::poll(omp.filter(|_| self.reads_omp()), wait);
        self.opencode = OpenCode::new(opencode.filter(|_| self.tools[UsageTool::OpenCode] != Show::Hide), wait);
        self
    }

    /// The descriptor of a local database read that is still running.
    fn database_fd(&self) -> Option<RawFd> {
        self.omp.fd().or_else(|| self.opencode.poll.fd())
    }

    fn reads_omp(&self) -> bool {
        [UsageTool::Codex, UsageTool::Claude].into_iter().any(|tool| self.tools[tool] != Show::Hide)
    }

    /// A report for a hidden tool is dropped as soon as it is read.
    fn keep(&mut self, reports: omp::Reports) -> bool {
        let codex = reports.codex.filter(|_| self.tools[UsageTool::Codex] != Show::Hide);
        let claude = reports.claude.filter(|_| self.tools[UsageTool::Claude] != Show::Hide);
        if self.codex.omp == codex && self.claude.omp == claude {
            return false;
        }
        self.codex.omp = codex;
        self.claude.omp = claude;
        true
    }

    fn for_home(home: Option<&Path>, now: i64, tools: UsageTools) -> Self {
        let codex = (tools[UsageTool::Codex] != Show::Hide).then(|| codex_sessions(home)).flatten();
        let (claude, config, state) =
            if tools[UsageTool::Claude] == Show::Hide { (None, None, None) } else { claude_sources() };
        let mut usage = Self::from_dirs(codex, claude, state, now);
        usage.claude_config = config;
        usage.showing(tools)
    }

    /// Tests use the same reader without depending on a real home or clock.
    fn from_dirs(codex_dir: Option<PathBuf>, claude_dir: Option<PathBuf>, state: Option<PathBuf>, now: i64) -> Self {
        Self {
            codex_dir,
            claude_dir,
            limits_file: state.map(|dir| dir.join(SAVED_LIMITS)),
            claude_config: None,
            omp: omp::poll(None, sqlite::QUERY_WAIT),
            opencode: OpenCode::new(None, sqlite::QUERY_WAIT),
            accounts: accounts::Accounts::none(),
            codex: Codex::default(),
            claude: Claude::default(),
            today: Today::at(now, when::local_day(now)),
            large_figures: false,
            tools: UsageTools::default(),
            preference_note: None,
            #[cfg(test)]
            fixed_now: Some(now),
            #[cfg(test)]
            bytes_read: 0,
        }
    }

    fn now(&self) -> i64 {
        #[cfg(test)]
        if let Some(now) = self.fixed_now {
            return now;
        }
        when::now()
    }

    /// Give each provider its own I/O allowance so one busy log cannot starve the other.
    fn refresh(&mut self, now: i64, clock: Instant, manual: bool) -> bool {
        self.collect_omp();
        self.opencode.collect();
        self.accounts.collect(clock);
        let cutoff = self.today.since(now);
        if manual {
            self.omp.reload(now, clock);
            self.opencode.poll.reload(now, clock);
        } else {
            self.omp.start(now, clock);
            // The local day can move under totals nothing has written to since, as when the
            // time zone changes. Those say nothing of the day the pane now shows.
            if matches!(self.opencode.report, opencode::Report::Today(read) if read.since != cutoff) {
                self.opencode.poll.forget();
            }
            self.opencode.poll.start(now, clock);
        }
        self.accounts.start(now, clock, manual);
        let mut codex_budget = Budget::new(CODEX_BYTES);
        let mut claude_budget = Budget::new(CLAUDE_BYTES);
        self.check_limits(&mut claude_budget);
        self.codex.check(self.codex_dir.as_deref(), now, clock, manual, &mut codex_budget);
        let claude = self.claude.check(self.claude_dir.as_deref(), cutoff, clock, manual, &mut claude_budget);
        #[cfg(test)]
        {
            self.bytes_read = codex_budget.read + claude_budget.read;
        }
        claude
    }

    /// A background result only needs a new frame when it changes a snapshot.
    fn collect_omp(&mut self) -> bool {
        let checking = self.omp.checking();
        let Some(result) = self.omp.collect() else { return false };
        result.is_some_and(|reports| self.keep(reports)) || checking
    }

    fn check_limits(&mut self, claude_budget: &mut Budget) {
        let mut unreadable = false;
        if let Some(path) = &self.limits_file {
            unreadable |= files::read_cached(
                path,
                &mut self.claude.saved_version,
                &mut self.claude.status,
                claude_budget,
                status::saved,
            );
        }
        if let Some(path) = &self.claude_config {
            unreadable |= files::read_cached(
                path,
                &mut self.claude.native_version,
                &mut self.claude.native,
                claude_budget,
                status::native,
            );
        }
        self.claude.unreadable_limits = unreadable;
    }
}

/// What doctor says about usage, from one bounded, read-only look: a line for
/// each tool that is found or always shown, and none for a tool that is
/// hidden or absent. Transcripts are found but not read. An account that is
/// turned on is described from its settings and last saved answer; doctor
/// does not ask its provider.
pub fn doctor(tools: UsageTools, chosen: UsageAccounts) -> Vec<String> {
    let home = paths::home();
    let now = when::now();
    let usage = Usage::for_home(home.as_deref(), now, tools);
    let mut lines =
        usage.doctor(omp::database(home.as_deref()), opencode::database(home.as_deref()), sqlite::QUERY_WAIT);
    let accounts = accounts::doctor(chosen, now);
    // With an account to describe, there is something to say besides that no tool was found.
    if !accounts.is_empty() {
        lines.retain(|line| line != NO_TOOLS);
    }
    lines.extend(accounts);
    lines
}

impl Usage {
    fn doctor(mut self, omp: Option<PathBuf>, opencode: Option<PathBuf>, wait: Duration) -> Vec<String> {
        let (now, clock) = (self.now(), Instant::now());
        let saved = omp.filter(|_| self.reads_omp()).and_then(|path| omp::read(&path, now, wait));
        if let Some(reports) = saved {
            self.keep(reports);
        }
        self.check_limits(&mut Budget::new(CLAUDE_BYTES));
        self.codex.check(self.codex_dir.as_deref(), now, clock, true, &mut Budget::new(CODEX_BYTES));
        let cutoff = self.today.since(now);
        self.claude.check(self.claude_dir.as_deref(), cutoff, clock, true, &mut Budget::new(0));
        let codex = self.codex.reported(now);
        let claude = self.claude.reported(now);
        let limits = [
            (UsageTool::Codex, self.codex.presence(codex), codex, "run Codex to save one"),
            (UsageTool::Claude, self.claude.presence(claude), claude, "turn on Show Claude Code limits in milo setup"),
        ];
        let mut lines = Vec::new();
        for (tool, presence, reported, step) in limits {
            if presence.relevant(self.tools[tool]) {
                let name = tool.label();
                lines.push(match reported {
                    Some((_, source)) => format!("{name} limits: {}", source.label()),
                    None => format!("{name} limits: no local report; {step}"),
                });
            }
        }
        let show = self.tools[UsageTool::OpenCode];
        if show != Show::Hide {
            let report = opencode.as_deref().map_or(opencode::Report::Missing, |path| opencode::read(path, now, wait));
            if show == Show::Always || report != opencode::Report::Missing {
                lines.extend(report.describe(opencode.as_deref(), now));
            }
        }
        if lines.is_empty() && UsageTool::ALL.into_iter().any(|tool| self.tools[tool] != Show::Hide) {
            lines.push(NO_TOOLS.into());
        }
        lines
    }
}

impl Pane for Usage {
    fn notice(&self) -> Option<&'static str> {
        self.preference_note
    }

    fn check(&mut self) -> bool {
        self.refresh(self.now(), Instant::now(), false)
    }

    fn check_with_watcher(&mut self, watching: bool, started: Instant) -> bool {
        // Without notifications, allow each fallback look to discover sessions.
        // The discovery rest still applies if a look runs early.
        if !watching {
            self.filesystem_changed();
        }
        self.refresh(self.now(), started, false)
    }

    fn filesystem_changed(&mut self) {
        self.codex.files.changed();
        self.claude.files.changed();
    }

    /// One job is watched at a time. A finished job's descriptor stays
    /// readable, so the next is picked up as soon as the first is collected.
    fn external_fd(&self) -> Option<RawFd> {
        self.database_fd().or_else(|| self.accounts.fd())
    }

    fn check_external(&mut self) -> External {
        let omp = self.collect_omp();
        let accounts = self.accounts.collect(Instant::now());
        if self.opencode.collect() || omp || accounts { External::Redraw } else { External::Unchanged }
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
        // FSEvents can miss appends to Codex's held-open rollout. Keep the
        // watcher, but poll known paths while a writer has been active recently.
        let mut complete =
            !self.codex.files.pending() && !self.claude.files.pending() && !self.codex.has_recent_rollout(self.now());
        for path in paths.into_iter().flatten() {
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
                roots.push(path.to_owned());
            } else {
                complete = false;
            }
        }
        Scope { roots, ignored: Default::default(), complete }
    }

    fn key(&mut self, key: Key) -> After {
        match key {
            Key::Char('q') => return After::Leave,
            Key::Char('r') => {
                self.refresh(self.now(), Instant::now(), true);
            }
            _ => return After::Ignored,
        }
        After::Stay
    }

    /// Redraw when the clock changes which readings and windows are usable.
    fn next_frame(&self) -> Option<Duration> {
        let now = self.now();
        let earliest = self
            .codex
            .reported(now)
            .into_iter()
            .chain(self.claude.reported(now))
            .flat_map(|(reading, source)| reading.windows.windows().map(move |window| (window, source)))
            .filter(|(window, _)| !window.is_reset(now))
            .flat_map(|(window, source)| [window.resets, Some(window.stale_at(source))])
            .flatten()
            .chain(self.codex.next_reading(now))
            .chain(self.claude.next_reading(now))
            .filter(|&change| change > now)
            .min();
        let wait = earliest.map_or(MINUTE, |ts| ts.saturating_sub(now).min(MINUTE));
        Some(Duration::from_secs(u64::try_from(wait).unwrap_or(60)))
    }

    /// A tool Milo neither shows nor found is left out of the help as well.
    fn help(&self) -> Page {
        let today = self.today.since;
        let claude = self.claude.presence(self.claude.reported(self.now())).relevant(self.tools[UsageTool::Claude]);
        let opencode = self.opencode.presence(today).relevant(self.tools[UsageTool::OpenCode]);
        let accounts = self.accounts.iter().next().is_some();
        Page::new(help_rows(claude, opencode, accounts))
    }
}

fn help_rows(claude: bool, opencode: bool, accounts: bool) -> Vec<help::HelpRow> {
    use help::{HelpRow, heading, key, note};
    let mut rows = vec![
        heading("Usage limits"),
        note("Figures come from the"),
        note("tool's last report."),
        note("An elapsed reset is not"),
        note("a new reading."),
        note("Open the tool to update."),
        HelpRow::new(),
        heading("Tools"),
        note("A tool appears once it"),
        note("has a figure to show."),
        note("milo setup can always"),
        note("show or hide each one."),
        HelpRow::new(),
    ];
    if claude {
        rows.extend([
            heading("Claude Code"),
            note("Tokens today show local"),
            note("activity, not a limit."),
            note("Setup can add a status"),
            note("line to collect limits."),
            HelpRow::new(),
        ]);
    }
    if opencode {
        rows.extend([
            heading("OpenCode"),
            note("Tokens today come from"),
            note("its local database."),
            note("Cost is its estimate."),
            note("It saves no limits."),
            HelpRow::new(),
        ]);
    }
    if accounts {
        rows.extend([
            heading("Accounts"),
            note("Balances, spend and"),
            note("credits come from the"),
            note("provider you turned on."),
            note("They are not limits."),
            note("r asks again, at most"),
            note("every 30 seconds."),
            HelpRow::new(),
        ]);
    }
    rows.extend([key("r", "reload"), key("q", "quit")]);
    rows
}

/// Claude Code must get a line and a successful exit even if it sent bad data.
pub fn claude_status() -> i32 {
    let claude = paths::claude();
    let config = claude.as_ref().map(paths::ClaudePaths::config);
    let path = claude_state(claude.as_ref()).map(|dir| dir.join(SAVED_LIMITS));
    let target = path.as_deref().map(|path| status::SaveTo { path, config: config.as_deref() });
    status::run_fd(io::stdin(), io::stdout().lock(), target, when::now(), status::INPUT_WAIT)
}

#[cfg(test)]
mod tests;
