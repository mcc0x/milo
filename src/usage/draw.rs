//! Gives both tools the same frame, with less detail when the pane is small.

mod digits;
mod figure;

use std::borrow::Cow;
use std::sync::LazyLock;

use crate::help;
use crate::picture::Picture;
use crate::screen::{Screen, Style};
use crate::ui::FAINT;
use crate::when::{self, Detail};
use crate::width::str_width;

use self::figure::Figure;
use super::{AGE_NOTE, Activity, CodexSessions, Limits, Usage, Window};

const SIDE_BY_SIDE: usize = 64;
// Two short names, two four-character figures and the rule.
const ONE_LINE_SPLIT: usize = 15;
const LARGE_ROWS: usize = 3;
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
    /// Says that the figure is what is left, and when there is room, when it starts over.
    fn draw(self, screen: &mut Screen, area: Area, row: usize) {
        match self {
            Self::Reset => area.note(screen, row, if area.w >= 12 { "left · reset" } else { "left" }),
            Self::Running(seconds) => {
                let time = when::duration(seconds, Detail::Exact);
                if time.len() + 17 <= area.w {
                    area.note(screen, row, &format!("left · resets in {time}"));
                } else if time.len() + 7 <= area.w {
                    area.note(screen, row, &format!("left · {time}"));
                } else {
                    area.note(screen, row, "left");
                }
            }
            Self::Unknown => area.note(screen, row, "left"),
        }
    }
}

/// Prepared display data lets each layout use the same figure and reset wording.
struct Metric<'a> {
    label: Cow<'a, str>,
    number: Option<Figure>,
    when: When,
    /// Seconds since the figure was reported, once that is worth saying:
    /// one a few minutes old is as good as current.
    age: Option<i64>,
}

