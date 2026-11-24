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

    pub(crate) fn span(&self, index: usize) -> Option<std::ops::Range<usize>> {
        self.spans().find_map(|(i, _, span)| (i == index).then_some(span))
    }

    fn spans(&self) -> impl Iterator<Item = (usize, &str, std::ops::Range<usize>)> {
        self.items().scan(1, |left, (index, text)| {
            let end = *left + str_width(text);
            let span = *left..end;
            *left = end + self.gap;
            Some((index, text, span))
        })
    }

    pub(crate) fn hit(&self, x: usize, y: usize) -> Option<(usize, &str, std::ops::Range<usize>)> {
        if y != self.y {
            return None;
        }
        self.spans().find(|(_, _, span)| span.contains(&x))
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
    /// How many rows the last frame had room for, over all its columns.
    visible: usize,
    footer: HintHits<'static>,
}

/// A column of a page: the 26 characters of its widest row and a gap.
const COLUMN: usize = 28;

impl Page {
    /// A page of a pane's own making, from the row builders above.
    pub fn new(rows: Vec<HelpRow>) -> Self {
        Self { rows, top: 0, visible: 0, footer: HintHits::default() }
    }

    /// Puts rows above those the page was made with.
    pub fn lead(&mut self, rows: impl IntoIterator<Item = HelpRow>) {
        self.rows.splice(0..0, rows);
    }

    #[cfg(test)]
    pub fn assert_rows_fit(&self, room: usize) {
        for row in &self.rows {
            let width: usize = row.iter().map(|(text, _)| str_width(text)).sum();
            assert!(width <= room, "{width} columns: {:?}", row.iter().map(|(text, _)| text).collect::<Vec<_>>());
        }
    }

    /// Every row's words, one row to a line.
    #[cfg(test)]
    pub fn text(&self) -> String {
        self.rows.iter().map(|row| row.iter().map(|(text, _)| *text).collect::<String>() + "\n").collect()
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
        // A page too long for the pane continues in a second column, and a
        // third, where the pane is wide enough: fewer rows are left to scroll to.
        let most = ((w - 1) / COLUMN).max(1);
        let (sections, covered) = self.sections(shown).fold((0, 0), |(count, _), rows| (count + 1, rows.end));
        if most > 1 && sections <= most && covered == self.rows.len() {
            // The whole page fits, each column ending with a section.
            (self.top, self.visible) = (0, self.rows.len());
            for (column, rows) in self.sections(shown).enumerate() {
                draw_column(s, column, sections, &self.rows[rows]);
            }
        } else {
            let columns = self.rows.len().div_ceil(shown).clamp(1, most);
            self.visible = shown * columns;
            self.top = self.top.min(self.rows.len().saturating_sub(self.visible));
            for (column, rows) in self.rows[self.top..].chunks(shown).take(columns).enumerate() {
                draw_column(s, column, columns, rows);
            }
        }
        // The thumb covers the rows still out of sight, so it ends at the bottom with the page.
        let hidden = self.rows.len().saturating_sub(self.visible);
        ui::thumb(s, w - 1, 0, shown, shown + hidden, self.top);
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
        let last = self.rows.len().saturating_sub(self.visible.max(shown));
        self.top = self.top.saturating_add_signed(rows).min(last);
        true
    }

    /// The page as columns of at most `shown` rows that each end where a
    /// section does: the blank row there is left out. A section longer than
    /// a column ends the columns short of the page.
    fn sections(&self, shown: usize) -> impl Iterator<Item = std::ops::Range<usize>> {
        let mut start = Some(0);
        std::iter::from_fn(move || {
            let from = start?;
            if self.rows.len() - from <= shown {
                start = None;
                return Some(from..self.rows.len());
            }
            let blank = (from + 1..=from + shown).rev().find(|&row| self.rows[row].is_empty());
            start = blank.map(|row| row + 1);
            blank.map(|row| from..row)
        })
    }
}

