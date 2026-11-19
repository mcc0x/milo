//! Milo's home picker, with settings and maintenance in the same terminal session.

mod mascot;

use std::ops::Range;

use crate::help::{self, HintHits, Page};
use crate::keys::Key;
use crate::pane::{After, Pane};
use crate::screen::{self, Screen, Style};
use crate::setup::{Setup, doctor, wrap};
use crate::ui::{self, DEL, FAINT, HEADING};

use mascot::Sprite;

const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));
const BRAND_WIDTH: usize = "Milo  ".len() + VERSION.len();
const MENU: &[&str] = &["Settings", "Doctor", "Uninstall"];
const MENU_WIDTH: usize = 11;
const CONFIRM: &[&str] = &["Cancel", "Uninstall Milo"];
const CONFIRM_NOTE: &str =
    "Removes Milo and its managed Rex and Claude Code integrations. Saved preferences and open panes will stay.";

/// Draws the largest prepared sprite that fits, centered within the supplied rectangle.
pub(crate) fn mascot(s: &mut Screen, x: usize, y: usize, width: usize, height: usize) {
    let width = width.min(s.w.saturating_sub(x));
    let height = height.min(s.h.saturating_sub(y));
    if let Some(art) = Sprite::fit(width, height) {
        art.draw(s, x + (width - art.width()) / 2, y + (height - art.height()) / 2);
    }
}

pub(crate) struct Home {
    view: View,
    selected: usize,
    picker: Picker,
    footer: HintHits<'static>,
    transition_click: crate::keys::ConfirmClick,
}

enum View {
    Menu,
    Settings(Box<Setup>),
    Report(Report),
    Confirm { selected: usize },
}

impl Home {
    pub(crate) fn new() -> Self {
        Self {
            view: View::Menu,
            selected: 0,
            picker: Picker::default(),
            footer: HintHits::default(),
            transition_click: Default::default(),
        }
    }

    fn show(&mut self, view: View) -> After {
        // Clicks received before the next draw must not hit controls from the previous view.
        self.picker = Picker::default();
        self.footer = HintHits::default();
        self.transition_click.clear();
        self.view = view;
        After::Stay
    }

    fn show_guarded(&mut self, view: View, click: Option<(usize, usize)>) -> After {
        let after = self.show(view);
        if let Some((x, y)) = click {
            self.transition_click.opened(x, y);
        }
        after
    }

    fn footer_hit(&self, x: usize, y: usize) -> Option<(Key, Range<usize>)> {
        let (index, _, span) = self.footer.hit(x, y)?;
        let key = match (&self.view, index) {
            (View::Menu | View::Confirm { .. }, 2) | (View::Report(_), 1) => Key::Esc,
            // A pending keyboard move must not activate a different option than the one shown.
            (View::Menu, 0) if self.selected == self.picker.selected => Key::Enter,
            (View::Confirm { selected }, 1) if *selected == self.picker.selected => Key::Enter,
            _ => return None,
        };
        Some((key, span))
    }

    fn click(&mut self, x: usize, y: usize) -> Option<Key> {
        if let Some((key, _)) = self.footer_hit(x, y) {
            return Some(key);
        }
        let (selected, labels) = match &mut self.view {
            View::Menu => (&mut self.selected, MENU),
            View::Confirm { selected } => (selected, CONFIRM),
            _ => return None,
        };
        let (index, _) = self.picker.row_at(x, y, labels)?;
        *selected = index;
        Some(Key::Enter)
    }

    fn draw_menu(&mut self, s: &mut Screen, body: usize) {
        const GAP: usize = 5;
        let width = s.w.saturating_sub(MENU_WIDTH + GAP + 2);
        if let Some(art) = Sprite::fit(width, body.saturating_sub(2)).filter(|_| width >= BRAND_WIDTH) {
            let brand_width = art.width().max(BRAND_WIDTH);
            let x = (s.w - (brand_width + GAP + MENU_WIDTH)) / 2;
            let y = (body - art.height() - 2) / 2;
            art.draw(s, x + (brand_width - art.width()) / 2, y);
            brand(s, x, y + art.height() + 1, brand_width);

            let rows = 2 * MENU.len() - 1;
            let pos = (x + brand_width + GAP, (body - rows) / 2);
            self.picker = Picker::draw(s, pos, MENU_WIDTH, rows, MENU, self.selected);
        } else {
            // In a narrow or short terminal the actions take priority over the artwork.
            let art = Sprite::fit(s.w.saturating_sub(2), body.saturating_sub(MENU.len() + 2));
            let art_height = art.map_or(0, Sprite::height);
            let show_brand = body > MENU.len();
            let header = art_height + usize::from(show_brand);
            let gap = usize::from(show_brand && body > header + MENU.len());
            let rows = (body - header - gap).min(if art.is_some() { MENU.len() } else { 2 * MENU.len() - 1 });
            let y = (body - header - gap - rows) / 2;
            if let Some(art) = art {
                art.draw(s, (s.w - art.width()) / 2, y);
            }
            if show_brand {
                brand(s, 0, y + art_height, s.w);
            }
            let width = MENU_WIDTH.min(s.w);
            let pos = ((s.w - width) / 2, y + header + gap);
            self.picker = Picker::draw(s, pos, width, rows, MENU, self.selected);
        }
    }
}

