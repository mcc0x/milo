//! Fits each tool's most useful figure into the available rows and columns.

use std::borrow::Cow;

use crate::screen::{GREEN, RED, Screen, Style, YELLOW};
use crate::ui::FAINT;

use super::{AGE_NOTE, Activity, FIVE_HOURS, Snapshot, Usage, Window, time};

const GAP: &str = "  ·  ";
const GAP_CELLS: usize = 5;
const BAR: usize = 6;
const MIN_BAR: usize = 3;
const STACK_WIDTH: usize = 56;

#[derive(Clone, Copy)]
enum Tool {
    Codex,
    Claude,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
        }
    }

    fn short(self) -> &'static str {
        match self {
            Self::Codex => "Cdx",
            Self::Claude => "Cl",
        }
    }

    fn missing(self) -> &'static str {
        match self {
            Self::Codex => "no Codex use found",
            Self::Claude => "no Claude Code use found",
        }
    }
}

struct Metric {
    label: Cow<'static, str>,
    number: String,
    details: Option<Cow<'static, str>>,
    bar: Option<f64>,
}

impl Metric {
    fn limit(window: Window, now: i64) -> Self {
        let label = match window.minutes {
            10_080 => "week".into(),
            1440 => "day".into(),
            n if n.is_multiple_of(1440) => format!("{} d", n / 1440).into(),
            n if n.is_multiple_of(60) => format!("{} h", n / 60).into(),
            n => format!("{n} m").into(),
        };
        if window.is_reset(now) {
            return Self { label, number: "reset".into(), details: None, bar: None };
        }
        let details = window.resets.map_or_else(
            || "reset time unknown".into(),
            |when| format!("resets in {}", time::relative(when.saturating_sub(now))).into(),
        );
        Self { label, number: window.number(), details: Some(details), bar: Some(window.percent) }
    }

    fn activity(label: &'static str, count: u64, partial: bool) -> Self {
        let mut number = tokens(count);
        if partial {
            number.push('+');
        }
        Self { label: label.into(), number, details: Some("tokens".into()), bar: None }
    }

    fn bare_width(&self) -> usize {
        self.label.len() + 1 + self.number.len()
    }

    fn full_width(&self, bar: usize) -> usize {
        self.bare_width() + self.details.as_ref().map_or(0, |s| s.len() + 1) + self.bar.map_or(0, |_| bar + 1)
    }

    fn draw(&self, row: &mut Row<'_>, bar_max: usize, details: bool) {
        let width = row.left();
        if self.number.len() > width {
            return;
        }
        let label = self.bare_width() <= width;
        let base = if label { self.bare_width() } else { self.number.len() };
        let detail_width = self.details.as_ref().map_or(0, |s| s.len() + 1);
        let details = details && base + detail_width <= width;
        let bar = width.saturating_sub(base + if details { detail_width } else { 0 } + 1).min(bar_max);
        if label {
            row.add(&self.label, FAINT);
            row.add(" ", FAINT);
        }
        if bar >= MIN_BAR
            && let Some(percent) = self.bar
        {
            row.bar(bar, percent);
            row.add(" ", FAINT);
        }
        row.add(&self.number, Style::PLAIN);
        if details && let Some(text) = &self.details {
            row.add(" ", FAINT);
            row.add(text, FAINT);
        }
    }
}

struct Report {
    tool: Tool,
    detail: Option<&'static str>,
    metrics: [Option<Metric>; 2],
    age: Option<String>,
}

impl Report {
    fn limits(tool: Tool, snapshot: Option<Snapshot>, partial: bool, now: i64) -> Self {
        let Some(snapshot) = snapshot else { return Self::empty(tool, partial) };
        let age = now.saturating_sub(snapshot.at.seconds);
        let age = (age >= AGE_NOTE).then(|| format!("as of {} ago", time::relative(age)));
        let metrics = snapshot.windows.map(|window| window.map(|w| Metric::limit(w, now)));
        let [first, second] = metrics;
        let metrics = if first.is_some() { [first, second] } else { [second, None] };
        let detail = partial.then_some("scan limited");
        Self { tool, detail, metrics, age }
    }

