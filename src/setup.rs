//! Choose the panes around a Rex tab, and opt in to Claude Code's usage limits.

mod claude;
mod files;
mod install;
mod layout;
mod lock;
mod rex_files;
mod settings;

use std::ffi::OsStr;
use std::io;

use crate::help::{self, Page};
use crate::keys::Key;
use crate::pane::{After, Pane};
use crate::screen::{Screen, Style};
use crate::ui::{self, FAINT, HEADING};
use crate::width::char_width;
use crate::{paths, rex};
use claude::Claude;
use install::{Saved, Store};
use layout::{Layout, TabPane};

/// Identify a choice independently of its position or the text that fits on screen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Pane(TabPane),
    Width,
    NewTabs,
    ClaudeLimits,
}

impl Choice {
    const ALL: [Self; 6] = [
        Self::Pane(TabPane::Outline),
        Self::Pane(TabPane::History),
        Self::Pane(TabPane::Usage),
        Self::Width,
        Self::NewTabs,
        Self::ClaudeLimits,
    ];

    /// Shorten the labels so a 28-column pane still has room for all three columns.
    fn labels(self, compact: bool) -> (&'static str, &'static str) {
        match (self, compact) {
            (Self::Pane(TabPane::Outline), false) => ("Outline", "Files on the left"),
            (Self::Pane(TabPane::History), false) => ("History", "Commits on the left"),
            (Self::Pane(TabPane::Usage), false) => ("Usage", "Limits on the left"),
            (Self::Width, false) => ("Column width", "Left side of the tab"),
            (Self::NewTabs, false) => ("Arrange new shell tabs", "Skip tabs that run a command"),
            (Self::ClaudeLimits, false) => ("Show Claude Code limits", "For the usage pane"),
            (Self::Pane(TabPane::Outline), true) => ("Outline", "Files"),
            (Self::Pane(TabPane::History), true) => ("History", "Git"),
            (Self::Pane(TabPane::Usage), true) => ("Usage", "Limits"),
            (Self::Width, true) => ("Column width", "Left"),
            (Self::NewTabs, true) => ("Shell tabs", "Auto"),
            (Self::ClaudeLimits, true) => ("Claude limits", "Usage"),
        }
    }
}

enum Installed {
    Unreadable,
    Absent,
    Active(Layout),
}

/// Holds unsaved choices separately from what is already installed.
pub struct Setup<R = fn(&[&OsStr]) -> bool> {
    store: Option<Store>,
    inside: bool,
    offered: bool,
    target: Option<rex::Target>,
    run: R,
    layout: Layout,
    installed: Installed,
    claude: Claude,
    show_limits: bool,
    selected: Choice,
    message: Option<(&'static str, Style)>,
}

impl Setup {
    pub fn open(offered: bool) -> Self {
        let mut setup = Self::new(Store::real(), rex::inside(), rex::Target::current(), rex::run);
        setup.offered = offered;
        setup
    }
}

impl<R: Fn(&[&OsStr]) -> bool> Setup<R> {
    /// Start from installed choices without reading Rex's files outside Rex.
    fn new(store: Option<Store>, inside: bool, target: Option<rex::Target>, run: R) -> Self {
        let mut message = None;
        let mut layout = Layout::default();
        let mut installed = Installed::Unreadable;
        if inside && let Some(store) = &store {
            match store.layout() {
                Ok((saved, active)) => {
                    installed = if active { Installed::Active(saved.clone()) } else { Installed::Absent };
                    layout = saved;
                    if store.rex_reload_pending() {
                        message = Some(("Rex still needs to reload. Check your choices, then press s.", FAINT));
                    }
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
            selected: if inside { Choice::Pane(TabPane::Outline) } else { Choice::ClaudeLimits },
            message,
        }
    }

    /// Only a successful save makes the current choices the new baseline.
    fn save(&mut self) -> Result<&'static str, &'static str> {
        let Some(store) = &self.store else { return Err("Could not find your settings folders.") };
        let limits = self.limit_change();
        let remove = !self.layout.enabled() && matches!(self.installed, Installed::Active(_));
        let saved = store.save(self.inside.then_some(&self.layout), limits, &self.run)?;
        if self.inside {
            self.installed =
                if self.layout.enabled() { Installed::Active(self.layout.clone()) } else { Installed::Absent };
        }
        self.claude = store.claude();
        self.show_limits = self.claude == Claude::On;
        Ok(match saved {
            Saved::ListenerDidNotStart => "Saved, but new tabs won't be arranged. Save again to retry.",
            Saved::ListenerDidNotStop if remove => "Removed milo from Rex, but could not stop arranging new tabs.",
            Saved::ListenerDidNotStop => "Saved, but could not stop arranging new tabs.",
            Saved::Ready if remove && limits.is_some() => "Removed milo from Rex. Saved Claude Code choices.",
            Saved::Ready if remove => "Removed milo from Rex.",
            Saved::Ready if self.inside && self.layout.enabled() && limits.is_some() => {
                "Saved Rex and Claude Code choices."
            }
            Saved::Ready if self.inside && self.layout.enabled() => "Saved Rex layout choices.",
            Saved::Ready if limits.is_some() => "Saved Claude Code choices.",
            Saved::Ready => "No changes to save.",
        })
    }

    /// Leave Claude's file alone if the existing choice is unchanged or not ours.
    fn limit_change(&self) -> Option<bool> {
        (self.claude.editable() && self.show_limits != (self.claude == Claude::On)).then_some(self.show_limits)
    }

    /// Try the draft in this tab without saving or touching other tabs.
    fn arrange(&self) -> Result<(), &'static str> {
        let Some(store) = &self.store else { return Err("Could not find your settings folders.") };
        let Some(target) = &self.target else { return Err("Could not find this Rex pane.") };
        store.arrange(&self.layout, target, &self.run)
    }

    /// Rex choices are irrelevant outside Rex; Claude limits work in either place.
    fn choices(&self) -> &'static [Choice] {
        if self.inside { &Choice::ALL } else { &[Choice::ClaudeLimits] }
    }

