//! Gives each tool with a figure a place in the frame, with less detail when
//! the pane is small. A tool with limits has a panel; activity a harness
//! recorded has a row beneath the panels, or the pane to itself.

mod digits;
mod figure;

use std::borrow::Cow;
use std::sync::LazyLock;

use crate::help;
use crate::picture::Picture;
use crate::preferences::{Show, UsageTool, UsageTools};
use crate::screen::{Screen, Style};
use crate::setup::wrap;
use crate::ui::FAINT;
use crate::when::{self, Detail};
use crate::width::str_width;

use self::figure::Figure;
use super::{Activity, CodexSessions, Limits, Source, Usage, Window, accounts, opencode};

const SIDE_BY_SIDE: usize = 64;
/// Space for two useful summaries beside each other when a third tool is required.
const THREE_SUMMARIES: usize = 52;
const LARGE_ROWS: usize = 3;
const FIGURES_PER_TOOL: usize = 3;
const TILE: &str = "██";
const NO_USAGE: [&str; 2] = ["No usage to show yet", "No usage yet"];
/// A lone panel keeps its windows together rather than spreading them over a wide pane.
const LONE: usize = 72;

#[derive(Clone, Copy)]
enum Tool {
    Codex,
    Claude,
}

impl Tool {
    fn name(self) -> &'static str {
        self.names()[0]
    }

    /// Progressively shorter names keep room for a figure on the narrowest panes.
    fn names(self) -> [&'static str; 3] {
        match self {
            Self::Codex => ["Codex", "Cx", "X"],
            Self::Claude => ["Claude", "Cl", "C"],
        }
    }

    const fn open(self) -> &'static str {
        match self {
            Self::Codex => "Open Codex",
            Self::Claude => "Open Claude Code",
        }
    }

    fn colour(self) -> Style {
        // Closest fixed ANSI colours to #10a37f and #d97757.
        Style::fg(match self {
            Self::Codex => 36,
            Self::Claude => 173,
        })
    }

    fn first_figure_slot(self) -> usize {
        match self {
            Self::Codex => 0,
            Self::Claude => FIGURES_PER_TOOL,
        }
    }

    /// Share each logo across frames instead of building a fresh image.
    fn picture(self) -> &'static Picture {
        static CODEX: LazyLock<Picture> =
            LazyLock::new(|| Picture::embedded(include_bytes!("marks/codex.png")).expect("Codex mark is a PNG"));
        static CLAUDE: LazyLock<Picture> =
            LazyLock::new(|| Picture::embedded(include_bytes!("marks/claude.png")).expect("Claude mark is a PNG"));
        match self {
            Self::Codex => &CODEX,
            Self::Claude => &CLAUDE,
        }
    }
}

#[cfg(test)]
pub(super) fn mark_placements(screen: &Screen) -> impl Iterator<Item = (usize, usize, usize, usize)> + '_ {
    [Tool::Codex, Tool::Claude].into_iter().filter_map(|tool| screen.placement_of(tool.picture()))
}

/// What the small line under a figure says about its window.
#[derive(Clone, Copy)]
enum When {
    /// The reported reset passed; no subsequent value has been observed.
    Elapsed,
    /// Seconds until the window starts over.
    Running(i64),
    Unknown,
}

impl When {
    /// Says that the figure is what is left, and when there is room, when it starts over.
    fn draw(self, screen: &mut Screen, area: Area, row: usize, historical: bool, age: Option<i64>) {
        if let Some(age) = age {
            let exact = format!("{} ago", when::duration(age, Detail::Exact));
            let age = if exact.len() <= area.w { exact } else { format!("{} ago", when::duration(age, Detail::Rounded)) };
            if age.len() <= area.w {
                if let Self::Running(seconds) = self
                    && age.len() + 10 <= area.w
                {
                    let reset = when::duration(seconds, Detail::Exact);
                    if age.len() + 9 + reset.len() <= area.w {
                        return area.note(screen, row, &format!("{age} · reset {reset}"));
                    }
                }
                return area.note(screen, row, &age);
            }
        }
        match self {
            Self::Elapsed => area.note(screen, row, if area.w >= 13 { "Reset elapsed" } else { "elapsed" }),
            Self::Running(seconds) => {
                let time = when::duration(seconds, Detail::Exact);
                if historical {
                    let text = if time.len() + 17 <= area.w {
                        format!("Reported reset · {time}")
                    } else if time.len() + 6 <= area.w {
                        format!("reset {time}")
                    } else {
                        return;
                    };
                    return area.note(screen, row, &text);
                }
                if time.len() + 17 <= area.w {
                    area.note(screen, row, &format!("left · resets in {time}"));
                } else if time.len() + 7 <= area.w {
                    area.note(screen, row, &format!("left · {time}"));
                } else {
                    area.note(screen, row, "left");
                }
            }
            Self::Unknown if area.w >= 18 => area.note(screen, row, "Reset time unknown"),
            Self::Unknown if !historical => area.note(screen, row, "left"),
            Self::Unknown => {}
        }
    }
}

/// Prepared display data lets each layout use the same figure and reset wording.
struct Metric<'a> {
    label: Cow<'a, str>,
    number: Option<Figure>,
    when: When,
    used: f64,
    /// Seconds since the figure was reported, once that is worth saying:
    /// one a few minutes old is as good as current.
    age: Option<i64>,
}

