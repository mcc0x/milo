//! Draws and scrolls the in-pane guide to marks and keys.

use crate::git::Kind;
use crate::keys::Key;
use crate::screen::{Screen, Style};
use crate::ui::{self, ADD, DEL, FAINT};

/// One row of a page: pieces of text, each in its own style.
type Row = Vec<(&'static str, Style)>;

const FOOTER: &str = "↑↓ scroll · Esc closes";

fn heading(text: &'static str) -> Row {
    vec![(text, Style::PLAIN.bold())]
}

/// Leave the sample's padding plain so a strike stops at the last letter.
fn sample(text: &'static str, st: Style, meaning: &'static str) -> Row {
    let shown = text.trim_end();
    vec![
        ("  ", Style::PLAIN),
        (shown, st),
        (&text[shown.len()..], Style::PLAIN),
        ("  ", Style::PLAIN),
        (meaning, FAINT),
    ]
}

fn key(keys: &'static str, does: &'static str) -> Row {
    vec![("  ", Style::PLAIN), (keys, Style::PLAIN), ("  ", Style::PLAIN), (does, FAINT)]
}

/// The page for the outline. Without git there are no colours to explain.
fn outline(git: bool) -> Vec<Row> {
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
            Row::new(),
        ]);
    }
    rows.extend([
        heading("Tree keys"),
        key("↑ ↓  ", "move (k j)"),
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
        rows.extend([Row::new(), heading("Strip keys"), key("← →  ", "scroll (h l)"), key("g G  ", "first/last")]);
    }
    rows
}

fn history() -> Vec<Row> {
    vec![
        heading("Commits"),
        sample("○", Style::PLAIN.bold(), "files to commit"),
        sample("●", Style::PLAIN.bold(), "a commit to push"),
        sample("●", FAINT, "already pushed"),
        sample("●", Style::PLAIN, "no remote"),
        sample("•", Style::PLAIN.bold(), "just arrived"),
        sample("+2", ADD, "lines added"),
        sample("−1", DEL, "lines removed"),
        Row::new(),
        heading("Keys"),
        key("↑ ↓  ", "move (k j)"),
        key("Pg↑↓ ", "move by a page"),
        key("g G  ", "first/last"),
        key("Enter", "show/hide files"),
        key("r    ", "reload"),
        key("q    ", "quit"),
    ]
}

fn reader() -> Vec<Row> {
    vec![
        heading("Lines"),
        sample("+", ADD.bold(), "added"),
        sample("-", DEL, "removed"),
        sample("word", Style::PLAIN.reverse(), "what changed"),
        Row::new(),
        heading("Keys"),
        key("↑ ↓  ", "scroll (k j)"),
        key("Space", "page down"),
        key("b    ", "page up"),
        key("n N  ", "next/prev change"),
        key("← →  ", "scroll (h l)"),
        key("g G  ", "top/bottom"),
        key("q Esc", "close"),
    ]
}

/// A page over the pane, with its own scroll position.
pub struct Page {
    rows: Vec<Row>,
    top: usize,
}

impl Page {
    pub fn outline(git: bool) -> Self {
        Self { rows: outline(git), top: 0 }
    }

    pub fn history() -> Self {
        Self { rows: history(), top: 0 }
    }

    pub fn reader() -> Self {
        Self { rows: reader(), top: 0 }
    }

    pub fn draw(&mut self, s: &mut Screen) {
        s.clear();
        let (w, h) = (s.w, s.h);
        if w < 4 || h < 2 {
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
        s.put_fit(1, h - 1, w - 2, FOOTER, FAINT);
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
    use crate::width::str_width;

    #[test]
    fn every_help_row_fits_a_28_column_pane() {
        let room = 28 - 2; // The first and last columns are reserved.
        assert!(str_width(FOOTER) <= room);
        for row in outline(true).into_iter().chain(outline(false)).chain(history()).chain(reader()) {
            let width: usize = row.iter().map(|(text, _)| str_width(text)).sum();
            assert!(width <= room, "{width} columns: {:?}", row.iter().map(|(text, _)| text).collect::<Vec<_>>());
        }
    }
}