fn draw_column(s: &mut Screen, column: usize, columns: usize, rows: &[HelpRow]) {
    let end = if column + 1 == columns { s.w - 1 } else { (column + 1) * COLUMN - 1 };
    for (y, row) in rows.iter().enumerate() {
        let mut x = 1 + column * COLUMN;
        for (text, st) in row {
            x += s.put(x, y, end.saturating_sub(x), text, *st);
        }
    }
}

#[cfg(test)]
mod column_tests {
    use super::*;

    const NAMES: [&str; 9] = ["one", "two", "three", "four", "five", "six", "seven", "eight", "nine"];

    fn page() -> Page {
        Page::new(NAMES.iter().map(|name| note(name)).collect())
    }

    fn drawn(page: &mut Page, w: usize, h: usize) -> Vec<String> {
        let mut screen = Screen::sized(w, h);
        page.draw(&mut screen);
        screen.text()
    }

    #[test]
    fn a_long_page_continues_in_a_second_column_only_where_one_fits() {
        let mut page = page();
        // Four rows and the hints: one column until the pane has room for two of 28.
        let narrow = drawn(&mut page, 56, 5);
        assert_eq!(narrow[..4], [&format!("   {:<52}▐", "one"), "   two", "   three", "   four"]);
        let wide = drawn(&mut page, 57, 5);
        assert_eq!(
            wide[..2],
            [format!("   {:<25}   {:<25}▐", "one", "five"), format!("   {:<25}   {:<25}▐", "two", "six")]
        );
        assert_eq!(wide[3], format!("   {:<25}   eight", "four"));
        // Three columns hold all nine rows, so nothing is left to scroll to.
        let all = drawn(&mut page, 85, 5);
        assert_eq!(all[0], format!("   {:<25}   {:<25}   nine", "one", "five"));
        assert!(all.iter().all(|row| !row.contains('▐')), "{all:?}");
        assert!(page.scroll(Key::Down, 5));
        assert_eq!(drawn(&mut page, 85, 5), all);
    }

    #[test]
    fn a_page_that_fits_one_column_stays_in_one_however_wide_the_pane() {
        let rows = drawn(&mut page(), 120, 10);
        assert_eq!(rows[..9], NAMES.map(|name| format!("   {name}")));
    }

    #[test]
    fn scrolling_columns_stops_with_the_last_row_at_the_bottom_of_the_last_column() {
        let mut page = page();
        drawn(&mut page, 57, 4);
        assert!(page.scroll(Key::End, 4));
        let end = drawn(&mut page, 57, 4);
        assert_eq!(end[0], format!("   {:<25}   seven", "four"));
        assert_eq!(end[2], format!("   {:<25}   {:<25}▐", "six", "nine"));
        assert!(page.scroll(Key::Up, 4));
        assert_eq!(drawn(&mut page, 57, 4)[0], format!("   {:<25}   six", "three"));
    }

    #[test]
    fn columns_break_between_sections_where_the_whole_page_then_fits() {
        let section = |name| [heading(name), note("first"), note("second"), HelpRow::new()];
        let mut page = Page::new(["Marks", "Keys", "Mouse"].into_iter().flat_map(section).collect());
        // Five rows hold a section and most of the next: the next starts a column instead.
        let rows = drawn(&mut page, 85, 6);
        assert_eq!(rows[0], format!(" {:<27} {:<27} Mouse", "Marks", "Keys"));
        assert_eq!(rows[2], format!("   {:<25}   {:<25}   second", "second", "second"));
        assert_eq!(rows[3..5], ["", ""]);
        // With room for two columns only, the page scrolls through even ones.
        let rows = drawn(&mut page, 57, 6);
        assert_eq!(rows[0], format!(" {:<27}   {:<25}▐", "Marks", "first"));
        assert_eq!(rows[4], format!(" {:<27}   first", "Keys"));
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
        assert_eq!(hits.span(0), None);
        assert_eq!(hits.span(1), None);
        assert_eq!(hits.span(2), Some(1..3));
        assert_eq!(hits.span(3), Some(6..12));
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
