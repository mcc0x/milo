//! Draws and scrolls the in-pane guide to marks and keys.

use crate::git::Kind;
use crate::keys::Key;
use crate::screen::{Screen, Style};
use crate::ui::{self, ADD, DEL, FAINT, HEADING};
use crate::width::str_width;

/// One row of a page: pieces of text, each in its own style. A row must
/// fit a pane 28 columns wide: 26 characters at most.
pub type HelpRow = Vec<(&'static str, Style)>;

const SEPARATOR: &str = " · ";

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

/// The page for the outline. Without git there are no colours to explain.
fn outline(git: bool) -> Vec<HelpRow> {
    let mut rows = Vec::new();
    if git {
        let name = |kind| ui::kind_style(kind).bold();
        rows.extend([
            heading("Files"),
            sample("edited  ", name(Kind::Modified), "changed"),
            sample("new     ", name(Kind::Untracked), "added"),
            sample("removed ", name(Kind::Deleted), "deleted"),
            sample("moved   ", name(Kind::Renamed), "renamed"),
            sample("conflict", name(Kind::Conflict), "needs merging"),
            sample("bold    ", Style::PLAIN.bold(), "to commit"),
            sample("regular ", Style::PLAIN, "to push"),
            sample("docs 3  ", Style::PLAIN, "3 changed"),
            sample("+2      ", ADD, "lines added"),
            sample("−1      ", DEL, "lines removed"),
            sample("● ○     ", Style::PLAIN.bold(), "being written"),
            sample("•       ", Style::PLAIN, "just changed"),
            HelpRow::new(),
        ]);
    }
    rows.extend([
        heading("Tree keys"),
        key("↑↓   ", "move (k j)"),
        key("Enter", "toggle folder"),
        key("Enter", "open file"),
        key("Space", "toggle folder"),
        key("←    ", "close or parent"),
        key("→    ", "open folder"),
        key("{ }  ", "prev/next folder"),
        key("K J  ", "prev/next sibling"),
        key("p    ", "parent folder"),
        key("e    ", "open all/restore"),
    ]);
    if git {
        rows.extend([key("n N  ", "next/prev change"), key("c    ", "changed only")]);
    }
    rows.extend([key("r    ", "reload"), key("q    ", "quit")]);
    if git {
        rows.extend([HelpRow::new(), heading("Strip keys"), key("←→   ", "scroll (h l)"), key("g G  ", "first/last")]);
    }
    rows
}

fn history() -> Vec<HelpRow> {
    vec![
        heading("Commits"),
        sample("○", Style::PLAIN.bold(), "files to commit"),
        sample("●", Style::PLAIN.bold(), "a commit to push"),
        sample("●", FAINT, "already pushed"),
        sample("●", Style::PLAIN, "no comparison"),
        sample("•", Style::PLAIN.bold(), "just arrived"),
        sample("+2", ADD, "lines added"),
        sample("−1", DEL, "lines removed"),
        HelpRow::new(),
        heading("Keys"),
        key("↑↓   ", "move (k j)"),
        key("Page Up", "previous page"),
        key("Page Down", "next page"),
        key("g G  ", "first/last"),
        key("Enter", "show files"),
        key("Enter", "hide files"),
        key("Enter", "show all"),
        key("Enter", "read"),
        key("← h  ", "parent or close"),
        key("r    ", "reload"),
        key("q    ", "quit"),
        HelpRow::new(),
        note("Click to select a file."),
        note("Double-click to read."),
        note("Click to toggle files."),
    ]
}

fn reader(committed: bool) -> Vec<HelpRow> {
    let mut rows = vec![
        heading("Lines"),
        sample("+", ADD.bold(), "added"),
        sample("-", DEL, "removed"),
        sample("word", Style::PLAIN.reverse(), "what changed"),
        HelpRow::new(),
        heading("Keys"),
        key("↑↓   ", "scroll (k j)"),
        key("Space", "page down"),
        key("b    ", "page up"),
        key("n N  ", "next/prev change"),
        key("←→   ", "scroll (h l)"),
        key("g G  ", "top/bottom"),
        key("q Esc", "close"),
    ];
    if committed {
        rows.insert(1, note("As of a commit"));
    }
    rows
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

    pub fn outline(git: bool) -> Self {
        Self { rows: outline(git), top: 0 }
    }

    pub fn history() -> Self {
        Self { rows: history(), top: 0 }
    }

    pub fn reader(committed: bool) -> Self {
        Self { rows: reader(committed), top: 0 }
    }

    /// Help needs a row for its words and a row for its keys.
    pub fn fits(s: &Screen) -> bool {
        s.w >= 4 && s.h >= 2
    }

    pub fn draw(&mut self, s: &mut Screen) {
        s.clear();
        let (w, h) = (s.w, s.h);
        if !Self::fits(s) {
            return;
        }
        let shown = h - 1;
        self.top = self.top.min(self.rows.len().saturating_sub(shown));
        for (y, row) in self.rows.iter().skip(self.top).take(shown).enumerate() {
            let mut x = 1;
            for (text, st) in row {
                x += s.put(x, y, (w - 1).saturating_sub(x), text, *st);
            }
        }
        ui::thumb(s, w - 1, 0, shown, self.rows.len(), self.top);
        draw_hint(s, "↑↓ scroll", &["Esc close"], "q close");
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
    fn every_help_row_fits_a_28_column_pane() {
        let room = 28 - 2; // The first and last columns are reserved.
        let pages = [outline(true), outline(false), history(), reader(false), reader(true)];
        for row in pages.into_iter().flatten() {
            let width: usize = row.iter().map(|(text, _)| str_width(text)).sum();
            assert!(width <= room, "{width} columns: {:?}", row.iter().map(|(text, _)| text).collect::<Vec<_>>());
        }
    }

    #[test]
    fn a_narrow_hint_drops_middle_items_without_clipping_the_end() {
        let mut screen = Screen::sized(23, 1);
        draw_hint(&mut screen, "Enter read", &["r reload", "q quit"], "? help");
        assert_eq!(screen.text(), [" Enter read · ? help"]);
        let mut help = Screen::sized(28, 2);
        Page::history().draw(&mut help);
        assert_eq!(help.text()[1], " ↑↓ scroll · q close");
    }
}