    fn selection(&self) -> usize {
        self.choices().iter().position(|&choice| choice == self.selected).expect("selected choice is visible")
    }

    /// Keep the cursor on the first or last choice rather than wrapping.
    fn move_selection(&mut self, by: isize) {
        let choices = self.choices();
        self.selected = choices[self.selection().saturating_add_signed(by).min(choices.len() - 1)];
    }

    fn toggle(&mut self) {
        match self.selected {
            Choice::Pane(pane) => self.layout.toggle(pane),
            Choice::Width => self.layout.next_width(),
            Choice::NewTabs => self.layout.new_tabs = !self.layout.new_tabs,
            Choice::ClaudeLimits if self.claude.editable() => self.show_limits = !self.show_limits,
            Choice::ClaudeLimits => {}
        }
        self.message = None;
    }

    /// Keep the label, value and explanation aligned even on the narrow layout.
    fn choice(&self, s: &mut Screen, choice: Choice, y: usize, width: usize) {
        let on = |enabled| if enabled { "On" } else { "Off" };
        let status = match choice {
            Choice::Pane(pane) => on(self.layout.chosen(pane)),
            Choice::Width => self.layout.width_name(),
            Choice::NewTabs => on(self.layout.new_tabs),
            Choice::ClaudeLimits if self.claude == Claude::NotOwned => "On",
            Choice::ClaudeLimits if !self.claude.editable() => "—",
            Choice::ClaudeLimits => on(self.show_limits),
        };
        let selected = choice == self.selected;
        let style = if selected { Style::PLAIN.bold() } else { Style::PLAIN };
        let compact = width < 49;
        let (value, explanation) =
            if compact { (16.min(width.saturating_sub(status.len().min(6))), 23) } else { (28, 36) };
        let (name, detail) = choice.labels(compact);
        if selected {
            s.put(0, y, 1, "›", style);
        }
        s.put_fit(2, y, value.min(width).saturating_sub(3), name, style);
        s.put_fit(value, y, width.saturating_sub(value).min(6), status, style);
        s.put_fit(explanation, y, width.saturating_sub(explanation), detail, FAINT);
    }

    fn footer(&self, s: &mut Screen) {
        if s.w < 17 {
            if s.w >= 8 {
                s.put(1, s.h - 1, s.w - 1, if self.offered { "q next" } else { "q quit" }, FAINT);
            }
            return;
        }
        let last = if self.offered { "q next · ? help" } else { "q quit · ? help" };
        let (first, keys): (&str, &[&str]) = if !self.can_save() {
            ("", &[])
        } else if s.w < 40 {
            ("s save", if self.inside { &["a try"] } else { &["Space"] })
        } else if self.inside {
            ("↑↓ move", &["Space change", "a try", "s save"])
        } else {
            ("Space change", &["s save"])
        };
        help::draw_hint(s, first, keys, last);
    }

