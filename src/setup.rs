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
use crate::width::char_width;
use crate::{paths, rex};
use install::{Claude, Layout, Saved, Store};

const CLAUDE: usize = 5;
const CHOICES: [(&str, &str); 6] = [
    ("Outline", "Files on the left"),
    ("History", "Commits on the left"),
    ("Usage", "A strip across the top"),
    ("Column width", "Room for files and commits"),
    ("Arrange every new tab", "Only tabs opened as a shell"),
    ("Show Claude Code limits", "For the usage strip"),
];
// The short labels leave room for all three columns in a 28-column pane.
const COMPACT: [(&str, &str); 6] = [
    ("Outline", "Files"),
    ("History", "Git"),
    ("Usage", "Top"),
    ("Column width", "Left"),
    ("New tabs", "Shell"),
    ("Claude limits", "Usage"),
];

pub struct Setup<R = fn(&[&OsStr]) -> bool> {
    store: Option<Store>,
    inside: bool,
    offered: bool,
    target: Option<rex::Target>,
    run: R,
    layout: Layout,
    installed: Option<bool>,
    claude: Claude,
    show_limits: bool,
    cursor: usize,
    message: Option<(&'static str, Style)>,
}

impl Setup {
    pub fn open(offered: bool) -> Self {
        let mut setup = Self::new(Store::real(), rex::inside(), rex::Target::current(), rex::run);
        setup.offered = offered;
        setup
    }
}

impl<R: Fn(&[&OsStr]) -> bool + 'static> Setup<R> {
    fn new(store: Option<Store>, inside: bool, target: Option<rex::Target>, run: R) -> Self {
        let mut message = None;
        let mut layout = Layout::default();
        let mut installed = None;
        if inside && let Some(store) = &store {
            match store.layout() {
                Ok((saved, active)) => {
                    layout = saved;
                    installed = Some(active);
                }
                Err(why) => message = Some((why, Style::PLAIN.bold())),
            }
        }
        let claude = store.as_ref().map_or(Claude::Invalid, Store::claude);
        Self {
            store,
            inside,
            offered: false,
            target,
            run,
            layout,
            installed,
            claude,
            show_limits: claude == Claude::On,
            cursor: 0,
            message,
        }
    }

    fn save(&mut self) -> Result<&'static str, &'static str> {
        let Some(store) = &self.store else { return Err("Could not find your settings folders.") };
        let limits = self.limit_change();
        let remove = self.inside && !self.layout.enabled() && self.installed == Some(true);
        let saved = store.save(self.inside.then_some(&self.layout), limits, &self.run)?;
        if self.inside {
            self.installed = Some(self.layout.enabled());
        }
        self.claude = store.claude();
        self.show_limits = self.claude == Claude::On;
        Ok(match saved {
            Saved::ListenerDidNotStart => "Saved. The Rex listener is not running.",
            Saved::ListenerDidNotStop if remove => "Removed milo from Rex. The listener is still running.",
            Saved::ListenerDidNotStop => "Saved. The Rex listener is still running.",
            Saved::Ready if remove && limits.is_some() => "Removed milo from Rex. Saved Claude Code choices.",
            Saved::Ready if remove => "Removed milo from Rex.",
            Saved::Ready if self.inside && self.layout.enabled() && limits.is_some() => {
                "Saved. Rex and Claude Code choices."
            }
            Saved::Ready if self.inside && self.layout.enabled() => "Saved. Rex layout choices.",
            Saved::Ready if limits.is_some() => "Saved. Claude Code choices.",
            Saved::Ready => "No changes to save.",
        })
    }

    fn limit_change(&self) -> Option<bool> {
        (self.claude.editable() && self.show_limits != (self.claude == Claude::On)).then_some(self.show_limits)
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
        let (value, explanation, choices) = if width < 49 { (16, 23, COMPACT) } else { (28, 36, CHOICES) };
        let (name, detail) = choices[i];
        if selected {
            s.put(0, y, 1, "›", style);
        }
        s.put_fit(2, y, value.min(width).saturating_sub(3), name, style);
        s.put_fit(value, y, width.saturating_sub(value).min(6), status, style);
        s.put_fit(explanation, y, width.saturating_sub(explanation), detail, FAINT);
    }

    fn footer(&self, s: &mut Screen) {
        let last = match self.offered {
            true if s.w < 30 => "q next · ? help",
            true => "q outline · ? help",
            false => "q quit · ? help",
        };
        let (first, keys): (&str, &[&str]) = if s.w < 40 {
            ("s save", if self.inside { &["a try"] } else { &["Space"] })
        } else if self.inside {
            ("↑↓ move", &["Space change", "a try", "s save"])
        } else {
            ("Space change", &["s save"])
        };
        help::draw_hint(s, first, keys, last);
    }