impl Metric<'_> {
    fn limit(window: Window, now: i64, source: Source) -> Self {
        let elapsed = window.is_reset(now);
        let age = (!elapsed && window.is_stale(now, source)).then(|| now.saturating_sub(window.observed));
        let when = if elapsed {
            When::Elapsed
        } else {
            window.resets.map_or(When::Unknown, |at| When::Running(at.saturating_sub(now)))
        };
        Self {
            label: window.label(),
            number: (!elapsed).then(|| {
                if age.is_some() {
                    Figure::historical(window.number())
                } else {
                    Figure::new(window.number(), window.percent)
                }
            }),
            when,
            age,
            used: window.percent,
        }
    }

    fn missing(label: &'static str) -> Self {
        Self { label: label.into(), number: None, when: When::Unknown, age: None, used: 0.0 }
    }

    fn number(&self) -> &str {
        self.number.map_or("—", |number| number.text)
    }

    fn number_width(&self) -> usize {
        self.number.map_or(1, |number| number.text.len())
    }

    fn colour(&self) -> Style {
        self.number.map_or(FAINT, Figure::style)
    }

    fn inline_value(&self) -> &str {
        if matches!(self.when, When::Elapsed) { "elapsed" } else { self.number() }
    }

    fn inline_width(&self) -> usize {
        self.label.len() + 1 + self.inline_value().len() + 5 * usize::from(self.age.is_some())
    }

    fn inline(&self, row: &mut RowWriter<'_>, label: bool) {
        if label {
            row.add(&self.label, FAINT);
            row.add(" ", FAINT);
            row.add(self.inline_value(), self.colour().bold());
            if self.age.is_some() {
                row.add(" last", FAINT);
            }
        } else {
            // Never show a stale or elapsed value with no room for its state.
            row.add(if self.age.is_some() { "—" } else { self.number() }, self.colour().bold());
        }
    }

    /// Columns the figure needs when drawn large on this screen.
    fn large_width(&self, screen: &Screen) -> usize {
        match self.number {
            Some(number) if screen.decorative_pictures() => number.cols(screen.cell_size()),
            Some(number) => digits::width(number.text),
            None => 1,
        }
    }

    /// Stay legible when either images or large text glyphs do not fit.
    fn panel(&self, screen: &mut Screen, area: Area, rows: Chart, slot: usize, shared_age: bool) {
        if self.age.is_some() {
            let suffix = if self.label.len() + 16 <= area.w { " · last reported" } else { " last" };
            if self.label.len() + suffix.len() <= area.w {
                let mut writer = RowWriter { screen, x: area.x, y: area.y + rows.label, left: area.w };
                writer.add(&self.label, FAINT);
                writer.add(suffix, FAINT);
            } else {
                area.note(screen, rows.label, "Last");
            }
        } else {
            area.note(screen, rows.label, &self.label);
        }
        self.figure(screen, area, area.y + rows.label + 1, rows.large, slot);
        if self.number.is_some() || matches!(self.when, When::Elapsed) {
            self.when.draw(screen, area, rows.note, self.age.is_some(), self.age.filter(|_| !shared_age));
        }
    }

    /// The figure as a picture, as block digits, or as plain text in the middle of their rows.
    fn figure(&self, screen: &mut Screen, area: Area, y: usize, large: bool, slot: usize) {
        match (large, self.number) {
            (true, Some(number)) if screen.decorative_pictures() && number.cols(screen.cell_size()) <= area.w => {
                number.draw(screen, area, y, slot);
            }
            (true, Some(number)) if digits::width(number.text) <= area.w => {
                digits::draw(screen, area.x, y, number.text, number.style());
            }
            _ if self.number_width() <= area.w => {
                screen.put(area.x, y + usize::from(large), area.w, self.number(), self.colour().bold());
            }
            _ => {}
        }
    }

    /// One row of words under a lone large figure, such as “of week left”,
    /// then the reading's age if it is old, or else when the window starts over.
    fn caption(&self, screen: &mut Screen, area: Area, row: usize) {
        if matches!(self.when, When::Elapsed) {
            return self.when.draw(screen, area, row, false, None);
        }
        if let Some(age) = self.age {
            let time = when::duration(age, Detail::Rounded);
            let prefix = self.label.len() + 16;
            if prefix + time.len() + 5 <= area.w {
                return area.note(screen, row, &format!("{} · last reported {time} ago", self.label));
            }
            if prefix <= area.w {
                return area.note(screen, row, &format!("{} · last reported", self.label));
            }
            return area.note(screen, row, "Last reported");
        }
        let (lead, detail) = match self.when {
            When::Elapsed => unreachable!("elapsed is handled above"),
            When::Running(seconds) => ("resets in ", Some(when::duration(seconds, Detail::Exact))),
            When::Unknown => ("", None),
        };
        let words = self.label.len() + 8;
        if words > area.w {
            return area.note(screen, row, &self.label);
        }
        let mut writer = RowWriter { screen, x: area.x, y: area.y + row, left: area.w };
        writer.add("of ", FAINT);
        writer.add(&self.label, FAINT);
        writer.add(" left", FAINT);
        if let Some(detail) = detail.filter(|detail| words + 3 + detail.len() <= area.w) {
            writer.add(" · ", FAINT);
            if words + 3 + lead.len() + detail.len() <= area.w {
                writer.add(lead, FAINT);
            }
            writer.add(&detail, FAINT);
        }
    }

    /// A window in words, for one without a large figure of its own.
    fn row(&self, screen: &mut Screen, area: Area, row: usize) {
        let mut writer = RowWriter { screen, x: area.x, y: area.y + row, left: area.w };
        self.inline(&mut writer, true);
        if self.number.is_some() && self.age.is_none() && self.inline_width() + 5 <= area.w {
            writer.add(" left", FAINT);
        }
    }
}

/// Long and short explanations let missing figures fit either layout.
#[derive(PartialEq, Eq)]
pub(super) struct Hint {
    full: &'static str,
    short: &'static str,
    state: &'static str,
    next: Option<&'static str>,
}

impl Hint {
    const CHECKING: Self = Self { full: "checking local limits", short: "checking", state: "checking", next: None };
    const CHECKING_ACTIVITY: Self =
        Self { full: "checking local activity", short: "checking", state: "checking", next: None };
    const OLD_READING: Self = Self { full: "last reported", short: "last", state: "old", next: None };
    const FUTURE_REPORT: Self =
        Self { full: "report is dated ahead", short: "future report", state: "dated ahead", next: None };
    const NO_SESSIONS: Self =
        Self { full: "no sessions found", short: "no sessions", state: "no log", next: Some("Run Codex") };
    const NO_SAVED_LIMITS: Self = Self {
        full: "no saved limits found",
        short: "no saved limits",
        state: "no limits",
        next: Some("Run milo setup"),
    };
    const UNREADABLE_LIMITS: Self = Self {
        full: "could not read saved limits",
        short: "cannot read limits",
        state: "cannot read",
        next: Some("Check limit file access"),
    };
    const UNUSABLE_LIMITS: Self = Self {
        full: "no usable saved limits",
        short: "unusable save",
        state: "no limits",
        next: Some("Open Claude Code"),
    };
    const UNREADABLE: Self = Self {
        full: "could not read sessions",
        short: "could not read",
        state: "cannot read",
        next: Some("Check session access"),
    };
    const NO_DATABASE: Self =
        Self { full: "no database found", short: "no database", state: "no source", next: Some("Run OpenCode") };
    const NO_TOKENS: Self =
        Self { full: "no tokens recorded today", short: "no tokens today", state: "no tokens", next: None };
    const ACTIVITY_OUTDATED: Self =
        Self { full: "no reading for today yet", short: "no reading today", state: "no reading", next: None };
    const UNREAD: Self = Self {
        full: "could not read its database",
        short: "could not read",
        state: "cannot read",
        next: Some("Press r to try again"),
    };
    const UNCHECKED: Self = Self {
        full: "database format not checked",
        short: "format not checked",
        state: "unknown",
        next: Some("Run milo doctor"),
    };
    const ASKING: Self = Self { full: "asking the provider", short: "checking", state: "checking", next: None };
    const NO_RECENT_ANSWER: Self =
        Self { full: "no recent answer", short: "no answer", state: "no answer", next: Some("Press r to ask again") };

    /// Why an account has no figure, and where to look next.
    const fn account(failure: accounts::Failure) -> Self {
        use accounts::Failure;
        const DOCTOR: Option<&str> = Some("Run milo doctor");
        match failure {
            Failure::NoKey => Self { full: "no key set", short: "no key", state: "no key", next: DOCTOR },
            Failure::NoScope => {
                Self { full: "account not named", short: "no account", state: "no account", next: DOCTOR }
            }
            Failure::OpenFile => Self {
                full: "settings file not private",
                short: "file not private",
                state: "not private",
                next: DOCTOR,
            },
            Failure::BadSetting => {
                Self { full: "setting cannot be sent", short: "unusable setting", state: "bad setting", next: DOCTOR }
            }
            Failure::Refused => {
                Self { full: "key was refused", short: "key refused", state: "refused", next: Some("Check the key") }
            }
            Failure::Denied => {
                Self { full: "access was denied", short: "access denied", state: "denied", next: DOCTOR }
            }
            Failure::Limited => Self { full: "rate limited", short: "rate limited", state: "limited", next: None },
            Failure::NoCurl => {
                Self { full: "curl not installed", short: "no curl", state: "no curl", next: Some("Install curl") }
            }
            Failure::Unreachable => Self {
                full: "could not reach provider",
                short: "unreachable",
                state: "offline",
                next: Some("Press r to try again"),
            },
            Failure::Unexpected => {
                Self { full: "reply not understood", short: "unknown reply", state: "unknown", next: DOCTOR }
            }
            Failure::Nothing => Self { full: "no usage reported", short: "no usage", state: "no usage", next: None },
        }
    }

    const fn no_limits(tool: Tool) -> Self {
        Self { full: "no limits to show", short: "no limits", state: "no limits", next: Some(tool.open()) }
    }

