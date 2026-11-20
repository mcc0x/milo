//! A proportional view of the layout Milo would build around a shell.

use crate::screen::Screen;
use crate::ui::{FAINT, HEADING};
use crate::width::str_width;

use super::layout::{DEFAULT_USAGE_ROWS, Layout, Side, TabPane};

const ASSUMED_TAB_ROWS: usize = 35;

pub(super) const fn pane_label(pane: TabPane) -> &'static str {
    match pane {
        TabPane::Outline => "Outline",
        TabPane::History => "History",
        TabPane::Usage => "Usage",
    }
}

pub(super) struct Preview<'a> {
    layout: &'a Layout,
    size: Option<(usize, usize)>,
    rail: f64,
    shares: [f64; 3],
    count: usize,
}

impl<'a> Preview<'a> {
    pub(super) fn new(layout: &'a Layout, size: Option<(usize, usize)>) -> Self {
        let size = size.filter(|&(cols, rows)| cols > 0 && rows > 0);
        let [outline, history, usage] = layout.panes;
        let count = layout.panes.into_iter().filter(|&on| on).count();
        let usage_part = if usage && count > 1 {
            let rows = size.map_or(ASSUMED_TAB_ROWS, |(_, rows)| rows);
            (number(&layout.usage_rows, f64::from(DEFAULT_USAGE_ROWS)) / rows as f64).min(0.4)
        } else {
            if usage { 1.0 } else { 0.0 }
        };
        let outline_part = if outline && history {
            number(&layout.outline_share, 0.7)
        } else if outline {
            1.0
        } else {
            0.0
        };
        let other = 1.0 - usage_part;
        let defaults = [outline_part * other, (1.0 - outline_part) * other, usage_part];
        let mut shares = std::array::from_fn(|i| {
            if !layout.panes[i] {
                0.0
            } else {
                layout
                    .geometry
                    .shares
                    .filter(|shares| shares[i] > 0)
                    .map_or(defaults[i], |shares| f64::from(shares[i]) / 1_000_000.0)
            }
        });
        let sum = shares.iter().sum::<f64>();
        if sum > 0.0 {
            for share in &mut shares {
                *share /= sum;
            }
        }
        let rail = number(&layout.width, 0.2).clamp(0.0, 1.0);
        Self { layout, size, rail, shares, count }
    }

    pub(super) fn proportions(&self) -> (f64, [f64; 3], Option<(usize, usize)>) {
        (if self.count == 0 { 0.0 } else { self.rail }, self.shares, self.size)
    }

    pub(super) fn minimum_height(&self) -> usize {
        self.count.max(1) * 2 + 1
    }

    pub(super) fn height(&self, width: usize, max: usize) -> usize {
        let (cols, rows) = self.size.unwrap_or((100, ASSUMED_TAB_ROWS));
        let scaled = width.saturating_mul(rows).div_ceil(cols);
        // Border and legible labels need at least this many rows. Above that
        // minimum, space is apportioned from the tab's real dimensions.
        scaled.max(self.minimum_height()).min(max)
    }

    pub(super) fn caption(&self, s: &mut Screen, x: usize, y: usize, width: usize, draft: bool) {
        let title = if draft { "Draft preview" } else { "Layout preview" };
        s.put_fit(x, y, width, title, HEADING);
        let mut at = x + title.len() + 2;
        let end = x + width;
        if self.count > 0 {
            let tenths = fraction_cells(self.rail, 1000) as u64;
            let (whole, decimal) = (tenths / 10, tenths % 10);
            let side = if self.layout.geometry.side == Side::Left { "% left" } else { "% right" };
            let whole_digits = (whole.max(1).ilog10() + 1) as usize;
            let show_decimal = decimal != 0 && at + whole_digits + 2 + side.len() <= end;
            let value = if show_decimal { whole } else { (tenths + 5) / 10 };
            let digits = (value.max(1).ilog10() + 1) as usize;
            if at + digits + side.len() > end {
                return;
            }
            at += digits;
            s.put_number(at, y, value, FAINT);
            if show_decimal {
                s.put(at, y, 1, ".", FAINT);
                at += 2;
                s.put_number(at, y, decimal, FAINT);
            }
            at += s.put(at, y, side.len(), side, FAINT) + 2;
        }
        if let Some((cols, rows)) = self.size {
            let cw = (cols.ilog10() + 1) as usize;
            let rw = (rows.ilog10() + 1) as usize;
            if at + cw + rw < end {
                s.put_number(at + cw, y, cols as u64, FAINT);
                s.put(at + cw, y, 1, "×", FAINT);
                s.put_number(at + cw + 1 + rw, y, rows as u64, FAINT);
            }
        }
    }

