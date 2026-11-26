//! Guides first-time users without changing settings until they choose Save or Skip.

use std::fs::{self, OpenOptions};
use std::io;
use std::ops::Range;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::help::{self, HintHits, Page};
use crate::keys::{ConfirmClick, Key};
use crate::pane::{After, Pane};
use crate::preferences::{IconMode, Preferences};
use crate::screen::{CYAN, Screen, Style};
use crate::ui::{self, DEL, FAINT, HEADING};
use crate::{fs_ops, home, paths, rex};

use super::claude::OWN_CLAUDE;
use super::install::{RELOAD_PENDING, Saved, Store};
use super::profiles::Scope;
use super::rex_files::LAYOUT_DISABLED;
use super::wrap;

const COMPLETE: &str = "welcome-complete";
const LEGACY_OFFER: &str = "setup-offered";
const NO_STATE: &str = "Could not find Milo's state folder.";
const NO_CONFIG: &str = "Could not find Milo's settings folder.";

/// Checking whether to show the guide does not create anything.
pub fn needs_welcome() -> Result<bool, String> {
    if cfg!(windows) {
        return Err("milo setup is not supported on Windows yet.".into());
    }
    let state = paths::state_dir().ok_or(NO_STATE)?;
    let config = paths::config_dir().ok_or(NO_CONFIG)?;
    let claude_state = paths::claude().and_then(|paths| paths.state_override(&state));
    needs_welcome_in(&state, &config, claude_state.as_deref())
        .map_err(|error| format!("Could not check Milo's setup state: {error}"))
}