    fn activity(activity: Activity) -> Self {
        if !activity.present {
            return Self::empty(Tool::Claude, activity.partial);
        }
        let first = Metric::activity("5 h", activity.five, activity.partial);
        let second = Metric::activity("today", activity.today, activity.partial);
        Self { tool: Tool::Claude, detail: Some("activity"), metrics: [Some(first), Some(second)], age: None }
    }

    fn empty(tool: Tool, partial: bool) -> Self {
        Self { tool, detail: partial.then_some("scan limited"), metrics: [None, None], age: None }
    }

    fn heading_width(&self) -> usize {
        self.tool.name().len() + self.detail.map_or(0, |s| s.len() + 1)
    }

    fn minimum(&self) -> usize {
        self.metrics[0].as_ref().map_or(self.tool.short().len(), |m| self.tool.short().len() + 1 + m.number.len())
    }

    fn ideal(&self) -> usize {
        if self.metrics[0].is_none() && self.detail.is_none() {
            return self.tool.missing().len();
        }
        let metrics = self.metrics.iter().flatten().map(|m| m.full_width(BAR) + GAP_CELLS).sum::<usize>();
        self.heading_width() + metrics + self.age.as_ref().map_or(0, |age| age.len() + GAP_CELLS)
    }

    fn label(&self, row: &mut Row<'_>, needed: usize) -> bool {
        if self.heading_width() + needed <= row.left() {
            row.add(self.tool.name(), FAINT);
            if let Some(detail) = self.detail {
                row.add(" ", FAINT);
                row.add(detail, FAINT);
            }
        } else if self.tool.name().len() + needed <= row.left() {
            row.add(self.tool.name(), FAINT);
        } else if self.tool.short().len() + needed <= row.left() {
            row.add(self.tool.short(), FAINT);
        } else {
            return false;
        }
        true
    }

    fn missing(&self, row: &mut Row<'_>) {
        if self.detail.is_none() && self.tool.missing().len() <= row.left() {
            row.add(self.tool.missing(), FAINT);
        } else if self.label(row, 0) && row.left() >= 2 && self.detail.is_none() {
            row.add(" —", FAINT);
        }
    }

    fn inline(&self, row: &mut Row<'_>) {
        let [Some(first), second] = &self.metrics else { return self.missing(row) };
        let ideal = self.ideal();
        let age = self.age.as_ref().filter(|_| ideal <= row.left());
        let all = ideal - self.age.as_ref().map_or(0, |s| GAP_CELLS + s.len());
        let second = second.as_ref().filter(|_| all <= row.left());
        let values = first.bare_width() + 1;
        let reserve = if self.tool.short().len() + values <= row.left() { values } else { first.number.len() + 1 };
        if self.label(row, reserve) {
            row.add(if self.heading_width() + GAP_CELLS + first.bare_width() <= row.width { GAP } else { " " }, FAINT);
        }
        if let Some(second) = second {
            let split = row.x + first.full_width(BAR);
            let width = row.end;
            row.end = split;
            first.draw(row, BAR, true);
            row.end = width;
            row.add(GAP, FAINT);
            second.draw(row, BAR, true);
        } else {
            first.draw(row, BAR, true);
        }
        if let Some(age) = age {
            row.add(GAP, FAINT);
            row.add(age, FAINT);
        }
    }

    fn stack_height(&self, width: usize) -> usize {
        if self.metrics[0].is_none() {
            return 1;
        }
        1 + self
            .metrics
            .iter()
            .flatten()
            .map(|m| {
                1 + usize::from(m.details.as_ref().is_some_and(|d| d.len() <= width && m.full_width(MIN_BAR) > width))
            })
            .sum::<usize>()
    }