    pub(super) fn text(&self) -> &'static str {
        self.full
    }

    fn words(&self) -> [&'static str; 3] {
        [self.full, self.short, self.state]
    }

    fn line(&self, screen: &mut Screen, area: Area, tool: Tool) {
        let mut row = RowWriter { screen, x: area.x, y: area.y, left: area.w };
        let names = tool.names();
        for name in names {
            if let Some(reason) = self.words().into_iter().find(|reason| name.len() + 1 + reason.len() <= area.w) {
                let gap = if name.len() + 3 + reason.len() <= area.w { " · " } else { " " };
                row.add(name, tool.colour().bold());
                row.add(gap, FAINT);
                row.add(reason, FAINT);
                return;
            }
        }
        if let Some(name) = names.into_iter().find(|name| name.len() <= area.w) {
            row.add(name, tool.colour().bold());
            if row.left >= 2 {
                row.add(" —", FAINT);
            }
        }
    }

    /// Draws the hint, with `also` after it on the same row when both fit.
    fn draw(&self, screen: &mut Screen, area: Area, row: usize, also: Option<&str>) {
        if let Some(also) = also
            && self.full.len() + 3 + str_width(also) <= area.w
        {
            let mut writer = RowWriter { screen, x: area.x, y: area.y + row, left: area.w };
            writer.add(self.full, FAINT);
            writer.add(" · ", FAINT);
            writer.add(also, FAINT);
        } else {
            area.note(screen, row, if self.full.len() <= area.w { self.full } else { self.short });
        }
    }
}

impl Usage {
    /// A recognized report without windows is more specific than the source's
    /// absence. An in-flight search must not erase a known read failure.
    pub(super) fn limit_hint(&self, tool: UsageTool, reported: bool, now: i64) -> Hint {
        let checking = self.omp.checking();
        match tool {
            UsageTool::Codex if reported => Hint::no_limits(Tool::Codex),
            UsageTool::Codex => match self.codex.sessions {
                CodexSessions::Unreadable => Hint::UNREADABLE,
                _ if self.codex.next_reading(now).is_some() => Hint::FUTURE_REPORT,
                _ if checking => Hint::CHECKING,
                CodexSessions::Missing => Hint::NO_SESSIONS,
                CodexSessions::Present => Hint::no_limits(Tool::Codex),
            },
            UsageTool::Claude if reported => Hint::no_limits(Tool::Claude),
            UsageTool::Claude if self.claude.unreadable_limits => Hint::UNREADABLE_LIMITS,
            UsageTool::Claude if self.claude.next_reading(now).is_some() => Hint::FUTURE_REPORT,
            UsageTool::Claude
                if self.claude.status.is_none() && self.claude.saved_version.is_some_and(|version| version.len() > 0) =>
            {
                Hint::UNUSABLE_LIMITS
            }
            UsageTool::Claude if checking => Hint::CHECKING,
            UsageTool::Claude => Hint::NO_SAVED_LIMITS,
            UsageTool::OpenCode => unreachable!("OpenCode has no limits source"),
        }
    }
}

impl super::OpenCode {
    pub(super) fn activity_hint(&self, today: i64) -> Hint {
        match self.report {
            opencode::Report::Unreadable => Hint::UNREAD,
            opencode::Report::Unsupported => Hint::UNCHECKED,
            opencode::Report::Today(activity) if activity.since != today => {
                if self.poll.fd().is_some() { Hint::CHECKING_ACTIVITY } else { Hint::ACTIVITY_OUTDATED }
            }
            opencode::Report::Today(_) => Hint::NO_TOKENS,
            opencode::Report::Missing if self.poll.checking() => Hint::CHECKING_ACTIVITY,
            opencode::Report::Missing => Hint::NO_DATABASE,
        }
    }
}

struct Report<'a> {
    tool: Tool,
    first: Metric<'a>,
    extra: [Option<Metric<'a>>; 2],
    hint: Option<Hint>,
}

/// Timing is optional on a one-line summary, but an old reading still needs a label.
enum SummaryEnd {
    Plain,
    Left(Option<i64>),
    Last(Option<i64>),
}

impl<'a> Report<'a> {
    /// Leave out unreported windows; a dash stands for the whole missing report.
    fn new(tool: Tool, report: Option<(&'a Limits, Source)>, missing: Hint, now: i64) -> Self {
        let Some((limits, source)) = report.filter(|(limits, _)| limits.any()) else {
            return Self { tool, first: Metric::missing("limits"), extra: [None, None], hint: Some(missing) };
        };
        let base = limits.base.into_iter().flatten().map(|window| Metric::limit(window, now, source));
        let model = limits.model_week.as_ref().map(|model| {
            let mut metric = Metric::limit(model.window, now, source);
            metric.label = model.caption().into();
            metric
        });
        let mut figures = base.chain(model);
        let first = figures.next().expect("a nonempty report has a metric");
        Self { tool, first, extra: [figures.next(), figures.next()], hint: None }
    }

    /// What a tool without a figure is waiting for, under its name.
    fn waiting(&self) -> Option<(&'static str, &Hint)> {
        Some((self.tool.name(), self.hint.as_ref()?))
    }

    fn metrics(&self) -> impl Iterator<Item = &Metric<'a>> + Clone {
        std::iter::once(&self.first).chain(self.extra.iter().flatten())
    }

