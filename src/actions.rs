//! The same explicit file choices in the tree, its search results, and the reader.

use std::path::Path;

use crate::help::{self, HintHits};
use crate::keys::Key;
use crate::screen::{Screen, Style};
use crate::ui::{self, FAINT, HEADING};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Read,
    Edit,
    Open,
    Reveal,
    Preview,
}

impl Choice {
    fn label(self) -> &'static str {
        match self {
            Self::Read => "Read in Milo",
            Self::Edit => "Edit in Milo",
            Self::Open => "Open in default app",
            Self::Reveal => "Reveal in Finder",
            Self::Preview => "Preview with tkmp",
        }
    }
}

pub(crate) enum Step {
    Stay,
    Close,
    Run(Choice),
}

pub(crate) struct Menu {
    choices: Vec<Choice>,
    preview_unavailable: Option<&'static str>,
    selected: usize,
    painted_selection: usize,
    first: usize,
    row: usize,
    shown: usize,
    width: usize,
    footer: HintHits<'static>,
}

impl Menu {
    pub(crate) fn new(choices: Vec<Choice>, root: &Path) -> Self {
        let preview_unavailable =
            if choices.contains(&Choice::Preview) { crate::preview::availability(root).err() } else { None };
        Self {
            choices,
            preview_unavailable,
            selected: 0,
            painted_selection: 0,
            first: 0,
            row: 0,
            shown: 0,
            width: 0,
            footer: HintHits::default(),
        }
    }

    fn unavailable(&self, choice: Choice) -> Option<&'static str> {
        if choice == Choice::Preview { self.preview_unavailable } else { None }
    }

    pub(crate) fn draw(&mut self, s: &mut Screen, path: &str) {
        s.clear();
        // In a short split keep at least one selectable row, ahead of decoration.
        self.row = usize::from(s.h >= 3);
        let room = s.w.saturating_sub(2);
        if self.row != 0 {
            let used = ui::section_heading(s, 0, room, "Actions");
            s.put_fit(used + 3, 0, room.saturating_sub(used + 2), path, FAINT);
        }
        let footer = usize::from(s.h >= 2);
        self.shown = self.choices.len().min(s.h.saturating_sub(self.row + footer));
        self.first = ui::selection_start(self.selected, self.choices.len(), self.shown);
        self.width = s.w;
        self.painted_selection = self.selected;
        for (i, choice) in self.choices.iter().skip(self.first).take(self.shown).enumerate() {
            let y = self.row + i;
            let selected = self.first + i == self.selected;
            let inactive = self.unavailable(*choice);
            let style = if inactive.is_some() {
                FAINT
            } else if selected {
                HEADING
            } else {
                Style::PLAIN
            };
            if selected {
                s.put(1, y, 1, "›", style);
            }
            s.put_fit(3, y, s.w.saturating_sub(4), inactive.unwrap_or_else(|| choice.label()), style);
        }
        if self.preview_unavailable.is_none()
            && self.choices.contains(&Choice::Preview)
            && self.row + self.shown + 2 < s.h
        {
            s.put_fit(1, self.row + self.shown + 1, room, "tkmp: trusted files only", FAINT);
        }
        self.footer = if footer == 0 {
            HintHits::default()
        } else {
            help::draw_hint(s, "↑↓ move", &["Enter choose"], "Esc back")
        };
    }

    fn hit(&self, x: usize, y: usize) -> Option<(Choice, std::ops::Range<usize>)> {
        let row = y.checked_sub(self.row).filter(|i| *i < self.shown)?;
        let choice = self.choices[self.first + row];
        let span = 1..self.width.saturating_sub(1).min(3 + choice.label().len());
        (self.width > 4 && span.contains(&x) && self.unavailable(choice).is_none()).then_some((choice, span))
    }

    pub(crate) fn hover_span(&self, x: usize, y: usize) -> Option<std::ops::Range<usize>> {
        self.footer.span_at(x, y).or_else(|| self.hit(x, y).map(|(_, span)| span))
    }

    pub(crate) fn key(&mut self, key: Key) -> Step {
        match key {
            Key::Esc | Key::Char('q') | Key::Ctrl(b'a') => Step::Close,
            Key::Click(x, y) => match self.footer.index_at(x, y) {
                Some(0) => self.key(if x == 1 { Key::Up } else { Key::Down }),
                Some(1) if self.selected == self.painted_selection => self.key(Key::Enter),
                Some(1) => Step::Stay,
                Some(2) => Step::Close,
                _ => self.hit(x, y).map_or(Step::Stay, |(choice, _)| Step::Run(choice)),
            },
            Key::Enter if (self.first..self.first + self.shown).contains(&self.selected) && self.width > 4 => {
                let choice = self.choices[self.selected];
                if self.unavailable(choice).is_none() { Step::Run(choice) } else { Step::Stay }
            }
            _ => {
                if let Some(delta) = ui::selection_delta(key, self.shown) {
                    self.selected =
                        self.selected.saturating_add_signed(delta).min(self.choices.len().saturating_sub(1));
                }
                Step::Stay
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_menu_click_uses_the_visible_scrolled_choice() {
        let mut menu = Menu::new(vec![Choice::Read, Choice::Open, Choice::Reveal], Path::new("."));
        let mut s = Screen::sized(27, 3);
        menu.draw(&mut s, "plan.md");
        menu.key(Key::End);
        assert!(matches!(menu.key(Key::Enter), Step::Stay), "must paint the new row before acting on it");
        menu.draw(&mut s, "plan.md");
        assert!(s.text()[1].contains("Reveal in Finder"));
        assert!(matches!(menu.key(Key::Click(5, 1)), Step::Run(Choice::Reveal)));
        assert!(matches!(menu.key(Key::Click(24, 1)), Step::Stay));
        assert!(matches!(menu.key(Key::Esc), Step::Close));
    }

    #[test]
    fn choose_footer_waits_for_the_new_selection_to_be_painted() {
        let mut menu = Menu::new(vec![Choice::Read, Choice::Open], Path::new("."));
        let mut s = Screen::sized(60, 5);
        menu.draw(&mut s, "plan.md");
        let choose = Key::Click(menu.footer.span(1).unwrap().start, 4);
        assert!(matches!(menu.key(choose), Step::Run(Choice::Read)));

        menu.key(Key::Down);
        assert!(matches!(menu.key(choose), Step::Stay));
        assert!(matches!(menu.key(Key::Click(5, 1)), Step::Run(Choice::Read)));
        menu.draw(&mut s, "plan.md");
        assert!(matches!(menu.key(choose), Step::Run(Choice::Open)));
    }
}