    fn stack(&self, screen: &mut Screen, y: usize, height: usize) {
        if height < 2 || self.metrics[0].is_none() {
            self.inline(&mut Row::new(screen, 0, y, screen.w));
            return;
        }
        let width = screen.w;
        let mut heading = Row::new(screen, 0, y, width);
        self.label(&mut heading, 0);
        if height >= self.stack_height(width)
            && let Some(age) = &self.age
            && heading.left() >= GAP_CELLS + age.len()
        {
            heading.add(GAP, FAINT);
            heading.add(age, FAINT);
        }
        let mut line = y + 1;
        for metric in self.metrics.iter().flatten() {
            if line >= y + height {
                break;
            }
            let separate = metric.full_width(MIN_BAR) > width && line + 1 < y + height;
            metric.draw(&mut Row::new(screen, 0, line, width), width, !separate);
            line += 1;
            if separate
                && let Some(detail) = &metric.details
                && detail.len() <= width
            {
                Row::new(screen, 0, line, width).add(detail, FAINT);
                line += 1;
            }
        }
    }
}

struct Row<'a> {
    screen: &'a mut Screen,
    x: usize,
    y: usize,
    width: usize,
    end: usize,
}

impl<'a> Row<'a> {
    fn new(screen: &'a mut Screen, x: usize, y: usize, width: usize) -> Self {
        Self { screen, x, y, width, end: x + width }
    }

    fn left(&self) -> usize {
        self.end.saturating_sub(self.x)
    }

    fn add(&mut self, text: &str, style: Style) {
        self.x += self.screen.put(self.x, self.y, self.left(), text, style);
    }

    fn bar(&mut self, width: usize, percent: f64) {
        let palette = if percent >= 90.0 {
            RED
        } else if percent >= 70.0 {
            YELLOW
        } else {
            GREEN
        };
        let count = f64::from(u32::try_from(width).unwrap_or(u32::MAX));
        for i in 0..width {
            let at = f64::from(u32::try_from(i).unwrap_or(u32::MAX)) + 0.5;
            if at < count * percent / 100.0 {
                self.add("━", Style::fg(palette));
            } else {
                self.add("┄", FAINT);
            }
        }
    }
}

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

fn split(width: usize, a: &Report, b: &Report) -> (usize, usize) {
    let spare = width - a.minimum() - b.minimum();
    let extra = a
        .ideal()
        .saturating_sub(a.minimum())
        .min(spare.saturating_sub(b.ideal().saturating_sub(b.minimum())).max(spare / 2));
    let left = a.minimum() + extra;
    (left, width - left)
}

fn single_row(screen: &mut Screen, reports: &[Report; 2]) {
    let [a, b] = reports;
    if screen.w < a.minimum() + b.minimum() + 3 {
        let best = if a.metrics[0].is_none() && b.metrics[0].is_some() { b } else { a };
        best.inline(&mut Row::new(screen, 0, 0, screen.w));
        return;
    }
    let (left, right) = split(screen.w - 3, a, b);
    a.inline(&mut Row::new(screen, 0, 0, left));
    Row::new(screen, left, 0, 3).add(" │ ", FAINT);
    b.inline(&mut Row::new(screen, left + 3, 0, right));
}

pub(super) fn usage(screen: &mut Screen, usage: &Usage, today: i64, now: i64) {
    screen.clear();
    let (w, h) = (screen.w, screen.h);
    if w == 0 || h == 0 {
        return;
    }
    let reports = [
        Report::limits(Tool::Codex, usage.codex.snapshot(), usage.codex.limited, now),
        if let Some(saved) = usage.claude.saved.filter(|s| s.fresh(now)) {
            Report::limits(Tool::Claude, Some(saved), false, now)
        } else {
            Report::activity(usage.claude.activity(today, now.saturating_sub(FIVE_HOURS), now))
        },
    ];
    if h == 1 {
        return single_row(screen, &reports);
    }
    let [a, b] = &reports;
    if h < 4 || w >= STACK_WIDTH {
        a.inline(&mut Row::new(screen, 0, 0, w));
        b.inline(&mut Row::new(screen, 0, 1, w));
        return;
    }
    let max = [a.stack_height(w), b.stack_height(w)];
    let mut height = max.map(|n| n.min(2));
    for _ in 0..h.min(max[0] + max[1]).saturating_sub(height[0] + height[1]) {
        let i = usize::from(height[1] < height[0] && height[1] < max[1] || height[0] == max[0]);
        height[i] += 1;
    }
    a.stack(screen, 0, height[0]);
    b.stack(screen, height[0], height[1]);
}
