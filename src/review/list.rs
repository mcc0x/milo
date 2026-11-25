//! The list of a review's notes: which are waiting, and the text each is copied as.

use super::notes::{self, Note};
use crate::help;
use crate::screen::{Screen, Style, Wash};
use crate::ui::{FAINT, HEADING};
use crate::width::str_width;

/// What is chosen in the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Chosen {
    /// Every waiting note, as one message. Offered while a note waits.
    Waiting,
    Note(usize),
}

#[derive(Clone, Copy)]
pub struct List {
    pub chosen: Chosen,
    /// The first entry shown, where the list is longer than its rows.
    top: usize,
    /// Asked whether to remove the chosen note.
    pub removing: bool,
    /// What the last key did, shown until the next one.
    pub said: Option<&'static str>,
}

fn waiting(notes: &[Note]) -> usize {
    notes.iter().filter(|note| !note.copied).count()
}

impl List {
    /// Opens on the waiting notes where there are any, as that is what is copied most.
    pub fn open(notes: &[Note]) -> Self {
        let chosen = if waiting(notes) > 0 { Chosen::Waiting } else { Chosen::Note(0) };
        Self { chosen, top: 0, removing: false, said: None }
    }

    /// The list's entries in order. `Waiting` comes first, while a note waits.
    fn entries(notes: &[Note]) -> impl Iterator<Item = Chosen> {
        (waiting(notes) > 0).then_some(Chosen::Waiting).into_iter().chain((0..notes.len()).map(Chosen::Note))
    }

    /// Keeps the choice on an entry the list still has, after notes were copied or removed.
    pub fn settle(&mut self, notes: &[Note]) {
        if !Self::entries(notes).any(|entry| entry == self.chosen) {
            self.chosen = match self.chosen {
                Chosen::Note(at) if !notes.is_empty() => Chosen::Note(at.min(notes.len() - 1)),
                _ => Self::entries(notes).next().unwrap_or(Chosen::Waiting),
            };
        }
    }

    /// Moves the choice by `delta` entries, stopping at the ends.
    pub fn step(&mut self, notes: &[Note], delta: isize) -> bool {
        let entries: Vec<Chosen> = Self::entries(notes).collect();
        let at = entries.iter().position(|entry| *entry == self.chosen).unwrap_or(0);
        let to = at.saturating_add_signed(delta).min(entries.len().saturating_sub(1));
        let moved = entries.get(to).is_some_and(|entry| *entry != self.chosen);
        if moved {
            self.chosen = entries[to];
        }
        moved
    }

    /// The text the chosen entry is copied as.
    pub fn text(&self, notes: &[Note]) -> Option<String> {
        match self.chosen {
            Chosen::Waiting => notes::waiting(notes),
            Chosen::Note(at) => notes.get(at).map(notes::one),
        }
    }

    pub fn draw(&mut self, s: &mut Screen, notes: &[Note]) {
        s.clear();
        if s.w < 2 || s.h == 0 {
            return;
        }
        let width = s.w - 2;
        let waits = waiting(notes);
        let x = 1 + s.put_fit(1, 0, width, "Notes", HEADING);
        let counts = format!(" · {waits} to copy · {} copied", notes.len() - waits);
        s.put_fit(x, 0, (width + 1).saturating_sub(x), &counts, FAINT);
        let entries: Vec<Chosen> = Self::entries(notes).collect();
        // The list takes up to half of what is left under the heading and above the hints.
        let body = s.h.saturating_sub(2);
        let rows = entries.len().min(body.div_ceil(2));
        let at = entries.iter().position(|entry| *entry == self.chosen).unwrap_or(0);
        self.top = self.top.clamp((at + 1).saturating_sub(rows.max(1)), at);
        for (i, entry) in entries.iter().enumerate().skip(self.top).take(rows) {
            let y = 1 + i - self.top;
            draw_entry(s, y, *entry, notes, waits);
            if *entry == self.chosen {
                s.wash(0, y, s.w, Wash::Bar);
                s.put(0, y, 1, "›", Style::PLAIN.bold());
                s.wash(0, y, 1, Wash::Bar);
            }
        }
        self.draw_preview(s, 1 + rows, body.saturating_sub(rows), notes);
        if s.h >= 2 {
            self.draw_hint(s, notes);
        }
    }