impl Pane for Home {
    fn draw(&mut self, s: &mut Screen) {
        if let View::Settings(setup) = &mut self.view {
            setup.draw(s);
            return;
        }
        s.clear();
        self.footer = HintHits::default();
        if s.w == 0 || s.h == 0 {
            self.picker = Picker::default();
            return;
        }
        let footer = s.h > MENU.len();
        let body = s.h - usize::from(footer);
        match &mut self.view {
            View::Menu => {
                self.draw_menu(s, body);
                if footer {
                    self.footer = help::draw_hint(s, "Enter open", &["↑↓ move"], "q quit");
                }
            }
            View::Report(report) => self.footer = report.draw(s),
            View::Confirm { selected } => {
                self.picker = draw_confirm(s, body, *selected);
                if footer {
                    self.footer = help::draw_hint(s, "↑↓ move", &["Enter choose"], "Esc back");
                }
            }
            View::Settings(_) => unreachable!("settings draws its own frame"),
        }
    }

    fn tracks_hover(&self) -> bool {
        true
    }

    fn hover_span(&self, x: usize, y: usize) -> Option<Range<usize>> {
        let row = match &self.view {
            View::Settings(setup) => return setup.hover_span(x, y),
            View::Menu => self.picker.row_at(x, y, MENU),
            View::Confirm { .. } => self.picker.row_at(x, y, CONFIRM),
            View::Report(_) => None,
        };
        row.map(|(_, span)| span).or_else(|| self.footer_hit(x, y).map(|(_, span)| span))
    }

    fn help(&self) -> Page {
        use help::{heading, key};
        match &self.view {
            View::Settings(setup) => setup.help(),
            View::Menu => Page::new(vec![
                heading("Milo home"),
                key("↑↓ / k j", "move"),
                key("Enter   ", "open"),
                key("q / Esc ", "quit"),
            ]),
            View::Report(report) => Page::new(vec![
                heading(report.title),
                key("↑↓ / k j", "scroll"),
                key("PgUp/Dn ", "page"),
                key("q / Esc ", "back to home"),
            ]),
            View::Confirm { .. } => Page::new(vec![
                heading("Uninstall Milo"),
                key("↑↓ / k j", "move"),
                key("Enter   ", "choose"),
                key("q / Esc ", "cancel"),
            ]),
        }
    }

    fn key(&mut self, key: Key) -> After {
        let (key, click) = if let Key::Click(x, y) = key {
            if self.transition_click.is_repeat(x, y) {
                return After::Ignored;
            }
            let key = if matches!(self.view, View::Settings(_)) {
                key
            } else {
                let Some(key) = self.click(x, y) else { return After::Ignored };
                key
            };
            (key, Some((x, y)))
        } else {
            (key, None)
        };
        let back = matches!(key, Key::Esc | Key::Char('q'));
        let choose = matches!(key, Key::Enter | Key::Char(' '));
        match &mut self.view {
            View::Menu if back => After::Leave,
            View::Menu if choose => match self.selected {
                0 => self.show_guarded(View::Settings(Box::new(Setup::open())), click),
                1 => self.show(View::Report(Report::new("Milo doctor", doctor::report()))),
                2 => self.show_guarded(View::Confirm { selected: 0 }, click),
                _ => unreachable!("selection stays within the menu"),
            },
            View::Menu => move_selection(&mut self.selected, MENU.len(), key),
            View::Settings(setup) => match setup.key(key) {
                After::Leave => self.show_guarded(View::Menu, click),
                after => after,
            },
            View::Report(_) if back => self.show(View::Menu),
            View::Report(report) => report.scroll(key),
            View::Confirm { selected: 1 } if choose => {
                self.show(View::Report(Report::new("Uninstall Milo", crate::uninstall::run())))
            }
            View::Confirm { .. } if back || choose => self.show(View::Menu),
            View::Confirm { selected } => move_selection(selected, CONFIRM.len(), key),
        }
    }
}