    /// Expired windows cannot block work now. Use the least remaining limit among the rest.
    fn limiting(&self) -> &Metric<'a> {
        self.metrics().reduce(|a, b| if tighter(b, a) { b } else { a }).unwrap_or(&self.first)
    }

    /// The tightest window in words. `rest` adds the tool's other windows
    /// below it, for a tool that shares the rail with no other limits.
    fn summary(
        &self,
        screen: &mut Screen,
        area: Area,
        inline_detail: bool,
        rest: bool,
        activity: impl FnOnce() -> Activity,
    ) {
        let metric = self.limiting();
        let mut line = 1;
        if let Some(hint) = &self.hint {
            hint.line(screen, area, self.tool);
            if let Some(next) = hint.next {
                area.note(screen, line, next);
                line += 1;
            }
        } else if matches!(metric.when, When::Elapsed) {
            let value = if area.w >= 17 { "reset elapsed" } else { "elapsed" };
            self.summary_line(screen, area, &metric.label, value, SummaryEnd::Plain, FAINT);
            area.note(screen, line, self.tool.open());
            line += 1;
        } else {
            let age_below = area.h > 1 && area.w >= "Last reported".len();
            let end = match metric.age {
                Some(_) if age_below => SummaryEnd::Plain,
                Some(age) => SummaryEnd::Last(inline_detail.then_some(age)),
                None => SummaryEnd::Left(match metric.when {
                    When::Running(seconds) if inline_detail => Some(seconds),
                    _ => None,
                }),
            };
            self.summary_line(screen, area, &metric.label, metric.number(), end, metric.colour());
            if let Some(age) = metric.age.filter(|_| age_below) {
                let age = compact_time(age);
                let text = if age.len() + 18 <= area.w {
                    format!("Last reported {age} ago")
                } else if age.len() + 13 <= area.w {
                    format!("Reported {age} ago")
                } else {
                    "Last reported".into()
                };
                area.note(screen, line, &text);
                line += 1;
            }
            if line < area.h {
                match metric.when {
                    When::Running(seconds) => {
                        let time = compact_time(seconds);
                        let reset = if metric.age.is_some() { "Reported reset · " } else { "Resets in " };
                        area.note(screen, line, &format!("{reset}{time}"));
                    }
                    When::Unknown => area.note(screen, line, "Reset time unknown"),
                    When::Elapsed => unreachable!("elapsed handled separately"),
                }
                line += 1;
            }
            if line < area.h && metric.age.is_some() {
                area.note(screen, line, self.tool.open());
                line += 1;
            }
        }
        if rest && self.hint.is_none() {
            for other in self.metrics().filter(|&other| !std::ptr::eq(other, metric)) {
                if line < area.h && other.inline_width() <= area.w {
                    other.row(screen, area, line);
                    line += 1;
                }
            }
        }
        if line < area.h
            && let Some(activity) = activity_text(activity())
        {
            area.note(screen, line, &activity);
        }
    }

    fn summary_line(&self, screen: &mut Screen, area: Area, label: &str, value: &str, end: SummaryEnd, style: Style) {
        let can_stand_alone = matches!(end, SummaryEnd::Left(_));
        let (required, fallback, detail) = match end {
            SummaryEnd::Plain => ("", "", None),
            SummaryEnd::Left(reset) => ("", " left", reset.map(|seconds| (" left · resets ", seconds, ""))),
            SummaryEnd::Last(age) => (" last", " last", age.map(|seconds| (" · seen ", seconds, " ago"))),
        };
        let names = self.tool.names();
        let full = |name: &str| name.len() + 1 + label.len() + 3 + value.len();
        // Timing never displaces the provider, window, or figure.
        let detail = detail.and_then(|(before, seconds, after)| {
            let room = area.w.checked_sub(full(names[0]) + str_width(before) + after.len())?;
            if room < 2 {
                return None;
            }
            let time = compact_time(seconds);
            (time.len() <= room).then_some((before, time, after))
        });
        let (before, time, after) =
            detail.as_ref().map_or((fallback, "", ""), |(before, time, after)| (*before, time.as_str(), *after));
        let suffix_width = str_width(before) + time.len() + after.len();
        let mut writer = RowWriter { screen, x: area.x, y: area.y, left: area.w };
        for name in names {
            let short_label = label.strip_suffix(" week").unwrap_or(label);
            let short_label = if short_label.len() > 7 { "model" } else { short_label };
            let label = if full(name) + suffix_width <= area.w { label } else { short_label };
            let show_label = !label.is_empty() && name.len() + 2 + label.len() + value.len() + required.len() <= area.w;
            if name.len() + 1 + value.len() + required.len() > area.w {
                continue;
            }
            writer.add(name, self.tool.colour().bold());
            if show_label {
                writer.add(" ", FAINT);
                writer.add(label, FAINT);
            }
            let gap = if writer.left >= 3 + value.len() + suffix_width { " · " } else { " " };
            writer.add(gap, FAINT);
            writer.add(value, style.bold());
            if suffix_width <= writer.left {
                writer.add(before, FAINT);
                if !time.is_empty() {
                    writer.add(time, FAINT);
                    writer.add(after, FAINT);
                }
            }
            return;
        }
        // Only a current figure can stand on its own. A historical figure
        // gives way to its status if the number and qualifier won't both fit.
        if matches!(end, SummaryEnd::Last(_)) {
            Hint::OLD_READING.line(screen, area, self.tool);
        } else if can_stand_alone {
            writer.add(if value.len() <= area.w { value } else { "—" }, style);
        } else if let Some(name) = names.into_iter().find(|name| name.len() <= area.w) {
            writer.add(name, self.tool.colour().bold());
            if writer.left >= 2 {
                writer.add(" —", FAINT);
            }
        }
    }

    /// Shorten the name and labels before giving up a reported figure.
    fn inline(&self, screen: &mut Screen, area: Area) {
        if let Some(hint) = &self.hint {
            return hint.line(screen, area, self.tool);
        }
        let first = &self.first;
        if (first.age.is_some() || matches!(first.when, When::Elapsed))
            && self.extra.iter().flatten().any(|metric| metric.number.is_some() && metric.age.is_none())
        {
            return self.summary(screen, area, false, false, Activity::default);
        }
        let name = self
            .tool
            .names()
            .into_iter()
            .find(|name| name.len() + 1 + first.number_width() <= area.w)
            .unwrap_or_default();
        let identity = if name.is_empty() { 0 } else { name.len() + 1 };
        let label = identity + first.inline_width() <= area.w;
        if !label && (first.age.is_some() || matches!(first.when, When::Elapsed)) {
            return self.summary(screen, area, false, false, Activity::default);
        }
        let mut used = identity + if label { first.inline_width() } else { first.number_width() };
        let extras = self.extra.iter().flatten().take_while(|metric| {
            let width = 2 + metric.inline_width();
            if label && used + width <= area.w {
                used += width;
                true
            } else {
                false
            }
        });
        let count = extras.count();
        let tile = label && !name.is_empty() && used + 3 <= area.w;
        let mut row = RowWriter { screen, x: area.x, y: area.y, left: area.w };
        if used > area.w {
            return;
        }
        if tile {
            row.add(TILE, self.tool.colour());
            row.add(" ", FAINT);
        }
        if !name.is_empty() {
            row.add(name, if tile { Style::PLAIN } else { self.tool.colour() });
            row.add(" ", FAINT);
        }
        first.inline(&mut row, label);
        for metric in self.extra.iter().flatten().take(count) {
            row.add("  ", FAINT);
            metric.inline(&mut row, true);
        }
        if first.number.is_some()
            && first.age.is_none()
            && self.extra.iter().flatten().take(count).all(|metric| metric.number.is_some() && metric.age.is_none())
            && row.left >= 5
        {
            row.add(" left", FAINT);
        }
    }

    /// Draws one tool in `area`. `marks` is the height both tools' marks are
    /// sized for, so that they come out the same.
    fn panel(&self, screen: &mut Screen, area: Area, marks: usize, activity: impl FnOnce() -> Activity) {
        if area.w < 17 || area.h < 3 {
            return self.inline(screen, area);
        }
        if area.w < 30 {
            if area.h == LARGE_ROWS {
                return self.summary(screen, area, false, false, activity);
            }
            return self.compact(screen, area, activity);
        }
        let chart = Chart::new(area.h);
        let lane = if area.w >= 56 { 16 } else { 10 };
        let text_row = chart.label + 1 + usize::from(chart.large);
        let column = Area { w: lane - 1, ..area };
        let mark = Area { h: marks.min(area.h), ..column };
        let beneath = self.mark(screen, mark, (4 * mark.h + 2) / 5, text_row);
        let figures = Area { x: area.x + lane, w: area.w - lane, ..area };
        let (mut count, gap) = self.figure_grid(figures.w);
        let all = || self.metrics();
        // Large figures too wide to sit side by side: the first keeps the
        // width, and the others follow below it as text.
        if chart.large && count > 1 {
            let each = (figures.w - (count - 1) * gap) / count;
            if all().take(count).any(|metric| metric.large_width(screen) > each) {
                count = 1;
            }
        }
        let metrics = all().take(count);
        // An age in the mark's lane labels the whole group only when every
        // displayed window has that age. A newer window keeps its own caption.
        let shared_age = if let Some(age) = self.first.age
            && beneath <= chart.note
            && metrics.clone().all(|metric| metric.age == Some(age))
        {
            let exact = format!("{} ago", when::duration(age, Detail::Exact));
            let text =
                if exact.len() <= column.w { exact } else { format!("{} ago", when::duration(age, Detail::Rounded)) };
            column.note(screen, chart.note, &text);
            text.len() <= column.w
        } else {
            false
        };
        let slot = self.tool.first_figure_slot();
        let width = figures.w - (count - 1) * gap;
        for (i, metric) in metrics.enumerate() {
            let start = width * i / count;
            let w = width * (i + 1) / count - start;
            metric.panel(screen, Area { x: figures.x + start + i * gap, w, ..figures }, chart, slot + i, shared_age);
        }
        // A window without a figure of its own gets a row below the captions,
        // across the whole panel once it is clear of the mark.
        let mut row = chart.note + 1;
        for metric in all().skip(count) {
            let line = if row >= beneath { area } else { figures };
            if row >= area.h || metric.inline_width() > line.w {
                break;
            }
            metric.row(screen, line, row);
            row += 1;
        }
        // Activity is not a limit, so it never sits under a window as if it
        // were that window's caption: it has a row of its own, or follows
        // the hint on the hint's row, or is left out.
        let spare = row < area.h;
        let activity = (spare || self.hint.is_some()).then(|| activity_text(activity())).flatten();
        if let Some(hint) = &self.hint {
            hint.draw(screen, figures, chart.note, activity.as_deref().filter(|_| !spare).or(hint.next));
        }
        if spare {
            if let Some(activity) = activity {
                figures.note(screen, row, &activity);
                row += 1;
            }
            if self.hint.is_none()
                && self.metrics().any(|metric| metric.age.is_some() || matches!(metric.when, When::Elapsed))
            {
                figures.note(screen, row, self.tool.open());
            }
        }
    }

    fn figure_grid(&self, width: usize) -> (usize, usize) {
        // Three columns still need room for large figures and the model's full caption.
        if self.extra[1].as_ref().is_some_and(|metric| width.saturating_sub(4) / 3 >= metric.label.len().max(15)) {
            return (3, 2);
        }
        if self.extra[0].is_none() {
            return (1, 0);
        }
        let gap = match width {
            36.. => 3,
            24..36 => 2,
            _ => 1,
        };
        (2, gap)
    }

    /// A narrow panel: the mark beside one large figure, then a row of
    /// words for each window, with nothing above the figure.
    fn compact(&self, screen: &mut Screen, area: Area, activity: impl FnOnce() -> Activity) {
        let windows = 1 + self.extra.iter().flatten().count();
        let top = area.h.saturating_sub(LARGE_ROWS + windows) / 2;
        let area = Area { y: area.y + top, h: area.h - top, ..area };
        // A mark three rows tall is six columns across; its name in words needs nine.
        let lane = if screen.pictures() == Some(true) { 8 } else { 10 };
        self.mark(screen, Area { w: lane - 1, h: LARGE_ROWS, ..area }, LARGE_ROWS, 1);
        self.first.figure(
            screen,
            Area { x: area.x + lane, w: area.w - lane, ..area },
            area.y,
            true,
            self.tool.first_figure_slot(),
        );
        let mut row = LARGE_ROWS;
        if row < area.h {
            match &self.hint {
                Some(hint) => hint.draw(screen, area, row, hint.next),
                None => self.first.caption(screen, area, row),
            }
            row += 1;
        }
        for metric in self.extra.iter().flatten() {
            if row >= area.h || metric.inline_width() > area.w {
                break;
            }
            metric.row(screen, area, row);
            row += 1;
        }
        if row < area.h
            && self.hint.is_none()
            && self.metrics().any(|metric| metric.age.is_some() || matches!(metric.when, When::Elapsed))
        {
            area.note(screen, row, self.tool.open());
            row += 1;
        }
        if row < area.h
            && let Some(activity) = activity_text(activity())
        {
            area.note(screen, row, &activity);
        }
    }

    /// Draws the tool's mark `rows` tall in the middle of `area`, and
    /// returns the first row beneath it.
    fn mark(&self, screen: &mut Screen, area: Area, rows: usize, text_row: usize) -> usize {
        if screen.decorative_pictures() {
            let mark = self.tool.picture();
            let (cols, rows) = mark.fit(screen.cell_size(), area.w, rows);
            let top = (area.h - rows) / 2;
            screen.show_picture(mark, area.x, area.y + top, cols, rows);
            top + rows
        } else {
            let mut row = RowWriter { screen, x: area.x, y: area.y + text_row, left: area.w };
            row.add(TILE, self.tool.colour());
            row.add(" ", FAINT);
            row.add(self.tool.name(), Style::PLAIN);
            text_row + 1
        }
    }
}

