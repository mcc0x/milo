//! Gives both tools the same frame, with less detail when the pane is small.

mod digits;
mod figure;

use std::borrow::Cow;
use std::sync::LazyLock;

use crate::picture::Picture;
use crate::screen::{Screen, Style};
use crate::ui::FAINT;
use crate::when::{self, Detail};
use crate::width::str_width;

use self::figure::Figure;
use super::{AGE_NOTE, Activity, Limits, Reading, Usage, Window};

const SIDE_BY_SIDE: usize = 64;
// Two short names, two four-character figures and the rule.
const ONE_LINE_SPLIT: usize = 15;
const TILE: &str = "██";

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

    fn colour(self) -> Style {
        // Closest fixed ANSI colours to #10a37f and #d97757.
        Style::fg(match self {
            Self::Codex => 36,
            Self::Claude => 173,
        })
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
    /// The window has started over since the figure was reported.
    Reset,
    /// Seconds until the window starts over.
    Running(i64),
    Unknown,
}

impl When {
    fn draw(self, screen: &mut Screen, area: Area, row: usize) {
        match self {
            Self::Reset => area.note(screen, row, "reset"),
            Self::Running(left) => {
                let left = when::duration(left, Detail::Exact);
                let prefix = if left.len() + 10 <= area.w { "resets in " } else { "in " };
                area.note(screen, row, &format!("{prefix}{left}"));
            }
            Self::Unknown => {}
        }
    }
}

/// Prepared display data lets each layout use the same figure and reset wording.
struct Metric {
    label: Cow<'static, str>,
    number: Option<Figure>,
    when: When,
    /// Seconds since the figure was reported, once that is worth saying:
    /// one a few minutes old is as good as current.
    age: Option<i64>,
}

impl Metric {
    /// A known, elapsed window is back at zero even before another report arrives.
    fn limit(mut window: Window, now: i64) -> Self {
        let age = now.saturating_sub(window.observed);
        let label = match window.minutes {
            10_080 => "week".into(),
            300 => "5h".into(),
            1440 => "day".into(),
            n if n.is_multiple_of(1440) => format!("{}d", n / 1440).into(),
            n if n.is_multiple_of(60) => format!("{}h", n / 60).into(),
            n => format!("{n}m").into(),
        };
        let (when, age) = if window.is_reset(now) {
            window.percent = 0.0;
            (When::Reset, None)
        } else {
            let when = window.resets.map_or(When::Unknown, |at| When::Running(at.saturating_sub(now)));
            (when, (age >= AGE_NOTE).then_some(age))
        };
        Self { label, number: Some(Figure::new(window.number(), window.percent)), when, age }
    }

    fn missing(label: &'static str) -> Self {
        Self { label: label.into(), number: None, when: When::Unknown, age: None }
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

    fn inline_width(&self) -> usize {
        self.label.len() + 1 + self.number_width()
    }

    fn inline(&self, row: &mut RowWriter<'_>, label: bool) {
        if label {
            row.add(&self.label, FAINT);
            row.add(" ", FAINT);
        }
        row.add(self.number(), self.colour().bold());
    }

    /// Stay legible when either images or large text glyphs do not fit.
    fn panel(&self, screen: &mut Screen, area: Area, rows: Chart, slot: usize) {
        area.note(screen, rows.label, &self.label);
        let y = area.y + rows.label + 1;
        match (rows.large, self.number) {
            (true, Some(number)) if screen.pictures() == Some(true) => number.draw(screen, area, y, slot),
            (true, Some(number)) if digits::width(number.text) <= area.w => {
                digits::draw(screen, area.x, y, number.text, number.style());
            }
            _ if self.number_width() <= area.w => {
                screen.put(area.x, y + usize::from(rows.large), area.w, self.number(), self.colour().bold());
            }
            _ => {}
        }
        self.when.draw(screen, area, rows.note);
    }
}

/// Long and short explanations let missing figures fit either layout.
struct Hint {
    full: &'static str,
    short: &'static str,
}

impl Hint {
    /// Draws the hint, with `also` after it on the same row when both fit.
    fn draw(&self, screen: &mut Screen, area: Area, row: usize, also: Option<String>) {
        match also {
            Some(also) if self.full.len() + 3 + str_width(&also) <= area.w => {
                area.note(screen, row, &format!("{} · {also}", self.full));
            }
            _ => area.note(screen, row, if self.full.len() <= area.w { self.full } else { self.short }),
        }
    }
}

/// Each provider can show at most two figures without crowding the strip.
struct Report {
    tool: Tool,
    first: Metric,
    second: Option<Metric>,
    hint: Option<Hint>,
}

impl Report {
    /// Distinguish missing sessions from unreadable or unreported limits.
    fn codex(reading: Option<Reading>, no_sessions: bool, now: i64) -> Self {
        let [first, second] = figures(reading.map(|reading| reading.windows), now);
        let (first, second, hint) = match (first, second) {
            (Some(first), second) => (first, second, None),
            (None, Some(second)) => (second, None, None),
            (None, None) => (
                Metric::missing("limits"),
                None,
                Some(if reading.is_some() {
                    Hint { full: "could not read Codex's figures", short: "could not read" }
                } else if no_sessions {
                    Hint { full: "no Codex sessions here", short: "no Codex sessions" }
                } else {
                    Hint { full: "no limits reported", short: "no limits" }
                }),
            ),
        };
        Self { tool: Tool::Codex, first, second, hint }
    }

