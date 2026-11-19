//! Guides first-time users without changing settings until they choose Save or Skip.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::help::{self, Page};
use crate::keys::Key;
use crate::pane::{After, Pane};
use crate::preferences::{IconMode, Preferences};
use crate::screen::{CYAN, Screen, Style};
use crate::ui::{DEL, FAINT, HEADING};
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
    let state = paths::state_dir().ok_or(NO_STATE)?;
    let config = paths::config_dir().ok_or(NO_CONFIG)?;
    needs_welcome_in(&state, &config).map_err(|error| format!("Could not check Milo's setup state: {error}"))
}

fn needs_welcome_in(state: &Path, config: &Path) -> io::Result<bool> {
    for name in [COMPLETE, LEGACY_OFFER, LAYOUT_DISABLED, RELOAD_PENDING, OWN_CLAUDE] {
        if present(&state.join(name))? {
            return Ok(false);
        }
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
    mark_complete(&paths::state_dir().ok_or(NO_STATE)?)
}

fn mark_complete(state: &Path) -> Result<(), String> {
    fn mark(state: &Path) -> io::Result<()> {
        fs::create_dir_all(state)?;
        fs_ops::open_dir(state)?;
        let path = state.join(COMPLETE);
        match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
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

type OptionRow = (&'static str, &'static str, Action);
const SKIP: OptionRow = ("Skip setup", "Keep settings as they are.", Action::Skip);
const REX_INTRO: &[OptionRow] = &[("Continue", "Choose a layout for Rex.", Action::Choose), SKIP];
const ICONS_INTRO: &[OptionRow] = &[("Continue", "Choose icons for your terminal.", Action::Choose), SKIP];
const LAYOUTS: &[OptionRow] = &[
    ("Save tree layout", "Just the file tree beside your shell.", Action::Layout { tree: true }),
    ("Save project sidebar", "Files, commits and usage beside your shell.", Action::Layout { tree: false }),
    SKIP,
];
const ICONS: &[OptionRow] = &[
    ("Save Nerd Font icons", "Use symbols from a Nerd Font.", Action::Icons(IconMode::Nerd)),
    ("Save plain icons", "Use symbols most terminals can show.", Action::Icons(IconMode::Plain)),
    SKIP,
];
const DONE: &[OptionRow] = &[("Continue", "", Action::Done)];

struct HitRows {
    x: usize,
    y: usize,
    w: usize,
    stride: usize,
    start: usize,
    count: usize,
}

impl HitRows {
    fn index_at(&self, x: usize, y: usize) -> Option<usize> {
        let row = y.checked_sub(self.y)? / self.stride;
        (row < self.count && x.checked_sub(self.x)? < self.w).then_some(self.start + row)
    }
}

pub struct Welcome {
    mode: Mode,
    step: Step,
    selected: usize,
    notice: Option<String>,
    hit: Option<HitRows>,
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
        Self { mode, step: Step::Intro, selected: 0, notice: None, hit: None }
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
    }

    fn error(&mut self, error: String) -> After {
        self.notice = Some(error);
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
        let heading = usize::from(h > 1);
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
        let start = (self.selected + 1).saturating_sub(count);
        let indent = if w >= 3 { 2 } else { 0 };
        for (i, (label, detail, _)) in rows[start..start + count].iter().enumerate() {
            let row = first + i * stride;
            let selected = start + i == self.selected;
            let style = if selected { HEADING } else { Style::PLAIN };
            if selected && indent > 0 {
                s.put(x, row, indent, "›", Style::fg(CYAN).bold());
            }
            s.put_fit(x + indent, row, w - indent, label, style);
            if stride > 1 {
                s.put_fit(x + indent, row + 1, w - indent, detail, FAINT);
            }
        }
        for (i, line) in notice.enumerate() {
            s.put(x, first + count * stride + i, w, line, DEL.bold());
        }
        self.hit = Some(HitRows { x, y: first, w, stride, start, count });
    }
}

fn save_icons(icons: IconMode) -> Result<(), String> {
    let mut prefs = Preferences::load(None).map_err(|error| format!("Could not read pane preferences: {error}"))?;
    prefs.icons = icons;
    prefs.save(None).map_err(|error| format!("Could not save pane preferences: {error}"))
}

fn brand(s: &mut Screen, x: usize, y: usize, w: usize, h: usize) {
    const NAME: &str = concat!("milo  v", env!("CARGO_PKG_VERSION"));
    home::mascot(s, x, y, w, h);
    s.put_fit(x + w.saturating_sub(NAME.len()) / 2, y + h, w, NAME, HEADING);
}

impl Pane for Welcome {
    fn draw(&mut self, s: &mut Screen) {
        let (w, h) = (s.w, s.h);
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
            let last = match self.step {
                Step::Intro => "q skip",
                Step::Choose => "Esc back",
                Step::Saved => "Enter continue",
            };
            if self.step == Step::Saved || w < 21 {
                s.put_fit(usize::from(w > 2), h - 1, w.saturating_sub(2), last, FAINT);
            } else {
                help::draw_hint(s, "Enter choose", &["↑↓ move"], last);
            }
        }
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
        if let Some(delta) = key.rows(rows.len()) {
            let selected = self.selected.saturating_add_signed(delta).min(rows.len() - 1);
            if selected != self.selected {
                self.selected = selected;
                return After::Stay;
            }
            return After::Ignored;
        }
        match key {
            Key::Enter | Key::Char(' ') => self.act(rows[self.selected].2),
            Key::Click(x, y) => {
                if let Some(index) = self.hit.as_ref().and_then(|hit| hit.index_at(x, y)) {
                    self.selected = index;
                    After::Stay
                } else {
                    After::Ignored
                }
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

    #[test]
    fn first_launch_is_read_only_until_explicit_skip() {
        let fixture = Fixture::folder();
        let state = fixture.root.join("state");
        let config = fixture.root.join("config");
        assert!(needs_welcome_in(&state, &config).unwrap());
        let mut welcome = Welcome::new(Mode::Icons(state.clone()));
        assert_eq!(welcome.key(Key::Enter), After::Stay);
        assert_eq!(welcome.key(Key::Esc), After::Stay);
        assert!(!state.exists());
        assert_eq!(welcome.key(Key::Down), After::Stay);
        assert_eq!(welcome.key(Key::Enter), After::Leave);
        assert!(!config.exists());
        assert!(!needs_welcome_in(&state, &config).unwrap());
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
            assert!(needs_welcome_in(&state, &config).unwrap());
            fs::write(&path, []).unwrap();
            assert!(!needs_welcome_in(&state, &config).unwrap());
            fs::remove_file(path).unwrap();
        }
    }

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