fn brand(s: &mut Screen, x: usize, y: usize, width: usize) {
    let x = x + width.saturating_sub(BRAND_WIDTH) / 2;
    let width = width.min(BRAND_WIDTH);
    let used = s.put(x, y, width, "Milo", HEADING);
    let start = used + 2;
    s.put_fit(x + start, y, width.saturating_sub(start), VERSION, FAINT);
}

#[derive(Clone, Copy, Default)]
struct Picker {
    x: usize,
    y: usize,
    width: usize,
    first: usize,
    len: usize,
    step: usize,
    marker: usize,
    selected: usize,
}

impl Picker {
    fn draw(
        s: &mut Screen,
        (x, y): (usize, usize),
        width: usize,
        rows: usize,
        labels: &[&str],
        selected: usize,
    ) -> Self {
        let len = rows.min(labels.len());
        let first = selected.saturating_sub(len.saturating_sub(1));
        let step = if rows >= 2 * labels.len() - 1 { 2 } else { 1 };
        let marker = if width >= labels.iter().map(|label| label.len()).max().unwrap_or(0) + 2 { 2 } else { 0 };
        for (i, &label) in labels.iter().enumerate().skip(first).take(len) {
            let row = y + (i - first) * step;
            let style = if i != selected {
                Style::PLAIN
            } else if marker == 0 {
                HEADING.reverse()
            } else {
                s.put(x, row, 1, "›", Style::fg(screen::BLUE).bold());
                HEADING
            };
            s.put(x + marker, row, width - marker, label, style);
        }
        Self { x, y, width, first, len, step, marker, selected }
    }

    fn row_at(self, x: usize, y: usize, labels: &[&str]) -> Option<(usize, Range<usize>)> {
        let row = y.checked_sub(self.y)?;
        let i = row.checked_div(self.step)?;
        if i >= self.len || !row.is_multiple_of(self.step) {
            return None;
        }
        let index = self.first + i;
        if self.marker != 0 && x == self.x && self.selected == index {
            return Some((index, x..x + 1));
        }
        let start = self.x + self.marker;
        let span = start..start + labels[index].len().min(self.width - self.marker);
        span.contains(&x).then_some((index, span))
    }
}

fn move_selection(selected: &mut usize, count: usize, key: Key) -> After {
    if let Some(rows) = key.rows(count) {
        let next = selected.saturating_add_signed(rows).min(count - 1);
        if next != *selected {
            *selected = next;
            return After::Stay;
        }
    }
    After::Ignored
}

fn draw_confirm(s: &mut Screen, body: usize, selected: usize) -> Picker {
    let width = (if s.w >= 3 { s.w - 2 } else { s.w }).min(54);
    let x = (s.w - width) / 2;
    let actions = body.min(CONFIRM.len());
    let title = usize::from(body > actions);
    let detail = wrap(CONFIRM_NOTE, width);
    let details = detail.clone().count().min(body - actions - title);
    let gap = usize::from(details > 0 && body > title + details + actions);
    let y = (body - title - details - gap - actions) / 2;
    if title != 0 {
        s.put_fit(x, y, width, "Uninstall Milo?", HEADING);
    }
    for (i, line) in detail.take(details).enumerate() {
        s.put(x, y + title + i, width, line, Style::PLAIN);
    }
    Picker::draw(s, (x, y + title + details + gap), width.min(16), actions, CONFIRM, selected)
}

struct Report {
    title: &'static str,
    text: String,
    failed: bool,
    top: usize,
    last: usize,
    shown: usize,
}

impl Report {
    fn new(title: &'static str, result: Result<String, String>) -> Self {
        let failed = result.is_err();
        let (Ok(text) | Err(text)) = result;
        Self { title, text, failed, top: 0, last: 0, shown: 0 }
    }

