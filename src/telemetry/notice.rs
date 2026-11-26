use std::rc::Rc;

use super::First;
use crate::help::Page;
use crate::keys::Key;
use crate::pane::{After, Pane};
use crate::screen::{Screen, Style};
use crate::setup::wrap;
use crate::ui::{self, FAINT, HEADING};

const TITLE: &str = "Anonymous usage counts";
const BODY: &str = "Starting next launch, Milo can count this install once per UTC day and month. It sends the date and whether this install is new or active this month. No IDs, files or account details.";
const SETTING: &str = "Change this in Settings or set MILO_TELEMETRY=0.";
const CHOICES: &str = "Enter keep on   o turn off";

pub(crate) struct Notice {
    first: Rc<First>,
    full: bool,
    failed: bool,
}

impl Notice {
    pub(crate) fn new(first: Rc<First>) -> Self {
        Self { first, full: false, failed: false }
    }
}

impl Pane for Notice {
    fn draw(&mut self, screen: &mut Screen) {
        screen.clear();
        let width = screen.w.saturating_sub(2).min(62);
        let x = (screen.w - width) / 2;
        let body = wrap(BODY, width);
        let setting = wrap(SETTING, width);
        let rows = body.clone().count() + setting.clone().count() + 7;
        self.full = width >= CHOICES.len() && rows <= screen.h;
        if !self.full {
            screen.put_fit(0, 0, screen.w, TITLE, HEADING);
            screen.put_fit(0, 1, screen.w, "Enlarge to read, or:", FAINT);
            screen.put_fit(0, 2, screen.w, "o off   Esc later", Style::PLAIN);
            return;
        }
        let top = (screen.h - rows) / 2;
        screen.put(x, top, width, TITLE, HEADING);
        let mut y = top + 2;
        for line in body.chain([""]).chain(setting) {
            screen.put(x, y, width, line, Style::PLAIN);
            y += 1;
        }
        screen.put(x, y + 1, width, CHOICES, HEADING);
        screen.put(x, y + 2, width, "Esc decide later", FAINT);
        if self.failed {
            screen.put_fit(x, y + 3, width, "Could not save. Esc to use Milo without reporting.", ui::DEL);
        }
    }

    fn help(&self) -> Page {
        Page::new(vec![
            vec![(TITLE, HEADING)],
            vec![("Enter  Keep on", Style::PLAIN)],
            vec![("o      Turn off", Style::PLAIN)],
            vec![("Esc    Decide later", Style::PLAIN)],
        ])
    }

    fn key(&mut self, key: Key) -> After {
        match key {
            Key::Enter if self.full => {}
            Key::Char('o' | 'O') => {}
            Key::Esc => {
                self.first.proceed.set(true);
                return After::Leave;
            }
            Key::Char('q') => return After::Leave,
            _ => return After::Ignored,
        }
        if self.first.visit.acknowledge(key == Key::Enter).is_err() {
            self.failed = true;
            return After::Stay;
        }
        self.first.proceed.set(true);
        After::Leave
    }
}