    fn draw_choices(&self, s: &mut Screen, end: usize) {
        let mut top = 1;
        if end > 4 {
            if self.offered {
                s.put_fit(1, top, s.w - 1, "Set up panes for Rex tabs.", FAINT);
                top += 1;
            }
            let status = if self.inside {
                match self.installed {
                    Some(true) => "Rex layout installed",
                    Some(false) => "Rex: not set up yet",
                    None => "Rex setup could not be read",
                }
            } else if self.claude == Claude::Off {
                "Not set up yet"
            } else {
                ""
            };
            if !status.is_empty() {
                s.put_fit(1, top, s.w - 1, status, FAINT);
                top += 1;
            }
            if !self.inside && end > 6 {
                s.put_fit(1, top, s.w - 1, "Tab layouts need Rex.", FAINT);
                s.put_fit(1, top + 1, s.w - 1, "Panes work anywhere: milo outline, milo history, milo usage.", FAINT);
                top += 2;
            }
        }
        let count = if self.inside { CHOICES.len() } else { 1 };
        let shown = end.saturating_sub(top).min(count);
        let skip = if self.inside { self.cursor.saturating_sub(shown.saturating_sub(1)) } else { 0 };
        let mut next = top + shown;
        for row in 0..shown {
            self.choice(s, if self.inside { row + skip } else { CLAUDE }, top + row, s.w);
        }
        if next < end {
            next = self.draw_save_plan(s, next, end);
        }
        let issue = match self.claude {
            Claude::Foreign => "Claude Code already has a status line; milo will not replace it.",
            Claude::NotOwned => "This Claude Code status line was not added by setup.",
            Claude::Invalid => "Claude Code settings must be a readable JSON object.",
            Claude::On | Claude::Off => "",
        };
        for text in wrap(issue, s.w - 1) {
            if next >= end {
                break;
            }
            s.put(1, next, s.w - 1, text, FAINT);
            next += 1;
        }
        if self.inside && s.w >= 36 && end >= next + 8 {
            sketch(s, 2, next + 1, &self.layout);
        }
    }

    fn draw_save_plan(&self, s: &mut Screen, mut y: usize, end: usize) -> usize {
        let compact = s.w <= 28;
        let (init, files, reload) = if !self.inside {
            ("", "", "")
        } else if self.layout.enabled() {
            let init = match (self.installed, compact) {
                (Some(true), true) => "On save: keep milo's line in Rex's init.lua.",
                (Some(true), false) => "Keep milo's line in Rex's init.lua.",
                (_, true) => "On save: add one line to Rex's init.lua.",
                (_, false) => "Add one line to Rex's init.lua.",
            };
            let reload =
                if self.installed == Some(true) { "Ask Rex to reload if needed." } else { "Ask Rex to reload." };
            (init, "Write milo's two files.", reload)
        } else if self.installed == Some(true) {
            let init = if compact {
                "On save: remove milo's line from Rex's init.lua."
            } else {
                "Remove milo's line from Rex's init.lua."
            };
            (init, "Delete milo's two files.", "Ask Rex to reload.")
        } else {
            ("", "", "")
        };
        let listener = if self.inside && self.layout.new_tabs {
            "Start arranging new tabs."
        } else if self.inside && self.installed == Some(true) {
            "Stop arranging new tabs."
        } else {
            ""
        };
        let claude = match self.limit_change() {
            Some(true) => "Add milo to Claude Code's status line.",
            Some(false) => "Take milo off Claude Code's status line.",
            None => "",
        };
        if init.is_empty() && claude.is_empty() {
            return y;
        }
        if !compact || init.is_empty() {
            s.put(1, y, s.w - 1, "On save:", Style::PLAIN.bold());
            y += 1;
        }
        let width = s.w - 1;
        for text in [init, files, reload, listener, claude].into_iter().flat_map(|text| wrap(text, width)) {
            if y == end {
                break;
            }
            s.put(1, y, s.w - 1, text, FAINT);
            y += 1;
        }
        y
    }
}