    fn draw(&mut self, s: &mut Screen) -> HintHits<'static> {
        let x = usize::from(s.w >= 3);
        let width = s.w - 2 * x;
        let footer = s.h > 1;
        let y = usize::from(s.h > 2);
        let text = if y > 0 {
            let style = if self.failed { DEL.bold() } else { HEADING };
            s.put_fit(x, 0, width, self.title, style);
            self.text.strip_prefix(self.title).and_then(|text| text.strip_prefix('\n')).unwrap_or(&self.text)
        } else {
            &self.text
        };
        let lines = wrap(text, width);
        let len = lines.clone().count();
        self.shown = s.h - y - usize::from(footer);
        self.last = len.saturating_sub(self.shown);
        self.top = self.top.min(self.last);
        for (row, line) in lines.skip(self.top).take(self.shown).enumerate() {
            s.put(x, y + row, width, line, Style::PLAIN);
        }
        if x != 0 {
            ui::thumb(s, s.w - 1, y, self.shown, len, self.top);
        }
        if footer { help::draw_hint(s, "↑↓ scroll", &[], "Esc back") } else { HintHits::default() }
    }

    fn scroll(&mut self, key: Key) -> After {
        if let Some(rows) = key.rows(self.shown) {
            let next = self.top.saturating_add_signed(rows).min(self.last);
            if next != self.top {
                self.top = next;
                return After::Stay;
            }
        }
        After::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click_on(s: &Screen, label: &str) -> Key {
        s.text()
            .iter()
            .enumerate()
            .find_map(|(y, line)| line.find(label).map(|x| Key::Click(crate::width::str_width(&line[..x]), y)))
            .unwrap()
    }

    #[test]
    fn home_places_the_picker_beside_art_or_below_it() {
        let mut home = Home::new();
        let mut wide = Screen::sized(60, 20);
        home.draw(&mut wide);
        let lines = wide.text();
        let title = lines.iter().find(|line| line.contains("Milo  v")).unwrap();
        let settings = lines.iter().find(|line| line.contains("Settings")).unwrap();
        assert!(settings.chars().position(|ch| ch == 'S') > title.chars().position(|ch| ch == 'M'));

        let mut narrow = Screen::sized(18, 13);
        home.draw(&mut narrow);
        let lines = narrow.text();
        let title = lines.iter().position(|line| line.contains("Milo  v")).unwrap();
        assert!(lines[..title].iter().any(|line| line.contains('█')));
        assert!(lines[title..].iter().any(|line| line.contains("Uninstall")));
    }

    #[test]
    fn uninstall_opens_with_cancel_selected_every_time() {
        let mut home = Home::new();
        let mut screen = Screen::sized(28, 12);
        home.draw(&mut screen);
        assert_eq!(home.key(Key::End), After::Stay);
        assert_eq!(home.key(Key::Enter), After::Stay);
        home.draw(&mut screen);
        assert!(screen.text().iter().any(|line| line.contains("› Cancel")));
        assert_eq!(home.key(Key::Down), After::Stay);
        assert_eq!(home.key(Key::Esc), After::Stay);
        assert_eq!(home.key(Key::Enter), After::Stay);
        assert!(matches!(home.view, View::Confirm { selected: 0 }));
        assert_eq!(home.key(Key::Enter), After::Stay);
        assert!(matches!(home.view, View::Menu));
    }

    #[test]
    fn home_hover_only_matches_drawn_actions() {
        let mut home = Home::new();
        let mut screen = Screen::sized(60, 20);
        home.draw(&mut screen);
        for label in ["Settings", "Doctor", "Uninstall", "Enter open", "q quit"] {
            let Key::Click(x, y) = click_on(&screen, label) else { unreachable!() };
            let end = x + label.len();
            assert_eq!(home.hover_span(x, y), Some(x..end));
            assert_eq!(home.hover_span(end - 1, y), Some(x..end));
            assert_eq!(home.hover_span(end, y), None);
        }
        let Key::Click(x, y) = click_on(&screen, "Settings") else { unreachable!() };
        assert_eq!(home.hover_span(x - 2, y), Some(x - 2..x - 1));
        assert_eq!(home.hover_span(x - 1, y), None);
        for label in ["Milo  v", "↑↓ move"] {
            let Key::Click(x, y) = click_on(&screen, label) else { unreachable!() };
            assert_eq!(home.hover_span(x, y), None);
        }
        assert_eq!(home.selected, 0);
        let mut rail = Screen::sized(28, 12);
        home.draw(&mut rail);
        assert_eq!(rail.text()[11], " Enter open · q quit");
        let Key::Click(x, y) = click_on(&rail, "Enter open") else { unreachable!() };
        assert_eq!(home.click(x, y), Some(Key::Enter));

        let mut narrow = Screen::sized(4, 1);
        home.draw(&mut narrow);
        assert_eq!(narrow.text(), ["Sett"]);
        assert_eq!(home.hover_span(3, 0), Some(0..4));
        assert_eq!(home.hover_span(4, 0), None);
    }

    #[test]
    fn home_and_confirmation_clicks_only_act_on_drawn_controls() {
        let mut home = Home::new();
        let mut screen = Screen::sized(60, 20);
        home.draw(&mut screen);
        let uninstall = click_on(&screen, "Uninstall");
        let old_footer = click_on(&screen, "Enter open");
        assert_eq!(home.key(uninstall), After::Stay);
        assert!(matches!(home.view, View::Confirm { selected: 0 }));
        assert_eq!(home.key(uninstall), After::Ignored);
        assert_eq!(home.key(old_footer), After::Ignored);

        home.draw(&mut screen);
        let cancel = click_on(&screen, "Cancel");
        let Key::Click(x, y) = cancel else { unreachable!() };
        assert_eq!(home.hover_span(x, y), Some(x..x + "Cancel".len()));
        assert_eq!(home.key(Key::Click(x + "Cancel".len(), y)), After::Ignored);
        let Key::Click(x, y) = click_on(&screen, "Enter choose") else { unreachable!() };
        assert_eq!(home.hover_span(x, y), Some(x..x + "Enter choose".len()));
        assert_eq!(home.key(Key::Down), After::Stay);
        // Test the mapping without risking a real uninstall if the guard regresses.
        assert_eq!(home.click(x, y), None);
        assert_eq!(home.hover_span(x, y), None);
        assert!(matches!(home.view, View::Confirm { selected: 1 }));
        assert_eq!(home.key(Key::Up), After::Stay);
        assert_eq!(home.key(cancel), After::Stay);
        assert!(matches!(home.view, View::Menu));

        home.draw(&mut screen);
        assert_eq!(home.key(click_on(&screen, "Enter open")), After::Stay);
        home.draw(&mut screen);
        assert_eq!(home.key(click_on(&screen, "Esc back")), After::Stay);
        assert!(matches!(home.view, View::Menu));
        home.draw(&mut screen);
        assert_eq!(home.key(click_on(&screen, "q quit")), After::Leave);
    }

    #[test]
    fn a_one_row_terminal_keeps_the_selected_action_visible() {
        let mut home = Home::new();
        let mut screen = Screen::sized(9, 1);
        home.draw(&mut screen);
        assert_eq!(screen.text(), ["Settings"]);
        assert_eq!(home.key(Key::Down), After::Stay);
        home.draw(&mut screen);
        assert_eq!(screen.text(), ["Doctor"]);
        assert_eq!(home.key(Key::End), After::Stay);
        home.draw(&mut screen);
        assert_eq!(screen.text(), ["Uninstall"]);
    }

    #[test]
    fn reports_wrap_scroll_and_back_out() {
        let mut home = Home::new();
        home.show(View::Report(Report::new("Milo doctor", Ok("Milo doctor\nReads your settings.\nLast line.".into()))));
        let mut screen = Screen::sized(16, 4);
        home.draw(&mut screen);
        assert!(screen.text()[1].contains("Reads your"));
        assert_eq!(home.key(Key::End), After::Stay);
        home.draw(&mut screen);
        assert!(screen.text()[2].contains("Last line."));
        let Key::Click(x, y) = click_on(&screen, "Esc back") else { unreachable!() };
        assert_eq!(home.hover_span(x, y), Some(x..x + "Esc back".len()));
        assert_eq!(home.key(click_on(&screen, "Esc back")), After::Stay);
        assert!(matches!(home.view, View::Menu));
    }

    #[test]
    fn mascot_respects_the_caller_rectangle() {
        let mut screen = Screen::sized(48, 20);
        mascot(&mut screen, 5, 3, 15, 7);
        let lines = screen.text();
        assert!(lines[3..10].iter().any(|line| line.contains('█')));
        for (row, line) in lines.iter().enumerate() {
            for (col, ch) in line.chars().enumerate() {
                assert!(ch == ' ' || (3..10).contains(&row) && (5..20).contains(&col));
            }
        }
    }
}
