//! Draws and scrolls the in-pane guide to marks and keys.

use crate::keys::Key;
use crate::screen::{Screen, Style};
use crate::ui::{self, FAINT, HEADING};
use crate::width::str_width;

/// One row of a page: pieces of text, each in its own style. A row must
/// fit a pane 28 columns wide: 26 characters at most.
pub type HelpRow = Vec<(&'static str, Style)>;

const SEPARATOR: &str = " · ";
const CLOSE: &str = "q close";

/// Hit-test the exact labels drawn, not the separators or shortcuts omitted for space.
#[derive(Default)]
pub struct HintHits<'a> {
    y: usize,
    gap: usize,
    first: &'a str,
    middle: &'a [&'a str],
    start: usize,
    last: &'a str,
}

impl HintHits<'_> {
    fn items(&self) -> impl Iterator<Item = (usize, &str)> {
        std::iter::once((0, self.first))
            .chain(self.middle.iter().copied().enumerate().skip(self.start).map(|(i, text)| (i + 1, text)))
            .chain(std::iter::once((self.middle.len() + 1, self.last)))
            .filter(|(_, text)| !text.is_empty())
    }

    pub fn index_at(&self, x: usize, y: usize) -> Option<usize> {
        self.hit(x, y).map(|(index, _, _)| index)
    }

    pub fn item_at(&self, x: usize, y: usize) -> Option<&str> {
        self.hit(x, y).map(|(_, text, _)| text)
    }

    pub fn span_at(&self, x: usize, y: usize) -> Option<std::ops::Range<usize>> {
        self.hit(x, y).map(|(_, _, span)| span)
    }

    pub(crate) fn hit(&self, x: usize, y: usize) -> Option<(usize, &str, std::ops::Range<usize>)> {
        if y != self.y {
            return None;
        }
        let mut left = 1;
        for (index, text) in self.items() {
            let end = left + str_width(text);
            if (left..end).contains(&x) {
                return Some((index, text, left..end));
            }
            left = end + self.gap;
        }
        None
    }
}

/// Keeps the last hint whole, dropping middle items before the first.
pub fn draw_hint<'a>(s: &mut Screen, first: &'a str, middle: &'a [&'a str], last: &'a str) -> HintHits<'a> {
    draw_hint_with(s, first, middle, last, SEPARATOR)
}

pub fn draw_compact_hint<'a>(s: &mut Screen, first: &'a str, middle: &'a [&'a str], last: &'a str) -> HintHits<'a> {
    draw_hint_with(s, first, middle, last, " ")
}

fn draw_hint_with<'a>(
    s: &mut Screen,
    first: &'a str,
    middle: &'a [&'a str],
    last: &'a str,
    separator: &str,
) -> HintHits<'a> {
    let room = s.w.saturating_sub(2);
    let last_width = str_width(last);
    if last_width > room || s.h == 0 {
        return HintHits::default();
    }
    let gap = str_width(separator);
    let first_width = str_width(first);
    let first = if first_width + gap + last_width <= room { first } else { "" };
    let mut used = last_width + if first.is_empty() { 0 } else { first_width + gap };
    let mut start = middle.len();
    for item in middle.iter().rev() {
        used += gap + str_width(item);
        if used > room {
            break;
        }
        start -= 1;
    }
    let y = s.h - 1;
    let hits = HintHits { y, gap, first, middle, start, last };
    let mut x = 1;
    for (i, (_, text)) in hits.items().enumerate() {
        if i != 0 {
            x += s.put(x, y, gap, separator, FAINT);
        }
        x += s.put(x, y, room, text, FAINT);
    }
    hits
}

pub fn heading(text: &'static str) -> HelpRow {
    vec![(text, HEADING)]
}

/// A sample drawn as it appears on screen, and what it means. The
/// sample's padding stays plain so a strike stops at the last letter.
pub fn sample(text: &'static str, st: Style, meaning: &'static str) -> HelpRow {
    let shown = text.trim_end();
    vec![
        ("  ", Style::PLAIN),
        (shown, st),
        (&text[shown.len()..], Style::PLAIN),
        ("  ", Style::PLAIN),
        (meaning, FAINT),
    ]
}