/// Wrap setup notices without allocating or cutting key words off a narrow pane.
fn wrap(mut text: &str, width: usize) -> impl Iterator<Item = &str> + Clone {
    std::iter::from_fn(move || {
        if text.is_empty() {
            return None;
        }
        let mut end = text.len();
        let mut columns = 0;
        let mut space = None;
        for (i, ch) in text.char_indices() {
            columns += char_width(ch);
            if columns > width {
                end = if ch == ' ' { i } else { space.unwrap_or(i) };
                break;
            }
            if ch == ' ' {
                space = Some(i);
            }
        }
        let (line, rest) = text.split_at(end);
        text = rest.trim_start();
        Some(line)
    })
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
        let mut end = s.h - 1;
        if let Some((text, style)) = self.message {
            let rows = wrap(text, s.w - 1).take(end.saturating_sub(1));
            end -= rows.clone().count();
            for (row, text) in rows.enumerate() {
                s.put(1, end + row, s.w - 1, text, style);
            }
        }
        self.draw_choices(s, end);
        self.footer(s);
    }

    fn help(&self) -> Page {
        use help::{heading, key};
        let mut rows = vec![heading("Setup keys"), key("Space", "change (Enter)")];
        if self.inside {
            rows.extend([key("↑↓   ", "move (k j)"), key("a    ", "try in this tab")]);
        }
        rows.extend([key("s    ", "save choices"), key("q    ", if self.offered { "open outline" } else { "quit" })]);
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
            Key::Char('a') if self.inside => {
                let tried = self.arrange().map(|()| "Tried in this tab. Not saved yet: press s to keep it.");
                return self.report(tried);
            }
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
        let before = frame(&mut pane, 64, 24);
        for (label, _) in CHOICES {
            assert!(before[..8].iter().any(|row| row.contains(label)));
        }
        for label in ["Outline", "History", "Usage", "Your work"] {
            assert!(before[12..].iter().any(|row| row.contains(label)));
        }
        assert!(!pane.key(Key::Char(' ')));
        let after = frame(&mut pane, 64, 24);
        assert!(!after[12..].iter().any(|row| row.contains("Outline")));
        assert!(after[12..].iter().any(|row| row.contains("History")));
        for _ in 0..3 {
            pane.key(Key::Down);
        }
        pane.key(Key::Enter);
        let wider = frame(&mut pane, 64, 24);
        assert!(wider[5].contains("Medium"));
        assert_ne!(after[14], wider[14]);
    }

    #[test]
    fn a_small_pane_drops_the_sketch_and_keeps_the_cursor_visible() {
        let f = Fixture::folder();
        let mut pane = Setup::new(Some(store(&f)), true, None, |_| panic!("drawing needs no Rex"));
        for _ in 0..4 {
            pane.key(Key::Char('j'));
        }
        let rows = frame(&mut pane, 28, 5);
        assert!(rows.iter().any(|row| row.starts_with('›') && row.contains("New tabs")));
        assert!(!rows.iter().any(|row| row.contains('┌')));
        assert_eq!(rows[4].trim(), "s save · q quit · ? help");
    }

    #[test]
    fn outside_rex_only_claude_settings_can_be_changed() {
        let f = Fixture::folder();
        let store = store(&f);
        let mut pane = Setup::new(Some(store), false, None, |_| panic!("outside Rex"));
        let rows = frame(&mut pane, 75, 10);
        assert!(rows[1].contains("Not set up yet"));
        assert!(rows[2].contains("Tab layouts need Rex"));
        assert!(rows[3].contains("milo outline, milo history, milo usage"));
        assert!(rows[4].contains("Show Claude Code limits"));
        assert!(!rows.iter().any(|row| row.contains("Arrange every new tab")));
        pane.key(Key::Enter);
        pane.key(Key::Char('s'));
        assert!(fs::read_to_string(f.root.join("settings.json")).unwrap().contains("milo claude-status"));
        assert!(!f.root.join("init.lua").exists());
        assert_eq!(frame(&mut pane, 75, 10)[8].trim(), "Saved. Claude Code choices.");
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

    #[test]
    fn the_save_plan_and_footer_stay_visible_after_trying_at_28_columns() {
        let f = Fixture::folder();
        let target = rex::Target { session: "session:test".into(), block: "block:test".into() };
        let mut pane = Setup::new(Some(store(&f)), true, Some(target), |args| args[0] == "do");
        pane.offered = true;
        for _ in 0..4 {
            pane.key(Key::Down);
        }
        pane.key(Key::Enter);
        pane.key(Key::Down);
        pane.key(Key::Enter);
        assert!(!pane.key(Key::Char('a')));
        let rows = frame(&mut pane, 28, 20);
        let words = rows.iter().map(|row| row.trim()).collect::<Vec<_>>().join(" ");
        for text in [
            "Set up panes for Rex tabs.",
            "not set up yet",
            "add one line to Rex's init.lua.",
            "Write milo's two files.",
            "Ask Rex to reload.",
            "Start arranging new tabs.",
            "Add milo to Claude Code's status line.",
            "Tried in this tab. Not saved yet: press s to keep it.",
        ] {
            assert!(words.contains(text), "{text:?} missing from {rows:?}");
        }
        assert_eq!(rows[19].trim(), "s save · q next · ? help");
        assert!(!f.root.join("init.lua").exists());
    }

    #[test]
    fn save_and_removal_report_on_a_row_of_their_own() {
        let f = Fixture::folder();
        let mut pane = Setup::new(Some(store(&f)), true, None, |_| true);
        assert!(!pane.key(Key::Char('s')));
        let rows = frame(&mut pane, 80, 24);
        assert_eq!(rows[22].trim(), "Saved. Rex layout choices.");
        assert!(rows[23].ends_with("q quit · ? help"));
        assert_eq!(rows[2].chars().nth(28), Some('O'));
        assert_eq!(rows[5].chars().nth(28), Some('T'));
        for _ in 0..3 {
            pane.key(Key::Enter);
            pane.key(Key::Down);
        }
        let rows = frame(&mut pane, 80, 24);
        assert!(rows.iter().any(|row| row.contains("Remove milo's line from Rex's init.lua.")));
        assert!(!pane.key(Key::Char('s')));
        let rows = frame(&mut pane, 80, 24);
        assert_eq!(rows[22].trim(), "Removed milo from Rex.");
        assert!(rows[23].ends_with("q quit · ? help"));
    }
}
