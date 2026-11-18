//! Choose the panes around a Rex tab, and opt in to Claude Code's usage limits.

mod files;
mod install;
mod lock;
mod settings;

use std::ffi::OsStr;
use std::io;

use crate::help::{self, Page};
use crate::keys::Key;
use crate::screen::{Screen, Style};
use crate::ui::{FAINT, Pane};
use crate::watch::Scope;
use crate::{paths, rex};
use install::{Claude, Layout, Store};

const CLAUDE: usize = 5;
const CHOICES: [(&str, &str); 6] = [
    ("Outline", "Files on the left"),
    ("History", "Commits on the left"),
    ("Usage", "A strip across the top"),
    ("Column width", "Room for files and commits"),
    ("Arrange every new tab", "Only tabs opened as a shell"),
    ("Show Claude Code limits", "For the usage strip"),
];

pub struct Setup<R = fn(&[&OsStr]) -> bool> {
    store: Option<Store>,
    inside: bool,
    target: Option<rex::Target>,
    run: R,
    layout: Layout,
    claude: Claude,
    show_limits: bool,
    cursor: usize,
    message: Option<(&'static str, Style)>,
}

impl Setup {
    pub fn open() -> Self {
        Self::new(Store::real(), rex::inside(), rex::Target::current(), rex::run)
    }
}

impl<R: Fn(&[&OsStr]) -> bool + 'static> Setup<R> {
    fn new(store: Option<Store>, inside: bool, target: Option<rex::Target>, run: R) -> Self {
        let mut message = None;
        let mut layout = Layout::default();
        if inside && let Some(store) = &store {
            match store.layout() {
                Ok(installed) => layout = installed,
                Err(why) => message = Some((why, Style::PLAIN.bold())),
            }
        }
        let claude = store.as_ref().map_or(Claude::Invalid, Store::claude);
        Self { store, inside, target, run, layout, claude, show_limits: claude == Claude::On, cursor: 0, message }
    }

    fn save(&mut self) -> Result<&'static str, &'static str> {
        let Some(store) = &self.store else { return Err("Could not find your settings folders.") };
        let limits =
            (self.claude.editable() && self.show_limits != (self.claude == Claude::On)).then_some(self.show_limits);
        store.save(self.inside.then_some(&self.layout), limits, &self.run)?;
        self.claude = store.claude();
        self.show_limits = self.claude == Claude::On;
        Ok(match (self.inside, limits) {
            (true, Some(_)) => "Saved Rex and Claude Code choices.",
            (true, None) => "Saved Rex layout choices.",
            (false, Some(_)) => "Saved Claude Code choices.",
            (false, None) => "No changes to save.",
        })
    }

    fn arrange(&self) -> Result<(), &'static str> {
        let Some(store) = &self.store else { return Err("Could not find your settings folders.") };
        let Some(target) = &self.target else { return Err("Could not find this Rex pane.") };
        store.arrange(&self.layout, target, &self.run)
    }

    fn selected(&self) -> usize {
        if self.inside { self.cursor } else { CLAUDE }
    }

    fn toggle(&mut self) {
        match self.selected() {
            0..=2 => self.layout.panes[self.cursor] = !self.layout.panes[self.cursor],
            3 => self.layout.next_width(),
            4 => self.layout.new_tabs = !self.layout.new_tabs,
            CLAUDE if self.claude.editable() => self.show_limits = !self.show_limits,
            _ => {}
        }
        self.message = None;
    }

    fn choice(&self, s: &mut Screen, i: usize, y: usize, width: usize) {
        let on = |enabled| if enabled { "On" } else { "Off" };
        let status = match i {
            0..=2 => on(self.layout.panes[i]),
            3 => self.layout.width_name(),
            4 => on(self.layout.new_tabs),
            _ if self.claude == Claude::NotOwned => "On",
            _ if !self.claude.editable() => "—",
            _ => on(self.show_limits),
        };
        let selected = i == self.selected();
        let style = if selected { Style::PLAIN.bold() } else { Style::PLAIN };
        let (name, detail) = CHOICES[i];
        if selected {
            s.put(0, y, 1, "›", style);
        }
        let end = 3 + s.put_fit(2, y, width.saturating_sub(2), name, style);
        let end = end + 2 + s.put_fit(end, y, width.saturating_sub(end), status, style);
        if end < width {
            s.put_fit(end, y, width - end, detail, FAINT);
        }
    }

    fn footer(&self, s: &mut Screen) {
        let keys = if self.inside {
            "↑↓ move · Space change · a arrange · s save · q leave"
        } else {
            "Space change · s save · q leave"
        };
        let y = s.h - 1;
        let w = s.w.saturating_sub(1);
        if let Some((text, style)) = self.message {
            let end = 1 + s.put_fit(1, y, w, text, style);
            if end + 3 < s.w {
                s.put_fit(end, y, s.w - end, " · ", FAINT);
                s.put_fit(end + 3, y, s.w - end - 3, keys, FAINT);
            }
        } else {
            s.put_fit(1, y, w, keys, FAINT);
        }
    }

    fn draw_choices(&self, s: &mut Screen) {
        let end = s.h.saturating_sub(1);
        let top = if self.inside || s.h < 5 {
            1
        } else {
            s.put_fit(1, 1, s.w - 1, "Tab layouts need Rex.", FAINT);
            s.put_fit(1, 2, s.w - 1, "Panes work anywhere: milo outline, milo history, milo usage.", FAINT);
            3
        };
        let count = if self.inside { CHOICES.len() } else { 1 };
        let shown = end.saturating_sub(top).min(count);
        let skip = if self.inside { self.cursor.saturating_sub(shown.saturating_sub(1)) } else { 0 };
        let next = top + shown;
        let issue = match self.claude {
            Claude::Foreign => "Claude Code already has a status line; milo will not replace it.",
            Claude::NotOwned => "This Claude Code status line was not added by setup.",
            Claude::Invalid => "Claude Code settings must be a readable JSON object.",
            Claude::On | Claude::Off => "",
        };
        let notice_rows = usize::from(!issue.is_empty());
        let side = self.inside && s.w >= 87 && end >= 8 + notice_rows;
        let width = if side { 51 } else { s.w.saturating_sub(1) };
        for row in 0..shown {
            self.choice(s, if self.inside { row + skip } else { CLAUDE }, top + row, width);
        }
        if !issue.is_empty() && next < end {
            s.put_fit(2, if side { 8 } else { next }, s.w.saturating_sub(2), issue, FAINT);
        }
        if side {
            sketch(s, 52, 1, &self.layout);
        } else if self.inside && s.w >= 36 && end >= next + 8 + notice_rows {
            sketch(s, 2, next + 1 + notice_rows, &self.layout);
        }
    }
}

