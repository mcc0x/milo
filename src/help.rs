//! What the colours, weights and marks mean, and the keys: a page shown
//! over the pane on `?`, so nobody has to leave to find out.

use crate::git::Kind;
use crate::keys::Key;
use crate::screen::{Screen, Style};
use crate::ui::{self, ADD, DEL, DIM};

/// One row of a page: pieces of text, each in its own style.
type Row = Vec<(&'static str, Style)>;

fn heading(text: &'static str) -> Row {
    vec![(text, Style::PLAIN.bold())]
}

/// A sample drawn as it appears, then what it means. Only the sample's own
/// letters take its style: a struck-through gap would be a stray line.
fn sample(text: &'static str, st: Style, meaning: &'static str) -> Row {
    let shown = text.trim_end();
    vec![("  ", Style::PLAIN), (shown, st), (&text[shown.len()..], Style::PLAIN), ("  ", Style::PLAIN), (meaning, DIM)]
}

fn key(keys: &'static str, does: &'static str) -> Row {
    vec![("  ", Style::PLAIN), (keys, Style::PLAIN), ("  ", Style::PLAIN), (does, DIM)]
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
            sample("docs 3  ", Style::PLAIN, "3 changed inside"),
            sample("+2 −1   ", ADD, "lines changed"),
            sample("●       ", Style::PLAIN.bold(), "being written"),
            sample("•       ", Style::PLAIN, "changed, unseen"),
            Row::new(),
            heading("History"),
            sample("○", Style::PLAIN.bold(), "files to commit"),
            sample("●", Style::PLAIN.bold(), "a commit to push"),
            sample("●", Style::PLAIN.faint(), "already pushed"),
            Row::new(),
        ]);
    }
    rows.extend([heading("Keys"), key("↑ ↓  ", "move"), key("enter", "open"), key("e    ", "open every folder")]);
    if git {
        rows.extend([key("n N  ", "next, previous"), key("c    ", "changed only"), key("t    ", "history on or off")]);
    }
    rows.extend([key("r    ", "reload"), key("q    ", "quit")]);
    rows
}

/// The page for the reader.
fn reader() -> Vec<Row> {
    vec![
        heading("Lines"),
        sample("+", ADD.bold(), "added"),
        sample("-", DEL, "removed"),
        sample("word", Style::PLAIN.reverse(), "what changed"),
        Row::new(),
        heading("Keys"),
        key("↑ ↓  ", "scroll"),
        key("space", "page down"),
        key("b    ", "page up"),
        key("n N  ", "next, previous"),
        key("← →  ", "see long lines"),
        key("g G  ", "top, bottom"),
        key("q    ", "close"),
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
        s.put_fit(1, h - 1, w - 2, "scroll keys move · other keys close this", DIM);
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
