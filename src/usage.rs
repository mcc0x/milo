//! Shows the last reported usage limits and today's local Claude Code activity.

mod draw;
mod files;
mod omp;
mod status;
mod time;

use std::collections::HashMap;
use std::io;
use std::ops::ControlFlow;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::help::{self, Page};
use crate::json::Value;
use crate::keys::Key;
use crate::pane::{After, External, Pane};
use crate::screen::Screen;
use crate::watch::Scope;
use crate::when::{DAY, MINUTE};
use crate::{paths, preferences, when};

use self::files::{Budget, FileVersion, Reverse, SessionFile, SessionRoot, Sessions, SnapshotVersion, Stream, Tree};
use self::time::Timestamp;

const CLOCK_SKEW: i64 = 5 * MINUTE;
const CODEX_LIVE: i64 = 5 * MINUTE;
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
    /// Skip non-limit transcript lines and ignore separate Codex model buckets.
    fn codex(line: &[u8], now: i64) -> Option<Self> {
        if !files::has_marker(line, b"token_count") {
            return None;
        }
        let [kind, at, payload] = Value::parse(line)?.fields(["type", "timestamp", "payload"]);
        kind?.is("event_msg").then_some(())?;
        let at = Timestamp::iso(at?)?;
        (at.seconds <= now.saturating_add(CLOCK_SKEW)).then_some(())?;
        let [kind, limits] = payload?.fields(["type", "rate_limits"]);
        kind?.is("token_count").then_some(())?;
        let [bucket, primary, secondary] =
            limits.filter(|value| value.object())?.fields(["limit_id", "primary", "secondary"]);
        if bucket.and_then(|v| v.string(32)).is_some_and(|id| id != "codex") {
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

/// Retain the most recent complete report and the state needed to read appends.
struct CodexFile {
    version: FileVersion,
    stream: Stream,
    reading: Option<Reading>,
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
        let parse = |line: &[u8]| Reading::codex(line, now);
        if let Some(mut old) = cached {
            if version == old.version {
                return Ok(old);
            }
            let growing = version.same_file(old.version) && version.len > old.version.len;
            if growing && old.stream.can_resume(file, budget)? {
                if version.len - old.version.len <= budget.remaining - floor {
                    old.stream.read(file, version.len, budget, |line| {
                        if let Some(reading) = parse(line) {
                            old.reading = Some(reading);
                        }
                    })?;
                    if old.stream.offset != version.len {
                        return Err(io::ErrorKind::UnexpectedEof.into());
                    }
                    old.pending = parse(old.stream.tail());
                    old.version = version;
                    return Ok(old);
                }
                // A large append can push the previous report out of the bounded tail.
                retained = old.pending.or(old.reading);
            }
        }

        let (reading, stream) = files::tail(file, version.len, budget.remaining - floor, budget, parse)?;
        let pending = parse(stream.tail());
        Ok(Self { version, stream, reading: reading.or(retained), pending })
    }

    /// A valid last line is usable before its newline, but may still change.
    fn reported(&self) -> Option<&Reading> {
        self.pending.as_ref().or(self.reading.as_ref())
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
        } else if self.files.entries.is_empty() && !self.files.partial {
            CodexSessions::Missing
        } else {
            CodexSessions::Present
        };
        for candidate in &mut self.files.entries {
            if budget.remaining != 0 && !candidate.cached.as_ref().is_some_and(|file| file.version == candidate.version)
            {
                candidate.cached = root.open(&candidate.path).and_then(|(file, version)| {
                    CodexFile::read(&file, version, candidate.cached.take(), now, budget).ok()
                });
            }
            if candidate.cached.is_none() {
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
    fn reported(&self) -> Option<(&Reading, Source)> {
        self.files
            .cached()
            .filter_map(|file| file.reported().map(|r| (r, Source::CodexSession)))
            .chain(self.omp.iter().map(|r| (r, Source::Omp)))
            .max_by_key(|(r, _)| r.at)
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
            *self = Self::new(candidate);
            return true;
        };
        if !version.same_file(self.version) || version != self.version && version.len <= self.version.len {
            *self = Self::new(version);
        }
        self.version = version;
        self.samples.expire(cutoff);
        let loaded = self.read(&file, cutoff, budget);
        self.pending = if self.settled() {
            Sample::parse(self.stream.tail()).and_then(|(sample, id)| {
                let repeated = id.as_ref().is_some_and(|id| self.samples.identified.contains_key(id.as_ref()));
                (!repeated).then(|| (sample, id.map(|id| id.into_owned().into_boxed_str())))
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
    fn new(version: FileVersion) -> Self {
        Self {
            version,
            stream: Stream::resume(version.len),
            reverse: (version.len != 0).then(|| Reverse::new(version.len)),
            samples: Samples::default(),
            pending: None,
        }
    }

    /// Include a valid unterminated record provisionally until a newline commits it.
    fn identified(&self) -> impl Iterator<Item = (&str, Sample)> {
        self.samples
            .identified
            .iter()
            .map(|(id, sample)| (id.as_ref(), *sample))
            .chain(self.pending.as_ref().and_then(|(sample, id)| Some((id.as_deref()?, *sample))))
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
    status: Option<Reading>,
    native_version: Option<SnapshotVersion>,
    native: Option<Reading>,
    omp: Option<Reading>,
}

/// A partial total is a lower bound, not an exact amount of today's activity.
#[derive(Default)]
struct Activity {
    today: u64,
    partial: bool,
}

impl Claude {
    fn reported(&self) -> Option<(&Reading, Source)> {
        [(&self.status, Source::ClaudeStatus), (&self.native, Source::ClaudeCache), (&self.omp, Source::Omp)]
            .into_iter()
            .filter_map(|(reading, source)| Some((reading.as_ref()?, source)))
            .max_by_key(|(reading, _)| reading.at)
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
            if cached.as_ref().is_some_and(|file| file.version == *version && file.settled()) {
                continue;
            }
            let file = cached.get_or_insert_with(|| ClaudeFile::new(*version));
            settled &= file.check(&root, path, *version, cutoff, budget);
            self.partial |= !file.settled();
            changed = true;
        }
        self.partial |= !settled;
        if !changed {
            return settled;
        }
        let mut unique = HashMap::<&str, Sample>::new();
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
        activity.partial |= self.files.cached().any(|file| file.samples.incomplete_since.is_some_and(|at| at >= today));
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
    claude_config: Option<PathBuf>,
    omp: omp::Poll,
    codex: Codex,
    claude: Claude,
    today: Today,
    large_figures: bool,
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
        let mut usage = Self::for_home(home.as_deref(), when::now()).with_large_figures(prefs.usage_large_figures);
        usage.preference_note = note;
        usage.omp = omp::Poll::new(omp::database(home.as_deref()), omp::QUERY_WAIT);
        usage
    }

    pub fn with_large_figures(mut self, large: bool) -> Self {
        self.large_figures = large;
        self
    }

    fn for_home(home: Option<&Path>, now: i64) -> Self {
        let codex = home.map(|p| p.join(".codex/sessions"));
        let claude = home.map(|p| p.join(".claude/projects"));
        let mut usage = Self::from_dirs(codex, claude, paths::state_dir(), now);
        usage.claude_config = home.map(|p| p.join(".claude.json"));
        usage
    }

    /// Tests use the same reader without depending on a real home or clock.
    fn from_dirs(codex_dir: Option<PathBuf>, claude_dir: Option<PathBuf>, state: Option<PathBuf>, now: i64) -> Self {
        Self {
            codex_dir,
            claude_dir,
            limits_file: state.map(|dir| dir.join(SAVED_LIMITS)),
            claude_config: None,
            omp: omp::Poll::new(None, omp::QUERY_WAIT),
            codex: Codex::default(),
            claude: Claude::default(),
            today: Today::at(now, when::local_day(now)),
            large_figures: false,
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
        if manual {
            self.omp.reload(now, clock);
        } else {
            self.omp.start(now, clock);
        }
        let mut codex_budget = Budget::new(CODEX_BYTES);
        let mut claude_budget = Budget::new(CLAUDE_BYTES);
        self.check_limits(now, &mut claude_budget);
        self.codex.check(self.codex_dir.as_deref(), now, clock, manual, &mut codex_budget);
        let cutoff = self.today.since(now);
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
        let Some(reports) = result else { return checking };
        if self.codex.omp == reports.codex && self.claude.omp == reports.claude {
            return checking;
        }
        self.codex.omp = reports.codex;
        self.claude.omp = reports.claude;
        true
    }

    fn check_limits(&mut self, now: i64, claude_budget: &mut Budget) {
        if let Some(path) = &self.limits_file {
            files::read_cached(
                path,
                &mut self.claude.saved_version,
                &mut self.claude.status,
                claude_budget,
                status::saved,
            );
        }
        if let Some(path) = &self.claude_config {
            files::read_cached(
                path,
                &mut self.claude.native_version,
                &mut self.claude.native,
                claude_budget,
                |file, version, budget| status::native(file, version, budget, now),
            );
        }
    }
}

/// A bounded, read-only probe for doctor. Activity transcripts are not needed.
pub fn sources() -> [(&'static str, Option<&'static str>); 2] {
    let now = when::now();
    let home = paths::home();
    let mut usage = Usage::for_home(home.as_deref(), now);
    if let Some(reports) = omp::database(home.as_deref()).and_then(|path| omp::read(&path, now, omp::QUERY_WAIT)) {
        usage.codex.omp = reports.codex;
        usage.claude.omp = reports.claude;
    }
    usage.check_limits(now, &mut Budget::new(CLAUDE_BYTES));
    usage.codex.check(usage.codex_dir.as_deref(), now, Instant::now(), true, &mut Budget::new(CODEX_BYTES));
    [
        ("Codex", usage.codex.reported().map(|(_, source)| source.label())),
        ("Claude", usage.claude.reported().map(|(_, source)| source.label())),
    ]
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

    fn external_fd(&self) -> Option<RawFd> {
        self.omp.fd()
    }

    fn check_external(&mut self) -> External {
        if self.collect_omp() { External::Redraw } else { External::Unchanged }
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

    /// Redraw when a report gets old or its known reset elapses, even without new data.
    fn next_frame(&self) -> Option<Duration> {
        let now = self.now();
        let earliest = self
            .codex
            .reported()
            .into_iter()
            .chain(self.claude.reported())
            .flat_map(|(reading, source)| reading.windows.windows().map(move |window| (window, source)))
            .filter(|(window, _)| !window.is_reset(now))
            .flat_map(|(window, source)| [window.resets, Some(window.stale_at(source))])
            .flatten()
            .filter(|&change| change > now)
            .min();
        let wait = earliest.map_or(MINUTE, |ts| ts.saturating_sub(now).min(MINUTE));
        Some(Duration::from_secs(u64::try_from(wait).unwrap_or(60)))
    }

    fn help(&self) -> Page {
        Page::new(help_rows())
    }
}

fn help_rows() -> Vec<help::HelpRow> {
    use help::{HelpRow, heading, key, note};
    vec![
        heading("Usage limits"),
        note("Figures come from the"),
        note("tool's last report."),
        note("An elapsed reset is not"),
        note("a new reading."),
        note("Open the tool to update."),
        HelpRow::new(),
        heading("Claude Code"),
        note("Tokens today show local"),
        note("activity, not a limit."),
        note("Setup can add a status"),
        note("line to collect limits."),
        key("r", "reload"),
        key("q", "quit"),
    ]
}

/// Claude Code must get a line and a successful exit even if it sent bad data.
pub fn claude_status() -> i32 {
    let path = paths::state_dir().map(|dir| dir.join(SAVED_LIMITS));
    status::run_fd(io::stdin(), io::stdout().lock(), path.as_deref(), when::now(), status::INPUT_WAIT)
}

#[cfg(test)]
mod tests;