fn tighter(a: &Metric<'_>, b: &Metric<'_>) -> bool {
    let rank = |metric: &Metric<'_>| match (&metric.number, metric.age, metric.when) {
        (Some(_), None, _) => 3,
        (Some(_), Some(_), _) => 2,
        (None, _, When::Elapsed) => 1,
        _ => 0,
    };
    let seconds = |metric: &Metric<'_>| match metric.when {
        When::Running(seconds) => seconds,
        _ => i64::MAX,
    };
    let a_rank = rank(a);
    let b_rank = rank(b);
    a_rank.cmp(&b_rank).then_with(|| a.used.total_cmp(&b.used)).then_with(|| seconds(b).cmp(&seconds(a))).is_gt()
}

fn compact_time(seconds: i64) -> String {
    let mut time = when::duration(seconds, Detail::Exact);
    time.retain(|c| c != ' ');
    time
}

/// Leaves a row for a caption, and one more for activity when it fits.
#[derive(Clone, Copy)]
struct Chart {
    label: usize,
    note: usize,
    large: bool,
}

impl Chart {
    fn new(height: usize) -> Self {
        let large = height >= LARGE_ROWS + 2;
        let body = if large { LARGE_ROWS + 2 } else { 3 };
        let label = height.saturating_sub(body + 1) / 2;
        Self { label, note: label + body - 1, large }
    }
}

/// A part of the frame measured in cells.
#[derive(Clone, Copy)]
struct Area {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl Area {
    fn split(self, gap: usize) -> (Self, Self) {
        let left = (self.w - gap) / 2;
        (Self { w: left, ..self }, Self { x: self.x + left + gap, w: self.w - left - gap, ..self })
    }

    /// Keep a little breathing room without losing columns on a narrow pane.
    fn inset(self) -> Self {
        let pad = if self.w >= 56 { 2 } else { usize::from(self.w >= 16) };
        Self { x: self.x + pad, w: self.w - 2 * pad, ..self }
    }

    /// Leave off an explanation rather than show a misleading fragment.
    fn note(self, screen: &mut Screen, row: usize, text: &str) {
        if row < self.h && str_width(text) <= self.w {
            screen.put(self.x, self.y + row, self.w, text, FAINT);
        }
    }
}

/// Writes styled fragments without measuring and clipping each one by hand.
struct RowWriter<'a> {
    screen: &'a mut Screen,
    x: usize,
    y: usize,
    left: usize,
}

impl RowWriter<'_> {
    fn add(&mut self, text: &str, style: Style) {
        let used = self.screen.put(self.x, self.y, self.left, text, style);
        self.x += used;
        self.left -= used;
    }
}

/// What was used today, in words, or nothing when nothing was.
pub(super) fn activity_text(activity: Activity) -> Option<String> {
    let more = if activity.partial { "+" } else { "" };
    (activity.today > 0).then(|| format!("{}{more} tokens today", tokens(activity.today)))
}

/// Keep token counts short enough for the strip without rounding across units.
fn tokens(tokens: u64) -> String {
    const UNITS: [(u64, char); 6] = [
        (1000, 'k'),
        (1_000_000, 'M'),
        (1_000_000_000, 'B'),
        (1_000_000_000_000, 'T'),
        (1_000_000_000_000_000, 'P'),
        (1_000_000_000_000_000_000, 'E'),
    ];
    let Some((base, suffix)) = UNITS.into_iter().rev().find(|&(base, _)| tokens >= base) else {
        return tokens.to_string();
    };
    let whole = tokens / base;
    let fraction = (tokens % base) / (base / 10);
    if fraction == 0 || whole >= 100 { format!("{whole}{suffix}") } else { format!("{whole}.{fraction}{suffix}") }
}

/// Fills rows from `row` with as many of the items as fit on each, in order.
fn flow(screen: &mut Screen, area: Area, mut row: usize, items: impl Iterator<Item = String>) {
    let mut line = String::new();
    for item in items {
        if !line.is_empty() && str_width(&line) + 3 + str_width(&item) > area.w {
            area.note(screen, row, &line);
            row += 1;
            line.clear();
        }
        if !line.is_empty() {
            line.push_str(" · ");
        }
        line.push_str(&item);
    }
    area.note(screen, row, &line);
}

