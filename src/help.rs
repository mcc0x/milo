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

/// Keeps the last hint whole, dropping middle items before the first.
pub fn draw_hint(s: &mut Screen, first: &str, middle: &[&str], last: &str) {
    let room = s.w.saturating_sub(2);
    let last_width = str_width(last);
    if last_width > room {
        return;
    }
    let gap = str_width(SEPARATOR);
    let first = if str_width(first) + gap + last_width <= room { first } else { "" };
    let mut used = last_width + if first.is_empty() { 0 } else { str_width(first) + gap };
    let mut start = middle.len();
    for item in middle.iter().rev() {
        used += gap + str_width(item);
        if used > room {
            break;
        }
        start -= 1;
    }
    let y = s.h - 1;
    let mut x = 1;
    for item in std::iter::once(first).chain(middle[start..].iter().copied()).filter(|item| !item.is_empty()) {
        x += s.put(x, y, room, item, FAINT);
        x += s.put(x, y, gap, SEPARATOR, FAINT);
    }
    s.put(x, y, last_width, last, FAINT);
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
}

impl Page {
    /// A page of a pane's own making, from the row builders above.
    pub fn new(rows: Vec<HelpRow>) -> Self {
        Self { rows, top: 0 }
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
        draw_hint(s, "↑↓ scroll", &["Esc close"], CLOSE);
    }

    /// Scrolls the page. Returns false when the key closes it instead.
    pub fn scroll(&mut self, key: Key, height: usize) -> bool {
        let shown = height.saturating_sub(1);
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
    fn a_narrow_hint_drops_middle_items_without_clipping_the_end() {
        let mut screen = Screen::sized(23, 1);
        draw_hint(&mut screen, "Enter read", &["r reload", "q quit"], "? help");
        assert_eq!(screen.text(), [" Enter read · ? help"]);
        let mut help = Screen::sized(28, 2);
        Page::new(vec![heading("Keys")]).draw(&mut help);
        assert_eq!(help.text()[1], " ↑↓ scroll · q close");
    }
}
