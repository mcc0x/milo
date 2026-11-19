//! Milo's home picker, with settings and maintenance in the same terminal session.

mod mascot;

use crate::help::{self, Page};
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
}

enum View {
    Menu,
    Settings(Box<Setup>),
    Report(Report),
    Confirm { selected: usize },
}

impl Home {
    pub(crate) fn new() -> Self {
        Self { view: View::Menu, selected: 0, picker: Picker::default() }
    }

    fn show(&mut self, view: View) -> After {
        // Mouse input received before the next draw must not hit the previous page's picker.
        self.picker = Picker::default();
        self.view = view;
        After::Stay
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
                    help::draw_hint(s, "↑↓ move", &["Enter open"], "q quit");
                }
            }
            View::Report(report) => report.draw(s),
            View::Confirm { selected } => {
                self.picker = draw_confirm(s, body, *selected);
                if footer {
                    help::draw_hint(s, "↑↓ move", &["Enter choose"], "Esc back");
                }
            }
            View::Settings(_) => unreachable!("settings draws its own frame"),
        }
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
        let back = matches!(key, Key::Esc | Key::Char('q'));
        let choose = matches!(key, Key::Enter | Key::Char(' '));
        match &mut self.view {
            View::Menu if back => After::Leave,
            View::Menu if choose => self.show(match self.selected {
                0 => View::Settings(Box::new(Setup::open())),
                1 => View::Report(Report::new("Milo doctor", doctor::report())),
                2 => View::Confirm { selected: 0 },
                _ => unreachable!("selection stays within the menu"),
            }),
            View::Menu => self.picker.select(&mut self.selected, MENU.len(), key),
            View::Settings(setup) => match setup.key(key) {
                After::Leave => self.show(View::Menu),
                after => after,
            },
            View::Report(_) if back => self.show(View::Menu),
            View::Report(report) => report.scroll(key),
            View::Confirm { selected: 1 } if choose => {
                self.show(View::Report(Report::new("Uninstall Milo", crate::uninstall::run())))
            }
            View::Confirm { .. } if back || choose => self.show(View::Menu),
            View::Confirm { selected } => self.picker.select(selected, CONFIRM.len(), key),
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
        Self { x, y, width, first, len, step }
    }

    fn select(self, selected: &mut usize, count: usize, key: Key) -> After {
        let next = match key {
            Key::Click(x, y) if self.len > 0 && x >= self.x && x - self.x < self.width && y >= self.y => {
                let row = y - self.y;
                let i = row / self.step;
                (i < self.len && row.is_multiple_of(self.step)).then_some(self.first + i)
            }
            _ => key.rows(count).map(|rows| selected.saturating_add_signed(rows).min(count - 1)),
        };
        if let Some(next) = next
            && next != *selected
        {
            *selected = next;
            return After::Stay;
        }
        After::Ignored
    }
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

    fn draw(&mut self, s: &mut Screen) {
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
        if footer {
            help::draw_hint(s, "↑↓ scroll", &[], "Esc back");
        }
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
        assert_eq!(home.key(Key::Esc), After::Stay);
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