/// A key, or a few, and what it does.
pub fn key(keys: &'static str, does: &'static str) -> HelpRow {
    vec![("  ", Style::PLAIN), (keys, Style::PLAIN), ("  ", Style::PLAIN), (does, FAINT)]
}

/// A line of plain explanation.
pub fn note(text: &'static str) -> HelpRow {
    vec![("  ", Style::PLAIN), (text, FAINT)]
}

/// A page over the pane, with its own scroll position.
pub struct Page {
    rows: Vec<HelpRow>,
    top: usize,
    footer: HintHits<'static>,
}

impl Page {
    /// A page of a pane's own making, from the row builders above.
    pub fn new(rows: Vec<HelpRow>) -> Self {
        Self { rows, top: 0, footer: HintHits::default() }
    }

    #[cfg(test)]
    pub fn assert_rows_fit(&self, room: usize) {
        for row in &self.rows {
            let width: usize = row.iter().map(|(text, _)| str_width(text)).sum();
            assert!(width <= room, "{width} columns: {:?}", row.iter().map(|(text, _)| text).collect::<Vec<_>>());
        }
    }

    /// Don't cover the pane unless help can show how to close it.
    pub fn fits(s: &Screen) -> bool {
        s.w >= 2 + CLOSE.len() && s.h >= 2
    }

    pub fn draw(&mut self, s: &mut Screen) {
        if !Self::fits(s) {
            self.footer = HintHits::default();
            return;
        }
        s.clear();
        let (w, h) = (s.w, s.h);
        let shown = h - 1;
        self.top = self.top.min(self.rows.len().saturating_sub(shown));
        for (y, row) in self.rows.iter().skip(self.top).take(shown).enumerate() {
            let mut x = 1;
            for (text, st) in row {
                x += s.put(x, y, (w - 1).saturating_sub(x), text, *st);
            }
        }
        ui::thumb(s, w - 1, 0, shown, self.rows.len(), self.top);
        self.footer = draw_hint(s, "↑↓ scroll", &["Esc close"], CLOSE);
    }

    pub fn hover_span(&self, x: usize, y: usize) -> Option<std::ops::Range<usize>> {
        self.footer.span_at(x, y)
    }

    /// Scrolls the page. Returns false when the key closes it instead.
    pub fn scroll(&mut self, key: Key, height: usize) -> bool {
        let shown = height.saturating_sub(1);
        let key = match key {
            Key::Click(x, y) => match self.footer.index_at(x, y) {
                Some(1 | 2) => return false,
                Some(0) if x == 1 => Key::Up,
                Some(0) => Key::Down,
                _ => return true,
            },
            key => key,
        };
        let Some(rows) = key.rows(shown) else { return false };
        let rows = if matches!(key, Key::WheelUp | Key::WheelDown) { rows.signum() } else { rows };
        let last = self.rows.len().saturating_sub(shown);
        self.top = self.top.saturating_add_signed(rows).min(last);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_clicks_follow_visible_cells_after_reflow() {
        let mut screen = Screen::sized(14, 2);
        let hits = draw_hint(&mut screen, "too long to fit", &["first", "界"], "q quit");
        assert_eq!(hits.index_at(2, 1), Some(2)); // Second column of the visible wide glyph.
        assert_eq!(hits.index_at(3, 1), None); // Separator, not an action.
        assert_eq!(hits.index_at(6, 1), Some(3));
        assert_eq!(hits.index_at(6, 0), None);
    }

    #[test]
    fn a_narrow_hint_drops_middle_items_without_clipping_the_end() {
        let mut screen = Screen::sized(23, 1);
        draw_hint(&mut screen, "Enter read", &["r reload", "q quit"], "? help");
        assert_eq!(screen.text(), [" Enter read · ? help"]);
        let mut help = Screen::sized(28, 2);
        Page::new(vec![heading("Keys")]).draw(&mut help);
        assert_eq!(help.text()[1], " ↑↓ scroll · q close");
    }
}