    /// Shows the windows the account has, in the order 5h, week, one
    /// model's week. A window nobody reported gets no empty place, and the
    /// two slots go to the first two there are.
    fn claude(limits: Option<Limits>, now: i64) -> Self {
        let model = limits.and_then(|limits| limits.model_week).map(|model| {
            let mut metric = Metric::limit(model.window, now);
            metric.label = format!("{} week", model.label()).into();
            metric
        });
        let mut known = figures(limits, now).into_iter().flatten().chain(model);
        let (first, second) = match known.next() {
            Some(first) => (first, known.next()),
            None => (Metric::missing("5h"), Some(Metric::missing("week"))),
        };
        Self {
            tool: Tool::Claude,
            first,
            second,
            hint: limits.is_none().then_some(Hint { full: "turn on in milo setup", short: "milo setup" }),
        }
    }

    fn has_number(&self) -> bool {
        self.first.number.is_some() || self.second.as_ref().is_some_and(|second| second.number.is_some())
    }

    /// Shorten the name and labels before giving up a reported figure.
    fn inline(&self, screen: &mut Screen, area: Area) {
        let (first, second) = self.inline_metrics(area.w);
        let name = self
            .tool
            .names()
            .into_iter()
            .find(|name| name.len() + 1 + first.number_width() <= area.w)
            .unwrap_or_default();
        let identity = if name.is_empty() { 0 } else { name.len() + 1 };
        let label = identity + first.inline_width() <= area.w;
        let used = identity + if label { first.inline_width() } else { first.number_width() };
        let second = second.filter(|second| label && used + 2 + second.inline_width() <= area.w);
        let tile =
            label && !name.is_empty() && used + second.map_or(0, |second| second.inline_width() + 2) + 3 <= area.w;
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
        if let Some(second) = second {
            row.add("  ", FAINT);
            second.inline(&mut row, true);
        }
    }

    /// When one figure fits, prefer a limit somebody has actually reported.
    fn inline_metrics(&self, width: usize) -> (&Metric, Option<&Metric>) {
        let second = self.second.as_ref();
        if self.first.number.is_none()
            && let Some(available) = second
            && available.number.is_some()
            && width < self.tool.name().len() + 3 + self.first.inline_width() + available.inline_width()
        {
            return (available, None);
        }
        (&self.first, second)
    }

    /// The age of the oldest figure shown, when it is old enough to say.
    fn age(&self) -> Option<i64> {
        self.first.age.max(self.second.as_ref().and_then(|second| second.age))
    }

    /// Draws one tool in `area`. `marks` is the height both tools' marks are
    /// sized for, so that they come out the same.
    fn panel(&self, screen: &mut Screen, area: Area, marks: usize, activity: impl FnOnce() -> Activity) {
        if area.w < 24 || area.h < 3 {
            return self.inline(screen, area);
        }
        let chart = Chart::new(area.h);
        let lane = if area.w >= 56 { 16 } else { 10 };
        let text_row = chart.label + 1 + usize::from(chart.large);
        let column = Area { w: lane - 1, ..area };
        let beneath = self.mark(screen, Area { h: marks.min(area.h), ..column }, text_row);
        // How old the reading is, said once for the tool rather than under
        // each of its windows.
        if let Some(age) = self.age()
            && beneath <= chart.note
        {
            let exact = format!("{} ago", when::duration(age, Detail::Exact));
            let text =
                if exact.len() <= column.w { exact } else { format!("{} ago", when::duration(age, Detail::Rounded)) };
            column.note(screen, chart.note, &text);
        }
        let figures = Area { x: area.x + lane, w: area.w - lane, ..area };
        let gap = match figures.w {
            36.. => 3,
            24..36 => 2,
            _ => 1,
        };
        // A lone figure has the whole width, so that its caption fits.
        let (first, second) = if self.second.is_some() { figures.split(gap) } else { (figures, figures) };
        let slot = self.tool as usize * 2;
        self.first.panel(screen, first, chart, slot);
        if let Some(metric) = &self.second {
            metric.panel(screen, second, chart, slot + 1);
        }
        // Activity is not a limit, so it never sits under a window as if it
        // were that window's caption: it has a row of its own, or follows
        // the hint on the hint's row, or is left out.
        let spare = chart.note + 1 < area.h;
        match &self.hint {
            Some(hint) if spare => {
                hint.draw(screen, figures, chart.note, None);
                figures.note(screen, chart.note + 1, &activity_text(activity()).unwrap_or_default());
            }
            Some(hint) => hint.draw(screen, figures, chart.note, activity_text(activity())),
            None if spare => figures.note(screen, chart.note + 1, &activity_text(activity()).unwrap_or_default()),
            None => {}
        }
    }

