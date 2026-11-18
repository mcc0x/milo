//! What the colours, weights and marks mean, and the keys: a page shown
//! over the pane on `?`, so nobody has to leave to find out.

use crate::git::Kind;
use crate::term::{Screen, Style};
use crate::ui::{self, ADD, DEL, DIM};

/// One row of a page: pieces of text, each in its own style.
pub type Row = Vec<(&'static str, Style)>;

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
pub fn outline(git: bool) -> Vec<Row> {
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
    rows.extend([
        heading("Keys"),
        key("↑ ↓  ", "move"),
        key("enter", "open"),
        key("e    ", "open every folder"),
    ]);
    if git {
        rows.extend([
            key("n N  ", "next, previous"),
            key("c    ", "changed only"),
            key("t    ", "history on or off"),
        ]);
    }
    rows.extend([key("r    ", "reload"), key("q    ", "quit")]);
    rows
}

/// The page for the reader.
pub fn reader() -> Vec<Row> {
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

/// Draws a page from row `top`, which is first brought within range.
pub fn draw(s: &mut Screen, rows: &[Row], top: &mut usize) {
    s.clear();
    let (w, h) = (s.w, s.h);
    if w < 4 || h < 2 {
        return;
    }
    let body = h - 1;
    *top = (*top).min(rows.len().saturating_sub(body));
    for (y, row) in rows.iter().skip(*top).take(body).enumerate() {
        let mut x = 1;
        for (text, st) in row {
            x += s.put(x, y, (w - 1).saturating_sub(x), text, *st);
        }
    }
    ui::thumb(s, w - 1, 0, body, rows.len(), *top);
    s.put_fit(1, h - 1, w - 2, "any key closes this", DIM);
}