/// OpenCode's place in the frame. It saves no limits, so it never stands
/// between two tools that do: it takes the place a second one would have had,
/// or one row beneath both, or the pane to itself.
enum Harness<'a> {
    Today(&'a opencode::Activity),
    /// Shown only when asked for, or when no tool has a figure.
    Unknown(Hint),
}

impl<'a> Harness<'a> {
    const NAMES: [&'static str; 2] = ["OpenCode", "OC"];

    fn new(usage: &'a Usage, today: i64) -> Self {
        if let Some(activity) = usage.opencode.today(today) {
            return Self::Today(activity);
        }
        Self::Unknown(usage.opencode.activity_hint(today))
    }

    /// The name and what follows it, from the fullest wording to the barest.
    /// `priced` adds the cost where no other row will carry it.
    fn wording(&self, choice: usize, priced: bool) -> Option<(&'static str, String)> {
        let [name, short] = Self::NAMES;
        Some(match (self, choice) {
            (Self::Today(activity), 0) => {
                let cost = activity.cost.and_then(opencode::Cost::text).filter(|_| priced)?;
                (name, format!(" · {} tokens today · {cost} est.", tokens(activity.tokens.fresh())))
            }
            (Self::Today(activity), 1) => (name, format!(" · {} tokens today", tokens(activity.tokens.fresh()))),
            (Self::Today(activity), 2) => (name, format!(" {} tokens today", tokens(activity.tokens.fresh()))),
            (Self::Today(activity), 3) => (name, format!(" {} tokens", tokens(activity.tokens.fresh()))),
            (Self::Today(activity), 4) => (name, format!(" {}", tokens(activity.tokens.fresh()))),
            (Self::Today(activity), _) => (short, format!(" {}", tokens(activity.tokens.fresh()))),
            (Self::Unknown(hint), 0) => (name, format!(" · {}", hint.full)),
            (Self::Unknown(hint), 1) => (name, format!(" · {}", hint.short)),
            (Self::Unknown(hint), 2) => (name, format!(" · {}", hint.state)),
            (Self::Unknown(_), 3) => (name, String::new()),
            (Self::Unknown(_), _) => (short, String::new()),
        })
    }

    /// The longest wording that fits `width`, or nothing when even the barest does not.
    fn fitting(&self, width: usize, priced: bool) -> Option<(&'static str, String)> {
        (0..6)
            .filter_map(|choice| self.wording(choice, priced))
            .find(|(name, text)| name.len() + str_width(text) <= width)
    }

    fn line(&self, screen: &mut Screen, area: Area, priced: bool) {
        if let Some((name, text)) = self.fitting(area.w, priced) {
            let mut row = RowWriter { screen, x: area.x, y: area.y, left: area.w };
            row.add(name, Style::PLAIN.bold());
            row.add(&text, FAINT);
        }
    }

    /// What else OpenCode recorded today, the most useful first.
    fn details(activity: &opencode::Activity, now: i64) -> impl Iterator<Item = String> {
        let cost = activity.cost.and_then(opencode::Cost::text).map(|cost| format!("{cost} est."));
        let steps = if activity.steps == 1 { "1 step".into() } else { format!("{} steps", activity.steps) };
        let last = activity.last.map(|at| format!("last {} ago", compact_time(now.saturating_sub(at))));
        let reads = activity.tokens.cache_read;
        let reads = (reads > 0).then(|| format!("{} cache reads", tokens(reads)));
        [cost, Some(steps), last, reads].into_iter().flatten()
    }

    /// The rail's form: the name and today's tokens, then the details in the rows below.
    fn summary(&self, screen: &mut Screen, area: Area, now: i64) {
        self.line(screen, area, area.h == 1);
        match self {
            Self::Today(activity) => flow(screen, area, 1, Self::details(activity, now)),
            Self::Unknown(hint) => area.note(screen, 1, hint.next.unwrap_or_default()),
        }
    }

    /// Alone in a roomy pane: the name beside today's tokens drawn large.
    fn panel(&self, screen: &mut Screen, area: Area, now: i64) {
        let lane = if area.w >= 56 { 16 } else { 12 };
        let (Self::Today(activity), true) = (self, area.w >= 30 && area.h >= LARGE_ROWS + 2) else {
            return self.summary(screen, area, now);
        };
        let used = tokens(activity.tokens.fresh());
        let figures = Area { x: area.x + lane, w: area.w - lane, ..area };
        if digits::width(&used) > figures.w {
            return self.summary(screen, area, now);
        }
        let chart = Chart::new(area.h);
        let mut name = RowWriter { screen, x: area.x, y: area.y + chart.label + 2, left: lane - 1 };
        name.add(TILE, Style::PLAIN);
        name.add(" ", FAINT);
        name.add(Self::NAMES[0], Style::PLAIN);
        figures.note(screen, chart.label, "tokens today");
        digits::draw(screen, figures.x, figures.y + chart.label + 1, &used, Style::PLAIN);
        flow(screen, figures, chart.note, Self::details(activity, now));
    }
}

/// An account's place in the frame. Its figure is money or a count the
/// provider reported, never a share of a limit, so it is written in words on
/// one row: beneath the tools, or in rows of its own when no tool has a place.
struct AccountRow {
    name: &'static str,
    short: &'static str,
    show: Show,
    status: accounts::Status,
    hint: Option<Hint>,
}

/// Why an account has no figure to show, if it has none.
fn account_hint(status: accounts::Status) -> Option<Hint> {
    match status {
        accounts::Status::Reading(..) => None,
        accounts::Status::Checking => Some(Hint::ASKING),
        accounts::Status::Failed(failure) => Some(Hint::account(failure)),
        accounts::Status::Outdated => Some(Hint::NO_RECENT_ANSWER),
    }
}

/// What the provider said besides the figure, the most useful first.
fn account_details(status: accounts::Status) -> impl Iterator<Item = String> {
    let reading = match status {
        // Parts of an old answer are left out: the figure and its age are enough.
        accounts::Status::Reading(reading, None) => Some(reading),
        _ => None,
    };
    let depleted = reading.filter(|reading| reading.depleted).map(|_| "too low for requests".to_owned());
    depleted.into_iter().chain(reading.into_iter().flat_map(|r| r.details).flatten().map(accounts::Figure::text))
}

/// An account in one sentence for the menu: its figure with what else the
/// provider said, an old figure with its age, or why there is none.
pub(super) fn account_text(status: accounts::Status) -> String {
    match (status, account_hint(status)) {
        (accounts::Status::Reading(reading, Some(age)), _) => {
            format!("{} · {} ago", reading.figure.text(), compact_time(age))
        }
        (accounts::Status::Reading(reading, None), _) => {
            let mut text = reading.figure.text();
            for detail in account_details(status) {
                text.push_str(" · ");
                text.push_str(&detail);
            }
            text
        }
        (_, hint) => {
            // These fixed captions are ASCII, so the first byte is the first letter.
            let words = hint.map_or("", |hint| hint.full);
            let mut text = words.to_owned();
            text[..words.len().min(1)].make_ascii_uppercase();
            text
        }
    }
}

impl AccountRow {
    fn new(account: &accounts::Account, now: i64) -> Self {
        let status = account.status(now);
        let (name, short) = (account.which.label(), account.which.short());
        Self { name, short, show: account.show, status, hint: account_hint(status) }
    }

    /// Auto gives a place only to a figure, as it does for a tool.
    fn placed(&self) -> bool {
        self.show == Show::Always || self.hint.is_none()
    }

    fn details(&self) -> impl Iterator<Item = String> {
        account_details(self.status)
    }