/// A seven-row sketch. Each room can be read without the explanations beside it.
fn sketch(s: &mut Screen, x: usize, y: usize, layout: &Layout) {
    const LINE: &str = "───────────────────────────────";
    const INTERIOR: usize = 31;
    let [outline, history, usage] = layout.panes;
    let left = outline || history;
    let column = layout.sketch_width();
    let joint = if left { 1 + column } else { 0 };
    let right = INTERIOR + 1;
    let mut draw = |x0, row, text| {
        s.put(x + x0, y + row, INTERIOR + 2 - x0, text, FAINT);
    };
    for (row, first, last, split) in [(0, "┌", "┐", "┬"), (6, "└", "┘", "┴")] {
        draw(0, row, first);
        draw(1, row, LINE);
        draw(right, row, last);
        if left {
            draw(joint, row, split);
        }
    }
    for row in 1..6 {
        draw(0, row, "│");
        draw(right, row, "│");
        if left {
            draw(joint, row, "│");
        }
    }
    if left {
        draw(2, if outline && history { 2 } else { 3 }, if outline { "Outline" } else { "History" });
        if outline && history {
            draw(0, 4, "├");
            draw(1, 4, &LINE[..3 * column]);
            draw(joint, 4, "┤");
            draw(2, 5, "History");
        }
    }
    if usage {
        draw(joint + 2, 1, "Usage");
        draw(joint, 2, "├");
        draw(joint + 1, 2, &LINE[..3 * (INTERIOR - joint)]);
        draw(right, 2, "┤");
    }
    draw(joint + 2, if usage { 4 } else { 3 }, "Your work");
}

impl<R: Fn(&[&OsStr]) -> bool + 'static> Pane for Setup<R> {
    fn draw(&mut self, s: &mut Screen) {
        s.clear();
        if s.w < 4 || s.h == 0 {
            return;
        }
        s.put_fit(1, 0, s.w - 1, "Milo setup", Style::PLAIN.bold());
        if s.h == 1 {
            return;
        }
        self.draw_choices(s);
        self.footer(s);
    }

    fn help(&self) -> Page {
        use help::{heading, key};
        let mut rows = vec![heading("Setup keys"), key("Space", "change (Enter)")];
        if self.inside {
            rows.extend([key("↑ ↓  ", "move (k j)"), key("a    ", "arrange this tab")]);
        }
        rows.extend([key("s    ", "save choices"), key("q    ", "leave")]);
        Page::new(rows)
    }

    fn watch_scope(&self) -> Scope {
        Scope { roots: Vec::new(), ignored: Vec::new(), complete: true }
    }

    fn check(&mut self) -> bool {
        true
    }

    fn key(&mut self, key: Key) -> bool {
        match key {
            Key::Up | Key::Char('k') if self.inside => self.cursor = self.cursor.saturating_sub(1),
            Key::Down | Key::Char('j') if self.inside => self.cursor = (self.cursor + 1).min(CLAUDE),
            Key::Enter | Key::Char(' ') => self.toggle(),
            Key::Char('a') if self.inside => match self.arrange() {
                Ok(()) => return true,
                Err(why) => return self.report(Err(why)),
            },
            Key::Char('s') if self.inside || self.claude.editable() => {
                let saved = self.save();
                return self.report(saved);
            }
            Key::Esc => return true,
            _ => {}
        }
        false
    }
}