    pub(super) fn draw(&self, s: &mut Screen, rect: Rect, highlight: Option<TabPane>) {
        let Rect { x, y, width, height } = rect;
        if width < 8 || height < self.minimum_height() {
            return;
        }
        let (right, bottom) = (x + width - 1, y + height - 1);
        for (row, left, right_glyph) in [(y, "┌", "┐"), (bottom, "└", "┘")] {
            s.put(x, row, 1, left, FAINT);
            rule(s, x + 1, row, width - 2);
            s.put(right, row, 1, right_glyph, FAINT);
        }
        for row in y + 1..bottom {
            s.put(x, row, 1, "│", FAINT);
            s.put(right, row, 1, "│", FAINT);
        }
        if self.count == 0 {
            label(s, x, right, y + height / 2, "Your work", "Work", FAINT);
            return;
        }
        let content = width - 3;
        let rail_width = fraction_cells(self.rail, content).clamp(1, content - 1);
        let (split, rail_left, rail_right, work_left, work_right) = match self.layout.geometry.side {
            Side::Left => (x + 1 + rail_width, x, x + 1 + rail_width, x + 1 + rail_width, right),
            Side::Right => (right - 1 - rail_width, right - 1 - rail_width, right, x, right - 1 - rail_width),
        };
        s.put(split, y, 1, "┬", FAINT);
        for row in y + 1..bottom {
            s.put(split, row, 1, "│", FAINT);
        }
        s.put(split, bottom, 1, "┴", FAINT);
        label(s, work_left, work_right, y + height / 2, "Your work", "Work", FAINT);

        let total = height - 2 - (self.count - 1);
        let mut used = 0;
        let mut fraction = 0.0;
        let mut top = y + 1;
        let enabled = self.layout.geometry.order.into_iter().filter(|&pane| self.layout.chosen(pane));
        for (index, pane) in enabled.enumerate() {
            let remaining = self.count - 1 - index;
            fraction += self.shares[pane as usize];
            let through =
                if remaining == 0 { total } else { fraction_cells(fraction, total).clamp(used + 1, total - remaining) };
            let length = through - used;
            let short = match pane {
                TabPane::Outline => "Tree",
                TabPane::History => "Git",
                TabPane::Usage => "Use",
            };
            let style = if highlight == Some(pane) { HEADING } else { FAINT };
            label(s, rail_left, rail_right, top + (length - 1) / 2, pane_label(pane), short, style);
            top += length;
            if remaining > 0 {
                s.put(rail_left, top, 1, "├", FAINT);
                rule(s, rail_left + 1, top, rail_right - rail_left - 1);
                s.put(rail_right, top, 1, "┤", FAINT);
                top += 1;
            }
            used = through;
        }
    }
}

pub(super) struct Rect {
    pub(super) x: usize,
    pub(super) y: usize,
    pub(super) width: usize,
    pub(super) height: usize,
}

fn number(text: &str, fallback: f64) -> f64 {
    text.parse::<f64>().ok().filter(|n| n.is_finite()).unwrap_or(fallback)
}

/// Cumulative shares can stray slightly outside `0..=1` after normalization.
/// Rounded results are nonnegative and at most the available grid span.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn fraction_cells(fraction: f64, span: usize) -> usize {
    (fraction.clamp(0.0, 1.0) * span as f64).round() as usize
}

#[cfg(test)]
fn percent(part: f64) -> usize {
    fraction_cells(part, 100)
}

fn label(s: &mut Screen, left: usize, right: usize, y: usize, text: &str, short: &str, style: crate::screen::Style) {
    let room = right - left - 1;
    let text = if str_width(text) <= room { text } else { short };
    let padding = usize::from(str_width(text) < room);
    s.put(left + 1 + padding, y, room - padding, text, style);
}

fn rule(s: &mut Screen, mut x: usize, y: usize, mut width: usize) {
    const LINE: &str = "────────────────────────────────────────────────────────────────";
    while width > 0 {
        let len = width.min(LINE.len() / '─'.len_utf8());
        s.put(x, y, len, &LINE[..len * '─'.len_utf8()], FAINT);
        width -= len;
        x += len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_rail_uses_the_draft_width_order_and_captured_shares() {
        let mut layout = Layout { width: "0.25".into(), ..Layout::default() };
        layout.geometry.side = Side::Right;
        layout.geometry.order = [TabPane::History, TabPane::Usage, TabPane::Outline];
        layout.geometry.shares = Some([500_000, 200_000, 300_000]);
        let preview = Preview::new(&layout, Some((119, 60)));
        let mut screen = Screen::sized(83, 23);
        preview.draw(&mut screen, Rect { x: 0, y: 0, width: 83, height: 23 }, None);
        let text = screen.text();
        assert_eq!(text[0].chars().position(|c| c == '┬'), Some(61));
        assert!(text[2].contains("History"));
        assert!(text[5].contains('├'));
        assert!(text[8].contains("Usage"));
        assert!(text[12].contains('├'));
        assert!(text[17].contains("Outline"));
    }

    #[test]
    fn usage_rows_scale_against_the_tab_not_the_setup_pane() {
        let layout = Layout::default();
        for (rows, pct) in [(20, 10), (40, 5), (100, 2)] {
            assert_eq!(percent(Preview::new(&layout, Some((100, rows))).shares[TabPane::Usage as usize]), pct);
        }
        assert_eq!(percent(Preview::new(&layout, None).shares[TabPane::Usage as usize]), 6);
        assert!(Preview::new(&layout, Some((80, 40))).height(40, 24) > 7);
    }

    #[test]
    fn an_explicit_usage_size_and_captured_geometry_override_the_default() {
        let mut layout = Layout { usage_rows: "10".into(), ..Layout::default() };
        let usage_share =
            |layout: &Layout| percent(Preview::new(layout, Some((100, 50))).shares[TabPane::Usage as usize]);
        assert_eq!(usage_share(&layout), 20);
        layout.geometry.shares = Some([400_000, 250_000, 350_000]);
        assert_eq!(usage_share(&layout), 35);
    }
}