impl Metric<'_> {
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

    /// Columns the figure needs when drawn large on this screen.
    fn large_width(&self, screen: &Screen) -> usize {
        match self.number {
            Some(number) if screen.pictures() == Some(true) => number.cols(screen.cell_size()),
            Some(number) => digits::width(number.text),
            None => 1,
        }
    }

    /// Stay legible when either images or large text glyphs do not fit.
    fn panel(&self, screen: &mut Screen, area: Area, rows: Chart, slot: usize) {
        area.note(screen, rows.label, &self.label);
        self.figure(screen, area, area.y + rows.label + 1, rows.large, slot);
        self.when.draw(screen, area, rows.note);
    }

    /// The figure as a picture, as block digits, or as plain text in the middle of their rows.
    fn figure(&self, screen: &mut Screen, area: Area, y: usize, large: bool, slot: usize) {
        match (large, self.number) {
            (true, Some(number)) if screen.pictures() == Some(true) && number.cols(screen.cell_size()) <= area.w => {
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
        let (lead, detail) = match (self.age, self.when) {
            (Some(age), _) => ("", Some(format!("{} ago", when::duration(age, Detail::Rounded)))),
            (None, When::Reset) => ("", Some("reset".into())),
            (None, When::Running(seconds)) => ("resets in ", Some(when::duration(seconds, Detail::Exact))),
            (None, When::Unknown) => ("", None),
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
        if self.inline_width() + 5 <= area.w {
            writer.add(" left", FAINT);
        }
    }
}

/// Long and short explanations let missing figures fit either layout.
struct Hint {
    full: &'static str,
    short: &'static str,
}

impl Hint {
    const NO_SESSIONS: Self = Self { full: "no sessions found", short: "no sessions" };
    const NO_SAVED_LIMITS: Self = Self { full: "no saved limits found", short: "no saved limits" };
    const NO_LIMITS: Self = Self { full: "no limits to show", short: "no limits" };
    const UNREADABLE: Self = Self { full: "could not read sessions", short: "could not read" };

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

struct Report<'a> {
    tool: Tool,
    first: Metric<'a>,
    extra: [Option<Metric<'a>>; 2],
    hint: Option<Hint>,
}

impl<'a> Report<'a> {
    /// Leave out unreported windows; a dash stands for the whole missing report.
    fn new(tool: Tool, limits: Option<&'a Limits>, missing: Hint, now: i64) -> Self {
        let base = limits.into_iter().flat_map(|limits| limits.base.into_iter().flatten());
        let model = limits.and_then(|limits| limits.model_week.as_ref()).map(|model| {
            let mut metric = Metric::limit(model.window, now);
            metric.label = model.caption().into();
            metric
        });
        let mut figures = base.map(|window| Metric::limit(window, now)).chain(model);
        let (first, hint) = match figures.next() {
            Some(first) => (first, None),
            None => (Metric::missing("limits"), Some(if limits.is_some() { Hint::NO_LIMITS } else { missing })),
        };
        Self { tool, first, extra: [figures.next(), figures.next()], hint }
    }

    fn has_number(&self) -> bool {
        self.first.number.is_some()
    }

    /// Shorten the name and labels before giving up a reported figure.
    fn inline(&self, screen: &mut Screen, area: Area) {
        let first = &self.first;
        let name = self
            .tool
            .names()
            .into_iter()
            .find(|name| name.len() + 1 + first.number_width() <= area.w)
            .unwrap_or_default();
        let identity = if name.is_empty() { 0 } else { name.len() + 1 };
        let label = identity + first.inline_width() <= area.w;
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
        if self.has_number() && used + 3 * usize::from(tile) + 5 <= area.w {
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
        let all = || std::iter::once(&self.first).chain(self.extra.iter().flatten());
        // Large figures too wide to sit side by side: the first keeps the
        // width, and the others follow below it as text.
        if chart.large && count > 1 {
            let each = (figures.w - (count - 1) * gap) / count;
            if all().take(count).any(|metric| metric.large_width(screen) > each) {
                count = 1;
            }
        }
        let metrics = all().take(count);
        // How old the reading is, said once for the tool rather than under
        // each of its windows.
        if let Some(age) = metrics.clone().filter_map(|metric| metric.age).max()
            && beneath <= chart.note
        {
            let exact = format!("{} ago", when::duration(age, Detail::Exact));
            let text =
                if exact.len() <= column.w { exact } else { format!("{} ago", when::duration(age, Detail::Rounded)) };
            column.note(screen, chart.note, &text);
        }
        let slot = match self.tool {
            Tool::Codex => 0,
            Tool::Claude => 2,
        };
        let width = figures.w - (count - 1) * gap;
        for (i, metric) in metrics.enumerate() {
            let start = width * i / count;
            let w = width * (i + 1) / count - start;
            metric.panel(screen, Area { x: figures.x + start + i * gap, w, ..figures }, chart, slot + i);
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
            hint.draw(screen, figures, chart.note, activity.as_deref().filter(|_| !spare));
        }
        if spare && let Some(activity) = activity {
            figures.note(screen, row, &activity);
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
        let slot = match self.tool {
            Tool::Codex => 0,
            Tool::Claude => 2,
        };
        self.first.figure(screen, Area { x: area.x + lane, w: area.w - lane, ..area }, area.y, true, slot);
        let mut row = LARGE_ROWS;
        if row < area.h {
            match &self.hint {
                Some(hint) => hint.draw(screen, area, row, None),
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
            && let Some(activity) = activity_text(activity())
        {
            area.note(screen, row, &activity);
        }
    }

    /// Draws the tool's mark `rows` tall in the middle of `area`, and
    /// returns the first row beneath it.
    fn mark(&self, screen: &mut Screen, area: Area, rows: usize, text_row: usize) -> usize {
        if screen.pictures() == Some(true) {
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
    let missing = match usage.codex.sessions {
        CodexSessions::Missing => Hint::NO_SESSIONS,
        CodexSessions::Present => Hint::NO_LIMITS,
        CodexSessions::Unreadable => Hint::UNREADABLE,
    };
    let reports = [
        Report::new(Tool::Codex, usage.codex.latest().map(|reading| &reading.windows), missing, now),
        Report::new(Tool::Claude, usage.claude.latest().map(|reading| &reading.windows), Hint::NO_SAVED_LIMITS, now),
    ];
    let split = h == 1 || h >= 3 && w >= SIDE_BY_SIDE;
    // Leave the captions and activity their rows in a compact pane.
    let footer = w >= 8 && h >= if split { 7 } else { 14 };
    let area = Area { x: 0, y: 0, w, h: h - usize::from(footer) };
    let activity = || usage.claude.activity(today, now);
    if h == 1 && w < ONE_LINE_SPLIT {
        let tool = usize::from(!reports[0].has_number() && reports[1].has_number());
        reports[tool].inline(screen, area);
    } else if split {
        side_by_side(screen, area, &reports, activity);
    } else {
        stacked(screen, area, &reports, activity);
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
        let report = Report::new(Tool::Claude, limits, Hint::NO_SAVED_LIMITS, 1000);
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