    /// Draws the tool's mark four fifths as tall as `area`, in its middle,
    /// and returns the first row beneath it.
    fn mark(&self, screen: &mut Screen, area: Area, text_row: usize) -> usize {
        if screen.pictures() == Some(true) {
            let mark = self.tool.picture();
            let (cols, rows) = mark.fit(screen.cell_size(), area.w, (4 * area.h + 2) / 5);
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

fn figures(limits: Option<Limits>, now: i64) -> [Option<Metric>; 2] {
    limits.unwrap_or_default().map(|window| window.map(|window| Metric::limit(window, now)))
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
        let large = height >= 5;
        let body = if large { 5 } else { 3 };
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
        if str_width(text) <= self.w {
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
fn activity_text(activity: Activity) -> Option<String> {
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

fn side_by_side(screen: &mut Screen, area: Area, reports: &[Report; 2], activity: impl FnOnce() -> Activity) {
    let (left, right) = area.split(1);
    for row in area.y..area.y + area.h {
        screen.put(left.w, row, 1, "│", FAINT);
    }
    reports[0].panel(screen, left.inset(), area.h, Activity::default);
    reports[1].panel(screen, right.inset(), area.h, activity);
}

fn stacked(screen: &mut Screen, area: Area, reports: &[Report; 2], activity: impl FnOnce() -> Activity) {
    let rule = usize::from(area.h > 2);
    let top = (area.h - rule) / 2;
    if rule > 0 {
        for x in 0..area.w {
            screen.put(x, top, 1, "─", FAINT);
        }
    }
    let upper = Area { h: top, ..area }.inset();
    let lower = Area { y: top + rule, h: area.h - top - rule, ..area }.inset();
    // The lower half may be a row taller; both marks are sized for the shorter.
    let marks = upper.h.min(lower.h);
    reports[0].panel(screen, upper, marks, Activity::default);
    reports[1].panel(screen, lower, marks, activity);
}

/// Choose a grid that leaves room for a useful figure at each supported size.
pub(super) fn usage(screen: &mut Screen, usage: &Usage, today: i64, now: i64) {
    screen.clear();
    let (w, h) = (screen.w, screen.h);
    if w == 0 || h == 0 {
        return;
    }
    let reports =
        [Report::codex(usage.codex.latest(), usage.codex.no_sessions, now), Report::claude(usage.claude.saved, now)];
    let area = Area { x: 0, y: 0, w, h };
    let activity = || usage.claude.activity(today, now);
    if h == 1 && w < ONE_LINE_SPLIT {
        let tool = usize::from(!reports[0].has_number() && reports[1].has_number());
        reports[tool].inline(screen, area);
    } else if h == 1 || h >= 3 && w >= SIDE_BY_SIDE {
        side_by_side(screen, area, &reports, activity);
    } else {
        stacked(screen, area, &reports, activity);
    }
}

#[cfg(test)]
mod tests {
    use super::super::ModelWeek;
    use super::*;

    fn labels(limits: Option<Limits>) -> Vec<String> {
        let report = Report::claude(limits, 1000);
        std::iter::once(&report.first).chain(&report.second).map(|metric| metric.label.to_string()).collect()
    }

    #[test]
    fn claude_shows_the_windows_the_account_has_and_no_empty_place_for_one_it_lacks() {
        let window = |minutes| Window { minutes, percent: 27.0, resets: Some(5000), observed: 1000 };
        let week = Limits::new([None, Some(window(10_080))]);
        assert_eq!(labels(Some(week)), ["week"]);
        let model_week = ModelWeek::new("fable", window(10_080));
        assert_eq!(labels(Some(Limits { model_week, ..week })), ["week", "Fable week"]);
        // With all three, the two all-models windows come first.
        let all = Limits { base: [Some(window(300)), Some(window(10_080))], model_week };
        assert_eq!(labels(Some(all)), ["5h", "week"]);
        // Nothing known at all keeps both places, to say what is missing.
        assert_eq!(labels(None), ["5h", "week"]);
    }
}