    /// Shows the text the chosen entry is copied as, wrapped to the pane and cut to the rows there are.
    fn draw_preview(&self, s: &mut Screen, y: usize, rows: usize, notes: &[Note]) {
        if rows < 2 {
            return;
        }
        let width = s.w - 2;
        s.put_fit(1, y, width, "Copied as", FAINT);
        let text = self.text(notes).unwrap_or_default();
        let lines: Vec<&str> = super::compose::rows(&text, width).into_iter().map(|row| &text[row]).collect();
        let room = rows - 1;
        for (i, line) in lines.iter().take(room).enumerate() {
            let cut = i + 1 == room && lines.len() > room;
            s.put_fit(1, y + 1 + i, width, if cut { "…" } else { line }, Style::PLAIN);
        }
    }

    fn draw_hint(&self, s: &mut Screen, notes: &[Note]) {
        let y = s.h - 1;
        let width = s.w - 2;
        if self.removing {
            s.put_fit(1, y, width, "Remove this note? y remove · n keep", Style::PLAIN);
        } else if let Some(said) = self.said {
            let used = s.put_fit(1, y, width, said, Style::PLAIN);
            if used + 9 <= width {
                s.put(1 + used, y, 9, " · ? help", FAINT);
            }
        } else if matches!(self.chosen, Chosen::Waiting) {
            help::draw_hint(s, "y copy all", &["↑↓ choose", "Esc back"], "? help");
        } else if notes.is_empty() {
            help::draw_hint(s, "", &["Esc back"], "? help");
        } else {
            help::draw_hint(s, "y copy", &["↑↓ choose", "d remove", "e edit", "Esc back"], "? help");
        }
    }
}

fn draw_entry(s: &mut Screen, y: usize, entry: Chosen, notes: &[Note], waits: usize) {
    let width = s.w - 2;
    match entry {
        Chosen::Waiting => {
            let text = match waits {
                1 => "The 1 waiting note".to_string(),
                waits => format!("All {waits} waiting notes, as one message"),
            };
            s.put_fit(2, y, width.saturating_sub(1), &text, Style::PLAIN);
        }
        Chosen::Note(at) => {
            let note = &notes[at];
            let state = if note.copied { "copied" } else { "to copy" };
            let state_at = (s.w - 1).saturating_sub(state.len());
            let room = state_at.saturating_sub(4);
            let lead = format!("{}. ", at + 1);
            let mut x = 2 + put_place(s, (2, y), room, &lead, &note.path, &note.place(), Style::PLAIN);
            if x + 4 < state_at {
                x += s.put(x, y, 3, " · ", FAINT);
                s.put_fit(x, y, state_at.saturating_sub(x + 1), note.summary(), FAINT);
            }
            if state_at > 2 {
                s.put(state_at, y, state.len(), state, if note.copied { FAINT } else { Style::PLAIN });
            }
        }
    }
}

/// Draws `lead`, a file's name and which of its lines, as `LEADname, lines`. Where
/// they do not all fit, the name is shortened first: which lines, and whether
/// their numbers are old ones, is what a name alone cannot say. Returns the columns used.
pub(super) fn put_place(
    s: &mut Screen,
    (x, y): (usize, usize),
    room: usize,
    lead: &str,
    path: &str,
    place: &str,
    st: Style,
) -> usize {
    let fixed = str_width(lead) + 2 + str_width(place);
    if fixed + 2 > room {
        return s.put_fit(x, y, room, &format!("{lead}{path}, {place}"), st);
    }
    let mut used = s.put(x, y, room, lead, st);
    used += s.put_middle(x + used, y, room - fixed, path, st);
    used += s.put(x + used, y, 2, ", ", st);
    used + s.put(x + used, y, room - used, place, st)
}
