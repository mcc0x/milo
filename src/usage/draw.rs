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
use super::{Activity, CodexSessions, Limits, Source, Usage, Window};

const SIDE_BY_SIDE: usize = 64;
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

    fn open(self) -> &'static str {
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
    fn draw(self, screen: &mut Screen, area: Area, row: usize) {
        match self {
            Self::Elapsed => area.note(screen, row, if area.w >= 13 { "Reset elapsed" } else { "elapsed" }),
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
            Self::Unknown => {
                area.note(screen, row, if area.w >= 18 { "Reset time unknown" } else { "left" });
            }
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
        let label = match window.minutes {
            10_080 => "week".into(),
            300 => "5h".into(),
            1440 => "day".into(),
            n if n.is_multiple_of(1440) => format!("{}d", n / 1440).into(),
            n if n.is_multiple_of(60) => format!("{}h", n / 60).into(),
            n => format!("{n}m").into(),
        };
        let elapsed = window.is_reset(now);
        let when = if elapsed {
            When::Elapsed
        } else {
            window.resets.map_or(When::Unknown, |at| When::Running(at.saturating_sub(now)))
        };
        Self {
            label,
            number: (!elapsed).then(|| Figure::new(window.number(), window.percent)),
            when,
            age: (!elapsed && now >= window.stale_at(source)).then(|| now.saturating_sub(window.observed)),
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
            Some(number) if screen.pictures() == Some(true) => number.cols(screen.cell_size()),
            Some(number) => digits::width(number.text),
            None => 1,
        }
    }

    /// Stay legible when either images or large text glyphs do not fit.
    fn panel(&self, screen: &mut Screen, area: Area, rows: Chart, slot: usize) {
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
            self.when.draw(screen, area, rows.note);
        }
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
        if matches!(self.when, When::Elapsed) {
            return self.when.draw(screen, area, row);
        }
        let (lead, detail) = match (self.age, self.when) {
            (Some(age), _) => ("", Some(format!("{} ago", when::duration(age, Detail::Rounded)))),
            (None, When::Elapsed) => unreachable!("elapsed is handled above"),
            (None, When::Running(seconds)) => ("resets in ", Some(when::duration(seconds, Detail::Exact))),
            (None, When::Unknown) => ("", None),
        };
        let words = self.label.len() + 8;
        if self.age.is_some() && detail.as_ref().is_some_and(|detail| words + 3 + detail.len() > area.w) {
            return area.note(screen, row, "Last reported");
        }
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
struct Hint {
    full: &'static str,
    short: &'static str,
    state: &'static str,
    next: Option<&'static str>,
}

impl Hint {
    const NO_SESSIONS: Self =
        Self { full: "no sessions found", short: "no sessions", state: "no source", next: Some("Run Codex") };
    const NO_SAVED_LIMITS: Self = Self {
        full: "no saved limits found",
        short: "no saved limits",
        state: "no source",
        next: Some("Run milo setup"),
    };
    const NO_LIMITS: Self = Self { full: "no limits to show", short: "no limits", state: "no limits", next: None };
    const UNREADABLE: Self = Self {
        full: "could not read sessions",
        short: "could not read",
        state: "cannot read",
        next: Some("Check session access"),
    };

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
    fn new(tool: Tool, report: Option<(&'a Limits, Source)>, missing: Hint, now: i64) -> Self {
        let Some((limits, source)) = report else {
            return Self { tool, first: Metric::missing("limits"), extra: [None, None], hint: Some(missing) };
        };
        let base = limits.base.into_iter().flatten().map(|window| Metric::limit(window, now, source));
        let model = limits.model_week.as_ref().map(|model| {
            let mut metric = Metric::limit(model.window, now, source);
            metric.label = model.caption().into();
            metric
        });
        let mut figures = base.chain(model);
        let (first, hint) = match figures.next() {
            Some(first) => (first, None),
            None => (Metric::missing("limits"), Some(Hint::NO_LIMITS)),
        };
        Self { tool, first, extra: [figures.next(), figures.next()], hint }
    }

    fn metrics(&self) -> impl Iterator<Item = &Metric<'a>> + Clone {
        std::iter::once(&self.first).chain(self.extra.iter().flatten())
    }

    /// Expired windows cannot block work now. Use the least remaining limit among the rest.
    fn limiting(&self) -> &Metric<'a> {
        self.metrics().reduce(|a, b| if tighter(b, a) { b } else { a }).unwrap_or(&self.first)
    }

    fn summary(&self, screen: &mut Screen, area: Area, activity: impl FnOnce() -> Activity) {
        let metric = self.limiting();
        let mut line = 1;
        if let Some(hint) = &self.hint {
            self.summary_line(screen, area, "", hint.state, "", FAINT);
            area.note(screen, line, hint.next.unwrap_or_else(|| self.tool.open()));
            line += 1;
        } else if matches!(metric.when, When::Elapsed) {
            let value = if area.w >= 17 { "reset elapsed" } else { "elapsed" };
            self.summary_line(screen, area, &metric.label, value, "", FAINT);
            area.note(screen, line, self.tool.open());
            line += 1;
        } else {
            let age_below = area.h > 1 && area.w >= "Last reported".len();
            let last = if metric.age.is_some() && !age_below { " last" } else { "" };
            self.summary_line(screen, area, &metric.label, metric.number(), last, metric.colour());
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
                    When::Running(seconds) => area.note(screen, line, &format!("Resets in {}", compact_time(seconds))),
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
        if line < area.h
            && let Some(activity) = activity_text(activity())
        {
            area.note(screen, line, &activity);
        }
    }

    fn summary_line(&self, screen: &mut Screen, area: Area, label: &str, value: &str, state: &str, style: Style) {
        let left = if state.is_empty() && value.ends_with('%') { " left" } else { "" };
        let suffix = if state.is_empty() { left } else { state };
        let mut writer = RowWriter { screen, x: area.x, y: area.y, left: area.w };
        for name in self.tool.names() {
            let short_label = label.strip_suffix(" week").unwrap_or(label);
            let short_label = if short_label.len() > 7 { "model" } else { short_label };
            let full = name.len() + 1 + label.len() + 3 + value.len() + suffix.len();
            let label = if full <= area.w { label } else { short_label };
            let show_label = !label.is_empty() && name.len() + 2 + label.len() + value.len() + state.len() <= area.w;
            if name.len() + 1 + value.len() + state.len() > area.w {
                continue;
            }
            writer.add(name, self.tool.colour().bold());
            if show_label {
                writer.add(" ", FAINT);
                writer.add(label, FAINT);
            }
            let gap = if writer.left >= 3 + value.len() + suffix.len() { " · " } else { " " };
            writer.add(gap, FAINT);
            writer.add(value, style.bold());
            if suffix.len() <= writer.left {
                writer.add(suffix, FAINT);
            }
            return;
        }
        // A tiny pane cannot always say what an old figure means.
        let value = if !state.is_empty() || str_width(value) > area.w { "—" } else { value };
        writer.add(value, style);
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
        if self.first.number.is_some() && self.first.age.is_none() && used + 3 * usize::from(tile) + 5 <= area.w {
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
                return self.summary(screen, area, activity);
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
            let next = hint.next.unwrap_or_else(|| self.tool.open());
            hint.draw(screen, figures, chart.note, activity.as_deref().filter(|_| !spare).or(Some(next)));
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
        let slot = match self.tool {
            Tool::Codex => 0,
            Tool::Claude => 2,
        };
        self.first.figure(screen, Area { x: area.x + lane, w: area.w - lane, ..area }, area.y, true, slot);
        let mut row = LARGE_ROWS;
        if row < area.h {
            match &self.hint {
                Some(hint) => hint.draw(screen, area, row, hint.next.or(Some(self.tool.open()))),
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

fn tighter(a: &Metric<'_>, b: &Metric<'_>) -> bool {
    let rank =
        |metric: &Metric<'_>| if metric.number.is_some() { 2 } else { u8::from(matches!(metric.when, When::Elapsed)) };
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

/// On a sidebar, names and the most limiting window come before decoration.
fn text_first(screen: &mut Screen, area: Area, reports: &[Report; 2], activity: impl FnOnce() -> Activity) {
    if area.h == 1 {
        let i = usize::from(tighter(reports[1].limiting(), reports[0].limiting()));
        return reports[i].summary(screen, area, activity);
    }
    let rule = usize::from(area.h >= 5);
    let upper_h = (area.h - rule) / 2;
    if rule != 0 {
        for x in area.x..area.x + area.w {
            screen.put(x, upper_h, 1, "─", FAINT);
        }
    }
    let upper = Area { h: upper_h, ..area };
    let lower = Area { y: upper_h + rule, h: area.h - upper_h - rule, ..area };
    reports[0].summary(screen, upper, Activity::default);
    reports[1].summary(screen, lower, activity);
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
        Report::new(Tool::Codex, usage.codex.reported().map(|(r, source)| (&r.windows, source)), missing, now),
        Report::new(
            Tool::Claude,
            usage.claude.reported().map(|(r, source)| (&r.windows, source)),
            Hint::NO_SAVED_LIMITS,
            now,
        ),
    ];
    let split = h == 1 || h >= 3 && w >= SIDE_BY_SIDE;
    // At two rows, give each provider a full line for its tightest limit.
    // Leave the captions and activity their rows in a taller compact pane.
    let text = h == 2 || w < SIDE_BY_SIDE && (!usage.large_figures || w < 19 || h < 9);
    let footer = w >= 8 && h >= if split { 7 } else { 14 };
    let area = Area { x: 0, y: 0, w, h: h - usize::from(footer) };
    let activity = || usage.claude.activity(today, now);
    if text {
        let pad = usize::from(area.w >= 24);
        text_first(screen, Area { x: pad, w: area.w - 2 * pad, ..area }, &reports, activity);
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