impl<R> Setup<R> {
    fn report(&mut self, result: Result<&'static str, &'static str>) -> bool {
        self.message = Some(match result {
            Ok(message) => (message, FAINT),
            Err(why) => (why, Style::PLAIN.bold()),
        });
        false
    }
}

pub fn offer_once() -> bool {
    rex::inside() && Store::real().is_some_and(|store| store.offer_once())
}

pub fn listen(stop: bool) -> i32 {
    fn run(stop: bool) -> io::Result<()> {
        let state = paths::state_dir().ok_or(io::ErrorKind::NotFound)?;
        if stop {
            lock::stop(&state)
        } else {
            let config = paths::config_dir().ok_or(io::ErrorKind::NotFound)?;
            lock::listen(&state, &config.join("listener.lua"))
        }
    }
    if run(stop).is_ok() {
        return 0;
    }
    eprintln!("milo: could not {} the Rex listener.", if stop { "stop" } else { "start" });
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::fs;

    fn store(fixture: &Fixture) -> Store {
        Store {
            init: fixture.root.join("init.lua"),
            config: fixture.root.join("config"),
            state: fixture.root.join("state"),
            claude: fixture.root.join("settings.json"),
        }
    }

    fn frame(pane: &mut dyn Pane, width: usize, height: usize) -> Vec<String> {
        let mut s = Screen::sized(width, height);
        pane.draw(&mut s);
        s.text()
    }

    #[test]
    fn the_screen_sketch_tracks_panes_and_column_width() {
        let f = Fixture::folder();
        let mut pane = Setup::new(Some(store(&f)), true, None, |_| panic!("drawing needs no Rex"));
        let before = frame(&mut pane, 64, 18);
        for (label, _) in CHOICES {
            assert!(before[..7].iter().any(|row| row.contains(label)));
        }
        for label in ["Outline", "History", "Usage", "Your work"] {
            assert!(before[7..].iter().any(|row| row.contains(label)));
        }
        assert!(!pane.key(Key::Char(' ')));
        let after = frame(&mut pane, 64, 18);
        assert!(!after[7..].iter().any(|row| row.contains("Outline")));
        assert!(after[7..].iter().any(|row| row.contains("History")));
        for _ in 0..3 {
            pane.key(Key::Down);
        }
        pane.key(Key::Enter);
        let wider = frame(&mut pane, 64, 18);
        assert!(wider[4].contains("Medium"));
        assert_ne!(after[8], wider[8]);
    }

    #[test]
    fn a_small_pane_drops_the_sketch_and_keeps_the_cursor_visible() {
        let f = Fixture::folder();
        let mut pane = Setup::new(Some(store(&f)), true, None, |_| panic!("drawing needs no Rex"));
        for _ in 0..4 {
            pane.key(Key::Char('j'));
        }
        let rows = frame(&mut pane, 33, 5);
        assert!(rows.iter().any(|row| row.starts_with('›') && row.contains("Arrange every new tab")));
        assert!(!rows.iter().any(|row| row.contains('┌')));
        assert!(rows[4].contains("Space"));
    }

    #[test]
    fn outside_rex_only_claude_settings_can_be_changed() {
        let f = Fixture::folder();
        let store = store(&f);
        let mut pane = Setup::new(Some(store), false, None, |_| panic!("outside Rex"));
        let rows = frame(&mut pane, 75, 10);
        assert!(rows[1].contains("Tab layouts need Rex"));
        assert!(rows[2].contains("milo outline, milo history, milo usage"));
        assert!(rows[3].contains("Show Claude Code limits"));
        assert!(!rows.iter().any(|row| row.contains("Arrange every new tab")));
        pane.key(Key::Enter);
        pane.key(Key::Char('s'));
        assert!(fs::read_to_string(f.root.join("settings.json")).unwrap().contains("milo claude-status"));
        assert!(!f.root.join("init.lua").exists());
        assert!(frame(&mut pane, 75, 10)[9].contains("Saved Claude Code choices"));
    }

    #[test]
    fn a_foreign_status_line_is_explained_and_cannot_be_toggled() {
        let f = Fixture::folder();
        let store = store(&f);
        let original = r#"{"statusLine":{"type":"command","command":"another-program"}}"#;
        fs::write(&store.claude, original).unwrap();
        let mut pane = Setup::new(Some(store), false, None, |_| panic!("outside Rex"));
        let rows = frame(&mut pane, 78, 10);
        assert!(rows.iter().any(|row| row.contains("Claude Code already has a status line; milo will not replace it")));
        pane.key(Key::Enter);
        pane.key(Key::Char('s'));
        assert_eq!(fs::read_to_string(f.root.join("settings.json")).unwrap(), original);
    }
}