    /// The name and what follows it, from the fullest wording to the barest.
    /// `details` adds what else the provider said where no other row will.
    /// An old figure is never worded without saying that it is old.
    fn wordings(&self, details: bool) -> Vec<(&'static str, String)> {
        let (name, short) = (self.name, self.short);
        match (&self.status, &self.hint) {
            (accounts::Status::Reading(reading, Some(age)), _) => {
                let (figure, amount, age) = (reading.figure.text(), reading.figure.amount(), compact_time(*age));
                vec![
                    (name, format!(" · {figure} · {age} ago")),
                    (name, format!(" {figure} · {age} ago")),
                    (name, format!(" {amount} last")),
                    (short, format!(" {amount} last")),
                    (name, " · old".into()),
                    (short, String::new()),
                ]
            }
            (accounts::Status::Reading(reading, None), _) => {
                let (figure, amount) = (reading.figure.text(), reading.figure.amount());
                let details: Vec<String> = self.details().filter(|_| details).collect();
                let mut wordings: Vec<_> = (0..=details.len())
                    .rev()
                    .map(|count| {
                        let mut text = format!(" · {figure}");
                        for detail in &details[..count] {
                            text.push_str(" · ");
                            text.push_str(detail);
                        }
                        (name, text)
                    })
                    .collect();
                wordings.extend([
                    (name, format!(" {figure}")),
                    (name, format!(" {amount}")),
                    (short, format!(" {amount}")),
                ]);
                wordings
            }
            (_, Some(hint)) => vec![
                (name, format!(" · {}", hint.full)),
                (name, format!(" · {}", hint.short)),
                (name, format!(" · {}", hint.state)),
                (name, String::new()),
                (short, String::new()),
            ],
            (_, None) => Vec::new(),
        }
    }

    /// One row: the longest wording that fits, or nothing when even the barest does not.
    fn line(&self, screen: &mut Screen, area: Area, row: usize, details: bool) {
        let wordings = self.wordings(details);
        let fitting = wordings.iter().find(|(name, text)| name.len() + str_width(text) <= area.w);
        if let Some((name, text)) = fitting.filter(|_| row < area.h) {
            let mut writer = RowWriter { screen, x: area.x, y: area.y + row, left: area.w };
            writer.add(name, Style::PLAIN.bold());
            writer.add(text, FAINT);
        }
    }