    /// Scroll only the choices, leaving the instructions and footer in place when possible.
    fn draw_choices(&self, s: &mut Screen, end: usize) {
        let (status, instructions): (&str, &[&str]) = if self.inside {
            let status = match self.installed {
                Installed::Active(_) => "Rex layout installed",
                Installed::Absent => "Rex: not set up yet",
                Installed::Unreadable => "Could not read Rex settings",
            };
            (status, &[])
        } else {
            const OUTSIDE: &str = "Open setup in Rex to arrange tabs.";
            const PANES: &str = "Panes work anywhere: milo outline, milo history, milo usage.";
            let instructions: &[&str] = if s.w > PANES.len() {
                &[OUTSIDE, PANES]
            } else {
                &[OUTSIDE, "Panes work anywhere:", "milo outline", "milo history", "milo usage"]
            };
            (if self.claude == Claude::Off { "Not set up yet" } else { "" }, instructions)
        };
        let mut top = 1;
        if end > 4 {
            if self.offered {
                s.put_fit(1, top, s.w - 1, "Set up panes for Rex tabs.", FAINT);
                top += 1;
            }
            if !status.is_empty() {
                s.put_fit(1, top, s.w - 1, status, FAINT);
                top += 1;
            }
            let width = s.w - 1;
            let lines = instructions.iter().flat_map(|text| wrap(text, width)).take(end - top - 1);
            for text in lines {
                s.put(1, top, width, text, FAINT);
                top += 1;
            }
        } else if s.w >= 49 {
            let status = instructions.first().copied().unwrap_or(status);
            s.put(11, 0, 3, " · ", FAINT);
            s.put(14, 0, s.w - 14, status, FAINT);
        }
        let choices = self.choices();
        let shown = end.saturating_sub(top).min(choices.len());
        let skip = self.selection().saturating_sub(shown.saturating_sub(1));
        let mut next = top + shown;
        for (row, &choice) in choices[skip..skip + shown].iter().enumerate() {
            self.choice(s, choice, top + row, s.w);
        }
        if next < end {
            next = self.draw_save_plan(s, next, end);
        }
        let issue = match self.claude {
            Claude::Foreign => "Milo will leave your Claude Code status line alone.",
            Claude::NotOwned => "You already have Milo on the Claude Code status line.",
            Claude::Invalid => "Could not read Claude Code settings. Milo will leave them alone.",
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

    /// Explain actual file changes before saving, not just which toggles are on.
    fn draw_save_plan(&self, s: &mut Screen, mut y: usize, end: usize) -> usize {
        let saved = if let Installed::Active(layout) = &self.installed { Some(layout) } else { None };
        let rex = if !self.inside {
            ""
        } else if self.layout.enabled() {
            match saved {
                None => "Add Milo actions to Rex.",
                Some(layout) if layout != &self.layout => "Update the Rex layout.",
                Some(_) => "",
            }
        } else if saved.is_some() {
            "Remove Milo actions from Rex."
        } else {
            ""
        };
        let listener = match (saved.is_some_and(|layout| layout.new_tabs), self.layout.new_tabs) {
            (false, true) => "Arrange new shell tabs.",
            (true, false) => "Stop arranging new shell tabs.",
            _ => "",
        };
        let claude = match self.limit_change() {
            Some(true) => "Add milo to Claude Code's status line.",
            Some(false) => "Take milo off Claude Code's status line.",
            None => "",
        };
        if [rex, listener, claude].iter().all(|text| text.is_empty()) {
            return y;
        }
        s.put(1, y, s.w - 1, "On save:", HEADING);
        y += 1;
        let width = s.w - 1;
        for text in [rex, listener, claude].into_iter().flat_map(|text| wrap(text, width)) {
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
    let chosen = TabPane::ALL.map(|pane| layout.chosen(pane));
    let count = chosen.iter().filter(|&&pane| pane).count();
    let left = count > 0;
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
    // One pane sits in the middle of the column; more are spread down it.
    let (first, step) = match count {
        1 => (3, 0),
        2 => (2, 3),
        _ => (1, 2),
    };
    let names = ["Outline", "History", "Usage"].into_iter().zip(chosen).filter_map(|(name, on)| on.then_some(name));
    for (i, name) in names.enumerate() {
        let row = first + i * step;
        draw(2, row, name);
        if i + 1 < count {
            // Two panes leave a blank row above the rule so the first stays centred.
            let rule = if count == 2 { 4 } else { row + 1 };
            draw(0, rule, "├");
            draw(1, rule, &LINE[..3 * column]);
            draw(joint, rule, "┤");
        }
    }
    draw(joint + 2, 3, "Your work");
}

impl<R: Fn(&[&OsStr]) -> bool> Pane for Setup<R> {
    fn draw(&mut self, s: &mut Screen) {
        s.clear();
        if s.w < 4 || s.h == 0 {
            return;
        }
        ui::section_heading(s, 0, s.w - 1, "Milo setup");
        if s.h == 1 {
            return;
        }
        let mut end = s.h - 1;
        if let Some((text, style)) = self.message {
            let rows = wrap(text, s.w - 1).take(end.saturating_sub(2));
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
        let mut rows = vec![heading("Setup keys")];
        if self.can_save() {
            rows.push(key("Space", "change (Enter)"));
        }
        if self.inside {
            rows.extend([key("↑↓   ", "move (k j)"), key("a    ", "try in this tab")]);
        }
        if self.can_save() {
            rows.push(key("s    ", "save choices"));
        }
        rows.push(key("q    ", if self.offered { "open outline" } else { "quit" }));
        Page::new(rows)
    }

    fn key(&mut self, key: Key) -> After {
        match key {
            Key::Up | Key::Char('k') if self.inside => self.move_selection(-1),
            Key::Down | Key::Char('j') if self.inside => self.move_selection(1),
            Key::Enter | Key::Char(' ') if self.selected != Choice::ClaudeLimits || self.claude.editable() => {
                self.toggle();
            }
            Key::Char('a') if self.inside => {
                let tried = self.arrange().map(|()| "Tried in this tab. Press s to save your choices.");
                self.report(tried);
            }
            Key::Char('s') if self.can_save() => {
                let saved = self.save();
                self.report(saved);
            }
            Key::Char('q') | Key::Esc => return After::Leave,
            _ => return After::Ignored,
        }
        After::Stay
    }
}

impl<R> Setup<R> {
    fn can_save(&self) -> bool {
        self.inside || self.claude.editable()
    }

    fn report(&mut self, result: Result<&'static str, &'static str>) {
        self.message = Some(match result {
            Ok(message) => (message, FAINT),
            Err(why) => (why, Style::PLAIN.bold()),
        });
    }
}

/// A bare launch offers setup only when Rex can use it.
pub fn offer_once() -> bool {
    rex::inside() && Store::real().is_some_and(|store| store.offer_once())
}

/// Rex calls this separately so listener lifetime does not depend on a pane.
pub fn listen(stop: bool) -> i32 {
    fn run(stop: bool) -> io::Result<()> {
        let state = paths::state_dir().ok_or(io::ErrorKind::NotFound)?;
        if stop {
            lock::stop(&state)
        } else {
            let config = paths::config_dir().ok_or(io::ErrorKind::NotFound)?;
            rex_files::check_config_dir(&config).map_err(io::Error::other)?;
            lock::listen(&state, &config.join("listener.lua"))
        }
    }
    let Err(error) = run(stop) else { return 0 };
    if stop {
        eprintln!("milo: could not stop the Rex listener: {error}");
    } else if error.kind() == io::ErrorKind::NotFound {
        eprintln!(
            "milo: could not start the Rex listener. Check that Rex is installed and new shell tabs are on in milo setup."
        );
    } else {
        eprintln!("milo: could not start the Rex listener: {error}");
    }
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
        for (label, _) in Choice::ALL.map(|choice| choice.labels(false)) {
            assert!(before[..8].iter().any(|row| row.contains(label)));
        }
        for label in ["Outline", "History", "Usage", "Your work"] {
            assert!(before[12..].iter().any(|row| row.contains(label)));
        }
        assert_eq!(pane.key(Key::Char(' ')), After::Stay);
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
        assert!(rows.iter().any(|row| row.starts_with('›') && row.contains("Shell tabs")));
        assert!(!rows.iter().any(|row| row.contains('┌')));
        assert_eq!(rows[4].trim(), "s save · q quit · ? help");
    }

    #[test]
    fn a_short_setup_screen_still_shows_whether_rex_is_set_up() {
        let f = Fixture::folder();
        let mut pane = Setup::new(Some(store(&f)), true, None, |_| true);
        assert!(frame(&mut pane, 140, 5)[0].contains("Rex: not set up yet"));
        pane.key(Key::Char('s'));
        let rows = frame(&mut pane, 140, 5);
        assert!(rows[0].contains("Rex layout installed"));
        assert!(rows[3].contains("Saved Rex layout choices."));
    }

    #[test]
    fn outside_rex_only_claude_settings_can_be_changed() {
        let f = Fixture::folder();
        let store = store(&f);
        let mut pane = Setup::new(Some(store), false, None, |_| panic!("outside Rex"));
        let rows = frame(&mut pane, 75, 10);
        assert!(rows[1].contains("Not set up yet"));
        assert!(rows[2].contains("Open setup in Rex to arrange tabs"));
        assert!(rows[3].contains("milo outline, milo history, milo usage"));
        assert!(rows[4].contains("Show Claude Code limits"));
        assert!(!rows.iter().any(|row| row.contains("Arrange new shell tabs")));
        let compact = frame(&mut pane, 28, 20);
        let words = compact.iter().map(|row| row.trim()).collect::<Vec<_>>().join(" ");
        assert!(words.contains("Open setup in Rex to arrange tabs."));
        for tool in ["milo outline", "milo history", "milo usage"] {
            assert!(compact.iter().any(|row| row.trim() == tool));
        }
        assert!(words.contains("Claude limits Off"));
        pane.key(Key::Enter);
        pane.key(Key::Char('s'));
        assert!(fs::read_to_string(f.root.join("settings.json")).unwrap().contains("milo claude-status"));
        assert!(!f.root.join("init.lua").exists());
        assert_eq!(frame(&mut pane, 75, 10)[8].trim(), "Saved Claude Code choices.");
    }

    #[test]
    fn a_foreign_status_line_is_explained_and_cannot_be_toggled() {
        let f = Fixture::folder();
        let store = store(&f);
        let original = r#"{"statusLine":{"type":"command","command":"another-program"}}"#;
        fs::write(&store.claude, original).unwrap();
        let mut pane = Setup::new(Some(store), false, None, |_| panic!("outside Rex"));
        let rows = frame(&mut pane, 78, 10);
        assert!(rows.iter().any(|row| row.contains("Milo will leave your Claude Code status line alone")));
        assert!(!rows[9].contains("save"));
        assert_eq!(pane.key(Key::Enter), After::Ignored);
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
        assert_eq!(pane.key(Key::Char('a')), After::Stay);
        let rows = frame(&mut pane, 28, 20);
        let words = rows.iter().map(|row| row.trim()).collect::<Vec<_>>().join(" ");
        for text in [
            "Set up panes for Rex tabs.",
            "not set up yet",
            "Add Milo actions to Rex.",
            "Arrange new shell tabs.",
            "Add milo to Claude Code's status line.",
            "Tried in this tab. Press s to save your choices.",
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
        assert_eq!(pane.key(Key::Char('s')), After::Stay);
        let rows = frame(&mut pane, 80, 24);
        assert_eq!(rows[22].trim(), "Saved Rex layout choices.");
        assert!(rows[23].ends_with("q quit · ? help"));
        assert_eq!(rows[2].chars().nth(28), Some('O'));
        assert_eq!(rows[5].chars().nth(28), Some('T'));
        for _ in 0..3 {
            pane.key(Key::Enter);
            pane.key(Key::Down);
        }
        let rows = frame(&mut pane, 80, 24);
        assert!(rows.iter().any(|row| row.contains("Remove Milo actions from Rex.")));
        assert_eq!(pane.key(Key::Char('s')), After::Stay);
        let rows = frame(&mut pane, 80, 24);
        assert_eq!(rows[22].trim(), "Removed milo from Rex.");
        assert!(rows[23].ends_with("q quit · ? help"));
    }

    #[test]
    fn ten_columns_show_the_toggle_and_a_way_out_even_after_an_error() {
        let f = Fixture::folder();
        let mut pane = Setup::new(Some(store(&f)), true, None, |_| false);
        pane.key(Key::Char('a'));
        let rows = frame(&mut pane, 10, 5);
        assert!(rows.iter().any(|row| row.starts_with('›') && row.ends_with("On")));
        assert_eq!(rows[4].trim(), "q quit");
    }
}