fn needs_welcome_in(state: &Path, config: &Path, claude_state: Option<&Path>) -> io::Result<bool> {
    for name in [COMPLETE, LEGACY_OFFER, LAYOUT_DISABLED, RELOAD_PENDING, OWN_CLAUDE] {
        if present(&state.join(name))? {
            return Ok(false);
        }
    }
    if let Some(dir) = claude_state
        && present(&dir.join(OWN_CLAUDE))?
    {
        return Ok(false);
    }
    for name in ["preferences.conf", "rex.lua", "listener.lua"] {
        if present(&config.join(name))? {
            return Ok(false);
        }
    }
    // A worktree may have its own saved choices without any global settings.
    for (dir, prefs) in [("worktrees", true), ("layouts", false)] {
        let files = match fs::read_dir(config.join(dir)) {
            Ok(files) => files,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for file in files {
            let file = file?;
            let name = file.file_name();
            let name = name.as_encoded_bytes();
            let saved =
                if prefs { name.ends_with(b".conf") } else { name == b"global" || name.starts_with(b"worktree-") };
            if saved && file.file_type()?.is_file() {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Skip without opening a terminal or changing any user preferences.
pub fn skip_welcome() -> Result<(), String> {
    if cfg!(windows) {
        return Err("milo setup is not supported on Windows yet.".into());
    }
    mark_complete(&paths::state_dir().ok_or(NO_STATE)?)
}

fn mark_complete(state: &Path) -> Result<(), String> {
    fn mark(state: &Path) -> io::Result<()> {
        fs::create_dir_all(state)?;
        fs_ops::open_dir(state)?;
        let path = state.join(COMPLETE);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && fs::symlink_metadata(path)?.is_file() => {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    mark(state).map_err(|error| format!("Could not save welcome status: {error}"))
}

enum Mode {
    Rex(Store),
    Icons(PathBuf),
}

impl Mode {
    fn state(&self) -> &Path {
        match self {
            Self::Rex(store) => &store.state,
            Self::Icons(state) => state,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Intro,
    Choose,
    Saved,
}

#[derive(Clone, Copy)]
enum Action {
    Choose,
    Skip,
    Layout { tree: bool },
    Icons(IconMode),
    Done,
}

type OptionRow = (&'static str, Option<&'static str>, &'static str, Action);
const SKIP: OptionRow = ("Skip setup", None, "Keep settings as they are.", Action::Skip);
const REX_INTRO: &[OptionRow] = &[("Continue", None, "Choose a layout for Rex.", Action::Choose), SKIP];
const ICONS_INTRO: &[OptionRow] = &[("Continue", None, "Choose icons for your terminal.", Action::Choose), SKIP];
const LAYOUTS: &[OptionRow] = &[
    ("Save tree layout", Some("Tree layout"), "Just the file tree beside your shell.", Action::Layout { tree: true }),
    (
        "Save project sidebar",
        Some("Project sidebar"),
        "Files, commits and usage beside your shell.",
        Action::Layout { tree: false },
    ),
    SKIP,
];
const ICONS: &[OptionRow] = &[
    ("Save Nerd Font icons", Some("Nerd Font"), "Use symbols from a Nerd Font.", Action::Icons(IconMode::Nerd)),
    ("Save plain icons", Some("Plain icons"), "Use symbols most terminals can show.", Action::Icons(IconMode::Plain)),
    SKIP,
];
const DONE: &[OptionRow] = &[("Continue", None, "", Action::Done)];
const CHOOSE_HINT: &str = "Enter choose";

fn option_label((label, compact, ..): &OptionRow, room: usize) -> &'static str {
    if label.len() <= room { label } else { compact.unwrap_or(label) }
}

struct HitRows {
    x: usize,
    y: usize,
    w: usize,
    stride: usize,
    start: usize,
    count: usize,
    indent: usize,
    selected: usize,
}

impl HitRows {
    fn row_at(&self, x: usize, y: usize, rows: &[OptionRow]) -> Option<(usize, Range<usize>)> {
        let y = y.checked_sub(self.y)?;
        let row = y / self.stride;
        if row >= self.count {
            return None;
        }
        let index = self.start + row;
        let item = &rows[index];
        let label_line = y.is_multiple_of(self.stride);
        if label_line && self.indent != 0 && x == self.x && self.selected == index {
            return Some((index, x..x + 1));
        }
        let room = self.w - self.indent;
        let text = if label_line { option_label(item, room) } else { item.2 };
        let start = self.x + self.indent;
        let span = start..start + text.len().min(room);
        span.contains(&x).then_some((index, span))
    }
}

pub struct Welcome {
    mode: Mode,
    step: Step,
    selected: usize,
    notice: Option<String>,
    hit: Option<HitRows>,
    footer: HintHits<'static>,
    confirm_click: ConfirmClick,
}

impl Welcome {
    pub fn open() -> Result<Self, String> {
        let mode = if rex::inside() {
            Mode::Rex(Store::real().ok_or(NO_CONFIG)?)
        } else {
            paths::config_dir().ok_or(NO_CONFIG)?;
            Mode::Icons(paths::state_dir().ok_or(NO_STATE)?)
        };
        Ok(Self::new(mode))
    }

    fn new(mode: Mode) -> Self {
        Self {
            mode,
            step: Step::Intro,
            selected: 0,
            notice: None,
            hit: None,
            footer: HintHits::default(),
            confirm_click: ConfirmClick::default(),
        }
    }

    fn rows(&self) -> &'static [OptionRow] {
        match (self.step, &self.mode) {
            (Step::Intro, Mode::Rex(_)) => REX_INTRO,
            (Step::Intro, Mode::Icons(_)) => ICONS_INTRO,
            (Step::Choose, Mode::Rex(_)) => LAYOUTS,
            (Step::Choose, Mode::Icons(_)) => ICONS,
            (Step::Saved, _) => DONE,
        }
    }

    fn show(&mut self, step: Step, notice: Option<String>) {
        self.step = step;
        self.selected = 0;
        self.notice = notice;
        self.hit = None;
        self.footer = HintHits::default();
        self.confirm_click.clear();
    }

    fn footer_last(&self) -> (&'static str, Key) {
        match self.step {
            Step::Intro => ("q skip", Key::Char('q')),
            Step::Choose => ("Esc back", Key::Esc),
            Step::Saved => ("Enter continue", Key::Enter),
        }
    }

    fn footer_hit(&self, x: usize, y: usize) -> Option<(Key, Range<usize>)> {
        let (_, hint, span) = self.footer.hit(x, y)?;
        let (last, key) = self.footer_last();
        match hint {
            hint if hint == last => Some((key, span)),
            CHOOSE_HINT if self.hit.as_ref().is_some_and(|hit| hit.selected == self.selected) => {
                Some((Key::Enter, span))
            }
            _ => None,
        }
    }

    fn error(&mut self, error: String) -> After {
        self.notice = Some(error);
        self.hit = None;
        self.footer = HintHits::default();
        After::Stay
    }

    fn act(&mut self, action: Action) -> After {
        let listener_warning = match action {
            Action::Choose => {
                self.show(Step::Choose, None);
                return After::Stay;
            }
            Action::Skip => {
                return match mark_complete(self.mode.state()) {
                    Ok(()) => After::Leave,
                    Err(error) => self.error(error),
                };
            }
            Action::Layout { tree } => {
                let Mode::Rex(store) = &self.mode else { unreachable!("Rex choices only appear inside Rex") };
                let layout = super::layout_preset(tree, false);
                match store.save_for(&layout, Scope::Global, None, None, &rex::run) {
                    Ok(Saved::Ready) => None,
                    Ok(Saved::ListenerDidNotStart) => {
                        Some("The Rex listener did not start. Open Health in Settings to repair it.")
                    }
                    Ok(Saved::ListenerDidNotStop) => {
                        Some("The Rex listener did not stop. Open Health in Settings to repair it.")
                    }
                    Err(error) => return self.error(error.into()),
                }
            }
            Action::Icons(icons) => {
                if let Err(error) = save_icons(icons) {
                    return self.error(error);
                }
                None
            }
            Action::Done => return After::Leave,
        };
        let mark_warning = mark_complete(self.mode.state()).err();
        let warning = match (listener_warning, mark_warning) {
            (Some(listener), Some(mark)) => Some(format!("{listener}\n{mark}")),
            (Some(listener), None) => Some(listener.into()),
            (None, mark) => mark,
        };
        if let Some(warning) = warning {
            self.show(Step::Saved, Some(warning));
            After::Stay
        } else {
            After::Leave
        }
    }

    fn title(&self) -> &'static str {
        match (self.step, &self.mode) {
            (Step::Intro, _) => "Welcome to Milo",
            (Step::Choose, Mode::Rex(_)) => "Choose a Rex layout",
            (Step::Choose, Mode::Icons(_)) => "Choose pane icons",
            (Step::Saved, _) => "Setup saved",
        }
    }

    fn description(&self) -> &'static str {
        match (self.step, &self.mode) {
            (Step::Intro, _) => "Files, commits and usage in your terminal.",
            (Step::Choose, Mode::Rex(_)) => {
                "Save a default for all projects. Run Milo: Arrange This Tab in Rex to use it."
            }
            (Step::Choose, Mode::Icons(_)) => "Save your default icon style. Change it any time in Settings.",
            (Step::Saved, _) => "",
        }
    }

    fn picker(&mut self, s: &mut Screen, x: usize, y: usize, w: usize, h: usize) {
        let rows = self.rows();
        let heading = usize::from(h > rows.len());
        let stride = if h >= rows.len() * 2 + 5 && w >= 26 { 2 } else { 1 };
        let notice_room = (h - heading).saturating_sub(rows.len()).min(4);
        let notice = wrap(self.notice.as_deref().unwrap_or(""), w).take(notice_room);
        let notice_h = notice.clone().count();
        let available = h - heading - notice_h;
        let count = rows.len().min(available / stride);
        let spare = available - count * stride;
        let description = wrap(self.description(), w).take(spare.saturating_sub(1).min(3));
        let description_h = description.clone().count();
        let gap = usize::from(spare > description_h);
        let used = heading + description_h + gap + count * stride + notice_h;
        let top = y + (h - used) / 2;
        if heading != 0 {
            s.put_fit(x, top, w, self.title(), HEADING);
        }
        for (i, line) in description.enumerate() {
            s.put(x, top + heading + i, w, line, FAINT);
        }
        let first = top + heading + description_h + gap;
        let start = ui::selection_start(self.selected, rows.len(), count);
        let indent = if w >= 6 { 2 } else { 0 };
        let room = w - indent;
        for (i, item) in rows[start..start + count].iter().enumerate() {
            let row = first + i * stride;
            let selected = start + i == self.selected;
            let style = if !selected {
                Style::PLAIN
            } else if indent == 0 {
                HEADING.reverse()
            } else {
                HEADING
            };
            if selected && indent > 0 {
                s.put(x, row, indent, "›", Style::fg(CYAN).bold());
            }
            let label = option_label(item, room);
            if room == 1 {
                s.put(x + indent, row, room, label, style);
            } else {
                s.put_fit(x + indent, row, room, label, style);
            }
            if stride > 1 {
                s.put_fit(x + indent, row + 1, room, item.2, FAINT);
            }
        }
        for (i, line) in notice.enumerate() {
            s.put(x, first + count * stride + i, w, line, DEL.bold());
        }
        self.hit = Some(HitRows { x, y: first, w, stride, start, count, indent, selected: self.selected });
    }
}

fn save_icons(icons: IconMode) -> Result<(), String> {
    Preferences::set_icons(icons).map_err(|error| format!("Could not save pane preferences: {error}"))
}

fn brand(s: &mut Screen, x: usize, y: usize, w: usize, h: usize) {
    const NAME: &str = concat!("milo  v", env!("CARGO_PKG_VERSION"));
    home::mascot(s, x, y, w, h);
    s.put_fit(x + w.saturating_sub(NAME.len()) / 2, y + h, w, NAME, HEADING);
}

impl Pane for Welcome {
    fn draw(&mut self, s: &mut Screen) {
        s.clear();
        let (w, h) = (s.w, s.h);
        self.footer = HintHits::default();
        if w == 0 || h == 0 {
            self.hit = None;
            return;
        }
        let footer = usize::from(h > 2);
        let body = h - footer;
        if w >= 76 && h >= 11 {
            let art_width = 32;
            let art_height = body.saturating_sub(3).min(16);
            let gap = 4;
            let picker_width = (w - art_width - gap - 4).min(48);
            let x = (w - art_width - gap - picker_width) / 2;
            brand(s, x, (body - art_height - 1) / 2, art_width, art_height);
            self.picker(s, x + art_width + gap, 0, picker_width, body);
        } else {
            let width = (w - usize::from(w > 4) * 2).min(48);
            let x = (w - width) / 2;
            let top = if w >= 20 && body >= 16 {
                let art_height = (body - 11).min(11);
                brand(s, x, 0, width, art_height);
                art_height + 2
            } else {
                0
            };
            self.picker(s, x, top, width, body - top);
        }
        if footer != 0 {
            let (last, _) = self.footer_last();
            if self.step == Step::Saved || w < 21 {
                self.footer = help::draw_hint(s, "", &[], last);
            } else {
                self.footer = help::draw_hint(s, CHOOSE_HINT, &["↑↓ move"], last);
            }
        }
    }

    fn tracks_hover(&self) -> bool {
        true
    }

    fn hover_span(&self, x: usize, y: usize) -> Option<Range<usize>> {
        self.hit
            .as_ref()
            .and_then(|hit| hit.row_at(x, y, self.rows()))
            .map(|(_, span)| span)
            .or_else(|| self.footer_hit(x, y).map(|(_, span)| span))
    }

    fn help(&self) -> Page {
        use help::{heading, key};
        Page::new(vec![
            heading("Welcome keys"),
            key("↑↓    ", "move (k j)"),
            key("Enter ", "choose"),
            key("Esc   ", "back"),
            key("q     ", "skip setup"),
        ])
    }

    fn key(&mut self, key: Key) -> After {
        let rows = self.rows();
        let (key, click) = if let Key::Click(x, y) = key {
            let row = self.hit.as_ref().and_then(|hit| hit.row_at(x, y, rows));
            let key = if row.is_some() {
                Key::Enter
            } else {
                let Some((key, _)) = self.footer_hit(x, y) else { return After::Ignored };
                key
            };
            if self.step == Step::Choose && key == Key::Enter && self.confirm_click.is_repeat(x, y) {
                return After::Ignored;
            }
            if let Some((index, _)) = row {
                self.selected = index;
            }
            (key, Some((x, y)))
        } else {
            (key, None)
        };
        let shown = self.hit.as_ref().map_or(0, |hit| hit.count);
        if let Some(delta) = ui::selection_delta(key, shown) {
            let selected = self.selected.saturating_add_signed(delta).min(rows.len() - 1);
            if selected != self.selected {
                self.selected = selected;
                return After::Stay;
            }
            return After::Ignored;
        }
        match key {
            Key::Enter | Key::Char(' ') => {
                let step = self.step;
                let after = self.act(rows[self.selected].3);
                if step == Step::Intro
                    && self.step == Step::Choose
                    && let Some((x, y)) = click
                {
                    self.confirm_click.opened(x, y);
                }
                after
            }
            Key::Esc if self.step == Step::Choose => {
                self.show(Step::Intro, None);
                After::Stay
            }
            Key::Esc | Key::Char('q') if self.step == Step::Saved => After::Leave,
            Key::Char('q') => self.act(Action::Skip),
            _ => After::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    fn click_on(s: &Screen, label: &str) -> Key {
        s.text()
            .iter()
            .enumerate()
            .find_map(|(y, line)| line.find(label).map(|x| Key::Click(crate::width::str_width(&line[..x]), y)))
            .unwrap()
    }

    #[test]
    fn welcome_hover_only_matches_drawn_actions() {
        let fixture = Fixture::folder();
        let mut welcome = Welcome::new(Mode::Icons(fixture.root.join("state")));
        let mut screen = Screen::sized(48, 14);
        welcome.draw(&mut screen);
        for label in ["Continue", "Choose icons for your terminal.", CHOOSE_HINT, "q skip"] {
            let Key::Click(x, y) = click_on(&screen, label) else { unreachable!() };
            let end = x + label.len();
            assert_eq!(welcome.hover_span(x, y), Some(x..end));
            assert_eq!(welcome.hover_span(end - 1, y), Some(x..end));
            assert_eq!(welcome.hover_span(end, y), None);
        }
        let Key::Click(x, y) = click_on(&screen, "Continue") else { unreachable!() };
        assert_eq!(welcome.hover_span(x - 2, y), Some(x - 2..x - 1));
        assert_eq!(welcome.hover_span(x - 1, y), None);
        for label in ["Welcome to Milo", "↑↓ move"] {
            let Key::Click(x, y) = click_on(&screen, label) else { unreachable!() };
            assert_eq!(welcome.hover_span(x, y), None);
        }
        assert_eq!(welcome.selected, 0);

        let mut narrow = Screen::sized(6, 1);
        welcome.draw(&mut narrow);
        assert_eq!(narrow.text(), [" Con…"]);
        assert_eq!(welcome.hover_span(4, 0), Some(1..5));
        assert_eq!(welcome.hover_span(5, 0), None);
    }

    #[test]
    fn welcome_replaces_the_previous_frame_on_a_reused_screen() {
        let fixture = Fixture::folder();
        let mut welcome = Welcome::new(Mode::Icons(fixture.root.join("state")));
        let mut screen = Screen::sized(48, 14);
        welcome.draw(&mut screen);
        assert!(screen.text().iter().any(|line| line.contains("› Continue")));

        for (key, selected) in [(Key::Enter, "› Save Nerd Font icons"), (Key::Down, "› Save plain icons")] {
            assert_eq!(welcome.key(key), After::Stay);
            welcome.draw(&mut screen);
            let mut fresh = Screen::sized(48, 14);
            welcome.draw(&mut fresh);
            let frame = screen.text();
            assert!(frame.iter().any(|line| line.contains(selected)));
            assert_eq!(frame, fresh.text());
        }
    }

    #[test]
    fn welcome_keeps_earlier_choices_visible_and_uses_distinct_labels_in_a_rail() {
        let fixture = Fixture::folder();
        let mut welcome = Welcome::new(Mode::Icons(fixture.root.join("state")));
        assert_eq!(welcome.key(Key::Enter), After::Stay);
        assert_eq!(welcome.key(Key::End), After::Stay);
        let mut rail = Screen::sized(15, 5);
        welcome.draw(&mut rail);
        let rows = rail.text();
        for label in ["Nerd Font", "Plain icons", "› Skip setup"] {
            assert!(rows.iter().any(|row| row.contains(label)), "{rows:?}");
        }
        let Key::Click(x, y) = click_on(&rail, "Plain icons") else { unreachable!() };
        assert_eq!(welcome.hover_span(x, y), Some(x..x + "Plain icons".len()));

        let mut tiny = Screen::sized(1, 1);
        welcome.draw(&mut tiny);
        assert_eq!(tiny.text(), ["S"]);
        assert_eq!(welcome.key(Key::WheelUp), After::Stay);
        welcome.draw(&mut tiny);
        assert_eq!(tiny.text(), ["P"]);
    }

    #[test]
    fn welcome_clicks_only_activate_drawn_rows_and_footer_actions() {
        let fixture = Fixture::folder();
        let state = fixture.root.join("state");
        let mut welcome = Welcome::new(Mode::Icons(state.clone()));
        let mut wide = Screen::sized(48, 14);
        welcome.draw(&mut wide);
        let detail = "Choose icons for your terminal.";
        let click = click_on(&wide, detail);
        let Key::Click(x, y) = click else { unreachable!() };
        assert_eq!(welcome.key(Key::Click(x + detail.len(), y)), After::Ignored);
        assert_eq!(welcome.key(click), After::Stay);
        assert!(welcome.step == Step::Choose);
        assert_eq!(welcome.key(click), After::Ignored);
        assert_eq!(welcome.key(click_on(&wide, "q skip")), After::Ignored);
        let Key::Click(x, y) = click_on(&wide, "q skip") else { unreachable!() };
        assert_eq!(welcome.hover_span(x, y), None);

        let mut compact = Screen::sized(13, 5);
        welcome.draw(&mut compact);
        assert_eq!(welcome.key(click_on(&compact, "Esc back")), After::Stay);
        assert!(welcome.step == Step::Intro);
        assert!(!state.exists());

        welcome.draw(&mut wide);
        assert_eq!(welcome.key(click_on(&wide, CHOOSE_HINT)), After::Stay);
        assert!(welcome.step == Step::Choose);
        let mut choose = Screen::sized(48, 14);
        welcome.draw(&mut choose);
        assert_eq!(welcome.key(click_on(&choose, "Skip setup")), After::Leave);
        assert!(state.join(COMPLETE).exists());
    }

    #[test]
    fn repeated_continue_click_cannot_save_after_a_redraw() {
        let fixture = Fixture::folder();
        let state = fixture.root.join("state");
        // If a save is attempted, it must fail before invoking Rex.
        fs::write(&state, []).unwrap();
        for (w, h, opener, target) in [(24, 1, "Continue", "Save tree layout"), (48, 14, CHOOSE_HINT, CHOOSE_HINT)] {
            let mut welcome = Welcome::new(Mode::Rex(Store {
                init: fixture.root.join("init.lua"),
                config: fixture.root.join("config"),
                state: state.clone(),
                claude: fixture.root.join("settings.json"),
                claude_state: None,
            }));
            let mut screen = Screen::sized(w, h);
            welcome.draw(&mut screen);
            let click = click_on(&screen, opener);
            assert_eq!(welcome.key(click), After::Stay);
            assert_eq!(welcome.key(click), After::Ignored);

            welcome.draw(&mut screen);
            assert_eq!(click_on(&screen, target), click);
            assert_eq!(welcome.key(click), After::Ignored);
            assert_eq!(welcome.key(click), After::Ignored);
            assert!(welcome.notice.is_none());

            assert_eq!(welcome.key(Key::Enter), After::Stay);
            assert!(welcome.notice.is_some(), "an intentional action should try to save");
        }
    }

    #[test]
    fn first_launch_is_read_only_until_explicit_skip() {
        let fixture = Fixture::folder();
        let state = fixture.root.join("state");
        let config = fixture.root.join("config");
        assert!(needs_welcome_in(&state, &config, None).unwrap());
        let mut welcome = Welcome::new(Mode::Icons(state.clone()));
        assert_eq!(welcome.key(Key::Enter), After::Stay);
        assert_eq!(welcome.key(Key::Esc), After::Stay);
        assert!(!state.exists());
        assert_eq!(welcome.key(Key::Down), After::Stay);
        assert_eq!(welcome.key(Key::Enter), After::Leave);
        assert!(!config.exists());
        assert!(!needs_welcome_in(&state, &config, None).unwrap());
        mark_complete(&state).unwrap();
    }

    #[test]
    fn a_short_terminal_keeps_the_selected_action_and_back_hint() {
        let fixture = Fixture::folder();
        let state = fixture.root.join("state");
        let mut welcome = Welcome::new(Mode::Icons(state.clone()));
        assert_eq!(welcome.key(Key::Enter), After::Stay);
        assert_eq!(welcome.key(Key::Down), After::Stay);
        let mut one_row = Screen::sized(24, 1);
        welcome.draw(&mut one_row);
        assert!(one_row.text()[0].contains("› Save plain icons"));
        let mut short = Screen::sized(24, 5);
        welcome.draw(&mut short);
        assert!(short.text()[4].trim_end().ends_with("Esc back"));
        assert!(!state.exists());
    }

    #[test]
    fn legacy_offers_and_configured_installs_do_not_show_the_guide() {
        let fixture = Fixture::folder();
        let state = fixture.root.join("state");
        let config = fixture.root.join("config");
        for path in [
            state.join(LEGACY_OFFER),
            state.join(LAYOUT_DISABLED),
            state.join(RELOAD_PENDING),
            state.join(OWN_CLAUDE),
            config.join("rex.lua"),
            config.join("preferences.conf"),
            config.join("worktrees/123456789abcdef0.conf"),
            config.join("layouts/worktree-123456789abcdef0"),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            assert!(needs_welcome_in(&state, &config, None).unwrap());
            fs::write(&path, []).unwrap();
            assert!(!needs_welcome_in(&state, &config, None).unwrap());
            fs::remove_file(path).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn skip_does_not_follow_a_symlinked_state_directory() {
        let fixture = Fixture::folder();
        let state = fixture.root.join("state");
        let other = fixture.root.join("other");
        fs::create_dir(&other).unwrap();
        std::os::unix::fs::symlink(&other, &state).unwrap();
        let mut welcome = Welcome::new(Mode::Icons(state));
        assert_eq!(welcome.key(Key::Char('q')), After::Stay);
        assert!(!other.join(COMPLETE).exists());
    }
}