    /// With the pane to themselves, accounts are listed from the top. One
    /// alone has the rows below for what else its provider said.
    fn alone(rows: &[&Self], screen: &mut Screen, area: Area) {
        if let [row] = rows {
            row.line(screen, area, 0, area.h == 1);
            match &row.hint {
                Some(hint) => area.note(screen, 1, hint.next.unwrap_or_default()),
                None if area.h > 1 => flow(screen, area, 1, row.details()),
                None => {}
            }
            return;
        }
        for (i, row) in rows.iter().enumerate() {
            row.line(screen, area, i, true);
        }
    }
}

/// Two places beside each other, with a line between them.
fn beside(screen: &mut Screen, area: Area) -> [Area; 2] {
    let (left, right) = area.split(1);
    for row in area.y..area.y + area.h {
        screen.put(left.x + left.w, row, 1, "│", FAINT);
    }
    [left.inset(), right.inset()]
}

/// Two places one above the other. The lower may be a row taller.
fn above(screen: &mut Screen, area: Area) -> [Area; 2] {
    let rule = usize::from(area.h > 2);
    let top = (area.h - rule) / 2;
    if rule > 0 {
        for x in 0..area.w {
            screen.put(x, area.y + top, 1, "─", FAINT);
        }
    }
    [Area { h: top, ..area }.inset(), Area { y: area.y + top + rule, h: area.h - top - rule, ..area }.inset()]
}

/// A rail's two halves, with a rule between them once each has two rows.
/// On a sidebar, names and the most limiting window come before decoration.
fn halves(screen: &mut Screen, area: Area) -> [Area; 2] {
    let rule = usize::from(area.h >= 5);
    let upper = (area.h - rule) / 2;
    if rule != 0 {
        for x in area.x..area.x + area.w {
            screen.put(x, area.y + upper, 1, "─", FAINT);
        }
    }
    [Area { h: upper, ..area }, Area { y: area.y + upper + rule, h: area.h - upper - rule, ..area }]
}

/// One quiet block when nothing has a place: what each tool Milo found and
/// each account that is turned on is waiting for, or that no enabled source
/// has a reading.
fn quiet(screen: &mut Screen, area: Area, waiting: &[(&str, &Hint)], checking: bool, tools: UsageTools) {
    let title = |screen: &mut Screen, [long, short]: [&str; 2]| {
        if let Some(text) = [long, short].into_iter().find(|text| str_width(text) <= area.w) {
            screen.put(area.x, area.y, area.w, text, Style::PLAIN);
        }
    };
    // A sentence is shown whole or not at all. Too few rows leave its brief form.
    let words = |screen: &mut Screen, sentence: &str, brief: &str| {
        if wrap(sentence, area.w).count() < area.h {
            for (i, line) in wrap(sentence, area.w).enumerate() {
                area.note(screen, 1 + i, line);
            }
        } else {
            area.note(screen, 1, brief);
        }
    };
    let count = waiting.len();
    if count == 0 {
        let any_enabled = UsageTool::ALL.into_iter().any(|tool| tools[tool] != Show::Hide);
        if checking {
            title(screen, ["Checking local usage…", "Checking…"]);
        } else if !any_enabled {
            title(screen, ["Usage is hidden", "Hidden"]);
            words(screen, "Every tool is hidden. Change this in milo setup.", "See milo setup");
        } else {
            title(screen, NO_USAGE);
        }
        return;
    }
    // A lone tool has room for its next step. The title gives way to both.
    let next = waiting.first().filter(|_| count == 1).and_then(|(_, hint)| hint.next);
    let mut row = usize::from(area.h > count + usize::from(next.is_some()));
    if row != 0 {
        title(screen, NO_USAGE);
    }
    for (name, hint) in waiting {
        let text = hint.words().into_iter().find(|text| name.len() + 3 + text.len() <= area.w);
        if row < area.h && name.len() <= area.w {
            let mut writer = RowWriter { screen, x: area.x, y: area.y + row, left: area.w };
            writer.add(name, Style::PLAIN);
            if let Some(text) = text {
                writer.add(" · ", FAINT);
                writer.add(text, FAINT);
            }
        }
        row += 1;
    }
    area.note(screen, row, next.unwrap_or_default());
}

/// Choose a grid that leaves room for a useful figure at each supported size.
pub(super) fn usage(screen: &mut Screen, usage: &Usage, today: i64, now: i64) {
    screen.clear();
    let (w, h) = (screen.w, screen.h);
    if w == 0 || h == 0 {
        return;
    }
    let tools = usage.tools;
    let checking = usage.omp.checking();
    let codex_report = usage.codex.reported(now);
    let claude_report = usage.claude.reported(now);
    let presence =
        [usage.codex.presence(codex_report), usage.claude.presence(claude_report), usage.opencode.presence(today)];
    let [codex, claude, opencode] = UsageTool::ALL.map(|tool| tools[tool]);
    // A tool Milo found keeps its hint ready for when no tool has a place.
    let reports = [
        presence[0].relevant(codex).then(|| {
            Report::new(
                Tool::Codex,
                codex_report.map(|(r, source)| (&r.windows, source)),
                usage.limit_hint(UsageTool::Codex, codex_report.is_some(), now),
                now,
            )
        }),
        presence[1].relevant(claude).then(|| {
            Report::new(
                Tool::Claude,
                claude_report.map(|(r, source)| (&r.windows, source)),
                usage.limit_hint(UsageTool::Claude, claude_report.is_some(), now),
                now,
            )
        }),
    ];
    let harness = presence[2].relevant(opencode).then(|| Harness::new(usage, today));
    let limits = [
        reports[0].as_ref().filter(|_| presence[0].placed(codex)),
        reports[1].as_ref().filter(|_| presence[1].placed(claude)),
    ];
    let count = limits.iter().flatten().count();
    let split = h == 1 || h >= 3 && w >= SIDE_BY_SIDE;
    // At two rows, give each provider a full line for its tightest limit.
    // Leave the captions and activity their rows in a taller compact pane.
    let text = h == 2 || w < SIDE_BY_SIDE && (!usage.large_figures || w < 19 || h < 9);
    let footer = w >= 8 && h >= if split { 7 } else { 14 };
    let rows = h - usize::from(footer);
    let pad = usize::from(w >= 24);
    let placed = harness.as_ref().filter(|_| presence[2].placed(opencode));
    let accounts: Vec<AccountRow> = usage.accounts.iter().map(|account| AccountRow::new(account, now)).collect();
    let listed: Vec<&AccountRow> = accounts.iter().filter(|row| row.placed()).collect();
    // An account's figure is not a limit either. Under tools it has a row of
    // its own at the foot of the pane, once every tool above has the rows it
    // needs: limits keep their places first, then OpenCode, then accounts.
    let tools_placed = count + usize::from(placed.is_some());
    let least = match (text, count) {
        (_, 0) if placed.is_none() => rows,
        (true, 2) if placed.is_some() => 3,
        (true, 2) => 1 + usize::from(w < THREE_SUMMARIES),
        (true, _) => tools_placed,
        (false, _) if tools_placed == 2 && !split => 7,
        (false, _) => 3,
    };
    let beneath_accounts = listed.len().min(rows.saturating_sub(least));
    let rows = rows - beneath_accounts;
    // Harness activity is not a provider's limit. Beneath two tools with
    // limits it has one row, above the key hints and in line with them.
    // Beside panels it waits for a pane two rows taller than the hints need,
    // so that a panel with three windows still has a row for today's tokens.
    // On a wide two-row rail, an explicitly enabled OpenCode shares the
    // space without changing the pane height or displacing either limit tool.
    let inset = if text { pad } else { 1 };
    let line = Area { x: inset, y: rows - 1, w: w.saturating_sub(2 * inset), h: 1 };
    let room = if text {
        rows > 2 || rows == 2 && opencode == Show::Always && w >= THREE_SUMMARIES
    } else {
        w >= 8 && h >= if split { 9 } else { 16 }
    };
    let beneath = placed.filter(|harness| count == 2 && room && harness.fitting(line.w, true).is_some());
    // The rule needs two rows left for each tool above it, and their own rule.
    let ruled = text && beneath.is_some() && rows > 6;
    let taken = usize::from(beneath.is_some()) + usize::from(ruled);
    let area = Area { x: 0, y: 0, w, h: rows - taken };
    let rail = Area { x: pad, w: w - 2 * pad, ..area };
    let activity = || usage.claude.activity(today, now);
    match (limits, placed) {
        ([Some(codex), Some(claude)], _) if text && rail.h == 1 && (beneath.is_some() || beneath_accounts > 0) => {
            let [left, right] = beside(screen, rail);
            codex.summary(screen, left, false, false, Activity::default);
            claude.summary(screen, right, false, false, activity);
        }
        ([Some(codex), Some(claude)], _) if text && rail.h == 1 => {
            // One row goes to whichever tool is nearer its limit.
            let tightest = if tighter(claude.limiting(), codex.limiting()) { claude } else { codex };
            tightest.summary(screen, rail, false, false, activity);
        }
        ([Some(codex), Some(claude)], _) if text => {
            let [upper, lower] = halves(screen, rail);
            codex.summary(screen, upper, rail.h == 2, false, Activity::default);
            claude.summary(screen, lower, rail.h == 2, false, activity);
        }
        ([Some(codex), Some(claude)], _) => {
            let [first, second] = if split { beside(screen, area) } else { above(screen, area) };
            // Both marks are sized for the shorter place.
            let marks = first.h.min(second.h);
            codex.panel(screen, first, marks, Activity::default);
            claude.panel(screen, second, marks, activity);
        }
        // One tool with limits leaves no empty half: OpenCode takes the
        // other place when it has something to show, and otherwise the tool
        // has the frame to itself, with its other windows listed on a rail.
        ([Some(one), None] | [None, Some(one)], other) => {
            let tokens = || if matches!(one.tool, Tool::Claude) { activity() } else { Activity::default() };
            match other {
                Some(harness) if text && rail.h > 1 => {
                    let [upper, lower] = halves(screen, rail);
                    one.summary(screen, upper, upper.h == 1, true, tokens);
                    harness.summary(screen, lower, now);
                }
                Some(harness) if !text => {
                    let [first, second] = if split { beside(screen, area) } else { above(screen, area) };
                    one.panel(screen, first, first.h.min(second.h), tokens);
                    harness.panel(screen, second, now);
                }
                // With one row for both, the limit comes first.
                _ if text => one.summary(screen, rail, rail.h == 1, true, tokens),
                _ => one.panel(screen, Area { w: area.w.min(LONE), ..area }.inset(), area.h, tokens),
            }
        }
        ([None, None], Some(harness)) if text => harness.summary(screen, rail, now),
        ([None, None], Some(harness)) => harness.panel(screen, area.inset(), now),
        ([None, None], None) if !listed.is_empty() => AccountRow::alone(&listed, screen, rail),
        ([None, None], None) => {
            let waiting: Vec<(&str, &Hint)> = reports
                .iter()
                .filter_map(|report| report.as_ref().and_then(Report::waiting))
                .chain(match &harness {
                    Some(Harness::Unknown(hint)) => Some((Harness::NAMES[0], hint)),
                    _ => None,
                })
                .chain(accounts.iter().filter_map(|row| Some((row.name, row.hint.as_ref()?))))
                .collect();
            let checking = checking || usage.opencode.poll.checking() || usage.accounts.checking();
            quiet(screen, rail, &waiting, checking, tools);
        }
    }
    if let Some(harness) = beneath {
        if ruled {
            for x in rail.x..rail.x + rail.w {
                screen.put(x, area.h, 1, "─", FAINT);
            }
        }
        harness.line(screen, line, true);
    }
    let foot = Area { x: inset, y: rows, w: w.saturating_sub(2 * inset), h: beneath_accounts };
    for (i, row) in listed.iter().take(beneath_accounts).enumerate() {
        row.line(screen, foot, i, true);
    }
    if footer {
        help::draw_hint(screen, "r reload", &["q quit"], "? help");
    }
}

#[cfg(test)]
mod tests {
    use super::super::ModelWeek;
    use super::*;

    fn labels(limits: Option<&Limits>) -> Vec<String> {
        let report = Report::new(Tool::Claude, limits.map(|l| (l, Source::Omp)), Hint::NO_SAVED_LIMITS, 1000);
        std::iter::once(&report.first)
            .chain(report.extra.iter().flatten())
            .map(|metric| metric.label.to_string())
            .collect()
    }

    #[test]
    fn claude_shows_the_windows_the_account_has_and_no_empty_place_for_one_it_lacks() {
        let window = |minutes| Window { minutes, percent: 27.0, resets: Some(5000), observed: 1000 };
        let week = Limits::new([None, Some(window(10_080))]);
        assert_eq!(labels(Some(&week)), ["week"]);
        let model_week = ModelWeek::new("fable", window(10_080));
        assert_eq!(labels(Some(&Limits { model_week, ..week })), ["week", "Fable week"]);
        let all = Limits { base: [Some(window(300)), Some(window(10_080))], model_week };
        assert_eq!(labels(Some(&all)), ["5h", "week", "Fable week"]);
        assert_eq!(labels(None), ["limits"]);
    }
}
